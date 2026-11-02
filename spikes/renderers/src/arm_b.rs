//! Arm b: a Direct2D device context on a flip-model swap chain, from a hardware
//! Direct3D 11 device.
use crate::ffi::{self, BitmapProps1, Color, Com, GlyphRun, Guid, H, Hr, Point2f, check, v};
use crate::text::{self, Font, Layout};
use crate::{gpu, vcall};
use std::{mem, ptr, sync::Arc};

type CreateFactory = unsafe extern "system" fn(u32, *const Guid, H, *mut H) -> Hr;

pub struct ArmB {
    font: Arc<Font>,
    layout: Layout,
    device: Com,
    context3d: Com,
    swap: Com,
    back: Com,
    context: Com,
    brush: Com,
    _target: Com,
    _d2d_device: Com,
    _factory: Com,
}

impl ArmB {
    pub fn new(hwnd: H, font: Arc<Font>, layout: Layout) -> Result<Self, String> {
        let (device, context3d) = gpu::create_device(ffi::D3D11_CREATE_BGRA)?;
        let dxgi_device = device.query(&ffi::IID_DXGI_DEVICE, "IDXGIDevice")?;
        let dxgi_factory = gpu::factory_of(&dxgi_device)?;
        let desc = gpu::swap_desc(
            layout.width,
            layout.height,
            ffi::DXGI_ALPHA_MODE_UNSPECIFIED,
            0,
        );
        let mut swap = Com::null();
        check(
            vcall!(
                dxgi_factory.0,
                v::DXF_CREATE_SWAP_CHAIN_HWND,
                fn(H, H, *const ffi::SwapChainDesc1, H, H, *mut H) -> Hr,
                device.0,
                hwnd,
                &desc,
                ptr::null_mut(),
                ptr::null_mut(),
                swap.slot()
            ),
            "CreateSwapChainForHwnd",
        )?;

        let create: CreateFactory =
            unsafe { mem::transmute(ffi::system_proc("d2d1.dll", b"D2D1CreateFactory\0")?) };
        let mut factory = Com::null();
        check(
            unsafe { create(0, &ffi::IID_D2D1_FACTORY1, ptr::null_mut(), factory.slot()) },
            "D2D1CreateFactory",
        )?;
        let mut d2d_device = Com::null();
        check(
            vcall!(
                factory.0,
                v::D2F_CREATE_DEVICE,
                fn(H, *mut H) -> Hr,
                dxgi_device.0,
                d2d_device.slot()
            ),
            "ID2D1Factory1::CreateDevice",
        )?;
        let mut context = Com::null();
        check(
            vcall!(
                d2d_device.0,
                v::D2D_CREATE_CONTEXT,
                fn(u32, *mut H) -> Hr,
                0,
                context.slot()
            ),
            "CreateDeviceContext",
        )?;

        let mut surface = Com::null();
        check(
            vcall!(
                swap.0,
                v::DXS_GET_BUFFER,
                fn(u32, *const Guid, *mut H) -> Hr,
                0,
                &ffi::IID_DXGI_SURFACE,
                surface.slot()
            ),
            "GetBuffer(IDXGISurface)",
        )?;
        // Target only and cannot be drawn from: D2D1_BITMAP_OPTIONS 1 | 2.
        // Alpha mode 3 is ignore, which ClearType text needs.
        let props = BitmapProps1 {
            format: ffi::DXGI_FORMAT_B8G8R8A8_UNORM,
            alpha: 3,
            dpi_x: 96.0,
            dpi_y: 96.0,
            options: 3,
            color_context: ptr::null_mut(),
        };
        let mut target = Com::null();
        check(
            vcall!(
                context.0,
                v::D2C_BITMAP_FROM_SURFACE,
                fn(H, *const BitmapProps1, *mut H) -> Hr,
                surface.0,
                &props,
                target.slot()
            ),
            "CreateBitmapFromDxgiSurface",
        )?;
        vcall!(context.0, v::D2C_SET_TARGET, fn(H) -> (), target.0);
        // Layout values are device pixels, so one unit must be one pixel.
        vcall!(context.0, v::D2C_SET_DPI, fn(f32, f32) -> (), 96.0, 96.0);
        // D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, to match arm a and arm c.
        vcall!(context.0, v::D2C_SET_TEXT_AA, fn(u32) -> (), 1);
        let black = Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let mut brush = Com::null();
        check(
            vcall!(
                context.0,
                v::D2C_CREATE_BRUSH,
                fn(*const Color, H, *mut H) -> Hr,
                &black,
                ptr::null_mut(),
                brush.slot()
            ),
            "CreateSolidColorBrush",
        )?;
        let back = surface.query(&ffi::IID_D3D11_TEXTURE2D, "ID3D11Texture2D")?;
        Ok(ArmB {
            font,
            layout,
            device,
            context3d,
            swap,
            back,
            context,
            brush,
            _target: target,
            _d2d_device: d2d_device,
            _factory: factory,
        })
    }

    pub fn draw(&mut self, typed: &[char], dump_path: Option<&str>) -> Result<(), String> {
        let layout = self.layout;
        let white = Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        vcall!(self.context.0, v::D2C_BEGIN_DRAW, fn() -> ());
        vcall!(self.context.0, v::D2C_CLEAR, fn(*const Color) -> (), &white);
        for row in 0..text::ROWS {
            let glyphs = self.font.glyphs(&text::line_text(row, typed))?;
            if glyphs.is_empty() {
                continue;
            }
            let advances = vec![layout.cell as f32; glyphs.len()];
            let run = GlyphRun {
                face: self.font.face.0,
                size: layout.em_px,
                count: glyphs.len() as u32,
                indices: glyphs.as_ptr(),
                advances: advances.as_ptr(),
                offsets: ptr::null_mut(),
                sideways: 0,
                bidi: 0,
            };
            let origin = Point2f {
                x: text::MARGIN as f32,
                y: layout.baseline(row) as f32,
            };
            vcall!(
                self.context.0,
                v::D2C_DRAW_GLYPH_RUN,
                fn(Point2f, *const GlyphRun, H, u32) -> (),
                origin,
                &run,
                self.brush.0,
                0
            );
        }
        check(
            vcall!(
                self.context.0,
                v::D2C_END_DRAW,
                fn(*mut u64, *mut u64) -> Hr,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            "EndDraw",
        )?;
        if let Some(path) = dump_path {
            gpu::dump_texture(
                &self.device,
                &self.context3d,
                &self.back,
                (layout.width, layout.height),
                path,
            )?;
        }
        check(
            vcall!(self.swap.0, v::DXS_PRESENT, fn(u32, u32) -> Hr, 1, 0),
            "Present",
        )
    }
}
