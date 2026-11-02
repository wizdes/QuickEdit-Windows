//! Hand-written Windows declarations and helpers for calling COM methods.
//!
//! Why not the `windows` crate: `Cargo.lock` lists no dependencies for this
//! spike and only root edits that file. Every method index below was checked
//! against the vtable order in the `windows` 0.58 source and the Windows SDK
//! headers.
//!
//! Only user32, gdi32 and kernel32 are linked at load time. Every other DLL is
//! loaded with `system_proc` by the arm that needs it, so arm a never pays for
//! Direct3D and the launch timer sees each DLL load.
use std::{ffi::c_void, ptr};

pub type H = *mut c_void;
pub type Hr = i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Guid {
    pub a: u32,
    pub b: u16,
    pub c: u16,
    pub d: [u8; 8],
}
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> Guid {
    Guid { a, b, c, d }
}
pub const IID_DWRITE_FACTORY: Guid = guid(
    0xb859ee5a,
    0xd838,
    0x4b5b,
    [0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48],
);
pub const IID_DXGI_DEVICE: Guid = guid(
    0x54ec77fa,
    0x1377,
    0x44e6,
    [0x8c, 0x32, 0x88, 0xfd, 0x5f, 0x44, 0xc8, 0x4c],
);
pub const IID_DXGI_FACTORY2: Guid = guid(
    0x50c83a1c,
    0xe072,
    0x4c48,
    [0x87, 0xb0, 0x36, 0x30, 0xfa, 0x36, 0xa6, 0xd0],
);
pub const IID_DXGI_SURFACE: Guid = guid(
    0xcafcb56c,
    0x6ac3,
    0x4889,
    [0xbf, 0x47, 0x9e, 0x23, 0xbb, 0xd2, 0x60, 0xec],
);
pub const IID_DXGI_SWAP_CHAIN2: Guid = guid(
    0xa8be2ac4,
    0x199f,
    0x4946,
    [0xb3, 0x31, 0x79, 0x59, 0x9f, 0xb9, 0x8d, 0xe7],
);
pub const IID_D3D11_TEXTURE2D: Guid = guid(
    0x6f15aaf2,
    0xd208,
    0x4e89,
    [0x9a, 0xb4, 0x48, 0x95, 0x35, 0xd3, 0x4f, 0x9c],
);
pub const IID_D2D1_FACTORY1: Guid = guid(
    0xbb12d362,
    0xdaee,
    0x4b9a,
    [0xaa, 0x1d, 0x14, 0xba, 0x40, 0x1c, 0xfa, 0x1f],
);
pub const IID_DCOMP_DEVICE: Guid = guid(
    0xc37ea93a,
    0xe7aa,
    0x450d,
    [0xb1, 0x6f, 0x97, 0x46, 0xcb, 0x04, 0x07, 0xf3],
);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[repr(C)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
#[repr(C)]
pub struct Msg {
    pub hwnd: H,
    pub message: u32,
    pub wp: usize,
    pub lp: isize,
    pub time: u32,
    pub pt: Point,
    pub private: u32,
}
#[repr(C)]
pub struct WndClass {
    pub style: u32,
    pub proc: Option<unsafe extern "system" fn(H, u32, usize, isize) -> isize>,
    pub cls_extra: i32,
    pub wnd_extra: i32,
    pub instance: H,
    pub icon: H,
    pub cursor: H,
    pub background: H,
    pub menu: *const u16,
    pub name: *const u16,
}
/// DWRITE_GLYPH_RUN
#[repr(C)]
pub struct GlyphRun {
    pub face: H,
    pub size: f32,
    pub count: u32,
    pub indices: *const u16,
    pub advances: *const f32,
    pub offsets: H,
    pub sideways: i32,
    pub bidi: u32,
}
/// DWRITE_FONT_METRICS
#[repr(C)]
#[derive(Default)]
pub struct FontMetrics {
    pub em: u16,
    pub ascent: u16,
    pub descent: u16,
    pub line_gap: i16,
    pub cap_height: u16,
    pub x_height: u16,
    pub underline_position: i16,
    pub underline_thickness: u16,
    pub strike_position: i16,
    pub strike_thickness: u16,
}
/// DWRITE_GLYPH_METRICS
#[repr(C)]
#[derive(Default)]
pub struct GlyphMetrics {
    pub left_bearing: i32,
    pub advance_width: u32,
    pub right_bearing: i32,
    pub top_bearing: i32,
    pub advance_height: u32,
    pub bottom_bearing: i32,
    pub vertical_origin_y: i32,
}
/// DXGI_SWAP_CHAIN_DESC1
#[repr(C)]
pub struct SwapChainDesc1 {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stereo: i32,
    pub sample_count: u32,
    pub sample_quality: u32,
    pub usage: u32,
    pub buffers: u32,
    pub scaling: u32,
    pub effect: u32,
    pub alpha: u32,
    pub flags: u32,
}
/// D3D11_TEXTURE2D_DESC
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub array: u32,
    pub format: u32,
    pub sample_count: u32,
    pub sample_quality: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
}
/// D3D11_BUFFER_DESC
#[repr(C)]
pub struct BufferDesc {
    pub bytes: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
    pub stride: u32,
}
/// D3D11_INPUT_ELEMENT_DESC
#[repr(C)]
pub struct InputElement {
    pub name: *const u8,
    pub index: u32,
    pub format: u32,
    pub slot: u32,
    pub offset: u32,
    pub class: u32,
    pub step: u32,
}
/// D3D11_RENDER_TARGET_BLEND_DESC
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TargetBlend {
    pub enable: i32,
    pub src: u32,
    pub dest: u32,
    pub op: u32,
    pub src_alpha: u32,
    pub dest_alpha: u32,
    pub op_alpha: u32,
    pub write_mask: u8,
}
/// D3D11_BLEND_DESC
#[repr(C)]
pub struct BlendDesc {
    pub alpha_to_coverage: i32,
    pub independent: i32,
    pub targets: [TargetBlend; 8],
}
/// D3D11_VIEWPORT
#[repr(C)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}
/// D3D11_BOX
#[repr(C)]
pub struct Box3d {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}
/// D3D11_MAPPED_SUBRESOURCE
#[repr(C)]
pub struct Mapped {
    pub data: *mut u8,
    pub row_pitch: u32,
    pub depth_pitch: u32,
}
/// D2D1_BITMAP_PROPERTIES1
#[repr(C)]
pub struct BitmapProps1 {
    pub format: u32,
    pub alpha: u32,
    pub dpi_x: f32,
    pub dpi_y: f32,
    pub options: u32,
    pub color_context: H,
}
/// D2D1_POINT_2F
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Point2f {
    pub x: f32,
    pub y: f32,
}
/// D2D1_COLOR_F
#[repr(C)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}
/// BITMAPINFOHEADER
#[repr(C)]
pub struct BitmapInfoHeader {
    pub size: u32,
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bits: u16,
    pub compression: u32,
    pub image_size: u32,
    pub x_ppm: i32,
    pub y_ppm: i32,
    pub used: u32,
    pub important: u32,
}

