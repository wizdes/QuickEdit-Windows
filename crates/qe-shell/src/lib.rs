//! The top-level window, the Terminal-style top row, Snap and the tab strip.
mod error;
mod menu_host;
mod strip;
mod window;

pub use error::*;
pub use menu_host::PopupRendererFactory;
pub use strip::*;
pub use window::*;
