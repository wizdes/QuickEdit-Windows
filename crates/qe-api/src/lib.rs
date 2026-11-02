//! Shared, platform-independent QuickEdit contracts.

mod command;
mod display;
mod encoding;
mod input;
mod layout;
mod render;
mod replay;
mod settings;
pub mod testing;
mod text;
mod theme;
mod view;
mod widget;

pub use command::*;
pub use display::*;
pub use encoding::*;
pub use input::*;
pub use layout::*;
pub use render::*;
pub use replay::*;
pub use settings::*;
pub use text::*;
pub use theme::*;
pub use view::*;
pub use widget::*;

/// A device-independent pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Dip(pub f32);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: Dip,
    pub y: Dip,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: Dip,
    pub y: Dip,
    pub width: Dip,
    pub height: Dip,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}