pub const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
pub const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
pub const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
pub const DXGI_USAGE_RENDER_TARGET_OUTPUT: u32 = 0x20;
pub const DXGI_SWAP_EFFECT_FLIP_DISCARD: u32 = 4;
pub const DXGI_SCALING_STRETCH: u32 = 0;
pub const DXGI_ALPHA_MODE_UNSPECIFIED: u32 = 0;
pub const DXGI_ALPHA_MODE_IGNORE: u32 = 3;
pub const DXGI_FLAG_WAITABLE: u32 = 64;
pub const D3D_DRIVER_TYPE_HARDWARE: u32 = 1;
pub const D3D11_SDK_VERSION: u32 = 7;
pub const D3D11_CREATE_BGRA: u32 = 0x20;
pub const D3D_FEATURE_LEVEL_11_0: u32 = 0xb000;
pub const D3D_FEATURE_LEVEL_10_0: u32 = 0xa000;
pub const USAGE_DEFAULT: u32 = 0;
pub const USAGE_DYNAMIC: u32 = 2;
pub const USAGE_STAGING: u32 = 3;
pub const BIND_VERTEX: u32 = 0x1;
pub const BIND_SHADER_RESOURCE: u32 = 0x8;
pub const CPU_WRITE: u32 = 0x10000;
pub const CPU_READ: u32 = 0x20000;
pub const MAP_READ: u32 = 1;
pub const MAP_WRITE_DISCARD: u32 = 4;
pub const BLEND_ZERO: u32 = 1;
pub const BLEND_ONE: u32 = 2;
pub const BLEND_INV_SRC_COLOR: u32 = 4;
pub const BLEND_OP_ADD: u32 = 1;
pub const TOPOLOGY_TRIANGLE_STRIP: u32 = 5;
pub const INPUT_PER_INSTANCE: u32 = 1;

/// Method indices, from the vtable order. See the note at the top of this file.
pub mod v {
    pub const QUERY_INTERFACE: usize = 0;
    pub const RELEASE: usize = 2;

