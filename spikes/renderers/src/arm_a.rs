//! Arm a: DirectWrite draws into a bitmap, and BitBlt copies it to the window.
//! No GPU API is loaded.
use crate::dump;
use crate::ffi::{self, Com, GlyphRun, H, Hr, Rect, check, v};
use crate::text::{self, Font, Layout};
use crate::vcall;
use std::{ptr, sync::Arc};

const WHITENESS: u32 = 0x00ff0062;
const SRCCOPY: u32 = 0x00cc0020;

pub struct ArmA {
    hwnd: H,
    font: Arc<Font>,
    layout: Layout,
    target: Com,
    params: Com,
}

impl ArmA {
    pub fn new(hwnd: H, font: Arc<Font>, layout: Layout) -> Result<Self, String> {
        let mut interop = Com::null();
        check(
            vcall!(
                font.factory.0,
                v::DWF_GET_GDI_INTEROP,
                fn(*mut H) -> Hr,
                interop.slot()
            ),
            "GetGdiInterop",
        )?;
        let mut params = Com::null();
        check(
            vcall!(
                font.factory.0,
                v::DWF_CREATE_RENDERING_PARAMS,
                fn(*mut H) -> Hr,
                params.slot()
            ),
            "CreateRenderingParams",
        )?;
        let mut target = Com::null();
        let screen = unsafe { ffi::GetDC(hwnd) };
        let result = check(
            vcall!(
                interop.0,
                v::DWI_CREATE_BITMAP_TARGET,
                fn(H, u32, u32, *mut H) -> Hr,
                screen,
                layout.width as u32,
                layout.height as u32,
                target.slot()
            ),
            "CreateBitmapRenderTarget",
        );
        unsafe { ffi::ReleaseDC(hwnd, screen) };
        result?;
        Ok(ArmA {
            hwnd,
            font,
            layout,
            target,
            params,
        })
    }

    pub fn draw(&mut self, typed: &[char]) -> Result<(), String> {
        let layout = self.layout;
        let memdc = vcall!(self.target.0, v::DWT_GET_MEMORY_DC, fn() -> H);
        if unsafe { ffi::PatBlt(memdc, 0, 0, layout.width, layout.height, WHITENESS) } == 0 {
            return Err("PatBlt failed".into());
        }
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
            let mut dirty = Rect::default();
            check(
                vcall!(
                    self.target.0,
                    v::DWT_DRAW_GLYPH_RUN,
                    fn(f32, f32, u32, *const GlyphRun, H, u32, *mut Rect) -> Hr,
                    text::MARGIN as f32,
                    layout.baseline(row) as f32,
                    0,
                    &run,
                    self.params.0,
                    0,
                    &mut dirty
                ),
                "DrawGlyphRun",
            )?;
        }
        let dc = unsafe { ffi::GetDC(self.hwnd) };
        let copied =
            unsafe { ffi::BitBlt(dc, 0, 0, layout.width, layout.height, memdc, 0, 0, SRCCOPY) };
        // GDI can hold drawing calls in a batch. This sends them, so the timer
        // stops after the copy was really issued.
        unsafe {
            ffi::GdiFlush();
            ffi::ReleaseDC(self.hwnd, dc);
        }
        if copied == 0 {
            return Err("BitBlt failed".into());
        }
        Ok(())
    }

    /// Saves the bitmap for `--dump-frame`.
    pub fn dump(&self, path: &str) -> Result<(), String> {
        const OBJ_BITMAP: u32 = 7;
        let layout = self.layout;
        let memdc = vcall!(self.target.0, v::DWT_GET_MEMORY_DC, fn() -> H);
        let bitmap = unsafe { ffi::GetCurrentObject(memdc, OBJ_BITMAP) };
        // A negative height asks for rows from the top down.
        let mut info = ffi::BitmapInfoHeader {
            size: 40,
            width: layout.width,
            height: -layout.height,
            planes: 1,
            bits: 32,
            compression: 0,
            image_size: 0,
            x_ppm: 0,
            y_ppm: 0,
            used: 0,
            important: 0,
        };
        let mut pixels = vec![0u8; (layout.width * layout.height * 4) as usize];
        let lines = unsafe {
            ffi::GetDIBits(
                memdc,
                bitmap,
                0,
                layout.height as u32,
                pixels.as_mut_ptr(),
                &mut info,
                0,
            )
        };
        if lines != layout.height {
            return Err(format!("GetDIBits returned {lines} lines"));
        }
        dump::write_bmp(
            path,
            layout.width as u32,
            layout.height as u32,
            layout.width as u32 * 4,
            &pixels,
        )
    }
}
