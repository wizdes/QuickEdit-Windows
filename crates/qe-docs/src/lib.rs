//! The document layer of QuickEdit for Windows: opening, saving, autosave and restore,
//! outside changes, the blank-window rule, Open Recent and the file handoff between
//! launches. It owns no window and no message loop. `README.md` has the wiring list.
//!
//! Everything on [`DocumentStore`] runs on the UI thread and never waits on disk. Work
//! that touches disk runs on short-lived threads. Each one ends by sending a result and
//! calling the wake callback, and the host then calls [`DocumentStore::poll`].
mod autosave;
mod error;
mod escape;
pub mod handoff;
mod identity;
mod load;
mod paths;
mod recent;
mod store;
mod watch;

pub use error::{DocError, DocErrorKind};
pub use identity::FileIdentity;
pub use paths::check_open_path;
pub use recent::{RECENT_LIMIT, recent_menu_rows};
pub use store::{
    Answer, BLANK_SCAN_LIMIT_UTF16, DocCommands, DocEvent, DocId, DocumentStore, Faults,
    OpenTarget, PanelPurpose, Question, QuestionId, QuestionKind, SavePanelRequest, ShowState,
    StoreConfig, Wake, WindowFrame, WindowId, default_root, filename_with_txt_rule,
    large_title_note, read_only_title_note,
};