    pub const DWF_GET_SYSTEM_FONT_COLLECTION: usize = 3;
    pub const DWF_CREATE_RENDERING_PARAMS: usize = 10;
    pub const DWF_GET_GDI_INTEROP: usize = 17;
    pub const DWF_CREATE_GLYPH_RUN_ANALYSIS: usize = 23;
    pub const DWC_GET_FONT_FAMILY: usize = 4;
    pub const DWC_FIND_FAMILY_NAME: usize = 5;
    pub const DWFAM_GET_FIRST_MATCHING_FONT: usize = 7;
    pub const DWFONT_CREATE_FONT_FACE: usize = 13;
    pub const DWFACE_GET_METRICS: usize = 8;
    pub const DWFACE_GET_DESIGN_GLYPH_METRICS: usize = 10;
    pub const DWFACE_GET_GLYPH_INDICES: usize = 11;
    pub const DWA_GET_BOUNDS: usize = 3;
    pub const DWA_CREATE_TEXTURE: usize = 4;
    pub const DWI_CREATE_BITMAP_TARGET: usize = 7;
    pub const DWT_DRAW_GLYPH_RUN: usize = 3;
    pub const DWT_GET_MEMORY_DC: usize = 4;

    pub const DXO_GET_PARENT: usize = 6;
    pub const DXD_GET_ADAPTER: usize = 7;
    pub const DXF_CREATE_SWAP_CHAIN_HWND: usize = 15;
    pub const DXF_CREATE_SWAP_CHAIN_COMPOSITION: usize = 24;
    pub const DXS_PRESENT: usize = 8;
    pub const DXS_GET_BUFFER: usize = 9;
    pub const DXS2_SET_MAX_LATENCY: usize = 31;
    pub const DXS2_GET_WAITABLE: usize = 33;

    pub const D3D_CREATE_BUFFER: usize = 3;
    pub const D3D_CREATE_TEXTURE2D: usize = 5;
    pub const D3D_CREATE_SRV: usize = 7;
    pub const D3D_CREATE_RTV: usize = 9;
    pub const D3D_CREATE_INPUT_LAYOUT: usize = 11;
    pub const D3D_CREATE_VS: usize = 12;
    pub const D3D_CREATE_PS: usize = 15;
    pub const D3D_CREATE_BLEND: usize = 20;

    pub const CTX_PS_SET_SRV: usize = 8;
    pub const CTX_PS_SET_SHADER: usize = 9;
    pub const CTX_VS_SET_SHADER: usize = 11;
    pub const CTX_MAP: usize = 14;
    pub const CTX_UNMAP: usize = 15;
    pub const CTX_IA_SET_LAYOUT: usize = 17;
    pub const CTX_IA_SET_VB: usize = 18;
    pub const CTX_DRAW_INSTANCED: usize = 21;
    pub const CTX_IA_SET_TOPOLOGY: usize = 24;
    pub const CTX_OM_SET_TARGETS: usize = 33;
    pub const CTX_OM_SET_BLEND: usize = 35;
    pub const CTX_RS_SET_VIEWPORTS: usize = 44;
    pub const CTX_COPY_RESOURCE: usize = 47;
    pub const CTX_UPDATE_SUBRESOURCE: usize = 48;
    pub const CTX_CLEAR_RTV: usize = 50;

    pub const D2F_CREATE_DEVICE: usize = 17;
    pub const D2D_CREATE_CONTEXT: usize = 4;
    pub const D2C_CREATE_BRUSH: usize = 8;
    pub const D2C_DRAW_GLYPH_RUN: usize = 29;
    pub const D2C_SET_TEXT_AA: usize = 34;
    pub const D2C_CLEAR: usize = 47;
    pub const D2C_BEGIN_DRAW: usize = 48;
    pub const D2C_END_DRAW: usize = 49;
    pub const D2C_SET_DPI: usize = 51;
    pub const D2C_BITMAP_FROM_SURFACE: usize = 62;
    pub const D2C_SET_TARGET: usize = 74;

    pub const DC_COMMIT: usize = 3;
    pub const DC_WAIT_COMMIT: usize = 4;
    pub const DC_CREATE_TARGET_HWND: usize = 6;
    pub const DC_CREATE_VISUAL: usize = 7;
    pub const DCT_SET_ROOT: usize = 3;
    pub const DCV_SET_CONTENT: usize = 15;
}

