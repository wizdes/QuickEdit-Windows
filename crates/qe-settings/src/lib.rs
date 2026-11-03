//! Settings for QuickEdit: the store, the key table, the Settings window and its
//! Keyboard recorder. `README.md` is the wiring list for the app.
//!
//! Startup uses only `store` and `keys`: they read one small text file and make no
//! window. `panel`, `table` and `window` run when the user opens Settings.

mod font_dialog;
mod format;
mod keys;
mod panel;
mod recorder;
mod store;
mod table;
mod window;

pub use font_dialog::*;
pub use format::{StoredValue, integer_range, parse_font};
pub use keys::*;
pub use panel::*;
pub use recorder::*;
pub use store::*;
pub use table::*;
pub use window::*;
