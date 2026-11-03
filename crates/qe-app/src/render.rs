//! The window's renderer: `BitmapRenderer`, plus fonts by id, timers and saved pictures.
use crate::timing::Timing;
use qe_api::{Bitmap, Dip, DisplayList, DrawCommand, PixelRect, Rect, RenderError, Renderer};
use qe_render::{BitmapRenderer, TextSettings};
use std::cell::RefCell;
use std::rc::Rc;

/// A place for one frame's pixels, for the test channel's pictures.
pub type Capture = Rc<RefCell<CaptureState>>;
#[derive(Default)]
pub struct CaptureState {
    pub wanted: bool,
    pub frame: Option<Result<Bitmap, String>>,
}

pub struct AppRenderer {
    pub inner: BitmapRenderer,
    pub timing: Rc<Timing>,
    pub capture: Capture,
}
impl AppRenderer {
    fn register(&mut self, list: &DisplayList) {
        for command in &list.commands {
            if let DrawCommand::GlyphRun { font_id, .. } = command
                && let Some(face) = qe_layout::font_face(*font_id)
            {
                self.inner.register_font_face(*font_id, &face);
            }
        }
    }
}
impl Renderer for AppRenderer {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.inner.begin_frame()
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        self.register(list);
        self.inner.draw(list)
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        let done = self.inner.end_frame();
        let mut capture = self.capture.borrow_mut();
        if capture.wanted {
            capture.wanted = false;
            capture.frame = Some(self.inner.bitmap().map_err(|e| e.message));
        }
        done
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.inner.resize(width, height)
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.inner.set_dpi(dpi)
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        self.register(list);
        self.inner.render_to_bitmap(list, width, height)
    }
    fn invalidate(&mut self, rect: Rect) {
        self.inner.invalidate(rect);
    }
    fn invalidate_all(&mut self) {
        self.inner.invalidate_all();
    }
    fn scroll(&mut self, rect: Rect, dy: Dip) -> Result<(), RenderError> {
        self.inner.scroll(rect, dy)
    }
    fn present(&mut self, window: *mut std::ffi::c_void) -> Result<(), RenderError> {
        let result = <BitmapRenderer as Renderer>::present(&mut self.inner, window);
        self.timing.after_present();
        result
    }
    fn present_rect(
        &mut self,
        dc: *mut std::ffi::c_void,
        rect: PixelRect,
    ) -> Result<(), RenderError> {
        let result = <BitmapRenderer as Renderer>::present_rect(&mut self.inner, dc, rect);
        self.timing.after_present();
        result
    }
}

/// A renderer for a menu or a dialog, at this size in device pixels.
pub fn popup_renderer(width: u32, height: u32) -> Result<Box<dyn Renderer>, RenderError> {
    let mut renderer = BitmapRenderer::new(width, height)?;
    renderer.set_text_settings(TextSettings::reference())?;
    Ok(Box::new(renderer))
}
