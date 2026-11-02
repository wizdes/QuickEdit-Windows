//! Find, Replace, Select Line, Transformations, Complete and the text substitutions.
//!
//! `ToolsHost` is what a window uses. `WIRING.md` lists the calls. The engines beside it have
//! no UI and can run alone. Work on big text runs on a worker from `worker_snapshot`, and
//! nothing runs while the tools are closed.
//!
//! The Mac's find bar, Complete and substitutions are AppKit's, so their rules are not all in
//! the Mac source. Each module says which of its rules come from there and which do not.
mod complete;
mod host;
mod lines;
mod replace;
mod search;
mod substitute;
mod text;
mod transform;
mod ui;

pub use complete::*;
pub use host::*;
pub use lines::*;
pub use replace::*;
pub use search::*;
pub use substitute::*;
pub use text::{fold_char, is_cluster_edge, is_extend, is_word, readable_len};
pub use transform::*;
pub use ui::{BAR_ROW_HEIGHT, BarStop, FindBar, SelectLinePanel};
