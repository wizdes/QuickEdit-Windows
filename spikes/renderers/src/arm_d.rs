//! Arm d: the first frame comes from arm a's path. A background thread then
//! builds arm c's path, and the window switches to it.
//!
//! The switch uses a DirectComposition visual, not a child window:
//! - The visual sits on top of the window's GDI pixels in the same window, so
//!   the old pixels stay until the new frame is ready.
//! - A flip-model present on a hidden child window is thrown away. A child
//!   would have to be shown before it has a frame, which can flash.
//! - Keyboard messages stay on the one window, with no focus or input-thread
//!   joining to manage.
//!
//! The price: the swap chain is created for composition, not for the window as
//! in arm c. The drawing code is the same, but typing latency can differ from
//! arm c, so the race measures arm d on its own.
//!
//! The background thread starts only after the first frame's stamp. It cannot
//! slow the first frame, including through the DLL loader lock. Its cost shows
//! up in the takeover time instead.
use crate::arm_a::ArmA;
use crate::arm_c::{ArmC, Output};
use crate::ffi::{self, Com, Guid, H, Hr, check, v};
use crate::text::{Font, Layout};
use crate::vcall;
use std::{mem, sync::Arc, thread};

pub const WM_GPU_READY: u32 = 0x8001;
pub const WM_GPU_FAILED: u32 = 0x8002;

type CreateComposition = unsafe extern "system" fn(H, *const Guid, *mut H) -> Hr;

pub struct Gpu {
    arm_c: ArmC,
    composition: Com,
    _target: Com,
    _visual: Com,
}

fn build(hwnd: H, font: Arc<Font>, layout: Layout) -> Result<Gpu, String> {
    let mut arm_c = ArmC::new(Output::Composition, font, layout)?;
    arm_c.prepare()?;
    let dxgi_device = arm_c.device.query(&ffi::IID_DXGI_DEVICE, "IDXGIDevice")?;
    let create: CreateComposition = unsafe {
        mem::transmute(ffi::system_proc(
            "dcomp.dll",
            b"DCompositionCreateDevice\0",
        )?)
    };
    let mut composition = Com::null();
    check(
        unsafe { create(dxgi_device.0, &ffi::IID_DCOMP_DEVICE, composition.slot()) },
        "DCompositionCreateDevice",
    )?;
    let mut target = Com::null();
    // The second value is "topmost": the visual covers the window's GDI pixels.
    check(
        vcall!(
            composition.0,
            v::DC_CREATE_TARGET_HWND,
            fn(H, i32, *mut H) -> Hr,
            hwnd,
            1,
            target.slot()
        ),
        "CreateTargetForHwnd",
    )?;
    let mut visual = Com::null();
    check(
        vcall!(
            composition.0,
            v::DC_CREATE_VISUAL,
            fn(*mut H) -> Hr,
            visual.slot()
        ),
        "CreateVisual",
    )?;
    check(
        vcall!(visual.0, v::DCV_SET_CONTENT, fn(H) -> Hr, arm_c.swap.0),
        "SetContent",
    )?;
    check(
        vcall!(target.0, v::DCT_SET_ROOT, fn(H) -> Hr, visual.0),
        "SetRoot",
    )?;
    Ok(Gpu {
        arm_c,
        composition,
        _target: target,
        _visual: visual,
    })
}

/// Starts the background build. The result comes back as a posted message,
/// whose pointer `gpu_ready` or `gpu_failed` takes back.
pub fn start(hwnd: H, font: Arc<Font>, layout: Layout) -> Result<(), String> {
    let window = hwnd as usize;
    thread::Builder::new()
        .spawn(move || {
            let hwnd = window as H;
            let posted = match build(hwnd, font, layout) {
                Ok(gpu) => {
                    let gpu = Box::into_raw(Box::new(gpu));
                    unsafe { ffi::PostMessageW(hwnd, WM_GPU_READY, gpu as usize, 0) }
                }
                Err(e) => {
                    let e = Box::into_raw(Box::new(e));
                    unsafe { ffi::PostMessageW(hwnd, WM_GPU_FAILED, e as usize, 0) }
                }
            };
            if posted == 0 {
                eprintln!("arm d: posting the background result failed");
            }
        })
        .map(|_| ())
        .map_err(|e| format!("starting the background thread: {e}"))
}

pub fn gpu_ready(pointer: usize) -> Box<Gpu> {
    unsafe { Box::from_raw(pointer as *mut Gpu) }
}

pub fn gpu_failed(pointer: usize) -> String {
    *unsafe { Box::from_raw(pointer as *mut String) }
}

pub struct ArmD {
    gdi: Option<ArmA>,
    gpu: Option<Box<Gpu>>,
}

impl ArmD {
    pub fn new(hwnd: H, font: Arc<Font>, layout: Layout) -> Result<Self, String> {
        Ok(ArmD {
            gdi: Some(ArmA::new(hwnd, font, layout)?),
            gpu: None,
        })
    }

    pub fn draw(&mut self, typed: &[char], dump_path: Option<&str>) -> Result<(), String> {
        if let Some(gpu) = &mut self.gpu {
            return gpu.arm_c.draw(typed, dump_path);
        }
        let gdi = self.gdi.as_mut().ok_or("arm d has no drawing path")?;
        gdi.draw(typed)?;
        if let Some(path) = dump_path {
            gdi.dump(path)?;
        }
        Ok(())
    }

    /// The switch: present one frame with the current text, then show the
    /// visual. The GDI pixels stay until the composition engine has both.
    pub fn take_over(
        &mut self,
        mut gpu: Box<Gpu>,
        typed: &[char],
        dump_path: Option<&str>,
    ) -> Result<(), String> {
        gpu.arm_c.draw(typed, dump_path)?;
        check(
            vcall!(gpu.composition.0, v::DC_COMMIT, fn() -> Hr),
            "IDCompositionDevice::Commit",
        )?;
        check(
            vcall!(gpu.composition.0, v::DC_WAIT_COMMIT, fn() -> Hr),
            "WaitForCommitCompletion",
        )?;
        self.gpu = Some(gpu);
        self.gdi = None;
        Ok(())
    }

    pub fn has_taken_over(&self) -> bool {
        self.gpu.is_some()
    }
}
