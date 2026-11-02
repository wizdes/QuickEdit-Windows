use crate::{Dip, DisplayList};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMetrics {
    pub width: Dip,
    pub height: Dip,
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
}
