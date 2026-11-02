//! Screen reader support: the editor's text and the painted widgets, through Windows UI
//! Automation (UIA). The README has the wiring recipe and the rules.
//!
//! Nothing here exists until a client sends WM_GETOBJECT, and nothing here runs after that
//! unless a client calls or the host reports input.
pub mod clusters;
mod com;
pub mod events;
pub mod host;
pub mod marshal;
pub mod provider;
pub mod search;
pub mod text;
pub mod widgets;

pub use events::{EventFlags, EventTracker};
pub use host::{UiThreadOnly, UiaHost, WidgetAction};
pub use text::{GET_TEXT_CAP, ReadError, TextRange};
pub use widgets::{Accessibility, UiaSlot, WidgetEvents};

// The `implement` macro names `windows_core` directly. This re-exports the exact version
// that `windows` bundles, so this crate needs no lock-file edit.
extern crate self as windows_core;
#[doc(hidden)]
pub use windows::core::*;