#[link(name = "user32")]
unsafe extern "system" {
    pub fn SetProcessDpiAwarenessContext(v: isize) -> i32;
    pub fn RegisterClassW(c: *const WndClass) -> u16;
    pub fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: H,
        menu: H,
        inst: H,
        param: H,
    ) -> H;
    pub fn DefWindowProcW(h: H, m: u32, w: usize, l: isize) -> isize;
    pub fn ShowWindow(h: H, c: i32) -> i32;
    pub fn UpdateWindow(h: H) -> i32;
    pub fn GetMessageW(m: *mut Msg, h: H, min: u32, max: u32) -> i32;
    pub fn TranslateMessage(m: *const Msg) -> i32;
    pub fn DispatchMessageW(m: *const Msg) -> isize;
    pub fn PostMessageW(h: H, m: u32, w: usize, l: isize) -> i32;
    pub fn PostQuitMessage(c: i32);
    pub fn ValidateRect(h: H, r: *const Rect) -> i32;
    pub fn GetDC(h: H) -> H;
    pub fn ReleaseDC(h: H, dc: H) -> i32;
    pub fn GetDpiForWindow(h: H) -> u32;
    pub fn AdjustWindowRectExForDpi(r: *mut Rect, s: u32, menu: i32, ex: u32, dpi: u32) -> i32;
    pub fn SetWindowPos(h: H, after: H, x: i32, y: i32, w: i32, hgt: i32, flags: u32) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn PatBlt(dc: H, x: i32, y: i32, w: i32, h: i32, rop: u32) -> i32;
    pub fn BitBlt(
        dst: H,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: H,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
    pub fn GdiFlush() -> i32;
    pub fn GetCurrentObject(dc: H, kind: u32) -> H;
    pub fn GetDIBits(
        dc: H,
        bitmap: H,
        start: u32,
        lines: u32,
        bits: *mut u8,
        info: *mut BitmapInfoHeader,
        usage: u32,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn QueryPerformanceCounter(t: *mut i64) -> i32;
    pub fn GetModuleHandleW(name: *const u16) -> H;
    pub fn LoadLibraryExW(name: *const u16, file: H, flags: u32) -> H;
    pub fn GetProcAddress(module: H, name: *const u8) -> H;
    pub fn GetLastError() -> u32;
    pub fn WaitForSingleObject(h: H, ms: u32) -> u32;
    pub fn GetCurrentProcess() -> H;
    pub fn K32EnumProcessModules(p: H, mods: *mut H, bytes: u32, needed: *mut u32) -> i32;
    pub fn K32GetModuleBaseNameW(p: H, m: H, name: *mut u16, size: u32) -> u32;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn qpc() -> i64 {
    let mut v = 0;
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}

/// Finds a function in a DLL from System32, loading the DLL on first use.
/// `name` must end with a NUL byte.
pub fn system_proc(dll: &str, name: &'static [u8]) -> Result<H, String> {
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
    let module = unsafe {
        LoadLibraryExW(
            wide(dll).as_ptr(),
            ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(format!("loading {dll} failed: error {}", unsafe {
            GetLastError()
        }));
    }
    let proc = unsafe { GetProcAddress(module, name.as_ptr()) };
    if proc.is_null() {
        return Err(format!("{dll} has no such function: error {}", unsafe {
            GetLastError()
        }));
    }
    Ok(proc)
}

pub fn method(p: H, index: usize) -> H {
    unsafe { *(*(p as *const *const H)).add(index) }
}

/// Calls a COM method by its vtable index. The argument expressions sit inside
/// the unsafe call on purpose: the whole point of this private macro is an
/// unsafe call, and callers are all in this crate.
#[macro_export]
macro_rules! vcall {
    ($obj:expr, $index:expr, fn($($t:ty),*) -> $r:ty $(, $a:expr)* $(,)?) => {{
        let this: $crate::ffi::H = $obj;
        let f: unsafe extern "system" fn($crate::ffi::H $(, $t)*) -> $r =
            unsafe { ::std::mem::transmute($crate::ffi::method(this, $index)) };
        unsafe { f(this $(, $a)*) }
    }};
}

pub fn check(hr: Hr, what: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{what} failed: HRESULT {hr:#010x}"))
    } else {
        Ok(())
    }
}

/// An owned COM pointer. COM objects used across threads here are either
/// free-threaded (DirectWrite, DirectComposition) or used by one thread at a
/// time (the Direct3D context, handed over once in arm d).
pub struct Com(pub H);
unsafe impl Send for Com {}
unsafe impl Sync for Com {}
impl Com {
    pub fn null() -> Self {
        Com(ptr::null_mut())
    }
    pub fn slot(&mut self) -> *mut H {
        &mut self.0
    }
    pub fn query(&self, iid: &Guid, what: &str) -> Result<Com, String> {
        let mut out = Com::null();
        let hr = vcall!(
            self.0,
            v::QUERY_INTERFACE,
            fn(*const Guid, *mut H) -> Hr,
            iid,
            out.slot()
        );
        check(hr, what)?;
        Ok(out)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            vcall!(self.0, v::RELEASE, fn() -> u32);
        }
    }
}
