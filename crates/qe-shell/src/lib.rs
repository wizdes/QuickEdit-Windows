//! QuickEdit's tab row and native frame.
mod strip;
pub use strip::*;
#[cfg(windows)]
mod window;
#[cfg(windows)]
pub use window::*;
