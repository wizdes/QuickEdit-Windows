use crate::{Dip, DisplayList, Rect};
use std::ffi::c_void;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMetrics {
    pub width: Dip,
    /// The height of all rows of the text, using the fonts that really draw it.
    pub height: Dip,
    /// The distance from the top of the text down to its first baseline: the line
    /// ascent. `DrawCommand::TextRun` takes its position on the baseline.
    pub baseline: Dip,
}

/// UI widgets may measure text on any thread. The font token names a font
/// chosen by the caller; the renderer decides how that token maps to a font.
pub trait TextMeasurer: Send + Sync {
    fn measure(&self, text: &str, font: &str) -> TextMetrics;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderError {
    pub message: String,
}

impl RenderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for RenderError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Packed BGRA bytes in row-major order, with no row padding.
    pub bgra: Vec<u8>,
}

/// A rectangle of device pixels. `right` and `bottom` are one past the last pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A renderer is owned by the window thread. Call every method on that thread,
/// including bitmap rendering; other threads can prepare display lists.
pub trait Renderer {
    fn begin_frame(&mut self) -> Result<(), RenderError>;
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError>;
    fn end_frame(&mut self) -> Result<(), RenderError>;
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError>;
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError>;
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError>;
    /// Asks the next frame to repaint this rectangle of dips. A renderer that paints
    /// everything on every frame can keep this empty default.
    fn invalidate(&mut self, _rect: Rect) {}
    /// Asks the next frame to repaint everything. The empty default has the same limit.
    fn invalidate_all(&mut self) {}
    /// Moves the pixels of `rect` by `dy` dips. The default reports that it cannot.
    fn scroll(&mut self, _rect: Rect, _dy: Dip) -> Result<(), RenderError> {
        Err(RenderError::new("this renderer cannot scroll"))
    }
    /// Copies changed pixels to the window with this native handle (`HWND`). The
    /// default reports that it cannot.
    fn present(&mut self, _window: *mut c_void) -> Result<(), RenderError> {
        Err(RenderError::new("this renderer cannot present to a window"))
    }
    /// Copies this rectangle to a device context the caller holds (`HDC`), such as the
    /// one from `BeginPaint`. The default reports that it cannot.
    fn present_rect(&mut self, _dc: *mut c_void, _rect: PixelRect) -> Result<(), RenderError> {
        Err(RenderError::new("this renderer cannot present to a window"))
    }
}
