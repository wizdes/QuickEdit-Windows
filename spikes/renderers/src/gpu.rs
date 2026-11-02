//! Direct3D 11 pieces that arms b, c and d share.
use crate::dump;
use crate::ffi::{self, Com, Guid, H, Hr, Mapped, SwapChainDesc1, Texture2dDesc, check, v};
use crate::vcall;
use std::{mem, ptr};

type CreateDevice =
    unsafe extern "system" fn(H, u32, H, u32, *const u32, u32, u32, *mut H, *mut u32, *mut H) -> Hr;

/// A hardware device. There is no WARP fallback: a machine without a working
/// GPU device fails here with the HRESULT instead of timing the wrong thing.
pub fn create_device(flags: u32) -> Result<(Com, Com), String> {
    let create: CreateDevice =
        unsafe { mem::transmute(ffi::system_proc("d3d11.dll", b"D3D11CreateDevice\0")?) };
    let levels = [ffi::D3D_FEATURE_LEVEL_11_0, ffi::D3D_FEATURE_LEVEL_10_0];
    let (mut device, mut context) = (Com::null(), Com::null());
    let mut level = 0;
    check(
        unsafe {
            create(
                ptr::null_mut(),
                ffi::D3D_DRIVER_TYPE_HARDWARE,
                ptr::null_mut(),
                flags,
                levels.as_ptr(),
                levels.len() as u32,
                ffi::D3D11_SDK_VERSION,
                device.slot(),
                &mut level,
                context.slot(),
            )
        },
        "D3D11CreateDevice",
    )?;
    Ok((device, context))
}

/// The DXGI factory that owns the device, reached without loading dxgi.dll by name.
pub fn factory_of(dxgi_device: &Com) -> Result<Com, String> {
    let mut adapter = Com::null();
    check(
        vcall!(
            dxgi_device.0,
            v::DXD_GET_ADAPTER,
            fn(*mut H) -> Hr,
            adapter.slot()
        ),
        "GetAdapter",
    )?;
    let mut factory = Com::null();
    check(
        vcall!(
            adapter.0,
            v::DXO_GET_PARENT,
            fn(*const Guid, *mut H) -> Hr,
            &ffi::IID_DXGI_FACTORY2,
            factory.slot()
        ),
        "GetParent(IDXGIFactory2)",
    )?;
    Ok(factory)
}

pub fn swap_desc(width: i32, height: i32, alpha: u32, flags: u32) -> SwapChainDesc1 {
    SwapChainDesc1 {
        width: width as u32,
        height: height as u32,
        format: ffi::DXGI_FORMAT_B8G8R8A8_UNORM,
        stereo: 0,
        sample_count: 1,
        sample_quality: 0,
        usage: ffi::DXGI_USAGE_RENDER_TARGET_OUTPUT,
        buffers: 2,
        scaling: ffi::DXGI_SCALING_STRETCH,
        effect: ffi::DXGI_SWAP_EFFECT_FLIP_DISCARD,
        alpha,
        flags,
    }
}

/// Copies a BGRA texture to the CPU and saves it, for `--dump-frame` only.
pub fn dump_texture(
    device: &Com,
    context: &Com,
    texture: &Com,
    size: (i32, i32),
    path: &str,
) -> Result<(), String> {
    let desc = Texture2dDesc {
        width: size.0 as u32,
        height: size.1 as u32,
        mips: 1,
        array: 1,
        format: ffi::DXGI_FORMAT_B8G8R8A8_UNORM,
        sample_count: 1,
        sample_quality: 0,
        usage: ffi::USAGE_STAGING,
        bind: 0,
        cpu: ffi::CPU_READ,
        misc: 0,
    };
    let mut staging = Com::null();
    check(
        vcall!(
            device.0,
            v::D3D_CREATE_TEXTURE2D,
            fn(*const Texture2dDesc, H, *mut H) -> Hr,
            &desc,
            ptr::null_mut(),
            staging.slot()
        ),
        "CreateTexture2D(staging)",
    )?;
    vcall!(
        context.0,
        v::CTX_COPY_RESOURCE,
        fn(H, H) -> (),
        staging.0,
        texture.0
    );
    let mut mapped = Mapped {
        data: ptr::null_mut(),
        row_pitch: 0,
        depth_pitch: 0,
    };
    check(
        vcall!(
            context.0,
            v::CTX_MAP,
            fn(H, u32, u32, u32, *mut Mapped) -> Hr,
            staging.0,
            0,
            ffi::MAP_READ,
            0,
            &mut mapped
        ),
        "Map(staging)",
    )?;
    let bytes = mapped.row_pitch as usize * size.1 as usize;
    let pixels = unsafe { std::slice::from_raw_parts(mapped.data, bytes) };
    let result = dump::write_bmp(path, size.0 as u32, size.1 as u32, mapped.row_pitch, pixels);
    vcall!(context.0, v::CTX_UNMAP, fn(H, u32) -> (), staging.0, 0);
    result
}
