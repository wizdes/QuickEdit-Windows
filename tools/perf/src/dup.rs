//! Watches one rectangle of the screen through DXGI Desktop Duplication and
//! records each frame in which its pixels change.
//!
//! What the times mean: `present` is the frame's LastPresentTime, the moment
//! the desktop compositor (DWM) put that image on the desktop. Light leaves the
//! display after that, by up to one refresh. Duplication never sees the display
//! itself. Running it can also keep DWM from using a faster path that skips
//! composition, so numbers from here describe the composed path.
use crate::vcall;
use crate::win::{self, Com, Guid, H, Rect, check, qpc};
use qe_perf::{Change, ScreenRect, pixels_differ};
use std::{
    mem, ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

const IID_DXGI_FACTORY1: Guid = Guid {
    a: 0x770aae78,
    b: 0xf26f,
    c: 0x4dba,
    d: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
};
const IID_DXGI_OUTPUT1: Guid = Guid {
    a: 0x00cddea8,
    b: 0x939b,
    c: 0x4b83,
    d: [0xa3, 0x40, 0xa6, 0x85, 0x22, 0x66, 0x66, 0xcc],
};
const IID_D3D11_TEXTURE2D: Guid = Guid {
    a: 0x6f15aaf2,
    b: 0xd208,
    c: 0x4e89,
    d: [0x9a, 0xb4, 0x48, 0x95, 0x35, 0xd3, 0x4f, 0x9c],
};

const DXGI_ERROR_NOT_FOUND: i32 = 0x887a0002_u32 as i32;
const DXGI_ERROR_WAIT_TIMEOUT: i32 = 0x887a0027_u32 as i32;

// Method indices, checked against the `windows` 0.58 vtable order.
const F_ENUM_ADAPTERS: usize = 7;
const A_ENUM_OUTPUTS: usize = 7;
const O_GET_DESC: usize = 7;
const O1_DUPLICATE: usize = 22;
const D_GET_DESC: usize = 7;
const D_ACQUIRE: usize = 8;
const D_RELEASE: usize = 14;
const DEV_CREATE_TEXTURE2D: usize = 5;
const TEX_GET_DESC: usize = 10;
const CTX_MAP: usize = 14;
const CTX_UNMAP: usize = 15;
const CTX_COPY_REGION: usize = 46;

/// DXGI_OUTPUT_DESC
#[repr(C)]
struct OutputDesc {
    name: [u16; 32],
    coordinates: Rect,
    attached: i32,
    rotation: u32,
    monitor: H,
}
/// DXGI_OUTDUPL_DESC
#[repr(C)]
#[derive(Default)]
struct DuplDesc {
    width: u32,
    height: u32,
    refresh_numerator: u32,
    refresh_denominator: u32,
    format: u32,
    scanline: u32,
    scaling: u32,
    rotation: u32,
    image_in_system_memory: i32,
}
/// DXGI_OUTDUPL_FRAME_INFO
#[repr(C)]
#[derive(Default)]
struct FrameInfo {
    last_present: i64,
    last_mouse: i64,
    accumulated: u32,
    rects_coalesced: i32,
    protected: i32,
    pointer_x: i32,
    pointer_y: i32,
    pointer_visible: i32,
    metadata_size: u32,
    pointer_shape_size: u32,
}
/// D3D11_TEXTURE2D_DESC
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct TextureDesc {
    width: u32,
    height: u32,
    mips: u32,
    array: u32,
    format: u32,
    sample_count: u32,
    sample_quality: u32,
    usage: u32,
    bind: u32,
    cpu: u32,
    misc: u32,
}
/// D3D11_BOX
#[repr(C)]
struct Box3d {
    left: u32,
    top: u32,
    front: u32,
    right: u32,
    bottom: u32,
    back: u32,
}
/// D3D11_MAPPED_SUBRESOURCE
#[repr(C)]
struct Mapped {
    data: *const u8,
    row_pitch: u32,
    depth_pitch: u32,
}

/// IDXGIOutput1 hands out BGRA8 frames, even on an HDR desktop. Any other format
/// would need another noise rule, so this stops instead of guessing.
pub fn check_format(format: u32) -> Result<(), String> {
    match format {
        87 => Ok(()),
        _ => Err(format!(
            "the desktop uses DXGI format {format}, and only BGRA8 (87) is handled"
        )),
    }
}

type CreateFactory = unsafe extern "system" fn(*const Guid, *mut H) -> i32;
type CreateDevice = unsafe extern "system" fn(
    H,
    u32,
    H,
    u32,
    *const u32,
    u32,
    u32,
    *mut H,
    *mut u32,
    *mut H,
) -> i32;

struct Duplicator {
    device: Com,
    context: Com,
    duplication: Com,
    staging: Com,
    copy_box: Box3d,
    row_bytes: usize,
    height: usize,
    last: Vec<u8>,
    fresh: Vec<u8>,
    have_last: bool,
}

impl Duplicator {
    fn open(region: ScreenRect) -> Result<Self, String> {
        let create_factory: CreateFactory =
            unsafe { mem::transmute(win::system_proc("dxgi.dll", b"CreateDXGIFactory1\0")?) };
        let create_device: CreateDevice =
            unsafe { mem::transmute(win::system_proc("d3d11.dll", b"D3D11CreateDevice\0")?) };
        let mut factory = Com::null();
        check(
            unsafe { create_factory(&IID_DXGI_FACTORY1, factory.slot()) },
            "CreateDXGIFactory1",
        )?;
        let mut adapter_index = 0;
        loop {
            let mut adapter = Com::null();
            let hr = vcall!(
                factory.0,
                F_ENUM_ADAPTERS,
                fn(u32, *mut H) -> i32,
                adapter_index,
                adapter.slot()
            );
            if hr == DXGI_ERROR_NOT_FOUND {
                return Err("no display output holds the whole target window".into());
            }
            check(hr, "EnumAdapters")?;
            let mut output_index = 0;
            loop {
                let mut output = Com::null();
                let hr = vcall!(
                    adapter.0,
                    A_ENUM_OUTPUTS,
                    fn(u32, *mut H) -> i32,
                    output_index,
                    output.slot()
                );
                if hr == DXGI_ERROR_NOT_FOUND {
                    break;
                }
                check(hr, "EnumOutputs")?;
                let mut desc: OutputDesc = unsafe { mem::zeroed() };
                check(
                    vcall!(output.0, O_GET_DESC, fn(*mut OutputDesc) -> i32, &mut desc),
                    "IDXGIOutput::GetDesc",
                )?;
                let bounds = ScreenRect {
                    left: desc.coordinates.left,
                    top: desc.coordinates.top,
                    right: desc.coordinates.right,
                    bottom: desc.coordinates.bottom,
                };
                if desc.attached != 0 && region.is_inside(&bounds) {
                    return Self::on_output(&adapter, &output, &desc, region, create_device);
                }
                output_index += 1;
            }
            adapter_index += 1;
        }
    }

    fn on_output(
        adapter: &Com,
        output: &Com,
        desc: &OutputDesc,
        region: ScreenRect,
        create_device: CreateDevice,
    ) -> Result<Self, String> {
        // 0 is unspecified and 1 is identity. A rotated display would need the
        // region turned, which this tool does not do.
        if desc.rotation > 1 {
            return Err(format!("the display is rotated ({})", desc.rotation));
        }
        let (mut device, mut context) = (Com::null(), Com::null());
        let mut level = 0;
        check(
            unsafe {
                // Driver type 0 is "unknown", which an explicit adapter requires.
                create_device(
                    adapter.0,
                    0,
                    ptr::null_mut(),
                    0,
                    ptr::null(),
                    0,
                    7,
                    device.slot(),
                    &mut level,
                    context.slot(),
                )
            },
            "D3D11CreateDevice",
        )?;
        let output1 = output.query(&IID_DXGI_OUTPUT1, "IDXGIOutput1")?;
        let mut duplication = Com::null();
        check(
            vcall!(
                output1.0,
                O1_DUPLICATE,
                fn(H, *mut H) -> i32,
                device.0,
                duplication.slot()
            ),
            "DuplicateOutput",
        )?;
        let mut dupl_desc = DuplDesc::default();
        vcall!(
            duplication.0,
            D_GET_DESC,
            fn(*mut DuplDesc) -> (),
            &mut dupl_desc
        );
        if dupl_desc.image_in_system_memory != 0 {
            return Err(
                "the desktop image is in system memory, which this tool does not read".into(),
            );
        }
        let left = (region.left - desc.coordinates.left) as u32;
        let top = (region.top - desc.coordinates.top) as u32;
        let width = (region.right - region.left) as u32;
        let height = (region.bottom - region.top) as u32;
        Ok(Duplicator {
            device,
            context,
            duplication,
            staging: Com::null(),
            copy_box: Box3d {
                left,
                top,
                front: 0,
                right: left + width,
                bottom: top + height,
                back: 1,
            },
            row_bytes: 0,
            height: height as usize,
            last: Vec::new(),
            fresh: Vec::new(),
            have_last: false,
        })
    }

    /// Makes the CPU-readable copy target, once the frame's format is known.
    fn prepare_staging(&mut self, texture: &Com) -> Result<(), String> {
        let mut frame_desc = TextureDesc::default();
        vcall!(
            texture.0,
            TEX_GET_DESC,
            fn(*mut TextureDesc) -> (),
            &mut frame_desc
        );
        check_format(frame_desc.format)?;
        let width = self.copy_box.right - self.copy_box.left;
        let desc = TextureDesc {
            width,
            height: self.height as u32,
            mips: 1,
            array: 1,
            format: frame_desc.format,
            sample_count: 1,
            sample_quality: 0,
            usage: 3, // staging
            bind: 0,
            cpu: 0x20000, // CPU read
            misc: 0,
        };
        check(
            vcall!(
                self.device.0,
                DEV_CREATE_TEXTURE2D,
                fn(*const TextureDesc, H, *mut H) -> i32,
                &desc,
                ptr::null_mut(),
                self.staging.slot()
            ),
            "CreateTexture2D(staging)",
        )?;
        self.row_bytes = width as usize * 4;
        self.last = vec![0; self.row_bytes * self.height];
        self.fresh = vec![0; self.row_bytes * self.height];
        Ok(())
    }

    /// Copies the region and compares it with the last frame.
    fn region_changed(&mut self, texture: &Com) -> Result<bool, String> {
        if self.staging.0.is_null() {
            self.prepare_staging(texture)?;
        }
        vcall!(
            self.context.0,
            CTX_COPY_REGION,
            fn(H, u32, u32, u32, u32, H, u32, *const Box3d) -> (),
            self.staging.0,
            0,
            0,
            0,
            0,
            texture.0,
            0,
            &self.copy_box
        );
        let mut mapped = Mapped {
            data: ptr::null(),
            row_pitch: 0,
            depth_pitch: 0,
        };
        check(
            vcall!(
                self.context.0,
                CTX_MAP,
                fn(H, u32, u32, u32, *mut Mapped) -> i32,
                self.staging.0,
                0,
                1,
                0,
                &mut mapped
            ),
            "Map(staging)",
        )?;
        for y in 0..self.height {
            let row = unsafe {
                std::slice::from_raw_parts(
                    mapped.data.add(y * mapped.row_pitch as usize),
                    self.row_bytes,
                )
            };
            let start = y * self.row_bytes;
            self.fresh[start..start + self.row_bytes].copy_from_slice(row);
        }
        vcall!(
            self.context.0,
            CTX_UNMAP,
            fn(H, u32) -> (),
            self.staging.0,
            0
        );
        // The first frame is the starting image, not a change. The kept image
        // moves only on a real change, so noise cannot build up past the
        // tolerance one small step at a time.
        let changed = self.have_last && pixels_differ(&self.last, &self.fresh);
        if changed || !self.have_last {
            mem::swap(&mut self.last, &mut self.fresh);
        }
        self.have_last = true;
        Ok(changed)
    }

    /// Waits for one desktop frame. The outer `None` means no new frame came;
    /// the inner one means a frame came but the region did not change.
    fn next(&mut self, timeout_ms: u32) -> Result<Option<Option<Change>>, String> {
        let mut info = FrameInfo::default();
        let mut resource = Com::null();
        let hr = vcall!(
            self.duplication.0,
            D_ACQUIRE,
            fn(u32, *mut FrameInfo, *mut H) -> i32,
            timeout_ms,
            &mut info,
            resource.slot()
        );
        if hr == DXGI_ERROR_WAIT_TIMEOUT {
            return Ok(None);
        }
        check(hr, "AcquireNextFrame")?;
        let acquired = qpc();
        // LastPresentTime is zero when only the mouse pointer moved. The pointer
        // is not in the image, so the pixels cannot have changed. The very first
        // frame is read either way: without that starting image, the next update
        // anywhere on the display would look like a change in the region.
        let changed = if info.last_present != 0 || !self.have_last {
            let texture = resource.query(&IID_D3D11_TEXTURE2D, "ID3D11Texture2D")?;
            self.region_changed(&texture)?
        } else {
            false
        };
        let change = (changed && info.last_present != 0).then_some(Change {
            present: info.last_present,
            acquired,
        });
        drop(resource);
        check(
            vcall!(self.duplication.0, D_RELEASE, fn() -> i32),
            "ReleaseFrame",
        )?;
        Ok(Some(change))
    }
}

/// A background thread that records changes in the region. A lost duplication,
/// for example when a UAC prompt or a display change appears, ends the thread
/// with an error, and the run is then refused.
pub struct Capture {
    stop: Arc<AtomicBool>,
    changes: Arc<Mutex<Vec<Change>>>,
    frames: Arc<AtomicU64>,
    thread: JoinHandle<Result<(), String>>,
}

impl Capture {
    pub fn start(region: ScreenRect) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let changes = Arc::new(Mutex::new(Vec::new()));
        let frames = Arc::new(AtomicU64::new(0));
        let (ready_send, ready) = mpsc::channel();
        let thread = {
            let (stop, changes, frames) = (stop.clone(), changes.clone(), frames.clone());
            thread::spawn(move || {
                let mut duplicator = match Duplicator::open(region) {
                    Ok(duplicator) => {
                        ready_send
                            .send(Ok(()))
                            .map_err(|e| format!("capture start: {e}"))?;
                        duplicator
                    }
                    Err(e) => {
                        ready_send
                            .send(Err(e.clone()))
                            .map_err(|e| format!("capture start: {e}"))?;
                        return Err(e);
                    }
                };
                while !stop.load(Ordering::Relaxed) {
                    if let Some(frame) = duplicator.next(20)? {
                        frames.fetch_add(1, Ordering::Relaxed);
                        if let Some(change) = frame {
                            changes
                                .lock()
                                .map_err(|_| "the change list lock broke".to_owned())?
                                .push(change);
                        }
                    }
                }
                Ok(())
            })
        };
        ready
            .recv()
            .map_err(|e| format!("the capture thread ended early: {e}"))??;
        Ok(Capture {
            stop,
            changes,
            frames,
            thread,
        })
    }

    pub fn is_running(&self) -> bool {
        !self.thread.is_finished()
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The time of the newest change, if any.
    pub fn last_change(&self) -> Result<Option<i64>, String> {
        let changes = self
            .changes
            .lock()
            .map_err(|_| "the change list lock broke".to_owned())?;
        Ok(changes.last().map(|c| c.present))
    }

    /// Stops the capture. A failure at any time during the run comes back here.
    /// It returns the changes and the number of frames read.
    pub fn finish(self) -> Result<(Vec<Change>, u64), String> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .join()
            .map_err(|_| "the capture thread panicked".to_owned())??;
        let changes = self
            .changes
            .lock()
            .map_err(|_| "the change list lock broke".to_owned())?;
        Ok((changes.clone(), self.frames.load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_match_the_sizes_in_the_sdk_headers() {
        assert_eq!(size_of::<FrameInfo>(), 48);
        assert_eq!(size_of::<DuplDesc>(), 36);
        assert_eq!(size_of::<TextureDesc>(), 44);
        assert_eq!(size_of::<OutputDesc>(), 64 + 16 + 4 + 4 + 8);
    }

    #[test]
    fn a_format_this_tool_cannot_read_is_an_error() {
        assert!(check_format(87).is_ok());
        assert!(check_format(10).is_err());
    }
}
