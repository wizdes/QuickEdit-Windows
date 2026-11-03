//! `DocumentStore`: every open document, its windows and tabs, and the rules between them.
//! Each public call runs on the UI thread and does no disk work there.
use crate::autosave::{
    AutosaveWrite, DocMeta, OpenChoice, RestoreDoc, RestorePlan, SessionLock, WindowMeta,
    build_restore_plan, encode_session, remove_generations, write_generation, write_manifest,
    write_recent, write_session_end_failures,
};
pub use crate::autosave::{ShowState, WindowFrame};
use crate::error::{DocError, DocErrorKind};
use crate::identity::{FileBase, FileIdentity, file_base, is_in_recycle_bin, locate_by_identity};
use crate::load::{
    Body, Limits, LoadJob, LoadResult, Loaded, Source, read_all_text, remove_quietly, run_load,
};
use crate::paths::{check_new_path, check_open_path, is_network_path};
use crate::recent::{add_recent, encode_recent, same_name, same_path_text};
use crate::watch::{FolderNews, FolderWatch, watch_folder};
use qe_api::{ByteOffset, EncodingChoice, TextEncoding, TextSnapshot, ViewContext, ViewRequest};
use qe_buffer::Notify;
use qe_codec::{
    AtomicWriteResult, CodecError, FileStamp, Format, atomic_write, atomic_write_if_unchanged,
    can_encode, encode, file_stamp, find_leftovers, prepare_large_copy,
};
use qe_editor::{ChangesSince, EditorCommand, EditorView, LoadedFile};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The wake callback. It runs on a worker thread and must only post a message.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A text longer than this many UTF-16 units is never blank, as on the Mac: about 1 MB,
/// so a huge text cannot stall a close.
pub const BLANK_SCAN_LIMIT_UTF16: usize = 1_048_576;
const MAX_LOADS_AT_ONCE: usize = 4;
/// A network path can wait a long time, so these have their own slots apart from the four.
const MAX_REMOTE_LOADS_AT_ONCE: usize = 2;
/// The most tabs one call or one poll makes. The rest wait in a queue, so the UI stays awake.
const MAX_OPENS_PER_WAKE: usize = 32;
/// How long the UI gets for other messages between two groups of tabs.
const OPEN_SPACING: Duration = Duration::from_millis(5);
/// The most files that wait in that queue. A list past this is named as too long.
const MAX_QUEUED_OPENS: usize = 4096;
/// How long a thread that would not start waits before an autosave tries again.
const THREAD_RETRY: Duration = Duration::from_secs(5);
const MAX_FIT_CHECK_ITEMS: usize = 5_000_000;
/// An inserted text longer than this waits for the Save to learn whether it fits.
const FIT_CHECK_BYTES: usize = 64 * 1024;
/// A file gone from its name gets this long before anything is said or followed. Many
/// editors save by delete then create, or by a rename to a backup then a new file.
const MISSING_RECHECK: Duration = Duration::from_millis(300);
const RELOAD_RETRY: Duration = Duration::from_millis(250);
const RELOAD_TRIES: u32 = 4;

/// A number that names one document for this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocId(pub u64);
/// A number that names one window for this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuestionId(pub u64);

/// Failures a test can ask for. Every field is off by default.
#[doc(hidden)]
#[derive(Default)]
pub struct Faults {
    /// Non-zero: each Save fails with this Windows code before it writes.
    pub save_error_code: AtomicU32,
    /// While true, a Save waits between taking its text and writing it.
    pub hold_save: AtomicBool,
    /// Goes true when a Save reaches that wait.
    pub save_is_held: AtomicBool,
    /// While true, an autosave waits before it writes.
    pub hold_autosave: AtomicBool,
    pub autosave_is_held: AtomicBool,
    /// Non-zero: an autosave fails with this Windows code once it has written
    /// `autosave_fail_after` bytes.
    pub autosave_error_code: AtomicU32,
    pub autosave_fail_after: AtomicU64,
    /// Non-zero: an autosave stops for good here, for a test that kills the process.
    pub autosave_stall_after: AtomicU64,
    pub autosave_is_stalled: AtomicBool,
    /// While true, a new folder watch waits before it starts to listen.
    pub hold_watch: AtomicBool,
    pub watch_is_held: AtomicBool,
    /// While true, a reload waits before it reads.
    pub hold_reload: AtomicBool,
    pub reload_is_held: AtomicBool,
    /// Called on the worker just before a Save's swap.
    pub before_swap: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Called on the worker just after a Save's swap, before its stamp is taken.
    pub after_swap: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// While true, a restore's read of an autosaved copy waits before it reads.
    pub hold_restore_load: AtomicBool,
    pub restore_load_is_held: AtomicBool,
    /// While true, a load from a network path waits before it reads.
    pub hold_remote_load: AtomicBool,
    pub remote_load_is_held: AtomicBool,
    /// While true, no worker thread can start, as when Windows has no room for one.
    pub refuse_threads: AtomicBool,
}

/// What the store needs from the app. `from_settings` fills it from `qe-settings`.
pub struct StoreConfig {
    /// `%LOCALAPPDATA%\QuickEdit`, or a test's own folder.
    pub root: PathBuf,
    /// How long after the first unsaved edit a copy is written. Zero turns off the timer,
    /// as on the Mac. A quit still writes.
    pub autosave_delay: Duration,
    pub open_encoding: EncodingChoice,
    pub save_encoding: EncodingChoice,
    pub add_txt: bool,
    /// New documents start in Wrap to Page. Files from disk never do.
    pub new_docs_wrap_to_page: bool,
    pub large_bytes: u64,
    pub read_only_bytes: u64,
    /// How long a change of windows, tabs or caret waits before the list is written.
    pub session_delay: Duration,
    pub watch_folders: bool,
    pub wake: Wake,
    #[doc(hidden)]
    pub faults: Arc<Faults>,
}

impl StoreConfig {
    pub fn new(root: impl Into<PathBuf>, wake: Wake) -> Self {
        Self {
            root: root.into(),
            autosave_delay: Duration::from_secs(30),
            open_encoding: EncodingChoice::Automatic,
            save_encoding: EncodingChoice::Automatic,
            add_txt: true,
            new_docs_wrap_to_page: false,
            large_bytes: 50 * 1024 * 1024,
            read_only_bytes: 512 * 1024 * 1024,
            session_delay: Duration::from_secs(2),
            watch_folders: true,
            wake,
            faults: Arc::default(),
        }
    }
    /// The settings that matter here, read once. Open documents keep what they had.
    pub fn from_settings(settings: &qe_settings::SettingsStore, wake: Wake) -> Self {
        use qe_api::SettingKey as Key;
        let root = settings
            .path()
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let mut config = Self::new(root, wake);
        if let Some(seconds) = settings.get_integer(Key::AutosavingDelaySeconds) {
            config.autosave_delay = Duration::from_secs(seconds.max(0) as u64);
        }
        config.open_encoding = settings.encoding_choice(Key::OpenEncoding);
        config.save_encoding = settings.encoding_choice(Key::SaveEncoding);
        config.add_txt = settings.get_bool(Key::AddTxtExtension).unwrap_or(true);
        config.new_docs_wrap_to_page = settings.get_bool(Key::ShowPageBreaks).unwrap_or(false);
        config
    }
    /// The settings that the next document reads, from a Settings window's change.
    fn follow(&mut self, settings: &qe_settings::SettingsStore) {
        use qe_api::SettingKey as Key;
        self.open_encoding = settings.encoding_choice(Key::OpenEncoding);
        self.save_encoding = settings.encoding_choice(Key::SaveEncoding);
        self.add_txt = settings.get_bool(Key::AddTxtExtension).unwrap_or(true);
        self.new_docs_wrap_to_page = settings.get_bool(Key::ShowPageBreaks).unwrap_or(false);
    }
}

/// `%LOCALAPPDATA%\QuickEdit`.
pub fn default_root() -> Result<PathBuf, String> {
    let settings = qe_settings::default_settings_path()?;
    settings
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no folder", settings.display()))
}

/// "name" becomes "name.txt" when the box is on and the name has no extension. Any other
/// name stays as typed. The same rule as the Mac's.
pub fn filename_with_txt_rule(name: &str, add_txt: bool) -> String {
    let has_extension = name.rfind('.').is_some_and(|at| at + 1 < name.len());
    if !add_txt || name.is_empty() || has_extension {
        name.to_owned()
    } else {
        format!("{name}.txt")
    }
}

/// The same words and the same size style as the Mac's title, which uses decimal units.
pub fn read_only_title_note(byte_count: u64) -> String {
    let size = if byte_count >= 1_000_000_000 {
        format!("{:.2} GB", byte_count as f64 / 1e9)
    } else {
        format!("{} MB", (byte_count as f64 / 1e6).round() as u64)
    };
    format!("Read only: file too large to edit ({size})")
}
pub fn large_title_note() -> &'static str {
    "Large file: spelling and substitutions off"
}

/// Where a file should open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    /// A tab of the window the user used last, which is where Explorer's files go.
    LastUsedWindow,
    Window(WindowId),
    /// One new window, with these files as its tabs.
    NewWindow,
}

/// A decision only a person can make. The store never waits for it. Show it, then call
/// [`DocumentStore::answer`] with one of `choices`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: QuestionId,
    pub doc: DocId,
    pub kind: QuestionKind,
    pub title: String,
    pub detail: String,
    pub choices: Vec<Answer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    /// Close of a document with unsaved changes.
    SaveBeforeClose,
    /// The first edit of a file that would change on save.
    LossyFirstEdit,
    /// A Save over a file that would change.
    LossyOverwrite,
    /// `on_edit`: Cancel takes the edit back. Otherwise it came from a Save.
    EncodingNoLongerFits { on_edit: bool },
    /// Another program changed the file since it was read.
    ChangedOnDisk,
    /// The file has the read-only mark. `on_edit`: Cancel takes the edit back.
    LockedFile { on_edit: bool },
    /// Revert to Saved throws away the unsaved changes.
    ConfirmRevert,
    /// A quit could not keep this document's unsaved text.
    AutosaveFailedAtQuit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answer {
    Save,
    DontSave,
    Cancel,
    Duplicate,
    Overwrite,
    SaveAnyway,
    Revert,
    Unlock,
    ChooseEncoding,
}

/// Why a Save dialog is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelPurpose {
    /// A document with no file was saved.
    Save,
    /// Save As, or a choice of another encoding.
    SaveAs,
}

/// All that a Save dialog needs. The host then calls `save_as` or `panel_cancelled`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePanelRequest {
    pub doc: DocId,
    pub purpose: PanelPurpose,
    pub suggested_name: String,
    pub folder: Option<PathBuf>,
    pub initial_encoding: TextEncoding,
    /// The bytes go out as they are, so the encoding list is off.
    pub encoding_fixed: bool,
    /// The state of the ".txt" box. Use `filename_with_txt_rule` on the typed name.
    pub add_txt: bool,
}

/// What happened. Take them with [`DocumentStore::take_events`] after every call and poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocEvent {
    WindowAdded(WindowId),
    WindowClosed(WindowId),
    /// The window's tab list or its active tab changed.
    TabsChanged(WindowId),
    /// Bring this tab forward.
    Focus {
        window: WindowId,
        doc: DocId,
    },
    /// The title, the dirty mark, the commands or the large-file state may differ.
    DocChanged(DocId),
    /// The view holds other text now. Its caret and scroll are set. Read the size state.
    TextReplaced(DocId),
    DocClosed(DocId),
    OpenFailed {
        path: PathBuf,
        error: DocError,
    },
    /// This file was already open, in this tab. It was not opened twice.
    AlreadyOpen {
        doc: DocId,
        path: PathBuf,
    },
    Question(Question),
    NeedsSavePanel(SavePanelRequest),
    SaveFinished {
        doc: DocId,
        path: PathBuf,
        /// More typing came during the write, so the document stays dirty.
        still_dirty: bool,
        /// Another program wrote in the last instant. Its bytes are in this file.
        other_version: Option<PathBuf>,
        /// The old bytes could not be removed from here. It is safe to delete.
        backup_kept: Option<PathBuf>,
    },
    SaveFailed {
        doc: DocId,
        error: DocError,
        /// Save As can still work, so offer it.
        offer_save_as: bool,
    },
    AutosaveFailed {
        doc: DocId,
        error: DocError,
    },
    /// Another program changed the file, and the window now shows the new text.
    Reloaded(DocId),
    ReloadFailed {
        doc: DocId,
        error: DocError,
    },
    /// A Rename or a Move To ended, or another program moved the file.
    FileMoved {
        doc: DocId,
        path: PathBuf,
    },
    MoveFailed {
        doc: DocId,
        error: DocError,
    },
    /// The file is gone. The text stays open and counts as unsaved. A Save writes it again.
    FileDeleted {
        doc: DocId,
        path: PathBuf,
    },
    /// `Unlock` could not clear the read-only mark.
    UnlockFailed {
        doc: DocId,
        error: DocError,
    },
    /// Which of the listed encodings cannot hold this document's text.
    EncodingFit {
        doc: DocId,
        unfit: Vec<TextEncoding>,
    },
    /// A file changed on disk while QuickEdit was not running, and this document came back
    /// with unsaved text that is older. Saving asks first.
    RestoredOverNewerFile {
        doc: DocId,
        path: PathBuf,
    },
    /// Restore has made every window. No window means: make the launch window.
    RestoreFinished {
        windows: usize,
    },
    /// Something could not come back, in plain words.
    RestoreProblem(DocError),
    /// A crash left a file beside this path that may hold text. Offer it to the user, and
    /// call `recover_leftover` if they want it back.
    LeftoverFound {
        path: PathBuf,
        leftover: PathBuf,
    },
    /// `recover_leftover` put the old text back at this path. Open it as usual.
    LeftoverRecovered {
        path: PathBuf,
    },
    LeftoverNotRecovered {
        path: PathBuf,
        error: DocError,
    },
    RecentChanged,
    /// Writing the list of windows or Open Recent failed. The next change tries again.
    SessionWriteFailed(DocError),
    /// This folder cannot be watched, so changes show only at the next window activation.
    WatchUnavailable {
        folder: PathBuf,
        reason: String,
    },
    /// All text is on disk. The app may exit.
    ReadyToQuit,
    /// The last window has gone, after `RestoreFinished`. `opened_nothing`: a file failed to
    /// load and its window went with it, so nothing is on screen and the host must call
    /// `new_window(true)`. Otherwise the user closed it, and the host does as it does then.
    LastWindowGone {
        opened_nothing: bool,
    },
    /// `discard_leftover` removed the file.
    LeftoverDeleted {
        leftover: PathBuf,
    },
    LeftoverNotDeleted {
        leftover: PathBuf,
        error: DocError,
    },
}

/// Which File menu rows are on for a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocCommands {
    pub save: bool,
    pub save_as: bool,
    pub duplicate: bool,
    pub rename: bool,
    pub move_to: bool,
    pub revert: bool,
    pub prevent_editing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileState {
    Present,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Place {
    anchor: u64,
    caret: u64,
    top: u64,
}

#[derive(Default)]
struct AutosaveState {
    due: Option<Instant>,
    running: Option<Arc<AtomicBool>>,
    again: bool,
    generation: Option<u64>,
    written_revision: Option<u64>,
    last_failure: Option<String>,
    last_failure_kind: Option<DocErrorKind>,
}

enum PendingAdopt {
    /// Waits for a Cut to reach the clipboard, or for a composition to end before a reload.
    Waiting { body: Body, then: AdoptThen },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdoptThen {
    Plain,
    /// A reload from another program's change, so the place stays.
    Reloaded,
    /// The same file through another door, such as a UTF-8 copy. The place stays, and no
    /// other program changed anything.
    SameFile,
    /// Revert: the caret goes to the start, as on the Mac.
    Reverted,
}

struct Doc {
    id: DocId,
    key: String,
    view: EditorView,
    path: Option<PathBuf>,
    /// A name for a document with no file: "foo copy.txt".
    custom_name: Option<String>,
    untitled_number: u32,
    format: Format,
    open_choice: OpenChoice,
    byte_count: u64,
    read_only: bool,
    /// The large file whose bytes a Save As copies. A copy with no file has one too.
    copy_source: Option<PathBuf>,
    temp_copy: Option<PathBuf>,
    base: Option<FileBase>,
    stamp: Option<FileStamp>,
    file_state: FileState,
    /// Text that disk does not hold, though the view is at its save point: a restored
    /// autosave, a duplicate, or a deleted file.
    forced_dirty: bool,
    /// `forced_dirty` came only from the file going away, with no edit behind it. So the
    /// return of the same text ends it. A restored copy or a duplicate never has this.
    dirty_from_delete: bool,
    /// A worker compares a new file at this deleted file's name with the text here.
    return_check: bool,
    /// The file changed behind this text and the person has not answered, so a Save must
    /// ask. It lasts through a restart until a Save or a reload ends it.
    changed_while_away: bool,
    /// Moves on with each of this document's own changes to its file or marks. A look at the
    /// file that began before one of them says nothing about the file now.
    file_epoch: u64,
    loading: bool,
    hidden: bool,
    transient: bool,
    prevent_editing: bool,
    wrap_to_page: bool,
    seen_revision: u64,
    was_dirty: bool,
    autosave: AutosaveState,
    saving: bool,
    save_again: bool,
    close_after_save: bool,
    reloading: bool,
    reload_again: bool,
    reload_tries: u32,
    reload_retry_at: Option<Instant>,
    undecodable: Option<FileStamp>,
    missing_recheck_at: Option<Instant>,
    rename_hint: Option<PathBuf>,
    place: Place,
    pending_place: Option<Place>,
    pending_adopt: Option<PendingAdopt>,
    load_cancel: Arc<AtomicBool>,
    moving: bool,
    tried_copy: bool,
    asked_lossy: bool,
    asked_locked: bool,
    asked_fit: bool,
    /// The open dialog came from the first-edit encoding question, so its Cancel undoes.
    fit_panel_undoes: bool,
    unlocking: bool,
    panel_open: bool,
    /// The user said to quit without this document's unsaved text.
    quit_skip: bool,
    missing_confirm: bool,
}

struct Win {
    id: WindowId,
    frame: WindowFrame,
    show: ShowState,
    tabs: Vec<DocId>,
    active: Option<DocId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuestionUse {
    Plain,
    QuitReview,
}

/// A window whose Close is asking its questions.
struct WindowClosing {
    window: WindowId,
    /// Each Don't Save, with the revision that the answer was about.
    discard: Vec<(DocId, u64)>,
}

struct OpenQuestion {
    question: Question,
    usage: QuestionUse,
}

#[derive(Clone)]
enum LoadKind {
    Open {
        replaces: Option<DocId>,
        path: PathBuf,
    },
    Reload,
    /// A file came to the name of a deleted file. The tab's text may exist nowhere else, so
    /// this only compares. See `finish_return_check`.
    ReturnCheck,
    Revert,
    LargeRetry,
    AfterCopy,
    RestoreFile {
        path: PathBuf,
    },
    RestoreAutosave {
        path: Option<PathBuf>,
        saved_base: Option<FileBase>,
    },
    Duplicate,
    RestoreCopy,
}

struct SaveDone {
    written: u64,
    base: Option<FileBase>,
    stamp: Option<FileStamp>,
    other_version: Option<PathBuf>,
    backup_kept: Option<PathBuf>,
}

enum Completion {
    Load {
        doc: DocId,
        kind: LoadKind,
        remote: bool,
        result: Box<LoadResult>,
        identity_now: Option<(Option<FileBase>, Option<FileStamp>)>,
        leftovers: Vec<PathBuf>,
    },
    Save {
        doc: DocId,
        revision: u64,
        path: PathBuf,
        format: Format,
        save_as: bool,
        result: Result<SaveDone, SaveTrouble>,
    },
    Autosave {
        doc: DocId,
        key: String,
        generation: u64,
        revision: u64,
        result: Result<bool, DocError>,
    },
    AutosaveRemoved {
        doc: DocId,
        key: String,
        below: u64,
        result: Result<(), DocError>,
    },
    Stat {
        remote: bool,
        results: Vec<StatResult>,
    },
    Moved {
        doc: DocId,
        path: PathBuf,
        result: Result<(FileBase, Option<FileStamp>), DocError>,
    },
    Unlocked {
        doc: DocId,
        then_save: bool,
        result: Result<(), DocError>,
    },
    Fit {
        doc: DocId,
        unfit: Vec<TextEncoding>,
    },
    Recovered {
        path: PathBuf,
        result: Result<(), DocError>,
    },
    LeftoverGone {
        leftover: PathBuf,
        result: Result<(), DocError>,
    },
    SessionWritten(Result<(), DocError>),
    RecentWritten(Result<(), DocError>),
    Restored(Box<Result<RestorePlan, DocError>>),
    Folder {
        folder: PathBuf,
        news: FolderNews,
    },
}

enum SaveTrouble {
    Changed {
        new_text: Option<PathBuf>,
    },
    DoesNotFit,
    Locked,
    Other {
        error: DocError,
        offer_save_as: bool,
    },
}

/// Files waiting to become tabs, in order.
struct OpenBatch {
    paths: VecDeque<PathBuf>,
    target: OpenTarget,
    choice: Option<EncodingChoice>,
}

struct StatItem {
    doc: DocId,
    epoch: u64,
    path: PathBuf,
    identity: Option<FileIdentity>,
    links: u32,
    hint: Option<PathBuf>,
}

struct StatResult {
    doc: DocId,
    epoch: u64,
    /// `None`: no file at the path.
    now: Option<(FileBase, Option<FileStamp>)>,
    moved_to: Option<(PathBuf, FileBase, Option<FileStamp>)>,
}

pub struct DocumentStore {
    config: StoreConfig,
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    docs: BTreeMap<DocId, Doc>,
    windows: Vec<Win>,
    last_used: Option<WindowId>,
    next_id: u64,
    next_generation: u64,
    next_question: u64,
    events: Vec<DocEvent>,
    requests: Vec<(DocId, ViewRequest)>,
    questions: Vec<OpenQuestion>,
    load_queue: VecDeque<(DocId, LoadKind, LoadJob, bool)>,
    remote_load_queue: VecDeque<(DocId, LoadKind, LoadJob, bool)>,
    loads_running: usize,
    remote_loads_running: usize,
    open_queue: VecDeque<OpenBatch>,
    open_due: Option<Instant>,
    saves_running: usize,
    stat_running: bool,
    remote_stat_running: bool,
    stat_waiting: BTreeSet<DocId>,
    close_queue: VecDeque<DocId>,
    /// A window closing as one. Its tabs stay until every question has its answer.
    closing_window: Option<WindowClosing>,
    panel_queue: VecDeque<DocId>,
    want_focus: Option<DocId>,
    removals_running: usize,
    /// For each document key, the copies below this generation hold nothing to bring back.
    /// The list carries this until a removal is seen to finish, so a failed removal or a kill
    /// cannot bring a stale copy back.
    dead_copies: BTreeMap<String, u64>,
    /// Closed views that still have a Cut on its way to the clipboard.
    closing_views: Vec<EditorView>,
    session: Option<SessionLock>,
    session_failed: bool,
    restore_started: bool,
    restored: bool,
    restore_finished_sent: bool,
    window_gone_by_failure: bool,
    session_dirty: bool,
    session_due: Option<Instant>,
    session_running: bool,
    recent: Vec<PathBuf>,
    recent_dirty: bool,
    recent_running: bool,
    watches: Vec<FolderWatch>,
    watch_failed: BTreeSet<PathBuf>,
    quitting: bool,
    quit_ready_sent: bool,
    quit_review: VecDeque<DocId>,
    /// `undo` is taking back an edit, so that change raises no first-edit question.
    taking_back_edit: bool,
    /// `wait_for_quit` runs: nobody can answer, and Windows ends the process at its end.
    session_ending: bool,
    /// Saves that failed during `wait_for_quit`. The next launch tells of them.
    session_end_failures: Vec<String>,
    /// Which texts the last `session_end_copies` covered.
    session_end_covered: Vec<(DocId, u64)>,
}

/// Copies of unsaved text that can be written with no access to the store. `WM_ENDSESSION` can
/// reach the app while the store is in use lower on the same stack. That call cannot end until
/// the message handler returns, and Windows may end the process when it does. So the host keeps
/// one of these ready. It holds the text as of the last `session_end_copies`.
pub struct SessionEndCopies {
    docs_dir: PathBuf,
    items: Vec<SessionEndCopy>,
}

struct SessionEndCopy {
    meta: DocMeta,
    generation: u64,
    previous: Option<u64>,
    snapshot: Arc<dyn TextSnapshot>,
}

impl SessionEndCopies {
    /// Writes every copy, on this thread. It waits on the disk, so a host calls it only when
    /// Windows is ending the session. It returns what could not be written.
    pub fn write(&self) -> Vec<DocError> {
        let cancel = AtomicBool::new(false);
        let mut failed = Vec::new();
        for item in &self.items {
            let result = write_generation(&AutosaveWrite {
                docs_dir: &self.docs_dir,
                meta: &item.meta,
                generation: item.generation,
                previous: item.previous,
                snapshot: item.snapshot.as_ref(),
                cancel: &cancel,
                progress: &|_| Ok(()),
            });
            if let Err(error) = result {
                failed.push(error);
            }
        }
        failed
    }
}

fn is_blank_text(snapshot: &dyn TextSnapshot) -> bool {
    let length = snapshot.len_bytes();
    let mut blank = true;
    let read = snapshot.visit_chunks_while(ByteOffset(0)..length, &mut |chunk| {
        blank = chunk.chars().all(char::is_whitespace);
        blank
    });
    read && blank
}

fn random_key() -> String {
    use std::hash::{BuildHasher, Hasher};
    // Each `RandomState` has fresh keys from Windows, so two make a 128-bit name.
    let word = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0);
        hasher.finish()
    };
    format!("{:016x}{:016x}", word(), word())
}

impl DocumentStore {
    /// Touches no disk. Call [`begin_restore`](Self::begin_restore) next, once. No autosave
    /// runs until it ends.
    /// Settings apply to the next document (P52, I14). Open documents keep what they had.
    pub fn follow_settings(&mut self, settings: &qe_settings::SettingsStore) {
        self.config.follow(settings);
    }

    pub fn new(config: StoreConfig) -> Self {
        let (sender, receiver) = channel();
        Self {
            config,
            sender,
            receiver,
            docs: BTreeMap::new(),
            windows: Vec::new(),
            last_used: None,
            next_id: 1,
            next_generation: 1,
            next_question: 1,
            events: Vec::new(),
            requests: Vec::new(),
            questions: Vec::new(),
            load_queue: VecDeque::new(),
            remote_load_queue: VecDeque::new(),
            loads_running: 0,
            remote_loads_running: 0,
            open_queue: VecDeque::new(),
            open_due: None,
            saves_running: 0,
            stat_running: false,
            remote_stat_running: false,
            stat_waiting: BTreeSet::new(),
            close_queue: VecDeque::new(),
            closing_window: None,
            panel_queue: VecDeque::new(),
            want_focus: None,
            removals_running: 0,
            dead_copies: BTreeMap::new(),
            closing_views: Vec::new(),
            session: None,
            session_failed: false,
            restore_started: false,
            restored: false,
            restore_finished_sent: false,
            window_gone_by_failure: false,
            session_dirty: false,
            session_due: None,
            session_running: false,
            recent: Vec::new(),
            recent_dirty: false,
            recent_running: false,
            watches: Vec::new(),
            watch_failed: BTreeSet::new(),
            quitting: false,
            quit_ready_sent: false,
            quit_review: VecDeque::new(),
            taking_back_edit: false,
            session_ending: false,
            session_end_failures: Vec::new(),
            session_end_covered: Vec::new(),
        }
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn spawn(&self, name: &str, work: impl FnOnce() -> Completion + Send + 'static) -> bool {
        if self.config.faults.refuse_threads.load(Ordering::Relaxed) {
            return false;
        }
        let sender = self.sender.clone();
        let wake = self.config.wake.clone();
        let started = std::thread::Builder::new()
            .name(format!("qe-docs-{name}"))
            .spawn(move || {
                let completion = work();
                if sender.send(completion).is_ok() {
                    wake();
                }
            });
        match started {
            Ok(_) => true,
            Err(error) => {
                eprintln!("QuickEdit: a {name} thread did not start: {error}");
                false
            }
        }
    }

    // ---------------------------------------------------------------- windows and tabs

    /// All windows, in the order they were made.
    pub fn windows(&self) -> Vec<WindowId> {
        self.windows.iter().map(|w| w.id).collect()
    }
    /// The tabs of a window, in order. A file on its way to replace the launch window is not
    /// here until it has loaded.
    pub fn tabs(&self, window: WindowId) -> Vec<DocId> {
        self.window(window)
            .map_or_else(Vec::new, |w| w.tabs.clone())
    }
    pub fn active_tab(&self, window: WindowId) -> Option<DocId> {
        self.window(window).and_then(|w| w.active)
    }
    pub fn last_used_window(&self) -> Option<WindowId> {
        self.last_used
    }
    pub fn window_frame(&self, window: WindowId) -> Option<(WindowFrame, ShowState)> {
        self.window(window).map(|w| (w.frame, w.show))
    }
    pub fn window_of(&self, doc: DocId) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|w| w.tabs.contains(&doc))
            .map(|w| w.id)
    }
    fn window(&self, window: WindowId) -> Option<&Win> {
        self.windows.iter().find(|w| w.id == window)
    }
    fn window_mut(&mut self, window: WindowId) -> Option<&mut Win> {
        self.windows.iter_mut().find(|w| w.id == window)
    }

    /// Makes a window with one untitled tab. `launch`: this is the window made at launch or
    /// with no other window, which a file replaces while it stays untouched.
    pub fn new_window(&mut self, launch: bool) -> Result<(WindowId, DocId), DocError> {
        let window = self.add_window();
        let doc = self.add_untitled()?;
        let alone = self.visible_doc_count() == 0;
        if let Some(entry) = self.docs.get_mut(&doc) {
            entry.transient = launch && alone;
        }
        self.end_other_transients(doc);
        self.attach(window, doc, None, true);
        Ok((window, doc))
    }

    /// New Tab: an untitled tab in this window.
    pub fn new_tab(&mut self, window: WindowId) -> Result<DocId, DocError> {
        if self.window(window).is_none() {
            return Err(no_window());
        }
        let doc = self.add_untitled()?;
        self.end_other_transients(doc);
        self.attach(window, doc, None, true);
        Ok(doc)
    }

    fn visible_doc_count(&self) -> usize {
        self.docs
            .values()
            .filter(|d| !d.hidden)
            .count()
            .saturating_sub(1)
    }

    /// A second document ends the first one's launch state, as on the Mac.
    fn end_other_transients(&mut self, keep: DocId) {
        for doc in self.docs.values_mut() {
            if doc.id != keep {
                doc.transient = false;
            }
        }
    }

    fn add_window(&mut self) -> WindowId {
        let id = WindowId(self.fresh_id());
        self.windows.push(Win {
            id,
            frame: WindowFrame::default(),
            show: ShowState::Normal,
            tabs: Vec::new(),
            active: None,
        });
        self.last_used = Some(id);
        self.events.push(DocEvent::WindowAdded(id));
        self.touch_session();
        id
    }

    fn attach(&mut self, window: WindowId, doc: DocId, at: Option<usize>, activate: bool) {
        let Some(win) = self.window_mut(window) else {
            return;
        };
        let at = at.map_or(win.tabs.len(), |at| at.min(win.tabs.len()));
        win.tabs.insert(at, doc);
        if activate || win.active.is_none() {
            win.active = Some(doc);
        }
        self.events.push(DocEvent::TabsChanged(window));
        if activate {
            self.events.push(DocEvent::Focus { window, doc });
        }
        self.touch_session();
    }

    /// The user used this window: Explorer's next files go to it.
    pub fn window_activated(&mut self, window: WindowId) {
        if self.window(window).is_some() {
            self.last_used = Some(window);
            self.touch_session();
        }
    }
    pub fn set_active_tab(&mut self, window: WindowId, doc: DocId) {
        let Some(win) = self.window_mut(window) else {
            return;
        };
        if win.tabs.contains(&doc) && win.active != Some(doc) {
            win.active = Some(doc);
            self.events.push(DocEvent::TabsChanged(window));
            self.touch_session();
        }
    }
    /// The window's place on screen. `by_user`: the user moved or resized it, which ends the
    /// launch state of its document.
    pub fn set_window_frame(
        &mut self,
        window: WindowId,
        frame: WindowFrame,
        show: ShowState,
        by_user: bool,
    ) {
        let tabs = match self.window_mut(window) {
            Some(win) => {
                win.frame = frame;
                win.show = show;
                win.tabs.clone()
            }
            None => return,
        };
        if by_user {
            for tab in tabs {
                if let Some(doc) = self.docs.get_mut(&tab) {
                    doc.transient = false;
                }
            }
        }
        self.touch_session();
    }
    /// Moves a tab within its window or to another. An empty old window closes.
    pub fn move_tab(&mut self, doc: DocId, to: WindowId, index: usize) {
        let Some(from) = self.window_of(doc) else {
            return;
        };
        if self.window(to).is_none() {
            return;
        }
        self.detach(from, doc);
        self.attach(to, doc, Some(index), true);
        // The tab went there, so that is where the next file from Explorer belongs.
        self.last_used = Some(to);
        self.drop_empty_window(from);
    }
    /// Move Tab to New Window.
    pub fn move_tab_to_new_window(&mut self, doc: DocId) -> Option<WindowId> {
        let from = self.window_of(doc)?;
        if self.tabs(from).len() < 2 {
            return Some(from);
        }
        self.detach(from, doc);
        let window = self.add_window();
        self.attach(window, doc, None, true);
        Some(window)
    }
    /// Merge All Windows: every tab goes to the last used window, in order.
    pub fn merge_all_windows(&mut self) {
        let Some(into) = self
            .last_used
            .or_else(|| self.windows.first().map(|w| w.id))
        else {
            return;
        };
        let others: Vec<WindowId> = self.windows().into_iter().filter(|w| *w != into).collect();
        for window in others {
            for doc in self.tabs(window) {
                self.detach(window, doc);
                self.attach(into, doc, None, false);
            }
            self.drop_empty_window(window);
        }
    }
    fn detach(&mut self, window: WindowId, doc: DocId) {
        let Some(win) = self.window_mut(window) else {
            return;
        };
        let Some(at) = win.tabs.iter().position(|d| *d == doc) else {
            return;
        };
        win.tabs.remove(at);
        if win.active == Some(doc) {
            // The tab to its right, or else the one to its left.
            win.active = win.tabs.get(at).or_else(|| win.tabs.last()).copied();
        }
        self.events.push(DocEvent::TabsChanged(window));
        self.touch_session();
    }
    fn drop_empty_window(&mut self, window: WindowId) {
        let Some(at) = self.windows.iter().position(|w| w.id == window) else {
            return;
        };
        if !self.windows[at].tabs.is_empty() {
            return;
        }
        self.windows.remove(at);
        if self.last_used == Some(window) {
            self.last_used = self.windows.last().map(|w| w.id);
        }
        self.events.push(DocEvent::WindowClosed(window));
        self.touch_session();
        if self.windows.is_empty() && (!self.restore_started || self.restore_finished_sent) {
            self.events.push(DocEvent::LastWindowGone {
                opened_nothing: self.window_gone_by_failure,
            });
        }
    }

    // ---------------------------------------------------------------- reading a document

    /// The view that shows this document. Input goes to it as usual. After input that may
    /// have changed the text, call [`note_view_changed`](Self::note_view_changed).
    pub fn view(&self, doc: DocId) -> Option<&EditorView> {
        self.docs.get(&doc).map(|d| &d.view)
    }
    pub fn view_mut(&mut self, doc: DocId) -> Option<&mut EditorView> {
        self.docs.get_mut(&doc).map(|d| &mut d.view)
    }
    pub fn path(&self, doc: DocId) -> Option<PathBuf> {
        self.docs.get(&doc).and_then(|d| d.path.clone())
    }
    pub fn format(&self, doc: DocId) -> Option<Format> {
        self.docs.get(&doc).map(|d| d.format.clone())
    }
    pub fn is_loading(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|d| d.loading)
    }
    pub fn is_transient(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|d| d.transient)
    }
    /// An autosaved copy of this document is being written now.
    pub fn is_autosaving(&self, doc: DocId) -> bool {
        self.docs
            .get(&doc)
            .is_some_and(|d| d.autosave.running.is_some())
    }
    pub fn is_saving(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|d| d.saving)
    }
    pub fn all_documents(&self) -> Vec<DocId> {
        self.docs.keys().copied().collect()
    }
    /// Unsaved changes: the view is away from its save point, or disk does not hold the text.
    pub fn is_dirty(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(Doc::is_dirty)
    }
    /// Over 50 MB: spelling, substitutions, gutter and map turn off (parity I51).
    pub fn is_large(&self, doc: DocId) -> bool {
        self.docs
            .get(&doc)
            .is_some_and(|d| d.byte_count > self.config.large_bytes)
    }
    /// Over 512 MiB: read-only (parity I52).
    pub fn is_too_large_to_edit(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|d| d.read_only)
    }
    pub fn byte_count(&self, doc: DocId) -> u64 {
        self.docs.get(&doc).map_or(0, |d| d.byte_count)
    }
    /// The name for the tab and the title.
    pub fn display_name(&self, doc: DocId) -> String {
        self.docs
            .get(&doc)
            .map_or_else(String::new, Doc::display_name)
    }
    /// The note the title adds, if any (parity I64).
    pub fn title_note(&self, doc: DocId) -> Option<String> {
        let entry = self.docs.get(&doc)?;
        if entry.file_state == FileState::Deleted {
            return Some("Deleted".to_owned());
        }
        if entry.read_only {
            return Some(read_only_title_note(entry.byte_count));
        }
        (entry.byte_count > self.config.large_bytes).then(|| large_title_note().to_owned())
    }
    pub fn commands(&self, doc: DocId) -> DocCommands {
        let Some(d) = self.docs.get(&doc) else {
            return DocCommands {
                save: false,
                save_as: false,
                duplicate: false,
                rename: false,
                move_to: false,
                revert: false,
                prevent_editing: false,
            };
        };
        let free = !d.loading && !d.reloading && !d.moving;
        let quiet = free && !d.saving;
        DocCommands {
            // A file too large to edit that has gone can be written again only from a UTF-8
            // copy. The map of the file itself refuses to read once the file has gone.
            save: free
                && (!d.read_only || (d.file_state == FileState::Deleted && d.temp_copy.is_some())),
            save_as: free,
            duplicate: free,
            rename: quiet && d.path.is_some(),
            move_to: quiet && d.path.is_some(),
            revert: quiet
                && !d.read_only
                && d.path.is_some()
                && d.is_dirty()
                && d.file_state == FileState::Present,
            prevent_editing: !d.read_only && !d.loading,
        }
    }
    /// Blank: no file, nothing but whitespace, and short enough to read (Mac 1.2.0).
    pub fn is_blank_untitled(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(Doc::is_blank_untitled)
    }

    /// Format > Prevent Editing belongs to the tab. It is not an edit, and not saved in files.
    pub fn set_prevent_editing(&mut self, doc: DocId, on: bool) {
        if let Some(d) = self.docs.get_mut(&doc) {
            if d.read_only {
                return;
            }
            d.prevent_editing = on;
            d.view.set_prevent_editing(on);
            self.events.push(DocEvent::DocChanged(doc));
            self.touch_session();
        }
    }
    pub fn prevent_editing(&self, doc: DocId) -> bool {
        self.docs
            .get(&doc)
            .is_some_and(|d| d.prevent_editing || d.read_only)
    }
    /// Format > Wrap to Page belongs to the tab. It is not an edit.
    pub fn set_wrap_to_page(&mut self, doc: DocId, on: bool) {
        if let Some(d) = self.docs.get_mut(&doc) {
            d.wrap_to_page = on;
            self.touch_session();
        }
    }
    /// New documents start from the setting, and files from disk start in Wrap to Window.
    pub fn wrap_to_page(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|d| d.wrap_to_page)
    }

    // ---------------------------------------------------------------- creating documents

    fn make_doc(&mut self, format: Format) -> Result<Doc, DocError> {
        let id = DocId(self.fresh_id());
        let mut view = EditorView::from_text("").map_err(|message| {
            DocError::new(
                DocErrorKind::Other,
                None,
                format!("A text view could not start: {message}"),
            )
        })?;
        let wake = self.config.wake.clone();
        view.set_wake(wake);
        let seen_revision = view.revision();
        Ok(Doc {
            id,
            key: random_key(),
            view,
            path: None,
            custom_name: None,
            untitled_number: 0,
            format,
            open_choice: OpenChoice::FromSetting,
            byte_count: 0,
            read_only: false,
            copy_source: None,
            temp_copy: None,
            base: None,
            stamp: None,
            file_state: FileState::Present,
            forced_dirty: false,
            dirty_from_delete: false,
            return_check: false,
            changed_while_away: false,
            file_epoch: 0,
            loading: false,
            hidden: false,
            transient: false,
            prevent_editing: false,
            wrap_to_page: false,
            seen_revision,
            was_dirty: false,
            autosave: AutosaveState::default(),
            saving: false,
            save_again: false,
            close_after_save: false,
            reloading: false,
            reload_again: false,
            reload_tries: 0,
            reload_retry_at: None,
            undecodable: None,
            missing_recheck_at: None,
            rename_hint: None,
            place: Place::default(),
            pending_place: None,
            pending_adopt: None,
            load_cancel: Arc::new(AtomicBool::new(false)),
            moving: false,
            tried_copy: false,
            asked_lossy: false,
            asked_locked: false,
            asked_fit: false,
            fit_panel_undoes: false,
            unlocking: false,
            panel_open: false,
            quit_skip: false,
            missing_confirm: false,
        })
    }

    /// AppKit's naming: Untitled, Untitled 2, and so on, with the lowest free number.
    fn free_untitled_number(&self) -> u32 {
        let used: BTreeSet<u32> = self.docs.values().map(|d| d.untitled_number).collect();
        (1u32..).find(|n| !used.contains(n)).unwrap_or(1)
    }

    fn add_untitled(&mut self) -> Result<DocId, DocError> {
        let mut doc = self.make_doc(Format::new_document())?;
        doc.untitled_number = self.free_untitled_number();
        doc.wrap_to_page = self.config.new_docs_wrap_to_page;
        let id = doc.id;
        self.docs.insert(id, doc);
        Ok(id)
    }

    fn load_job(&self, source: Source, cancel: Arc<AtomicBool>) -> LoadJob {
        let notify: Notify = self.config.wake.clone();
        LoadJob {
            source,
            limits: Limits {
                read_only: self.config.read_only_bytes,
            },
            large_dir: self.config.root.join("large"),
            notify,
            cancel,
        }
    }

    fn queue_load(&mut self, doc: DocId, kind: LoadKind, source: Source, leftovers: bool) {
        let Some(entry) = self.docs.get(&doc) else {
            return;
        };
        let remote = load_is_remote(&kind, &source);
        let job = self.load_job(source, entry.load_cancel.clone());
        let item = (doc, kind, job, leftovers);
        if remote {
            self.remote_load_queue.push_back(item);
        } else {
            self.load_queue.push_back(item);
        }
        self.start_queued_loads();
    }

    fn start_queued_loads(&mut self) {
        self.start_loads_from(false);
        self.start_loads_from(true);
    }

    fn start_loads_from(&mut self, remote: bool) {
        loop {
            let (running, limit, queue) = if remote {
                (
                    self.remote_loads_running,
                    MAX_REMOTE_LOADS_AT_ONCE,
                    &mut self.remote_load_queue,
                )
            } else {
                (self.loads_running, MAX_LOADS_AT_ONCE, &mut self.load_queue)
            };
            if running >= limit {
                return;
            }
            let Some((doc, kind, job, leftovers)) = queue.pop_front() else {
                return;
            };
            if !self.docs.contains_key(&doc) {
                continue;
            }
            let thread_kind = kind.clone();
            let failed_kind = kind;
            let faults = self.config.faults.clone();
            let started = self.spawn("load", move || {
                if remote {
                    hold_while(&faults.hold_remote_load, &faults.remote_load_is_held);
                }
                if matches!(
                    thread_kind,
                    LoadKind::Reload | LoadKind::ReturnCheck | LoadKind::Revert
                ) {
                    hold_while(&faults.hold_reload, &faults.reload_is_held);
                }
                if matches!(thread_kind, LoadKind::RestoreAutosave { .. }) {
                    hold_while(&faults.hold_restore_load, &faults.restore_load_is_held);
                }
                let result = run_load(&job);
                let identity_now = match (&thread_kind, &job.source) {
                    (
                        LoadKind::RestoreAutosave {
                            path: Some(path), ..
                        },
                        _,
                    ) => Some(marks(path)),
                    (_, Source::File { path, .. }) if result.loaded.is_err() => Some(marks(path)),
                    _ => None,
                };
                let mut found = Vec::new();
                let scan = match (&job.source, &thread_kind) {
                    (Source::File { path, .. }, _) => Some(path.as_path()),
                    (
                        _,
                        LoadKind::RestoreAutosave {
                            path: Some(path), ..
                        },
                    ) => Some(path.as_path()),
                    _ => None,
                };
                if leftovers
                    && let Some(path) = scan
                    && let Ok(list) = find_leftovers(path)
                {
                    let keep = list.into_iter().filter(|l| l.offer_recovery);
                    found.extend(keep.map(|l| l.path));
                }
                Completion::Load {
                    doc,
                    kind: thread_kind,
                    remote,
                    result: Box::new(result),
                    identity_now,
                    leftovers: found,
                }
            });
            if started {
                if remote {
                    self.remote_loads_running += 1;
                } else {
                    self.loads_running += 1;
                }
            } else {
                let error = DocError::new(
                    DocErrorKind::Other,
                    None,
                    "Windows could not start a thread to read the file.",
                );
                self.finish_load(
                    doc,
                    failed_kind,
                    LoadResult {
                        loaded: Err(error),
                        problems: Vec::new(),
                    },
                    None,
                    Vec::new(),
                );
            }
        }
    }

    fn choice_of(&self, open_choice: OpenChoice) -> EncodingChoice {
        match open_choice {
            OpenChoice::FromSetting => self.config.open_encoding,
            OpenChoice::Chosen(choice) => choice,
        }
    }

    fn find_by_path_text(&self, path: &Path) -> Option<DocId> {
        self.docs
            .values()
            .find(|d| d.path.as_deref().is_some_and(|p| same_path_text(p, path)))
            .map(|d| d.id)
    }

    /// The launch document, when it can still give way to a file.
    fn replaceable_transient(&self) -> Option<DocId> {
        let mut visible = self.docs.values().filter(|d| !d.hidden);
        let only = visible.next()?;
        if visible.next().is_some() || !only.transient || only.is_dirty() || only.loading {
            return None;
        }
        Some(only.id)
    }

    /// Opens files as tabs, in this order. `choice` is what the Open dialog's encoding list
    /// said, and `None` means no dialog. Returns at once. A file already open comes forward.
    ///
    /// The list has the tabs made now. Three things keep files waiting, and those tabs show by
    /// `TabsChanged` later, in order: a restore that has not ended, since a tab opened before
    /// it would land beside the restored windows; a list longer than one group of tabs; and
    /// files already waiting. A path that is not a file QuickEdit may open gets `OpenFailed`
    /// with `PathRefused`. A list that would overflow the queue gets `OpenFailed` with
    /// `TooManyFiles`, once, and the files past the limit do not open.
    pub fn open_paths(
        &mut self,
        paths: &[PathBuf],
        target: OpenTarget,
        choice: Option<EncodingChoice>,
    ) -> Result<Vec<DocId>, DocError> {
        let mut good: VecDeque<PathBuf> =
            VecDeque::with_capacity(paths.len().min(MAX_OPENS_PER_WAKE));
        for raw in paths {
            match check_open_path(raw) {
                Ok(path) => good.push_back(path),
                Err(error) => self.events.push(DocEvent::OpenFailed {
                    path: raw.clone(),
                    error,
                }),
            }
        }
        if good.is_empty() {
            return Ok(Vec::new());
        }
        let waiting: usize = self.open_queue.iter().map(|b| b.paths.len()).sum();
        let room = MAX_QUEUED_OPENS.saturating_sub(waiting);
        if good.len() > room {
            let first_dropped = good[room].clone();
            let dropped = good.len() - room;
            good.truncate(room);
            let error = DocError::new(
                DocErrorKind::TooManyFiles,
                Some(&first_dropped),
                format!(
                    "{dropped} files did not open, because more than {MAX_QUEUED_OPENS} were waiting. The first is {}.",
                    first_dropped.display()
                ),
            );
            self.events.push(DocEvent::OpenFailed {
                path: first_dropped,
                error,
            });
        }
        if good.is_empty() {
            return Ok(Vec::new());
        }
        let restoring = self.restore_started && !self.restored;
        if restoring || !self.open_queue.is_empty() {
            self.open_queue.push_back(OpenBatch {
                paths: good,
                target,
                choice,
            });
            return Ok(Vec::new());
        }
        let mut batch = OpenBatch {
            paths: good,
            target,
            choice,
        };
        let opened = self.open_group(&mut batch);
        if !batch.paths.is_empty() {
            self.open_queue.push_back(batch);
            self.open_due = Some(Instant::now() + OPEN_SPACING);
        }
        Ok(opened)
    }

    /// One poll's share of the waiting files.
    fn run_open_queue(&mut self, now: Instant) {
        if self.restore_started && !self.restored {
            return;
        }
        if self.open_queue.is_empty() {
            self.open_due = None;
            return;
        }
        if self.open_due.is_some_and(|at| at > now) {
            return;
        }
        let mut budget = MAX_OPENS_PER_WAKE;
        while budget > 0 {
            let Some(mut batch) = self.open_queue.pop_front() else {
                break;
            };
            let before = batch.paths.len();
            self.open_group(&mut batch);
            budget = budget.saturating_sub(before - batch.paths.len());
            if !batch.paths.is_empty() {
                self.open_queue.push_front(batch);
                break;
            }
        }
        self.open_due = (!self.open_queue.is_empty()).then(|| now + OPEN_SPACING);
    }

    /// Opens one group from the front of a batch. `paths` have passed `check_open_path`.
    fn open_group(&mut self, batch: &mut OpenBatch) -> Vec<DocId> {
        let count = batch.paths.len().min(MAX_OPENS_PER_WAKE);
        let finishes_batch = count == batch.paths.len();
        let group: Vec<PathBuf> = batch.paths.drain(..count).collect();
        let target = batch.target;
        let choice = batch.choice;
        let mut opened = Vec::with_capacity(group.len());
        let mut replaces = self.replaceable_transient();
        if let Some(transient) = replaces
            && let Some(doc) = self.docs.get_mut(&transient)
        {
            // Claimed now, so that files opened meanwhile do not claim it too.
            doc.transient = false;
        }
        let mut new_window: Option<WindowId> = None;
        for (index, path) in group.into_iter().enumerate() {
            let is_last = finishes_batch && index + 1 == count;
            if let Some(existing) = self.find_by_path_text(&path) {
                self.bring_forward(existing, &path);
                continue;
            }
            let window = match (replaces, target) {
                (Some(transient), _) => self.window_of(transient),
                (None, OpenTarget::Window(window)) if self.window(window).is_some() => Some(window),
                (None, OpenTarget::LastUsedWindow) => self.last_used,
                _ => new_window,
            };
            let window = match window {
                Some(window) => window,
                None => {
                    let window = self.add_window();
                    new_window = Some(window);
                    window
                }
            };
            if replaces.is_some() {
                new_window = Some(window);
            }
            let mut doc = match self.make_doc(Format::new_document()) {
                Ok(doc) => doc,
                Err(error) => {
                    self.events.push(DocEvent::OpenFailed { path, error });
                    continue;
                }
            };
            doc.path = Some(path.clone());
            doc.loading = true;
            doc.view.begin_load();
            if let Some(choice) = choice {
                doc.open_choice = OpenChoice::Chosen(choice);
            }
            let open_choice = self.choice_of(doc.open_choice);
            let id = doc.id;
            let replacing = replaces.take();
            doc.hidden = replacing.is_some();
            self.docs.insert(id, doc);
            self.end_other_transients(replacing.unwrap_or(id));
            if let Some(entry) = replacing.and_then(|t| self.docs.get_mut(&t)) {
                entry.transient = false;
            }
            if replacing.is_none() {
                self.attach(window, id, None, is_last);
            }
            if is_last {
                self.want_focus = Some(id);
            }
            self.queue_load(
                id,
                LoadKind::Open {
                    replaces: replacing,
                    path: path.clone(),
                },
                Source::File {
                    path,
                    choice: open_choice,
                    compare: None,
                    force_copy: false,
                },
                true,
            );
            opened.push(id);
        }
        // The rest of a list for a new window goes to the window this group made.
        if let (OpenTarget::NewWindow, Some(window)) = (target, new_window) {
            batch.target = OpenTarget::Window(window);
        }
        opened
    }

    fn bring_forward(&mut self, doc: DocId, path: &Path) {
        if let Some(window) = self.window_of(doc) {
            self.set_active_tab(window, doc);
            self.last_used = Some(window);
            self.events.push(DocEvent::Focus { window, doc });
        }
        self.events.push(DocEvent::AlreadyOpen {
            doc,
            path: path.to_path_buf(),
        });
    }

    // ---------------------------------------------------------------- finishing a load

    fn finish_load(
        &mut self,
        doc: DocId,
        kind: LoadKind,
        result: LoadResult,
        identity_now: Option<(Option<FileBase>, Option<FileStamp>)>,
        leftovers: Vec<PathBuf>,
    ) {
        for problem in result.problems {
            self.events.push(DocEvent::RestoreProblem(problem));
        }
        if !self.docs.contains_key(&doc) {
            if let Ok(loaded) = result.loaded {
                free_off_thread(loaded);
            }
            return;
        }
        for leftover in leftovers {
            if let Some(path) = self.docs.get(&doc).and_then(|d| d.path.clone()) {
                self.events.push(DocEvent::LeftoverFound { path, leftover });
            }
        }
        if matches!(kind, LoadKind::ReturnCheck) {
            self.finish_return_check(doc, result.loaded, identity_now);
            return;
        }
        match result.loaded {
            Err(error) => {
                let stamp_now = identity_now.and_then(|(_, stamp)| stamp);
                self.load_failed(doc, kind, error, stamp_now);
            }
            Ok(Loaded::SameText {
                format,
                byte_count,
                base,
                stamp,
            }) => {
                // A date-only change: the text, the caret and undo all stay (parity I50).
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.format = format;
                    d.byte_count = byte_count;
                    d.base = base;
                    d.stamp = stamp;
                    d.undecodable = None;
                    d.reload_tries = 0;
                    // Disk holds what the view held at the read, so there is nothing to ask.
                    d.changed_while_away = false;
                }
                // A copy from when the file was gone: its text is on disk again.
                if self
                    .docs
                    .get(&doc)
                    .is_some_and(|d| !d.is_dirty() && d.autosave.generation.is_some())
                {
                    self.remove_autosave(doc);
                }
                self.events.push(DocEvent::DocChanged(doc));
                self.reload_ended(doc);
                self.touch_session();
            }
            Ok(Loaded::Body(body)) => self.load_succeeded(doc, kind, body, identity_now),
        }
    }

    /// The new file at a deleted file's name has been read. With the same text, the tab is
    /// clean again, with its caret and undo. With other text, nothing replaces this text,
    /// which may exist nowhere else: it stays with its copy, and the person is asked.
    fn finish_return_check(
        &mut self,
        doc: DocId,
        loaded: Result<Loaded, DocError>,
        identity_now: Option<(Option<FileBase>, Option<FileStamp>)>,
    ) {
        let now = Instant::now();
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.return_check = false;
        let unedited = d.dirty_from_delete && d.view.is_at_save_point();
        match loaded {
            Ok(Loaded::SameText {
                format,
                byte_count,
                base,
                stamp,
            }) if unedited => {
                d.format = format;
                d.byte_count = byte_count;
                d.base = base;
                d.stamp = stamp;
                d.changed_while_away = false;
                d.file_state_is_present(now);
                self.events.push(DocEvent::DocChanged(doc));
                self.reload_ended(doc);
                self.touch_session();
                return;
            }
            Err(error) if error.kind == DocErrorKind::FileNotFound => {
                // It has gone again. The tab stays as it was, and the next look decides.
                self.reload_ended(doc);
                return;
            }
            Ok(Loaded::Body(body)) => {
                d.base = body.base.or(d.base);
                d.stamp = body.stamp;
                free_off_thread(Loaded::Body(body));
            }
            Ok(Loaded::SameText { base, stamp, .. }) => {
                d.base = base.or(d.base);
                d.stamp = stamp;
            }
            Err(_) => {
                if let Some((base, stamp)) = identity_now {
                    d.base = base.or(d.base);
                    d.stamp = stamp;
                }
            }
        }
        d.file_state = FileState::Present;
        d.dirty_from_delete = false;
        d.changed_while_away = true;
        self.events.push(DocEvent::DocChanged(doc));
        self.reload_ended(doc);
        self.touch_session();
        if unedited {
            self.ask_changed_on_disk(doc);
        }
    }

    fn load_failed(
        &mut self,
        doc: DocId,
        kind: LoadKind,
        error: DocError,
        stamp_now: Option<FileStamp>,
    ) {
        match kind {
            LoadKind::Open { path, .. } | LoadKind::RestoreFile { path } => {
                self.events.push(DocEvent::OpenFailed { path, error });
                self.discard_failed_doc(doc);
            }
            LoadKind::RestoreCopy | LoadKind::Duplicate => {
                let path = self.docs.get(&doc).and_then(|d| d.copy_source.clone());
                self.events.push(DocEvent::OpenFailed {
                    path: path.unwrap_or_default(),
                    error,
                });
                self.discard_failed_doc(doc);
            }
            LoadKind::RestoreAutosave { path, .. } => {
                self.events.push(DocEvent::RestoreProblem(error));
                match path {
                    // The file itself is the best that is left.
                    Some(path) => {
                        let choice = self
                            .docs
                            .get(&doc)
                            .map_or(EncodingChoice::Automatic, |d| self.choice_of(d.open_choice));
                        if let Some(d) = self.docs.get_mut(&doc) {
                            // The copy may be whole, and only unreadable for now: held, or too
                            // big for this moment's memory. So nothing this document does may
                            // touch it. A new key leaves it to the next launch, which finds it
                            // as a copy with no row and tries again.
                            d.key = random_key();
                            d.autosave.generation = None;
                        }
                        self.queue_load(
                            doc,
                            LoadKind::RestoreFile { path: path.clone() },
                            Source::File {
                                path,
                                choice,
                                compare: None,
                                force_copy: false,
                            },
                            false,
                        );
                    }
                    None => self.discard_failed_doc(doc),
                }
            }
            LoadKind::Reload
            | LoadKind::ReturnCheck
            | LoadKind::AfterCopy
            | LoadKind::LargeRetry => {
                self.reload_failed(doc, error, stamp_now);
            }
            LoadKind::Revert => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.reloading = false;
                    d.view.end_load();
                }
                self.events.push(DocEvent::ReloadFailed { doc, error });
                self.events.push(DocEvent::DocChanged(doc));
            }
        }
    }

    fn reload_failed(&mut self, doc: DocId, error: DocError, stamp: Option<FileStamp>) {
        let now = Instant::now();
        let retry =
            error.kind == DocErrorKind::FileInUse || error.kind == DocErrorKind::ChangedOutside;
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.reloading = false;
        if retry && d.reload_tries + 1 < RELOAD_TRIES {
            d.reload_tries += 1;
            d.reload_retry_at = Some(now + RELOAD_RETRY);
            return;
        }
        d.reload_tries = 0;
        if error.kind == DocErrorKind::FileNotFound {
            self.stat_waiting.insert(doc);
            self.start_stat();
            return;
        }
        // Reported once for each state of the file, as the Mac reports each one once.
        if stamp.is_some() && d.undecodable == stamp {
            return;
        }
        d.undecodable = stamp;
        // Only bytes that cannot be decoded count as seen, as on the Mac. Any other failure
        // read nothing, so the stamp stays, and a Save finds the change and asks.
        if error.kind == DocErrorKind::EncodingDoesNotFit {
            d.stamp = stamp.or(d.stamp);
        }
        self.events.push(DocEvent::ReloadFailed { doc, error });
    }

    fn reload_ended(&mut self, doc: DocId) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.reloading = false;
        if d.reload_again {
            d.reload_again = false;
            self.stat_waiting.insert(doc);
            self.start_stat();
        }
    }

    fn discard_failed_doc(&mut self, doc: DocId) {
        let window = self.window_of(doc);
        let Some(entry) = self.docs.remove(&doc) else {
            return;
        };
        entry.load_cancel.store(true, Ordering::Release);
        if let Some(window) = window {
            self.detach(window, doc);
            self.window_gone_by_failure = true;
            self.drop_empty_window(window);
            self.window_gone_by_failure = false;
        }
        drop(entry);
        self.events.push(DocEvent::DocClosed(doc));
        self.sync_watches();
        self.touch_session();
    }

    fn load_succeeded(
        &mut self,
        doc: DocId,
        kind: LoadKind,
        body: Body,
        identity_now: Option<(Option<FileBase>, Option<FileStamp>)>,
    ) {
        match kind {
            LoadKind::Open { replaces, path } => self.finish_open(doc, replaces, path, body),
            LoadKind::RestoreFile { .. } => {
                if self.is_duplicate_of_open(doc, body.base) {
                    free_off_thread(Loaded::Body(body));
                    self.discard_failed_doc(doc);
                    return;
                }
                self.adopt_body(doc, body, AdoptThen::Plain);
            }
            LoadKind::RestoreCopy => {
                self.adopt_body(doc, body, AdoptThen::Plain);
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.forced_dirty = true;
                }
            }
            LoadKind::Duplicate => {
                self.adopt_body(doc, body, AdoptThen::Plain);
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.forced_dirty = true;
                    d.was_dirty = true;
                }
                self.arm_autosave(doc);
                self.events.push(DocEvent::DocChanged(doc));
            }
            LoadKind::RestoreAutosave { path, saved_base } => {
                self.finish_restore_autosave(doc, body, path, saved_base, identity_now);
            }
            LoadKind::LargeRetry | LoadKind::AfterCopy => {
                self.adopt_body(doc, body, AdoptThen::SameFile);
            }
            LoadKind::Reload => {
                let dirty = self.docs.get(&doc).is_some_and(Doc::is_dirty);
                if dirty {
                    // Typed into meanwhile: nothing is replaced, and a Save asks.
                    free_off_thread(Loaded::Body(body));
                    self.reload_ended(doc);
                    return;
                }
                self.adopt_body(doc, body, AdoptThen::Reloaded);
            }
            LoadKind::Revert => self.adopt_body(doc, body, AdoptThen::Reverted),
            // `finish_load` sends this kind to `finish_return_check`. It never replaces text.
            LoadKind::ReturnCheck => {
                free_off_thread(Loaded::Body(body));
                self.reload_ended(doc);
            }
        }
    }

    fn is_duplicate_of_open(&self, doc: DocId, base: Option<FileBase>) -> bool {
        let Some(identity) = base.map(|b| b.identity) else {
            return false;
        };
        self.docs
            .values()
            .any(|d| d.id != doc && d.base.is_some_and(|b| b.identity == identity))
    }

    fn finish_open(&mut self, doc: DocId, replaces: Option<DocId>, path: PathBuf, body: Body) {
        // The real file, not its spelling: a link or a short name finds the open tab.
        let identity = body.base.map(|b| b.identity);
        let already = identity.and_then(|identity| {
            self.docs
                .values()
                .find(|d| d.id != doc && d.base.is_some_and(|b| b.identity == identity))
                .map(|d| d.id)
        });
        if let Some(existing) = already {
            free_off_thread(Loaded::Body(body));
            self.discard_failed_doc(doc);
            self.bring_forward(existing, &path);
            return;
        }
        self.adopt_body(doc, body, AdoptThen::Plain);
        if let Some(d) = self.docs.get_mut(&doc) {
            d.hidden = false;
        }
        if let Some(transient) = replaces {
            self.place_over_transient(doc, transient);
        }
        add_recent(&mut self.recent, &path);
        self.recent_changed();
        self.sync_watches();
    }

    /// The read ran on a worker. The user may have typed into the launch window meanwhile.
    /// Then it stays, and the file gets its own tab beside it.
    fn place_over_transient(&mut self, doc: DocId, transient: DocId) {
        let Some(window) = self.window_of(transient) else {
            let target = self.last_used.unwrap_or_else(|| self.add_window());
            self.attach(target, doc, None, self.want_focus == Some(doc));
            return;
        };
        let at = self
            .tabs(window)
            .iter()
            .position(|d| *d == transient)
            .unwrap_or(0);
        let still_blank = self
            .docs
            .get(&transient)
            .is_some_and(|d| !d.is_dirty() && d.path.is_none() && !d.panel_open);
        let open_question = self.questions.iter().any(|q| q.question.doc == transient);
        if still_blank && !open_question {
            self.attach(window, doc, Some(at + 1), self.want_focus == Some(doc));
            self.finish_close(transient);
        } else {
            self.attach(window, doc, Some(at + 1), self.want_focus == Some(doc));
        }
    }

    fn finish_restore_autosave(
        &mut self,
        doc: DocId,
        body: Body,
        path: Option<PathBuf>,
        saved_base: Option<FileBase>,
        identity_now: Option<(Option<FileBase>, Option<FileStamp>)>,
    ) {
        // What the copy knew when it was written, not what a later list says. A list written
        // after a Save has the new file's marks, and that copy is older than that file.
        let head = body.copy_head;
        let saved_base = head.and_then(|h| h.base).or(saved_base);
        let flagged = head.is_some_and(|h| h.changed_while_away)
            || self.docs.get(&doc).is_some_and(|d| d.changed_while_away);
        self.adopt_body(doc, body, AdoptThen::Plain);
        let mut newer = None;
        if let Some(d) = self.docs.get_mut(&doc) {
            d.forced_dirty = true;
            d.dirty_from_delete = false;
            d.was_dirty = true;
            d.changed_while_away = flagged;
            if let Some(path) = path {
                let (now_base, now_stamp) = identity_now.unwrap_or((None, None));
                d.base = now_base.or(saved_base);
                d.stamp = now_stamp;
                match now_base {
                    None => {
                        // The file went while QuickEdit was not running.
                        d.file_state = FileState::Deleted;
                        d.stamp = None;
                        d.changed_while_away = false;
                        self.events.push(DocEvent::FileDeleted {
                            doc,
                            path: path.clone(),
                        });
                    }
                    Some(now) => {
                        let same = saved_base.is_some_and(|saved| {
                            saved.identity == now.identity
                                && saved.size == now.size
                                && saved.written == now.written
                        });
                        if !same || now_stamp.is_none() {
                            d.changed_while_away = true;
                        }
                        if d.changed_while_away {
                            newer = Some(path.clone());
                        }
                    }
                }
            }
            // The copy on disk holds this text, so nothing needs writing for it.
            d.autosave.written_revision = Some(d.view.revision());
        }
        if let Some(path) = newer {
            self.events
                .push(DocEvent::RestoredOverNewerFile { doc, path });
        }
        self.events.push(DocEvent::DocChanged(doc));
        self.sync_watches();
    }

    /// Puts loaded text in the view, now if it can, or at a later poll if a Cut or a
    /// composition holds the view.
    fn adopt_body(&mut self, doc: DocId, body: Body, then: AdoptThen) {
        let mut ctx = ViewContext::default();
        let Some(d) = self.docs.get_mut(&doc) else {
            free_off_thread(Loaded::Body(body));
            return;
        };
        d.view.flush_input(&mut ctx);
        let composing = then == AdoptThen::Reloaded && d.view.composition().is_some();
        if d.view.cut_is_running() || composing {
            d.pending_adopt = Some(PendingAdopt::Waiting { body, then });
            self.forward(doc, ctx);
            return;
        }
        // The flush above, or a wait for a Cut, may have let an edit in since the read began.
        // A reload never replaces edits: a Save finds the change and asks.
        if then == AdoptThen::Reloaded && d.is_dirty() {
            self.forward(doc, ctx);
            free_off_thread(Loaded::Body(body));
            self.reload_ended(doc);
            return;
        }
        let Body {
            file,
            format,
            byte_count,
            base,
            stamp,
            temp_copy,
            copy_head: _,
        } = body;
        // The text in the view goes, by the caller's rule, so the guard against losing it
        // has nothing to guard. A Revert has asked, and a reload runs only when clean.
        if !d.read_only {
            d.view.mark_save_point();
        }
        let read_only = matches!(file, LoadedFile::ReadOnly(_));
        let place = match then {
            AdoptThen::Reverted => Some(Place::default()),
            AdoptThen::Reloaded | AdoptThen::SameFile => Some(d.capture_place()),
            AdoptThen::Plain => d.pending_place.or(d.place_if_restored()),
        };
        if let Err(message) = d.view.adopt(file) {
            let error = DocError::new(
                DocErrorKind::Other,
                d.path.as_deref(),
                format!(
                    "The new text could not reach the window, so your text here is as it was: {message}"
                ),
            );
            let first_text = d.loading;
            let path = d.path.clone().unwrap_or_default();
            d.reloading = false;
            d.view.end_load();
            self.forward(doc, ctx);
            if first_text {
                // The view has none of this document's text, so it must not look like a clean
                // document that a Save could write. A copy stays for the next launch.
                self.events.push(DocEvent::OpenFailed { path, error });
                self.discard_failed_doc(doc);
            } else {
                self.events.push(DocEvent::ReloadFailed { doc, error });
            }
            return;
        }
        if let Some(old) = d.temp_copy.take() {
            remove_temp_later(old);
        }
        d.temp_copy = temp_copy;
        d.file_epoch += 1;
        d.format = format;
        d.byte_count = byte_count;
        if d.path.is_some() {
            if base.is_some() {
                d.base = base;
            }
            d.stamp = stamp.or(d.stamp);
        } else {
            // A copy with no file reads through its source, and it is not that file.
            d.base = None;
            d.stamp = None;
        }
        d.read_only = read_only;
        if read_only && d.copy_source.is_none() {
            d.copy_source = d.path.clone();
        }
        if !read_only {
            d.copy_source = None;
        }
        d.forced_dirty = false;
        d.dirty_from_delete = false;
        d.changed_while_away = false;
        d.file_state = FileState::Present;
        d.undecodable = None;
        d.reload_tries = 0;
        d.tried_copy = false;
        d.loading = false;
        d.seen_revision = d.view.revision();
        d.was_dirty = false;
        d.asked_lossy = false;
        d.asked_locked = false;
        d.asked_fit = false;
        d.fit_panel_undoes = false;
        d.view.set_prevent_editing(d.prevent_editing && !read_only);
        d.pending_place = place;
        d.autosave.due = None;
        let had_copy = matches!(then, AdoptThen::Reloaded | AdoptThen::Reverted)
            && d.autosave.generation.is_some();
        self.forward(doc, ctx);
        self.try_apply_place(doc);
        self.events.push(DocEvent::TextReplaced(doc));
        self.events.push(DocEvent::DocChanged(doc));
        if then == AdoptThen::Reloaded {
            self.events.push(DocEvent::Reloaded(doc));
        }
        if had_copy {
            self.remove_autosave(doc);
        }
        self.reload_ended(doc);
        self.touch_session();
    }

    fn try_apply_place(&mut self, doc: DocId) {
        let mut ctx = ViewContext::default();
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        let Some(place) = d.pending_place else {
            return;
        };
        let status = d.view.store_status();
        let known = status.known_bytes.0 as u64;
        let highest = place.anchor.max(place.caret).max(place.top);
        if !status.complete && highest > known {
            return;
        }
        d.view.select_directed(
            ByteOffset(place.anchor as usize),
            ByteOffset(place.caret as usize),
            &mut ctx,
        );
        let mut top = place.top.min(known) as usize;
        // A place inside a character steps back to its start. Three steps reach it.
        for _ in 0..4 {
            match d
                .view
                .reveal_range(ByteOffset(top)..ByteOffset(top), true, &mut ctx)
            {
                Ok(()) => break,
                Err(qe_api::LookupError::OutOfRange) if top > 0 => top -= 1,
                Err(qe_api::LookupError::NotYetKnown { .. }) => {
                    // A later poll asks again.
                    self.forward(doc, ctx);
                    return;
                }
                Err(error) => {
                    eprintln!("QuickEdit: a place could not come back: {error:?}");
                    break;
                }
            }
        }
        d.place = place;
        d.pending_place = None;
        self.forward(doc, ctx);
    }

    fn forward(&mut self, doc: DocId, ctx: ViewContext) {
        self.requests
            .extend(ctx.requests.into_iter().map(|request| (doc, request)));
    }

    // ---------------------------------------------------------------- edits

    /// Call after input that may have changed the text. It costs no more than the size of
    /// the last edit, at most 64 KiB. It may raise a question, as the first edit of a lossy
    /// or a read-only file does.
    pub fn note_view_changed(&mut self, doc: DocId) {
        let now = Instant::now();
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        let revision = d.view.revision();
        if revision == d.seen_revision {
            return;
        }
        let inserted = last_inserted(&d.view, d.seen_revision);
        d.seen_revision = revision;
        d.transient = false;
        let dirty = d.is_dirty();
        let flipped = dirty != d.was_dirty;
        d.was_dirty = dirty;
        if dirty && d.autosave.due.is_none() {
            d.autosave.due = autosave_time(now, self.config.autosave_delay);
        }
        // Back at the save point by Undo: the next time decides what to do with any copy.
        if !dirty && d.autosave.generation.is_some() && d.autosave.due.is_none() {
            d.autosave.due = Some(now);
        }
        if flipped {
            self.events.push(DocEvent::DocChanged(doc));
        }
        self.touch_session();
        if dirty {
            self.check_first_edit(doc, inserted.as_deref());
        }
        if let Some(d) = self.docs.get_mut(&doc) {
            d.view.mark_edits_seen();
        }
    }

    fn check_first_edit(&mut self, doc: DocId, inserted: Option<&str>) {
        let Some(d) = self.docs.get(&doc) else {
            return;
        };
        if self.taking_back_edit
            || d.path.is_none()
            || self.questions.iter().any(|q| q.question.doc == doc)
        {
            return;
        }
        let name = d.display_name();
        if d.format.lossy && !d.asked_lossy {
            self.part_asking_key(doc);
            self.ask(
                doc,
                QuestionKind::LossyFirstEdit,
                "Are you sure you want to modify the document in place?".to_owned(),
                "Modifying the document in place might cause you to lose some of the original formatting.  Would you like to duplicate the document first?".to_owned(),
                vec![Answer::Duplicate, Answer::Cancel, Answer::Overwrite],
            );
            if let Some(d) = self.docs.get_mut(&doc) {
                d.asked_lossy = true;
            }
            return;
        }
        if d.base.is_some_and(|b| b.read_only) && !d.asked_locked {
            self.part_asking_key(doc);
            self.ask(
                doc,
                QuestionKind::LockedFile { on_edit: true },
                format!("The file \u{201C}{name}\u{201D} is locked."),
                "It has the read-only mark. Unlock it to change it, or duplicate it to make your changes in a copy.".to_owned(),
                vec![Answer::Unlock, Answer::Duplicate, Answer::Cancel],
            );
            if let Some(d) = self.docs.get_mut(&doc) {
                d.asked_locked = true;
            }
            return;
        }
        let encoding = d.format.encoding;
        let does_not_fit = !d.asked_fit
            && inserted.is_some_and(|text| !can_encode(text, encoding).unwrap_or(true));
        if does_not_fit {
            self.part_asking_key(doc);
            self.ask(
                doc,
                QuestionKind::EncodingNoLongerFits { on_edit: true },
                format!(
                    "This document can no longer be saved using its original {} encoding.",
                    encoding.name()
                ),
                "Please choose another encoding (such as UTF-8).".to_owned(),
                vec![Answer::ChooseEncoding, Answer::Cancel],
            );
            if let Some(d) = self.docs.get_mut(&doc) {
                d.asked_fit = true;
            }
        }
    }

    /// The question is about this key alone, so its Cancel must not take back the typing before.
    fn part_asking_key(&mut self, doc: DocId) {
        if let Some(d) = self.docs.get_mut(&doc) {
            d.view.split_last_key();
        }
    }

    /// The caret or the scroll moved. The next list of windows holds the new place.
    pub fn note_place_changed(&mut self, doc: DocId) {
        if self.docs.contains_key(&doc) {
            self.touch_session();
        }
    }

    fn ask(
        &mut self,
        doc: DocId,
        kind: QuestionKind,
        title: String,
        detail: String,
        choices: Vec<Answer>,
    ) {
        self.ask_with(doc, kind, title, detail, choices, QuestionUse::Plain);
    }

    fn ask_with(
        &mut self,
        doc: DocId,
        kind: QuestionKind,
        title: String,
        detail: String,
        choices: Vec<Answer>,
        usage: QuestionUse,
    ) {
        // A second Close, or a close from another window, must not ask the same thing twice.
        if self
            .questions
            .iter()
            .any(|q| q.question.doc == doc && q.question.kind == kind)
        {
            return;
        }
        let id = QuestionId(self.next_question);
        self.next_question += 1;
        let question = Question {
            id,
            doc,
            kind,
            title,
            detail,
            choices,
        };
        self.questions.push(OpenQuestion {
            question: question.clone(),
            usage,
        });
        self.events.push(DocEvent::Question(question));
    }

    /// The answer to a question. An answer it did not offer is an error, and the question
    /// stays open.
    pub fn answer(&mut self, id: QuestionId, answer: Answer) -> Result<(), DocError> {
        let Some(at) = self.questions.iter().position(|q| q.question.id == id) else {
            return Err(DocError::new(
                DocErrorKind::NoSuchDocument,
                None,
                "That question is no longer open.",
            ));
        };
        if !self.questions[at].question.choices.contains(&answer) {
            return Err(DocError::new(
                DocErrorKind::Other,
                None,
                format!("{answer:?} is not one of this question's answers."),
            ));
        }
        let open = self.questions.remove(at);
        if let Err(error) = self.apply_answer(&open, answer) {
            // Nothing happened, so nothing may stay half done, and the person can answer again.
            if let Some(d) = self.docs.get_mut(&open.question.doc) {
                match (open.question.kind, answer) {
                    (QuestionKind::SaveBeforeClose, Answer::Save) => d.close_after_save = false,
                    (QuestionKind::LossyOverwrite, Answer::Overwrite) => d.format.lossy = true,
                    _ => {}
                }
            }
            let at = at.min(self.questions.len());
            self.questions.insert(at, open);
            return Err(error);
        }
        if open.usage == QuestionUse::QuitReview {
            self.advance_quit();
        }
        Ok(())
    }

    fn apply_answer(&mut self, open: &OpenQuestion, answer: Answer) -> Result<(), DocError> {
        let doc = open.question.doc;
        match (open.question.kind, answer) {
            (QuestionKind::SaveBeforeClose, Answer::Save) => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.close_after_save = true;
                }
                self.save(doc)?;
            }
            (QuestionKind::SaveBeforeClose, Answer::DontSave) => {
                // The answer covers the text the person saw. Keys that waited behind it raise
                // the question again, so they are not dropped with no word.
                if self
                    .docs
                    .get(&doc)
                    .is_some_and(|d| d.view.has_queued_edits())
                {
                    self.run_queued_input(doc);
                    self.ask_save_before_close(doc);
                    return Ok(());
                }
                if self.window_is_closing_with(doc) {
                    let revision = self.docs.get(&doc).map(|d| d.view.revision());
                    if let (Some(closing), Some(revision)) = (&mut self.closing_window, revision) {
                        closing.discard.push((doc, revision));
                    }
                    self.continue_window_close();
                } else {
                    self.finish_close(doc);
                    self.continue_closing();
                }
            }
            (QuestionKind::SaveBeforeClose, _) => self.cancel_closing(doc),
            (QuestionKind::AutosaveFailedAtQuit, Answer::Save) => {
                self.save(doc)?;
            }
            (QuestionKind::AutosaveFailedAtQuit, Answer::DontSave) => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.quit_skip = true;
                    d.autosave.due = None;
                }
                self.advance_quit();
            }
            (QuestionKind::AutosaveFailedAtQuit, _) => self.cancel_quit(),
            (QuestionKind::LossyFirstEdit, Answer::Overwrite) => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.format.lossy = false;
                }
            }
            (QuestionKind::LossyFirstEdit, Answer::Duplicate) => {
                // The copy has the edit, and the edit goes from this one.
                self.duplicate(doc)?;
                self.undo(doc);
                self.ask_lossy_again(doc);
            }
            (QuestionKind::LossyFirstEdit, _) => {
                self.undo(doc);
                self.ask_lossy_again(doc);
            }
            (QuestionKind::LossyOverwrite, Answer::Overwrite) => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.format.lossy = false;
                }
                self.save(doc)?;
            }
            (QuestionKind::LossyOverwrite, Answer::Duplicate) => {
                self.duplicate(doc)?;
                self.cancel_closing(doc);
            }
            (QuestionKind::LossyOverwrite, _) => self.cancel_closing(doc),
            (QuestionKind::EncodingNoLongerFits { on_edit }, Answer::ChooseEncoding) => {
                self.ask_for_panel(doc, PanelPurpose::SaveAs);
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.fit_panel_undoes = on_edit;
                }
            }
            (QuestionKind::EncodingNoLongerFits { on_edit }, _) => {
                if on_edit {
                    self.undo(doc);
                }
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.asked_fit = false;
                }
                self.cancel_closing(doc);
            }
            (QuestionKind::ChangedOnDisk, Answer::SaveAnyway) => {
                let d = self.docs.get(&doc).ok_or_else(|| self.no_doc())?;
                if d.loading || d.reloading || d.saving {
                    return Err(self.busy(doc, "is busy with another file action"));
                }
                self.start_save(doc, None, true)?;
            }
            (QuestionKind::ChangedOnDisk, Answer::Revert) => {
                self.revert_now(doc)?;
                // The text it was asked to save has gone, so a close that waited on it ends.
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.close_after_save = false;
                }
                self.close_queue.clear();
                self.closing_window = None;
            }
            (QuestionKind::ChangedOnDisk, _) => self.cancel_closing(doc),
            (QuestionKind::LockedFile { on_edit }, Answer::Unlock) => {
                self.unlock(doc, !on_edit);
            }
            (QuestionKind::LockedFile { on_edit }, Answer::Duplicate) => {
                self.duplicate(doc)?;
                if on_edit {
                    self.undo(doc);
                }
                self.cancel_closing(doc);
            }
            (QuestionKind::LockedFile { on_edit }, _) => {
                if on_edit {
                    self.undo(doc);
                }
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.asked_locked = false;
                }
                self.cancel_closing(doc);
            }
            (QuestionKind::ConfirmRevert, Answer::Revert) => self.revert_now(doc)?,
            (QuestionKind::ConfirmRevert, _) => {}
        }
        Ok(())
    }

    /// The edit that raised the question has gone, so the next edit asks as this one did.
    fn ask_lossy_again(&mut self, doc: DocId) {
        if let Some(d) = self.docs.get_mut(&doc) {
            d.asked_lossy = false;
        }
    }

    /// Takes back the edit a question was about. This raises no question of its own.
    fn undo(&mut self, doc: DocId) {
        let mut ctx = ViewContext::default();
        if let Some(d) = self.docs.get_mut(&doc) {
            d.view.flush_input(&mut ctx);
            d.view.run(EditorCommand::Undo, &mut ctx);
        }
        self.forward(doc, ctx);
        self.taking_back_edit = true;
        self.note_view_changed(doc);
        self.taking_back_edit = false;
    }

    /// Why this document's unsaved text has no good copy on disk now. It holds until a copy
    /// succeeds or the text no longer needs one, so a host can keep it on show.
    pub fn autosave_failure(&self, doc: DocId) -> Option<String> {
        let d = self.docs.get(&doc)?;
        if !d.is_dirty() {
            return None;
        }
        d.autosave.last_failure.clone()
    }

    /// New files came in while the quit ran, so the app stays open. Nothing written is undone.
    pub fn stop_quit_for_new_files(&mut self) {
        self.cancel_quit();
    }

    /// Questions not yet answered, oldest first. Part B shows one at a time.
    pub fn open_questions(&self) -> Vec<Question> {
        self.questions.iter().map(|q| q.question.clone()).collect()
    }

    fn unlock(&mut self, doc: DocId, then_save: bool) {
        let Some(path) = self.docs.get(&doc).and_then(|d| d.path.clone()) else {
            return;
        };
        let shown = path.clone();
        let started = self.spawn("unlock", move || {
            let result = std::fs::metadata(&path)
                .and_then(|meta| {
                    let mut permissions = meta.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    std::fs::set_permissions(&path, permissions)
                })
                .map_err(|e| DocError::io("QuickEdit could not unlock", &path, &e));
            Completion::Unlocked {
                doc,
                then_save,
                result,
            }
        });
        if started {
            if let Some(d) = self.docs.get_mut(&doc) {
                d.unlocking = true;
            }
        } else {
            self.finish_unlock(doc, then_save, Err(no_thread("unlock", &shown)));
        }
    }

    // ---------------------------------------------------------------- saving

    fn busy(&self, doc: DocId, why: &str) -> DocError {
        let name = self.display_name(doc);
        DocError::new(
            DocErrorKind::Busy,
            self.path(doc).as_deref(),
            format!("\u{201C}{name}\u{201D} {why}. Try again when it has finished."),
        )
    }

    fn no_doc(&self) -> DocError {
        DocError::new(
            DocErrorKind::NoSuchDocument,
            None,
            "That document is no longer open.",
        )
    }

    /// File > Save. Returns at once. The outcome is an event: `SaveFinished`, `SaveFailed`, a
    /// question, or `NeedsSavePanel`. It first runs queued typing into the text, and it takes
    /// the text as it then stands. Typing that follows belongs to the next save.
    pub fn save(&mut self, doc: DocId) -> Result<(), DocError> {
        let d = self.docs.get(&doc).ok_or_else(|| self.no_doc())?;
        if d.loading || d.reloading {
            return Err(self.busy(doc, "is being read again"));
        }
        if d.moving {
            return Err(self.busy(doc, "is being moved"));
        }
        if d.read_only && d.path.is_some() && d.file_state != FileState::Deleted {
            // A file too large to edit already holds its text. It is never written over.
            if d.close_after_save {
                self.close_after_saved(doc);
            }
            return Ok(());
        }
        if d.path.is_none() {
            self.ask_for_panel(doc, PanelPurpose::Save);
            return Ok(());
        }
        if d.saving {
            // One more save at the end of this one, with whatever the text is then.
            if let Some(d) = self.docs.get_mut(&doc) {
                d.save_again = true;
            }
            return Ok(());
        }
        if d.format.lossy {
            self.ask(
                doc,
                QuestionKind::LossyOverwrite,
                "Are you sure you want to modify the document in place?".to_owned(),
                "Modifying the document in place might cause you to lose some of the original formatting.  Would you like to duplicate the document first?".to_owned(),
                vec![Answer::Duplicate, Answer::Cancel, Answer::Overwrite],
            );
            return Ok(());
        }
        if d.changed_while_away {
            self.ask_changed_on_disk(doc);
            return Ok(());
        }
        self.start_save(doc, None, false)
    }

    fn ask_changed_on_disk(&mut self, doc: DocId) {
        let name = self.display_name(doc);
        self.ask(
            doc,
            QuestionKind::ChangedOnDisk,
            format!("The file \u{201C}{name}\u{201D} has been changed by another application."),
            "Do you want to save your changes over it, or revert to the version on disk?"
                .to_owned(),
            vec![Answer::SaveAnyway, Answer::Revert, Answer::Cancel],
        );
    }

    /// File > Save As, after the dialog. `encoding`: the dialog's choice, `None` for the
    /// document's own. The host has already asked about replacing a file that exists.
    pub fn save_as(
        &mut self,
        doc: DocId,
        path: PathBuf,
        encoding: Option<TextEncoding>,
    ) -> Result<(), DocError> {
        let d = self.docs.get_mut(&doc).ok_or_else(|| {
            DocError::new(
                DocErrorKind::NoSuchDocument,
                None,
                "That document is no longer open.",
            )
        })?;
        d.panel_open = false;
        // The dialog has ended, so its Cancel can no longer undo an edit.
        d.fit_panel_undoes = false;
        let path = std::path::absolute(&path).unwrap_or(path);
        let same_file = d
            .path
            .as_deref()
            .is_some_and(|own| same_path_text(own, &path));
        if d.saving {
            return Err(self.busy(doc, "is being saved"));
        }
        if d.loading || d.reloading {
            return Err(self.busy(doc, "is being read again"));
        }
        if d.read_only && same_file {
            return Err(DocError::new(
                DocErrorKind::TooLargeToEdit,
                Some(&path),
                "A file too large to edit is never written over. Choose another name.",
            ));
        }
        // Save As onto the file a lossy document came from asks, as a Save does.
        if same_file && d.format.lossy && encoding.is_none() {
            self.ask(
                doc,
                QuestionKind::LossyOverwrite,
                "Are you sure you want to modify the document in place?".to_owned(),
                "Modifying the document in place might cause you to lose some of the original formatting.  Would you like to duplicate the document first?".to_owned(),
                vec![Answer::Duplicate, Answer::Cancel, Answer::Overwrite],
            );
            return Ok(());
        }
        self.start_save(doc, Some((path, encoding)), same_file)
    }

    /// The Save dialog was cancelled. A close or a Save All that waited on it goes on or ends.
    pub fn panel_cancelled(&mut self, doc: DocId) {
        let was_edit_panel = self.docs.get_mut(&doc).is_some_and(|d| {
            d.panel_open = false;
            d.asked_fit = false;
            std::mem::take(&mut d.fit_panel_undoes)
        });
        if was_edit_panel {
            self.undo(doc);
        }
        self.cancel_closing(doc);
        self.next_panel();
    }

    fn cancel_closing(&mut self, doc: DocId) {
        if let Some(d) = self.docs.get_mut(&doc) {
            d.close_after_save = false;
            d.save_again = false;
        }
        self.close_queue.clear();
        self.closing_window = None;
        if self.in_quit_review(doc) {
            self.cancel_quit();
        }
    }

    fn in_quit_review(&self, doc: DocId) -> bool {
        self.quitting && self.quit_review.contains(&doc)
    }

    /// The person cancelled the quit. No answer given during it counts any more.
    fn cancel_quit(&mut self) {
        self.quitting = false;
        self.quit_ready_sent = false;
        self.quit_review.clear();
        self.questions
            .retain(|q| q.usage != QuestionUse::QuitReview);
        for d in self.docs.values_mut() {
            d.quit_skip = false;
        }
    }

    /// A Save failed. During a quit review the question comes back, by `advance_quit`.
    /// Otherwise the close that waited on it ends.
    fn save_did_not_happen(&mut self, doc: DocId) {
        if self.in_quit_review(doc) {
            if let Some(d) = self.docs.get_mut(&doc) {
                d.save_again = false;
            }
        } else {
            self.cancel_closing(doc);
        }
    }

    fn ask_for_panel(&mut self, doc: DocId, purpose: PanelPurpose) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.panel_open = true;
        let suggested = match (&d.path, &d.custom_name) {
            (Some(path), _) => path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            (None, Some(name)) => name.clone(),
            (None, None) => filename_with_txt_rule(&d.display_name(), self.config.add_txt),
        };
        let never_read = d.path.is_none() && d.custom_name.is_none();
        let request = SavePanelRequest {
            doc,
            purpose,
            suggested_name: suggested,
            folder: d
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            initial_encoding: match self.config.save_encoding {
                EncodingChoice::Specific(chosen) if never_read => chosen,
                _ if never_read => TextEncoding::Utf8,
                _ => d.format.encoding,
            },
            encoding_fixed: d.read_only,
            add_txt: self.config.add_txt,
        };
        self.events.push(DocEvent::NeedsSavePanel(request));
    }

    /// File > Save As, before the dialog.
    pub fn request_save_as(&mut self, doc: DocId) {
        self.ask_for_panel(doc, PanelPurpose::SaveAs);
    }

    fn start_save(
        &mut self,
        doc: DocId,
        target: Option<(PathBuf, Option<TextEncoding>)>,
        force: bool,
    ) -> Result<(), DocError> {
        // A swap during a copy across volumes would be removed with the old file.
        if self.docs.get(&doc).is_some_and(|d| d.moving) {
            return Err(self.busy(doc, "is being moved"));
        }
        let others: Vec<(DocId, FileIdentity)> = self
            .docs
            .values()
            .filter(|d| d.id != doc)
            .filter_map(|d| d.base.map(|b| (d.id, b.identity)))
            .collect();
        let faults = self.config.faults.clone();
        let mut ctx = ViewContext::default();
        let d = self.docs.get_mut(&doc).ok_or_else(|| {
            DocError::new(
                DocErrorKind::NoSuchDocument,
                None,
                "That document is no longer open.",
            )
        })?;
        d.view.flush_input(&mut ctx);
        let save_as = target.is_some();
        let (path, encoding) = match target {
            Some((path, encoding)) => (path, encoding),
            None => match d.path.clone() {
                Some(path) => (path, None),
                None => {
                    return Err(DocError::new(
                        DocErrorKind::Other,
                        None,
                        "This document has no file yet.",
                    ));
                }
            },
        };
        let same_file = d
            .path
            .as_deref()
            .is_some_and(|own| same_path_text(own, &path));
        let format = encoding.map_or_else(|| d.format.clone(), |e| d.format.changed_to(e));
        let revision = d.view.revision();
        let snapshot = d.view.worker_snapshot();
        // A deleted file has no bytes at its name to copy, so the text comes from the view.
        let copy_source = if d.read_only && d.file_state == FileState::Present {
            d.copy_source.clone()
        } else {
            None
        };
        // A save to its own file expects the file as it was read. `force` is the user's
        // answer to the question about that.
        let expected = if same_file && !force { d.stamp } else { None };
        d.saving = true;
        self.saves_running += 1;
        self.forward(doc, ctx);
        self.touch_session();
        let job_path = path.clone();
        let job_format = format.clone();
        let started = self.spawn("save", move || {
            let result = run_save(&SaveJob {
                doc,
                snapshot,
                path: &job_path,
                expected,
                format: &job_format,
                others: &others,
                copy_source: copy_source.as_deref(),
                faults: &faults,
                save_as,
            });
            Completion::Save {
                doc,
                revision,
                path: job_path,
                format: job_format,
                save_as,
                result,
            }
        });
        if !started {
            self.saves_running -= 1;
            if let Some(d) = self.docs.get_mut(&doc) {
                d.saving = false;
            }
            return Err(DocError::new(
                DocErrorKind::Other,
                Some(&path),
                "Windows could not start a thread to save the file. The text is still here.",
            ));
        }
        self.events.push(DocEvent::DocChanged(doc));
        Ok(())
    }

    fn finish_save(
        &mut self,
        doc: DocId,
        revision: u64,
        path: PathBuf,
        format: Format,
        save_as: bool,
        result: Result<SaveDone, SaveTrouble>,
    ) {
        self.saves_running = self.saves_running.saturating_sub(1);
        self.note_save_end_during_quit(doc, &path, &result);
        let Some(d) = self.docs.get_mut(&doc) else {
            if let Err(SaveTrouble::Other { error, .. }) = result {
                self.events.push(DocEvent::SaveFailed {
                    doc,
                    error,
                    offer_save_as: false,
                });
            }
            self.advance_quit();
            return;
        };
        d.saving = false;
        match result {
            Ok(done) => self.save_succeeded(doc, revision, path, format, save_as, done),
            Err(SaveTrouble::Changed { new_text }) => {
                if let Some(temp) = new_text {
                    remove_save_temp(&path, temp);
                }
                self.ask_changed_on_disk(doc);
            }
            Err(SaveTrouble::DoesNotFit) => {
                let encoding = format.encoding;
                if save_as {
                    let error = DocError::new(
                        DocErrorKind::EncodingDoesNotFit,
                        Some(&path),
                        format!(
                            "The text cannot be saved using {}. Choose another encoding.",
                            encoding.name()
                        ),
                    );
                    self.events.push(DocEvent::SaveFailed {
                        doc,
                        error,
                        offer_save_as: true,
                    });
                    self.save_did_not_happen(doc);
                } else {
                    self.ask(
                        doc,
                        QuestionKind::EncodingNoLongerFits { on_edit: false },
                        format!(
                            "This document can no longer be saved using its original {} encoding.",
                            encoding.name()
                        ),
                        "Please choose another encoding (such as UTF-8).".to_owned(),
                        vec![Answer::ChooseEncoding, Answer::Cancel],
                    );
                }
            }
            Err(SaveTrouble::Locked) => {
                let name = self.display_name(doc);
                self.ask(
                    doc,
                    QuestionKind::LockedFile { on_edit: false },
                    format!("The file \u{201C}{name}\u{201D} is locked."),
                    "It has the read-only mark. The file was not changed. Unlock it to save over it, or duplicate it to keep your changes in a copy.".to_owned(),
                    vec![Answer::Unlock, Answer::Duplicate, Answer::Cancel],
                );
            }
            Err(SaveTrouble::Other {
                error,
                offer_save_as,
            }) => {
                self.events.push(DocEvent::SaveFailed {
                    doc,
                    error,
                    offer_save_as,
                });
                self.save_did_not_happen(doc);
                self.next_panel();
            }
        }
        // A close at the end of this Save has already said its last word on this document.
        if self.docs.contains_key(&doc) {
            self.events.push(DocEvent::DocChanged(doc));
        }
        self.check_stat_after_save(doc);
        self.advance_quit();
    }

    /// A Save that does not write ends a quit that had not asked about it, so the person hears
    /// of it. At the end of a session Windows gives no such choice, so the next launch tells.
    fn note_save_end_during_quit(
        &mut self,
        doc: DocId,
        path: &Path,
        result: &Result<SaveDone, SaveTrouble>,
    ) {
        let Err(trouble) = result else {
            return;
        };
        if !self.quitting {
            return;
        }
        if self.session_ending {
            let why = match trouble {
                SaveTrouble::Other { error, .. } => error.message.clone(),
                SaveTrouble::Changed { .. } => "another program changed the file.".to_owned(),
                SaveTrouble::DoesNotFit => "its encoding cannot hold this text.".to_owned(),
                SaveTrouble::Locked => "it has the read-only mark.".to_owned(),
            };
            self.session_end_failures.push(format!(
                "When Windows ended the last session, {} could not be saved. Its unsaved text was kept, so check it is in its tab. The reason: {why}",
                path.display()
            ));
        } else if !self.in_quit_review(doc) {
            self.cancel_quit();
        }
    }

    fn check_stat_after_save(&mut self, doc: DocId) {
        if self.stat_waiting.contains(&doc) {
            self.start_stat();
        }
    }

    fn save_succeeded(
        &mut self,
        doc: DocId,
        revision: u64,
        path: PathBuf,
        format: Format,
        save_as: bool,
        done: SaveDone,
    ) {
        let now = Instant::now();
        let delay = self.config.autosave_delay;
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        let was_read_only = d.read_only;
        let was_deleted = d.file_state == FileState::Deleted;
        // The stamp is taken just after the swap, and it cannot cover that instant. A file of
        // another size there, or no stamp at all, shows the file is not known to be ours.
        let not_ours = done.base.is_none_or(|b| b.size != done.written) || done.stamp.is_none();
        if !was_read_only {
            // True when no typing came during the write. Otherwise the view stays dirty.
            d.view.mark_save_point_at(revision);
        }
        let encoding_changed = format.encoding != d.format.encoding;
        d.path = Some(path.clone());
        d.custom_name = None;
        d.untitled_number = 0;
        d.file_epoch += 1;
        d.base = done.base.or(d.base);
        d.stamp = if not_ours { None } else { done.stamp };
        d.forced_dirty = false;
        d.dirty_from_delete = false;
        d.changed_while_away = not_ours;
        d.file_state = FileState::Present;
        d.missing_recheck_at = None;
        d.panel_open = false;
        d.undecodable = None;
        d.asked_locked = false;
        d.asked_lossy = false;
        d.asked_fit = false;
        d.fit_panel_undoes = false;
        d.format = format;
        // The disk now holds what this text encodes to, so a next save changes nothing else.
        d.format.lossy = false;
        // Windows has no encoding tag on a file, so a reload needs the choice remembered.
        if encoding_changed {
            d.open_choice = OpenChoice::Chosen(match d.format.encoding {
                TextEncoding::Utf8 => EncodingChoice::Automatic,
                other => EncodingChoice::Specific(other),
            });
        }
        if d.byte_count == 0 {
            d.byte_count = done.written;
        }
        let still_dirty = d.is_dirty();
        d.was_dirty = still_dirty;
        d.seen_revision = d.view.revision();
        if still_dirty {
            if d.autosave.due.is_none() {
                d.autosave.due = autosave_time(now, delay);
            }
        } else {
            d.autosave.due = None;
            d.autosave.last_failure = None;
            d.autosave.last_failure_kind = None;
        }
        let close_after = d.close_after_save && !still_dirty;
        let again = std::mem::take(&mut d.save_again) && still_dirty;
        let reopen_large = was_read_only && (save_as || was_deleted);
        if !still_dirty {
            self.remove_autosave(doc);
        }
        if not_ours {
            // A clean document reads what is there now. A dirty one asks at its next Save.
            self.stat_waiting.insert(doc);
        }
        if save_as {
            add_recent(&mut self.recent, &path);
            self.recent_changed();
        }
        self.sync_watches();
        self.touch_session();
        self.events.push(DocEvent::SaveFinished {
            doc,
            path: path.clone(),
            still_dirty,
            other_version: done.other_version,
            backup_kept: done.backup_kept,
        });
        if reopen_large {
            // The view must read the new file: the old one may be moved or deleted now.
            let choice = self
                .docs
                .get(&doc)
                .map_or(EncodingChoice::Automatic, |d| self.choice_of(d.open_choice));
            if let Some(d) = self.docs.get_mut(&doc) {
                d.copy_source = Some(path.clone());
                d.reloading = true;
            }
            self.queue_load(
                doc,
                LoadKind::AfterCopy,
                Source::File {
                    path,
                    choice,
                    compare: None,
                    force_copy: false,
                },
                false,
            );
        }
        let question_outlived = !still_dirty
            && !close_after
            && self
                .questions
                .iter()
                .any(|q| q.question.doc == doc && q.question.kind == QuestionKind::SaveBeforeClose);
        if question_outlived {
            // The close question asked about text that is now on disk. The close goes on, as
            // its Save answer would have.
            self.questions.retain(|q| {
                !(q.question.doc == doc && q.question.kind == QuestionKind::SaveBeforeClose)
            });
            self.events.retain(|e| {
                !matches!(e, DocEvent::Question(q)
                    if q.doc == doc && q.kind == QuestionKind::SaveBeforeClose)
            });
            self.close_after_saved(doc);
        } else if close_after {
            self.close_after_saved(doc);
        } else if self.docs.get(&doc).is_some_and(|d| d.close_after_save) {
            // Typing came during the write, so the question comes again with the new text.
            if let Some(d) = self.docs.get_mut(&doc) {
                d.close_after_save = false;
            }
            if self.window_is_closing_with(doc) {
                self.continue_window_close();
            } else {
                self.close_queue.push_front(doc);
                self.continue_closing();
            }
        }
        if again && let Err(error) = self.save(doc) {
            self.events.push(DocEvent::SaveFailed {
                doc,
                error,
                offer_save_as: false,
            });
        }
        self.next_panel();
    }

    /// Save All: named documents start at once, and each untitled one gets its dialog in
    /// turn. A blank untitled document has nothing to keep.
    pub fn save_all(&mut self) -> Vec<(DocId, DocError)> {
        let mut failed = Vec::new();
        let dirty: Vec<DocId> = self
            .docs
            .values()
            .filter(|d| d.is_dirty() && !d.is_blank_untitled())
            .map(|d| d.id)
            .collect();
        for doc in dirty {
            if self.is_loading(doc) {
                failed.push((doc, self.busy(doc, "is still being read")));
            } else if self.path(doc).is_none() {
                self.panel_queue.push_back(doc);
            } else if let Err(error) = self.save(doc) {
                failed.push((doc, error));
            }
        }
        self.next_panel();
        failed
    }

    fn next_panel(&mut self) {
        if self.docs.values().any(|d| d.panel_open) {
            return;
        }
        while let Some(doc) = self.panel_queue.pop_front() {
            if self
                .docs
                .get(&doc)
                .is_some_and(|d| d.path.is_none() && d.is_dirty())
            {
                self.ask_for_panel(doc, PanelPurpose::Save);
                return;
            }
        }
    }

    /// Which of `encodings` cannot hold the text, as an `EncodingFit` event from a worker.
    /// Past the Mac's limit of 5,000,000 items times characters it reports none, so the
    /// list stays on and a Save names any failure.
    pub fn check_encodings(&mut self, doc: DocId, encodings: Vec<TextEncoding>) {
        let Some(d) = self.docs.get(&doc) else {
            return;
        };
        let snapshot = d.view.worker_snapshot();
        let units = d.view.store_status().known_utf16.0;
        if d.read_only || encodings.len().saturating_mul(units) >= MAX_FIT_CHECK_ITEMS {
            self.events.push(DocEvent::EncodingFit {
                doc,
                unfit: Vec::new(),
            });
            return;
        }
        let started = self.spawn("fit", move || {
            let mut unfit = Vec::new();
            if let Ok(text) = read_all_text(snapshot.as_ref()) {
                for encoding in encodings {
                    if !can_encode(&text, encoding).unwrap_or(false) {
                        unfit.push(encoding);
                    }
                }
            }
            Completion::Fit { doc, unfit }
        });
        if !started {
            // As past the limit: the list stays on, and a Save names any failure.
            self.events.push(DocEvent::EncodingFit {
                doc,
                unfit: Vec::new(),
            });
        }
    }

    // ---------------------------------------------------------------- other file commands

    /// Duplicate: a new untitled tab beside this one, with this text and format. The file is
    /// not touched. A file too large to edit stays read-only in its copy, and a Save As from
    /// that copy writes the original bytes.
    pub fn duplicate(&mut self, doc: DocId) -> Result<DocId, DocError> {
        let mut ctx = ViewContext::default();
        let d = self.docs.get_mut(&doc).ok_or_else(|| {
            DocError::new(
                DocErrorKind::NoSuchDocument,
                None,
                "That document is no longer open.",
            )
        })?;
        if d.loading || d.reloading {
            return Err(self.busy(doc, "is being read again"));
        }
        d.view.flush_input(&mut ctx);
        let snapshot = d.view.worker_snapshot();
        let read_only = d.read_only;
        let source = d.copy_source.clone();
        let format = d.format.clone();
        let name = copy_name(&d.display_name());
        let wrap = d.wrap_to_page;
        let read_as_copy = d.temp_copy.is_some();
        let source_choice = d.open_choice;
        self.forward(doc, ctx);
        let mut copy = self.make_doc(format.clone())?;
        let mut name = name;
        let mut number = 2;
        while self.docs.values().any(|o| o.display_name() == name) {
            let base = copy_name(&self.display_name(doc));
            name = numbered_copy_name(&base, number);
            number += 1;
        }
        copy.custom_name = Some(name);
        copy.loading = true;
        copy.view.begin_load();
        copy.copy_source = source.clone();
        copy.wrap_to_page = wrap;
        copy.open_choice = if read_only {
            source_choice
        } else {
            OpenChoice::Chosen(EncodingChoice::Automatic)
        };
        // Its text exists nowhere else if its source goes, from the first moment.
        copy.forced_dirty = true;
        copy.was_dirty = true;
        let id = copy.id;
        self.docs.insert(id, copy);
        let window = self.window_of(doc);
        if let Some(window) = window {
            let at = self.tabs(window).iter().position(|d| *d == doc);
            self.attach(window, id, at.map(|at| at + 1), true);
        }
        let job_source = match (read_only, source) {
            // The map shows a file's own UTF-8 bytes only, so another encoding needs a copy.
            (true, Some(path)) if read_as_copy => Source::File {
                path,
                choice: self.choice_of(source_choice),
                compare: None,
                force_copy: false,
            },
            (true, Some(path)) => Source::Mapped { path, format },
            _ => Source::Snapshot { snapshot, format },
        };
        self.queue_load(id, LoadKind::Duplicate, job_source, false);
        Ok(id)
    }

    /// Revert to Saved. A dirty document asks first.
    pub fn request_revert(&mut self, doc: DocId) -> Result<(), DocError> {
        self.run_queued_input(doc);
        let d = self.docs.get(&doc).ok_or_else(|| self.no_doc())?;
        if d.read_only {
            return Err(DocError::new(
                DocErrorKind::TooLargeToEdit,
                d.path.as_deref(),
                "A file too large to edit cannot have changes to revert.",
            ));
        }
        if d.path.is_none() {
            return Ok(());
        }
        if !d.is_dirty() {
            return self.revert_now(doc);
        }
        let name = d.display_name();
        self.ask(
            doc,
            QuestionKind::ConfirmRevert,
            format!("Do you want to revert to the most recently saved version of \u{201C}{name}\u{201D}?"),
            "Your current changes will be lost.".to_owned(),
            vec![Answer::Revert, Answer::Cancel],
        );
        Ok(())
    }

    fn revert_now(&mut self, doc: DocId) -> Result<(), DocError> {
        let d = self.docs.get_mut(&doc).ok_or_else(|| {
            DocError::new(
                DocErrorKind::NoSuchDocument,
                None,
                "That document is no longer open.",
            )
        })?;
        if d.saving {
            return Err(self.busy(doc, "is being saved"));
        }
        if d.loading || d.reloading {
            return Err(self.busy(doc, "is being read again"));
        }
        let Some(path) = d.path.clone() else {
            return Ok(());
        };
        // The answer covers what was typed up to now. From here the view refuses edits until
        // the text arrives, as the Mac does during a reload, so none can be lost to it.
        let mut ctx = ViewContext::default();
        d.view.flush_input(&mut ctx);
        d.view.begin_load();
        d.reloading = true;
        let open_choice = d.open_choice;
        let choice = self.choice_of(open_choice);
        self.forward(doc, ctx);
        self.queue_load(
            doc,
            LoadKind::Revert,
            Source::File {
                path,
                choice,
                compare: None,
                force_copy: false,
            },
            false,
        );
        Ok(())
    }

    /// Rename: a new name in the same folder. A document with no file gets the Save dialog.
    /// Unsaved changes stay unsaved.
    pub fn rename(&mut self, doc: DocId, new_name: &str) -> Result<(), DocError> {
        let d = self.docs.get(&doc).ok_or_else(|| self.no_doc())?;
        let Some(old) = d.path.clone() else {
            self.ask_for_panel(doc, PanelPurpose::Save);
            return Ok(());
        };
        let name = new_name.to_owned();
        if name.is_empty() || name.contains(['\\', '/']) {
            return Err(DocError::new(
                DocErrorKind::Other,
                Some(&old),
                format!("\u{201C}{new_name}\u{201D} cannot be a file name."),
            ));
        }
        // A colon would make the name a drive of its own in `with_file_name`.
        if name.contains(':') {
            return Err(DocError::new(
                DocErrorKind::PathRefused,
                Some(&old),
                format!(
                    "QuickEdit cannot use \u{201C}{new_name}\u{201D}: Windows does not allow a colon in a file name."
                ),
            ));
        }
        let target = check_new_path(&old.with_file_name(name))?;
        self.start_move(doc, target, false)
    }

    /// Move To, after the dialog. `replace`: the host has asked about a file that exists.
    pub fn move_to(&mut self, doc: DocId, path: PathBuf, replace: bool) -> Result<(), DocError> {
        let d = self.docs.get(&doc).ok_or_else(|| self.no_doc())?;
        if d.path.is_none() {
            return Err(DocError::new(
                DocErrorKind::Other,
                Some(&path),
                "This document has no file yet, so it needs a Save first.",
            ));
        }
        let path = check_new_path(&path)?;
        self.start_move(doc, path, replace)
    }

    fn start_move(&mut self, doc: DocId, to: PathBuf, replace: bool) -> Result<(), DocError> {
        let open_elsewhere = |to: &Path| {
            DocError::new(
                DocErrorKind::OpenInAnotherTab,
                Some(to),
                format!(
                    "{} is open in another tab. Close it there, or choose another name.",
                    to.display()
                ),
            )
        };
        if self.docs.values().any(|d| {
            d.id != doc
                && d.path
                    .as_deref()
                    .is_some_and(|other| same_path_text(other, &to))
        }) {
            return Err(open_elsewhere(&to));
        }
        let others: Vec<FileIdentity> = self
            .docs
            .values()
            .filter(|d| d.id != doc)
            .filter_map(|d| d.base.map(|b| b.identity))
            .collect();
        let d = self.docs.get_mut(&doc).ok_or_else(|| {
            DocError::new(
                DocErrorKind::NoSuchDocument,
                None,
                "That document is no longer open.",
            )
        })?;
        if d.saving || d.loading || d.reloading || d.moving {
            return Err(self.busy(doc, "is busy with another file action"));
        }
        let Some(from) = d.path.clone() else {
            return Ok(());
        };
        if from == to {
            return Ok(());
        }
        d.moving = true;
        let shown = to.clone();
        let started = self.spawn("move", move || {
            // The same file by another name counts, which only a look at the disk can tell.
            let same_file = file_base(&to).is_ok_and(|(base, _)| others.contains(&base.identity));
            let moved = if same_file {
                Err(open_elsewhere(&to))
            } else {
                move_file(&from, &to, replace)
            };
            let result = moved.and_then(|()| {
                let (base, _) = file_base(&to)
                    .map_err(|e| DocError::io("QuickEdit could not read", &to, &e))?;
                Ok((base, file_stamp(&to).ok().flatten()))
            });
            Completion::Moved {
                doc,
                path: to,
                result,
            }
        });
        if !started {
            if let Some(d) = self.docs.get_mut(&doc) {
                d.moving = false;
            }
            return Err(no_thread("move the file to", &shown));
        }
        Ok(())
    }

    /// Puts a file from `LeftoverFound` back at its document's path. It never replaces a file
    /// that is there. A kill inside the swap of a save can leave the old text only in that
    /// file, for the moment Windows takes between its two renames.
    pub fn recover_leftover(&mut self, leftover: PathBuf, target: PathBuf) {
        let shown = target.clone();
        let started = self.spawn("recover", move || {
            let result = move_file(&leftover, &target, false);
            Completion::Recovered {
                path: target,
                result,
            }
        });
        if !started {
            self.events.push(DocEvent::LeftoverNotRecovered {
                error: no_thread("recover", &shown),
                path: shown,
            });
        }
    }

    /// Deletes a file from `LeftoverFound`, when the user says it is not wanted. `path` is the
    /// document path from that event. The answer is `LeftoverDeleted` or `LeftoverNotDeleted`.
    pub fn discard_leftover(&mut self, leftover: PathBuf, path: PathBuf) {
        let shown = leftover.clone();
        let started = self.spawn("leftover", move || {
            let what = "QuickEdit could not delete";
            let result = find_leftovers(&path)
                .and_then(|list| match list.iter().find(|l| l.path == leftover) {
                    Some(found) => qe_codec::delete_leftover(
                        found,
                        qe_codec::LeftoverDelete::IncludeRecoverable,
                    ),
                    // A file no longer listed is one that has gone already.
                    None => Ok(()),
                })
                .map_err(|e| DocError::from_codec(what, &leftover, e));
            Completion::LeftoverGone { leftover, result }
        });
        if !started {
            self.events.push(DocEvent::LeftoverNotDeleted {
                error: no_thread("delete", &shown),
                leftover: shown,
            });
        }
    }

    // ---------------------------------------------------------------- closing

    /// Close, for a tab. A blank untitled or a clean document closes now. A dirty one raises
    /// `SaveBeforeClose`.
    pub fn request_close(&mut self, doc: DocId) {
        self.run_queued_input(doc);
        self.close_queue.clear();
        self.closing_window = None;
        self.close_queue.push_back(doc);
        self.continue_closing();
    }

    /// Close, for a window. Every dirty tab gets its question first, in tab order, and no tab
    /// closes until all of them have an answer. So a Cancel leaves the window as it was.
    pub fn request_close_window(&mut self, window: WindowId) {
        for doc in self.tabs(window) {
            self.run_queued_input(doc);
        }
        self.close_queue.clear();
        self.closing_window = Some(WindowClosing {
            window,
            discard: Vec::new(),
        });
        self.continue_window_close();
    }

    /// Keys that wait in the queue go into the text now, so that what decides next sees them.
    fn run_queued_input(&mut self, doc: DocId) {
        let mut ctx = ViewContext::default();
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        if d.loading || d.view.queued_input_count() == 0 {
            return;
        }
        d.view.flush_input(&mut ctx);
        self.forward(doc, ctx);
        self.note_view_changed(doc);
    }

    fn window_is_closing_with(&self, doc: DocId) -> bool {
        self.closing_window
            .as_ref()
            .is_some_and(|closing| self.tabs(closing.window).contains(&doc))
    }

    /// A save that a close waited on has ended, and the text is safe.
    fn close_after_saved(&mut self, doc: DocId) {
        if self.window_is_closing_with(doc) {
            if let Some(d) = self.docs.get_mut(&doc) {
                d.close_after_save = false;
            }
            self.continue_window_close();
        } else {
            self.finish_close(doc);
            self.continue_closing();
        }
    }

    /// The next question of a closing window. With none left, every tab closes at once. The
    /// tabs are read again each time, so typing or a new tab during a question counts: text
    /// changed since its Don't Save gets a new question.
    fn continue_window_close(&mut self) {
        let Some(closing) = &self.closing_window else {
            return;
        };
        let window = closing.window;
        let tabs = self.tabs(window);
        for &doc in &tabs {
            self.run_queued_input(doc);
        }
        let Some(closing) = &self.closing_window else {
            return;
        };
        for &doc in &tabs {
            let Some(d) = self.docs.get(&doc) else {
                continue;
            };
            if d.is_blank_untitled() || !d.is_dirty() {
                continue;
            }
            if closing.discard.contains(&(doc, d.view.revision())) {
                continue;
            }
            // Its Save is running, and its end brings this function back.
            if d.close_after_save {
                return;
            }
            self.ask_save_before_close(doc);
            return;
        }
        self.closing_window = None;
        for doc in tabs {
            self.finish_close(doc);
        }
    }

    fn continue_closing(&mut self) {
        while let Some(doc) = self.close_queue.pop_front() {
            self.run_queued_input(doc);
            let Some(d) = self.docs.get(&doc) else {
                continue;
            };
            if d.is_blank_untitled() || !d.is_dirty() {
                self.finish_close(doc);
                continue;
            }
            self.ask_save_before_close(doc);
            return;
        }
    }

    fn ask_save_before_close(&mut self, doc: DocId) {
        let Some(d) = self.docs.get(&doc) else {
            return;
        };
        let name = d.display_name();
        self.ask(
            doc,
            QuestionKind::SaveBeforeClose,
            format!("Do you want to keep the changes you made to \u{201C}{name}\u{201D}?"),
            "Your changes will be lost if you don't save them.".to_owned(),
            vec![Answer::Save, Answer::DontSave, Answer::Cancel],
        );
    }

    fn finish_close(&mut self, doc: DocId) {
        let window = self.window_of(doc);
        self.questions.retain(|q| q.question.doc != doc);
        // A question still on its way to the host must not name a document that has gone.
        self.events
            .retain(|e| !matches!(e, DocEvent::Question(q) if q.doc == doc));
        self.stat_waiting.remove(&doc);
        self.panel_queue.retain(|d| *d != doc);
        if self.want_focus == Some(doc) {
            self.want_focus = None;
        }
        self.remove_autosave(doc);
        let Some(entry) = self.docs.remove(&doc) else {
            return;
        };
        entry.load_cancel.store(true, Ordering::Release);
        if let Some(window) = window {
            self.detach(window, doc);
            self.drop_empty_window(window);
        }
        if let Some(temp) = &entry.temp_copy {
            remove_temp_later(temp.clone());
        }
        if entry.view.cut_is_running() {
            // Its text is still on the way to the clipboard, and a drop would stop that.
            self.closing_views.push(entry.view);
        } else {
            // The drop gives the text to a worker to free, so a big close costs little here.
            drop(entry.view);
        }
        self.events.push(DocEvent::DocClosed(doc));
        self.sync_watches();
        self.touch_session();
    }

    // ---------------------------------------------------------------- autosave

    fn arm_autosave(&mut self, doc: DocId) {
        let now = Instant::now();
        let delay = self.config.autosave_delay;
        if let Some(d) = self.docs.get_mut(&doc)
            && d.autosave.due.is_none()
        {
            d.autosave.due = autosave_time(now, delay);
        }
    }

    fn doc_meta(&self, d: &Doc, window: u64) -> DocMeta {
        let place = d.pending_place.unwrap_or_else(|| d.capture_place());
        DocMeta {
            key: d.key.clone(),
            window,
            path: d.path.clone(),
            name: d.custom_name.clone(),
            untitled_number: d.untitled_number,
            format: d.format.clone(),
            open_choice: d.open_choice,
            anchor: place.anchor,
            caret: place.caret,
            top: place.top,
            base: d.base,
            prevent_editing: d.prevent_editing,
            wrap_to_page: d.wrap_to_page,
            read_only: d.read_only,
            copy_source: d.copy_source.clone(),
            has_autosave: d.autosave.generation.is_some(),
            changed_while_away: d.changed_while_away,
        }
    }

    fn start_autosave(&mut self, doc: DocId) {
        let Some(dir) = self.session.as_ref().map(|s| s.dir.join("docs")) else {
            return;
        };
        let window = self.window_of(doc).map_or(0, |w| w.0);
        let Some(d) = self.docs.get(&doc) else {
            return;
        };
        let mut meta = self.doc_meta(d, window);
        meta.has_autosave = true;
        let snapshot = d.view.worker_snapshot();
        let revision = d.view.revision();
        let previous = d.autosave.generation;
        let key = d.key.clone();
        let generation = self.next_generation;
        self.next_generation += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let job_cancel = cancel.clone();
        let faults = self.config.faults.clone();
        let job_key = key.clone();
        let started = self.spawn("autosave", move || {
            hold_while(&faults.hold_autosave, &faults.autosave_is_held);
            let progress = |written: u64| -> std::io::Result<()> {
                let stall = faults.autosave_stall_after.load(Ordering::Relaxed);
                if stall != 0 && written >= stall {
                    faults.autosave_is_stalled.store(true, Ordering::Release);
                    loop {
                        std::thread::sleep(Duration::from_secs(60));
                    }
                }
                let code = faults.autosave_error_code.load(Ordering::Relaxed);
                if code != 0 && written >= faults.autosave_fail_after.load(Ordering::Relaxed) {
                    return Err(std::io::Error::from_raw_os_error(code as i32));
                }
                Ok(())
            };
            let result = write_generation(&AutosaveWrite {
                docs_dir: &dir,
                meta: &meta,
                generation,
                previous,
                snapshot: snapshot.as_ref(),
                cancel: &job_cancel,
                progress: &progress,
            });
            drop(snapshot);
            Completion::Autosave {
                doc,
                key: job_key,
                generation,
                revision,
                result,
            }
        });
        if started {
            if let Some(d) = self.docs.get_mut(&doc) {
                d.autosave.running = Some(cancel);
                d.autosave.due = None;
            }
            return;
        }
        let error = DocError::new(
            DocErrorKind::AutosaveFailed,
            None,
            "Windows could not start a thread to write the autosaved copy. The text is still in the window.",
        );
        let now = Instant::now();
        let delay = self.config.autosave_delay;
        if let Some(d) = self.docs.get_mut(&doc) {
            d.autosave.due = autosave_time(now, delay.max(THREAD_RETRY));
            let new_failure = d.autosave.last_failure_kind != Some(error.kind);
            d.autosave.last_failure_kind = Some(error.kind);
            d.autosave.last_failure = Some(error.message.clone());
            if new_failure {
                self.events.push(DocEvent::AutosaveFailed { doc, error });
            }
        }
    }

    fn finish_autosave(
        &mut self,
        doc: DocId,
        key: &str,
        generation: u64,
        revision: u64,
        result: Result<bool, DocError>,
    ) {
        let now = Instant::now();
        let delay = self.config.autosave_delay;
        let Some(d) = self.docs.get_mut(&doc) else {
            // Closed while it ran. The copy goes with the document.
            self.remove_generations_later(doc, key.to_owned());
            self.advance_quit();
            return;
        };
        d.autosave.running = None;
        match result {
            Ok(true) => {
                d.autosave.generation = Some(generation);
                d.autosave.written_revision = Some(revision);
                d.autosave.last_failure = None;
                d.autosave.last_failure_kind = None;
                let keep = d.is_dirty() && !d.is_blank_untitled();
                let again = std::mem::take(&mut d.autosave.again);
                if !keep {
                    self.remove_autosave(doc);
                } else if again {
                    d.autosave.due = Some(now);
                }
                self.touch_session_now();
            }
            // Cancelled because the document had no unsaved text then. Its older copy goes.
            Ok(false) => self.remove_generations_later(doc, key.to_owned()),
            Err(error) if !d.is_dirty() => {
                drop(error);
                self.remove_generations_later(doc, key.to_owned());
            }
            Err(error) => {
                // The last good copy is still there. The text is in the window. Try again later.
                // Each try names its own new file, so the kind says whether it is news.
                let new_failure = d.autosave.last_failure_kind != Some(error.kind);
                d.autosave.last_failure_kind = Some(error.kind);
                d.autosave.last_failure = Some(error.message.clone());
                d.autosave.due = autosave_time(now, delay);
                if new_failure {
                    self.events.push(DocEvent::AutosaveFailed { doc, error });
                }
            }
        }
        self.keep_copy_timer(doc, now);
        self.advance_quit();
    }

    /// Whatever ended a copy, unsaved text with no copy of its own keeps a due time. A kill
    /// would otherwise lose that text until the next edit, with no message.
    fn keep_copy_timer(&mut self, doc: DocId, now: Instant) {
        let delay = self.config.autosave_delay;
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        let again = std::mem::take(&mut d.autosave.again);
        if d.autosave.due.is_some() || d.autosave.running.is_some() {
            return;
        }
        let has_own_copy = d.autosave.generation.is_some()
            && d.autosave.written_revision == Some(d.view.revision());
        if has_own_copy || d.read_only || !d.is_dirty() || d.is_blank_untitled() {
            return;
        }
        d.autosave.due = if again {
            Some(now)
        } else {
            autosave_time(now, delay)
        };
    }

    fn review_failed_quit_autosave(&mut self, doc: DocId, error: &DocError) {
        let name = self.display_name(doc);
        if !self.quit_review.contains(&doc) {
            self.quit_review.push_back(doc);
        }
        self.ask_with(
            doc,
            QuestionKind::AutosaveFailedAtQuit,
            format!("QuickEdit could not keep the unsaved changes to \u{201C}{name}\u{201D}."),
            format!(
                "{} If you quit now, those changes will be lost.",
                error.message
            ),
            vec![Answer::Save, Answer::DontSave, Answer::Cancel],
            QuestionUse::QuitReview,
        );
    }

    /// Removes this document's autosaved copies. A running autosave removes its own at its
    /// end, by the rule in `finish_autosave`.
    fn remove_autosave(&mut self, doc: DocId) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.autosave.due = None;
        d.autosave.again = false;
        d.autosave.written_revision = None;
        if let Some(running) = &d.autosave.running {
            running.store(true, Ordering::Release);
            d.autosave.generation = None;
            return;
        }
        if d.autosave.generation.take().is_some() {
            let key = d.key.clone();
            self.remove_generations_later(doc, key);
            self.touch_session_now();
        }
    }

    fn remove_generations_later(&mut self, doc: DocId, key: String) {
        let Some(dir) = self.session.as_ref().map(|s| s.dir.join("docs")) else {
            return;
        };
        // A copy this run writes later is newer than this removal, and it stays.
        let below = self.next_generation;
        // The list carries this before the removal is known to work, so that a failed removal
        // or a kill cannot bring back a copy whose text is on disk or was discarded.
        self.dead_copies.insert(key.clone(), below);
        self.touch_session_now();
        self.removals_running += 1;
        let job_key = key;
        let started = self.spawn("autosave-remove", move || {
            let result = remove_generations(&dir, &job_key, below);
            Completion::AutosaveRemoved {
                doc,
                key: job_key,
                below,
                result,
            }
        });
        if !started {
            self.removals_running -= 1;
            self.events.push(DocEvent::AutosaveFailed {
                doc,
                error: DocError::new(
                    DocErrorKind::AutosaveFailed,
                    None,
                    "Windows could not start a thread to remove an old autosaved copy. The copy will not come back, and the next launch removes it.",
                ),
            });
        }
    }

    fn run_due_autosaves(&mut self, now: Instant) {
        if self.session.is_none() {
            if self.restored {
                self.no_session_for_autosaves(now);
            }
            return;
        }
        let due: Vec<DocId> = self
            .docs
            .values()
            .filter(|d| d.autosave.due.is_some_and(|at| at <= now))
            .map(|d| d.id)
            .collect();
        for doc in due {
            let Some(d) = self.docs.get_mut(&doc) else {
                continue;
            };
            d.autosave.due = None;
            if d.loading {
                continue;
            }
            if !d.is_dirty() || d.is_blank_untitled() {
                self.remove_autosave(doc);
                continue;
            }
            if d.read_only {
                continue;
            }
            if d.autosave.running.is_some() {
                d.autosave.again = true;
                continue;
            }
            if d.autosave.generation.is_some()
                && d.autosave.written_revision == Some(d.view.revision())
            {
                continue;
            }
            self.start_autosave(doc);
        }
    }

    /// With no session folder no copy can ever be written. So each due time is used up here,
    /// and the user hears it once.
    fn no_session_for_autosaves(&mut self, now: Instant) {
        let mut told = Vec::new();
        for d in self.docs.values_mut() {
            if !d.autosave.due.is_some_and(|at| at <= now) {
                continue;
            }
            d.autosave.due = None;
            if d.is_dirty()
                && !d.read_only
                && d.autosave.last_failure_kind != Some(DocErrorKind::AutosaveFailed)
            {
                let error = DocError::new(
                    DocErrorKind::AutosaveFailed,
                    d.path.as_deref(),
                    "QuickEdit has no session folder, so unsaved text is not being kept. Save it to keep it.",
                );
                d.autosave.last_failure_kind = Some(error.kind);
                d.autosave.last_failure = Some(error.message.clone());
                told.push((d.id, error));
            }
        }
        for (doc, error) in told {
            self.events.push(DocEvent::AutosaveFailed { doc, error });
        }
    }

    // ---------------------------------------------------------------- session list

    fn touch_session(&mut self) {
        self.session_dirty = true;
        if self.session_due.is_none() {
            self.session_due = Some(Instant::now() + self.config.session_delay);
        }
    }
    fn touch_session_now(&mut self) {
        self.session_dirty = true;
        self.session_due = Some(Instant::now());
    }

    fn encode_session(&self) -> String {
        let mut windows = Vec::new();
        let mut docs = Vec::new();
        for win in &self.windows {
            let mut rows = Vec::new();
            for tab in &win.tabs {
                let Some(d) = self.docs.get(tab) else {
                    continue;
                };
                // A blank untitled window stays off the list, as on the Mac (1.2.0).
                if d.is_blank_untitled() {
                    continue;
                }
                rows.push(self.doc_meta(d, win.id.0));
            }
            if rows.is_empty() {
                continue;
            }
            let active = win
                .active
                .and_then(|a| self.docs.get(&a))
                .map(|d| d.key.clone());
            windows.push(WindowMeta {
                id: win.id.0,
                frame: win.frame,
                show: win.show,
                active,
            });
            docs.extend(rows);
        }
        // The last row is the last used window, so a restored run knows it.
        if let Some(last) = self.last_used
            && let Some(at) = windows.iter().position(|w| w.id == last.0)
        {
            let row = windows.remove(at);
            windows.push(row);
        }
        // Read place of hidden loads keeps their row until they appear.
        for d in self.docs.values().filter(|d| d.hidden) {
            let mut meta = self.doc_meta(d, windows.last().map_or(0, |w: &WindowMeta| w.id));
            meta.has_autosave = false;
            if !windows.is_empty() {
                docs.push(meta);
            }
        }
        encode_session(&windows, &docs, &self.dead_copies)
    }

    fn start_session_write(&mut self) {
        let Some(dir) = self.session.as_ref().map(|s| s.dir.clone()) else {
            return;
        };
        if self.session_running {
            return;
        }
        let text = self.encode_session();
        self.session_dirty = false;
        self.session_due = None;
        self.session_running = true;
        let started = self.spawn("session", move || {
            Completion::SessionWritten(write_manifest(&dir, &text))
        });
        if !started {
            self.session_running = false;
            self.events.push(DocEvent::SessionWriteFailed(no_thread(
                "save the list of windows at",
                &self.config.root,
            )));
            // The next change tries again. No time is set here, so a poll does not spin.
            self.session_dirty = false;
        }
    }

    fn recent_changed(&mut self) {
        self.recent_dirty = true;
        self.events.push(DocEvent::RecentChanged);
        self.start_recent_write();
    }

    fn start_recent_write(&mut self) {
        if self.recent_running || !self.recent_dirty || !self.restored {
            return;
        }
        let root = self.config.root.clone();
        let text = encode_recent(&self.recent);
        self.recent_dirty = false;
        self.recent_running = true;
        let shown = root.clone();
        let started = self.spawn("recent", move || {
            Completion::RecentWritten(write_recent(&root, &text))
        });
        if !started {
            self.recent_running = false;
            self.events.push(DocEvent::SessionWriteFailed(no_thread(
                "save Open Recent at",
                &shown,
            )));
        }
    }

    /// Open Recent, newest first. `recent_menu_rows` makes the rows.
    pub fn recent(&self) -> &[PathBuf] {
        &self.recent
    }
    /// Clear Menu.
    pub fn clear_recent(&mut self) {
        self.recent.clear();
        self.recent_changed();
    }

    // ---------------------------------------------------------------- restore

    /// Starts the read of what the last run left. `RestoreFinished` says when the windows are
    /// made. Text arrives in each tab as it loads, and the app stays usable meanwhile.
    pub fn begin_restore(&mut self) {
        if self.restore_started {
            return;
        }
        self.restore_started = true;
        let root = self.config.root.clone();
        let started = self.spawn("restore", move || {
            Completion::Restored(Box::new(build_restore_plan(&root)))
        });
        if !started {
            self.session_failed = true;
            self.restored = true;
            self.events.push(DocEvent::RestoreProblem(DocError::new(
                DocErrorKind::Other,
                None,
                "Windows could not start a thread to read the last session.",
            )));
            self.finish_restore();
        }
    }

    fn apply_restore(&mut self, plan: Result<RestorePlan, DocError>) {
        self.restored = true;
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                self.session_failed = true;
                self.events.push(DocEvent::RestoreProblem(error));
                self.finish_restore();
                return;
            }
        };
        let RestorePlan {
            lock,
            windows,
            docs,
            problems,
            recent,
            next_generation,
            dead,
        } = plan;
        self.dead_copies.extend(dead);
        self.session = Some(lock);
        self.next_generation = self.next_generation.max(next_generation);
        for problem in problems {
            self.events.push(DocEvent::RestoreProblem(problem));
        }
        if self.recent.is_empty() {
            self.recent = recent;
            self.events.push(DocEvent::RecentChanged);
        } else {
            // Files opened before the read ended come first, then the old list.
            let newer = std::mem::replace(&mut self.recent, recent);
            for path in newer.iter().rev() {
                add_recent(&mut self.recent, path);
            }
            self.recent_dirty = true;
            self.events.push(DocEvent::RecentChanged);
        }
        self.start_recent_write();
        let mut made = BTreeMap::new();
        let mut active: Vec<(WindowId, String)> = Vec::new();
        for meta in &windows {
            let id = self.add_window();
            if let Some(win) = self.window_mut(id) {
                win.frame = meta.frame;
                win.show = meta.show;
            }
            made.insert(meta.id, id);
            if let Some(key) = &meta.active {
                active.push((id, key.clone()));
            }
        }
        for restored in docs {
            if let Some(window) = made.get(&restored.meta.window).copied() {
                self.restore_doc(window, restored);
            }
        }
        for (window, key) in active {
            let doc = self.docs.values().find(|d| d.key == key).map(|d| d.id);
            if let Some(doc) = doc {
                self.set_active_tab(window, doc);
            }
        }
        // A window whose every row failed to come back does not stay empty.
        for window in self.windows() {
            self.drop_empty_window(window);
        }
        self.sync_watches();
        self.touch_session();
        self.finish_restore();
    }

    /// Files that waited for the restore land now, in the windows it made, so that a request
    /// from before and one from after share a window, and the count below includes them.
    fn finish_restore(&mut self) {
        self.open_due = None;
        self.run_open_queue(Instant::now());
        self.restore_finished_sent = true;
        self.events.push(DocEvent::RestoreFinished {
            windows: self.windows.len(),
        });
    }

    fn restore_doc(&mut self, window: WindowId, restored: RestoreDoc) {
        let RestoreDoc { meta, generations } = restored;
        let Ok(mut doc) = self.make_doc(meta.format.clone()) else {
            self.events.push(DocEvent::RestoreProblem(DocError::new(
                DocErrorKind::Other,
                meta.path.as_deref(),
                "A text view could not start, so a document did not come back. Its autosaved copy stays on disk.",
            )));
            return;
        };
        doc.key = meta.key.clone();
        doc.path = meta.path.clone();
        doc.custom_name = meta.name.clone();
        doc.untitled_number = meta.untitled_number;
        doc.open_choice = meta.open_choice;
        doc.prevent_editing = meta.prevent_editing;
        doc.wrap_to_page = meta.wrap_to_page;
        doc.copy_source = meta.copy_source.clone();
        doc.base = meta.base;
        doc.loading = true;
        doc.read_only = meta.read_only;
        // Set from the start, so that a list written during the load still says it.
        doc.changed_while_away = meta.changed_while_away;
        doc.was_dirty = !generations.is_empty();
        doc.view.begin_load();
        let place = Place {
            anchor: meta.anchor,
            caret: meta.caret,
            top: meta.top,
        };
        doc.place = place;
        doc.pending_place = Some(place);
        doc.autosave.generation = generations.first().map(|(generation, _)| *generation);
        let id = doc.id;
        let choice = self.choice_of(meta.open_choice);
        self.docs.insert(id, doc);
        self.attach(window, id, None, false);
        if !generations.is_empty() {
            let files = generations.into_iter().map(|(_, path)| path).collect();
            self.queue_load(
                id,
                LoadKind::RestoreAutosave {
                    path: meta.path.clone(),
                    saved_base: meta.base,
                },
                Source::Autosave { generations: files },
                true,
            );
        } else if let (Some(path), true) = (&meta.copy_source, meta.path.is_none()) {
            self.queue_load(
                id,
                LoadKind::RestoreCopy,
                // Through a file read, since the source may be in another encoding.
                Source::File {
                    path: path.clone(),
                    choice,
                    compare: None,
                    force_copy: false,
                },
                false,
            );
        } else if let Some(path) = meta.path.clone() {
            self.queue_load(
                id,
                LoadKind::RestoreFile { path: path.clone() },
                Source::File {
                    path,
                    choice,
                    compare: None,
                    force_copy: false,
                },
                true,
            );
        } else {
            self.discard_failed_doc(id);
        }
    }

    // ---------------------------------------------------------------- outside changes

    /// A window of this app came to the front. A file that changed in a place no watch can
    /// hear shows now. Costs a list here and a worker's file checks.
    pub fn app_activated(&mut self) {
        let docs: Vec<DocId> = self
            .docs
            .values()
            .filter(|d| d.path.is_some() && !d.loading)
            .map(|d| d.id)
            .collect();
        self.stat_waiting.extend(docs);
        // A folder made again since its watch ended gets a new one.
        self.sync_watches();
        self.start_stat();
    }

    fn sync_watches(&mut self) {
        if !self.config.watch_folders {
            return;
        }
        let mut wanted: BTreeMap<String, PathBuf> = BTreeMap::new();
        for d in self.docs.values() {
            if let Some(folder) = d.path.as_deref().and_then(Path::parent) {
                wanted.insert(
                    folder.to_string_lossy().to_lowercase(),
                    folder.to_path_buf(),
                );
            }
        }
        self.watches.retain(|w| {
            !w.ended.load(Ordering::Acquire)
                && wanted.contains_key(&w.folder.to_string_lossy().to_lowercase())
        });
        let have: BTreeSet<String> = self
            .watches
            .iter()
            .map(|w| w.folder.to_string_lossy().to_lowercase())
            .collect();
        for (key, folder) in wanted {
            if have.contains(&key) {
                continue;
            }
            let sender = self.sender.clone();
            let wake = self.config.wake.clone();
            let faults = self.config.faults.clone();
            let before = move || hold_while(&faults.hold_watch, &faults.watch_is_held);
            let started = watch_folder(&folder, before, move |folder, news| {
                let sent = sender.send(Completion::Folder {
                    folder: folder.to_path_buf(),
                    news,
                });
                if sent.is_ok() {
                    wake();
                }
            });
            match started {
                Ok(watch) => self.watches.push(watch),
                Err(error) => self.events.push(DocEvent::WatchUnavailable {
                    folder,
                    reason: error.to_string(),
                }),
            }
        }
    }

    fn folder_news(&mut self, folder: &Path, news: FolderNews) {
        let in_folder = |d: &Doc, names: &[&std::ffi::OsStr]| {
            let Some(path) = d.path.as_deref() else {
                return false;
            };
            if !path.parent().is_some_and(|p| same_path_text(p, folder)) {
                return false;
            }
            names.is_empty()
                || path
                    .file_name()
                    .is_some_and(|own| names.iter().any(|n| same_name(own, n)))
        };
        let mut hit = Vec::new();
        match news {
            // Every open file here gets a look, not only the names heard. A new name may be a
            // hard link to one of them, and a later deletion must know it had two names.
            // A look reloads nothing unless that file's own stamp moved.
            FolderNews::Names => {
                hit.extend(
                    self.docs
                        .values()
                        .filter(|d| in_folder(d, &[]))
                        .map(|d| d.id),
                );
            }
            FolderNews::Renamed { old, new } => {
                let target = folder.join(&new);
                for d in self.docs.values_mut() {
                    if in_folder(d, &[old.as_os_str()]) {
                        d.rename_hint = Some(target.clone());
                        hit.push(d.id);
                    } else if in_folder(d, &[new.as_os_str()]) {
                        hit.push(d.id);
                    }
                }
            }
            FolderNews::Everything => {
                // A watch stands here, so a later failure is news again.
                self.watch_failed.remove(folder);
                hit.extend(
                    self.docs
                        .values()
                        .filter(|d| in_folder(d, &[]))
                        .map(|d| d.id),
                );
            }
            FolderNews::CannotWatch(reason) => {
                if self.watch_failed.insert(folder.to_path_buf()) {
                    self.events.push(DocEvent::WatchUnavailable {
                        folder: folder.to_path_buf(),
                        reason,
                    });
                }
            }
        }
        self.stat_waiting.extend(hit);
        self.start_stat();
    }

    fn start_stat(&mut self) {
        if (self.stat_running && self.remote_stat_running) || self.stat_waiting.is_empty() {
            return;
        }
        let mut local = Vec::new();
        let mut remote = Vec::new();
        let waiting = std::mem::take(&mut self.stat_waiting);
        for id in waiting {
            let Some(d) = self.docs.get(&id) else {
                continue;
            };
            // Its own swap can look like a change, so a running job's end looks again.
            if d.saving || d.moving || d.loading {
                self.stat_waiting.insert(id);
                continue;
            }
            let Some(path) = d.path.clone() else {
                continue;
            };
            let is_remote = is_network_path(&path);
            if (is_remote && self.remote_stat_running) || (!is_remote && self.stat_running) {
                self.stat_waiting.insert(id);
                continue;
            }
            let item = StatItem {
                doc: id,
                epoch: d.file_epoch,
                path,
                identity: d.base.map(|b| b.identity),
                links: d.base.map_or(0, |b| b.links),
                hint: d.rename_hint.clone(),
            };
            if is_remote {
                remote.push(item);
            } else {
                local.push(item);
            }
        }
        self.start_stat_batch(local, false);
        // A share that does not answer holds up only the other shares.
        self.start_stat_batch(remote, true);
    }

    fn start_stat_batch(&mut self, items: Vec<StatItem>, remote: bool) {
        if items.is_empty() {
            return;
        }
        let ids: Vec<DocId> = items.iter().map(|item| item.doc).collect();
        let started = self.spawn("check", move || Completion::Stat {
            remote,
            results: items.iter().map(check_file).collect(),
        });
        if started {
            if remote {
                self.remote_stat_running = true;
            } else {
                self.stat_running = true;
            }
        } else {
            // These wait for the next look, as a window coming forward asks for one.
            self.stat_waiting.extend(ids);
        }
    }

    fn apply_stat(&mut self, results: Vec<StatResult>) {
        let now_time = Instant::now();
        let delay = self.config.autosave_delay;
        for result in results {
            let doc = result.doc;
            let Some(d) = self.docs.get_mut(&doc) else {
                continue;
            };
            if d.saving || d.moving || d.loading || d.return_check || result.epoch != d.file_epoch {
                d.rename_hint = None;
                self.stat_waiting.insert(doc);
                continue;
            }
            if let Some((new_path, base, stamp)) = result.moved_to {
                // An editor may have moved the old file aside to save a new one in its place.
                // A real rename is still a rename a moment later.
                if !d.missing_confirm {
                    if d.missing_recheck_at.is_none() {
                        d.missing_recheck_at = Some(now_time + MISSING_RECHECK);
                    }
                    continue;
                }
                d.missing_confirm = false;
                d.rename_hint = None;
                // Another program moved it. The document follows, with its state as it was.
                let old = d.path.replace(new_path.clone());
                d.base = Some(base);
                d.stamp = stamp;
                d.missing_recheck_at = None;
                if d.file_state == FileState::Deleted {
                    d.file_state_is_present(now_time);
                }
                if let Some(old) = old {
                    for row in &mut self.recent {
                        if same_path_text(row, &old) {
                            row.clone_from(&new_path);
                        }
                    }
                }
                self.events.push(DocEvent::FileMoved {
                    doc,
                    path: new_path,
                });
                self.events.push(DocEvent::DocChanged(doc));
                self.recent_changed();
                self.sync_watches();
                self.touch_session();
                continue;
            }
            match result.now {
                None => {
                    if d.file_state == FileState::Deleted {
                        continue;
                    }
                    // A program that saves by delete then create comes back at once. Look
                    // once more, a moment later, before saying anything.
                    if !d.missing_confirm {
                        if d.missing_recheck_at.is_none() {
                            d.missing_recheck_at = Some(now_time + MISSING_RECHECK);
                        }
                        continue;
                    }
                    d.missing_confirm = false;
                    d.rename_hint = None;
                    d.dirty_from_delete = !d.is_dirty();
                    d.file_state = FileState::Deleted;
                    d.forced_dirty = true;
                    d.was_dirty = true;
                    d.stamp = None;
                    d.changed_while_away = false;
                    if d.autosave.due.is_none() {
                        d.autosave.due = autosave_time(now_time, delay);
                    }
                    if let Some(path) = d.path.clone() {
                        self.events.push(DocEvent::FileDeleted { doc, path });
                    }
                    self.events.push(DocEvent::DocChanged(doc));
                    self.touch_session();
                }
                Some((base, stamp)) => {
                    d.rename_hint = None;
                    d.missing_recheck_at = None;
                    d.missing_confirm = false;
                    let came_back = d.file_state == FileState::Deleted;
                    if came_back {
                        if d.dirty_from_delete && d.view.is_at_save_point() {
                            // Nothing but the file's absence made this document unsaved, but
                            // the file that held this text has gone. It may exist only here.
                            self.start_return_check(doc);
                            continue;
                        } else {
                            // It holds another program's bytes now, and this text is not on
                            // disk: a restored copy, or edits. Both stay, and a Save must ask.
                            d.file_state = FileState::Present;
                            d.changed_while_away = true;
                            d.stamp = stamp;
                            d.base = Some(base);
                        }
                        self.events.push(DocEvent::DocChanged(doc));
                        self.touch_session();
                    }
                    let Some(d) = self.docs.get_mut(&doc) else {
                        continue;
                    };
                    if !came_back && stamp.is_some() && d.stamp == stamp {
                        // Nothing to read. A mark or another name may still be news.
                        d.base = Some(base);
                        continue;
                    }
                    if d.is_dirty() {
                        // The Mac's rule: an edited document does not reload. The next Save
                        // asks, also after a restart. The file itself is news, so a later
                        // deletion or Save concerns this file and not an old one moved aside.
                        d.base = Some(base);
                        if !d.changed_while_away {
                            d.changed_while_away = true;
                            self.touch_session();
                        }
                        continue;
                    }
                    self.start_reload(doc);
                }
            }
        }
    }

    fn start_return_check(&mut self, doc: DocId) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        let Some(path) = d.path.clone() else {
            return;
        };
        d.return_check = true;
        d.reloading = true;
        d.reload_retry_at = None;
        let compare = Some(d.view.worker_snapshot());
        let open_choice = d.open_choice;
        let choice = self.choice_of(open_choice);
        self.queue_load(
            doc,
            LoadKind::ReturnCheck,
            Source::File {
                path,
                choice,
                compare,
                force_copy: false,
            },
            false,
        );
    }

    fn start_reload(&mut self, doc: DocId) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        if d.reloading {
            d.reload_again = true;
            return;
        }
        let Some(path) = d.path.clone() else {
            return;
        };
        d.reloading = true;
        d.reload_retry_at = None;
        let compare = Some(d.view.worker_snapshot());
        let open_choice = d.open_choice;
        let choice = self.choice_of(open_choice);
        self.queue_load(
            doc,
            LoadKind::Reload,
            Source::File {
                path,
                choice,
                compare,
                force_copy: false,
            },
            false,
        );
    }

    // ---------------------------------------------------------------- the pump

    /// When the host should call `poll` again, even with no wake. `None` means no timer at
    /// all. Set a one-shot timer for this after each poll.
    pub fn next_deadline(&self) -> Option<Instant> {
        // While a write runs, its end wakes the host, so a stale time here would only spin.
        let mut soonest = self
            .session_due
            .filter(|_| self.session.is_some() && !self.session_running);
        let mut consider = |at: Option<Instant>| {
            if let Some(at) = at
                && soonest.is_none_or(|s| at < s)
            {
                soonest = Some(at);
            }
        };
        if !self.open_queue.is_empty() && (!self.restore_started || self.restored) {
            consider(self.open_due);
        }
        for d in self.docs.values() {
            // During a restore no copy can start, and its end wakes the host.
            if self.session.is_some() || self.restored {
                consider(d.autosave.due);
            }
            consider(d.reload_retry_at);
            consider(d.missing_recheck_at);
        }
        soonest
    }

    /// Takes what the workers finished and runs what is due. It waits for nothing. Call it on
    /// each wake and when `next_deadline` passes.
    pub fn poll(&mut self, now: Instant) {
        while let Ok(completion) = self.receiver.try_recv() {
            self.handle(completion);
        }
        self.retry_held_work(now);
        self.run_due_autosaves(now);
        if self.session_dirty && self.session_due.is_some_and(|at| at <= now) {
            self.start_session_write();
        }
        self.run_open_queue(now);
        self.start_queued_loads();
        self.start_stat();
        self.advance_quit();
    }

    fn retry_held_work(&mut self, now: Instant) {
        let mut ctx = ViewContext::default();
        for view in &mut self.closing_views {
            view.poll_layout(&mut ctx);
        }
        self.closing_views.retain(EditorView::cut_is_running);
        let ids: Vec<DocId> = self.docs.keys().copied().collect();
        for doc in ids {
            let Some(d) = self.docs.get_mut(&doc) else {
                continue;
            };
            if d.pending_adopt.is_some()
                && !d.view.cut_is_running()
                && d.view.composition().is_none()
            {
                let waiting = d.pending_adopt.take();
                if let Some(PendingAdopt::Waiting { body, then }) = waiting {
                    self.adopt_body(doc, body, then);
                }
            }
            let Some(d) = self.docs.get_mut(&doc) else {
                continue;
            };
            if d.pending_place.is_some() && !d.loading {
                self.try_apply_place(doc);
            }
            let Some(d) = self.docs.get_mut(&doc) else {
                continue;
            };
            if d.reload_retry_at.is_some_and(|at| at <= now) {
                d.reload_retry_at = None;
                if !d.is_dirty() {
                    self.start_reload(doc);
                }
                continue;
            }
            if d.missing_recheck_at.is_some_and(|at| at <= now) {
                d.missing_recheck_at = None;
                d.missing_confirm = true;
                self.stat_waiting.insert(doc);
                continue;
            }
            // A large file with bytes past its first 64 KiB that are not UTF-8: the index
            // found them. The Mac would decode such a file, so a UTF-8 copy is made once.
            if d.read_only && !d.tried_copy && !d.reloading && !d.loading {
                let failure = d.view.store_status().failure;
                if let Some(failure) = failure {
                    if failure.valid_up_to.0 > 0 && d.temp_copy.is_none() {
                        d.tried_copy = true;
                        self.retry_as_copy(doc);
                    } else if failure.valid_up_to.0 == 0 {
                        d.tried_copy = true;
                        self.stat_waiting.insert(doc);
                    }
                }
            }
        }
    }

    fn retry_as_copy(&mut self, doc: DocId) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        let Some(path) = d.copy_source.clone().or_else(|| d.path.clone()) else {
            return;
        };
        d.reloading = true;
        let open_choice = d.open_choice;
        let choice = self.choice_of(open_choice);
        self.queue_load(
            doc,
            LoadKind::LargeRetry,
            Source::File {
                path,
                choice,
                compare: None,
                force_copy: true,
            },
            false,
        );
    }

    fn handle(&mut self, completion: Completion) {
        match completion {
            Completion::Load {
                doc,
                kind,
                remote,
                result,
                identity_now,
                leftovers,
            } => {
                if remote {
                    self.remote_loads_running = self.remote_loads_running.saturating_sub(1);
                } else {
                    self.loads_running = self.loads_running.saturating_sub(1);
                }
                self.finish_load(doc, kind, *result, identity_now, leftovers);
            }
            Completion::Save {
                doc,
                revision,
                path,
                format,
                save_as,
                result,
            } => self.finish_save(doc, revision, path, format, save_as, result),
            Completion::Autosave {
                doc,
                key,
                generation,
                revision,
                result,
            } => self.finish_autosave(doc, &key, generation, revision, result),
            Completion::AutosaveRemoved {
                doc,
                key,
                below,
                result,
            } => {
                self.removals_running = self.removals_running.saturating_sub(1);
                match result {
                    Ok(()) => {
                        // A later removal has its own mark and its own end.
                        if self.dead_copies.get(&key) == Some(&below) {
                            self.dead_copies.remove(&key);
                            self.touch_session();
                        }
                    }
                    // The mark stays, so the next launch does not bring the copy back.
                    Err(error) => self.events.push(DocEvent::AutosaveFailed { doc, error }),
                }
            }
            Completion::Stat { remote, results } => {
                if remote {
                    self.remote_stat_running = false;
                } else {
                    self.stat_running = false;
                }
                self.apply_stat(results);
            }
            Completion::Moved { doc, path, result } => self.finish_move(doc, path, result),
            Completion::Unlocked {
                doc,
                then_save,
                result,
            } => self.finish_unlock(doc, then_save, result),
            Completion::Recovered { path, result } => self.events.push(match result {
                Ok(()) => DocEvent::LeftoverRecovered { path },
                Err(error) => DocEvent::LeftoverNotRecovered { path, error },
            }),
            Completion::LeftoverGone { leftover, result } => self.events.push(match result {
                Ok(()) => DocEvent::LeftoverDeleted { leftover },
                Err(error) => DocEvent::LeftoverNotDeleted { leftover, error },
            }),
            Completion::Fit { doc, unfit } => {
                self.events.push(DocEvent::EncodingFit { doc, unfit });
            }
            Completion::SessionWritten(result) => {
                self.session_running = false;
                if let Err(error) = result {
                    self.events.push(DocEvent::SessionWriteFailed(error));
                }
            }
            Completion::RecentWritten(result) => {
                self.recent_running = false;
                if let Err(error) = result {
                    self.events.push(DocEvent::SessionWriteFailed(error));
                }
                self.start_recent_write();
            }
            Completion::Restored(plan) => self.apply_restore(*plan),
            Completion::Folder { folder, news } => self.folder_news(&folder, news),
        }
    }

    fn finish_move(
        &mut self,
        doc: DocId,
        path: PathBuf,
        result: Result<(FileBase, Option<FileStamp>), DocError>,
    ) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.moving = false;
        match result {
            Ok((base, stamp)) => {
                d.file_epoch += 1;
                let old = d.path.replace(path.clone());
                d.base = Some(base);
                d.stamp = stamp;
                if d.copy_source.is_some() {
                    d.copy_source = Some(path.clone());
                }
                if let Some(old) = old {
                    self.recent.retain(|row| !same_path_text(row, &old));
                }
                add_recent(&mut self.recent, &path);
                self.recent_changed();
                self.sync_watches();
                self.touch_session();
                self.events.push(DocEvent::FileMoved { doc, path });
                self.events.push(DocEvent::DocChanged(doc));
            }
            Err(error) => self.events.push(DocEvent::MoveFailed { doc, error }),
        }
    }

    fn finish_unlock(&mut self, doc: DocId, then_save: bool, result: Result<(), DocError>) {
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.unlocking = false;
        match result {
            Ok(()) => {
                if let Some(base) = &mut d.base {
                    base.read_only = false;
                }
                // The mark's change moves no write time, so the stamp stays good.
                if then_save && let Err(error) = self.save(doc) {
                    self.events.push(DocEvent::SaveFailed {
                        doc,
                        error,
                        offer_save_as: true,
                    });
                    self.save_did_not_happen(doc);
                }
            }
            Err(error) => {
                self.events.push(DocEvent::UnlockFailed { doc, error });
                self.save_did_not_happen(doc);
            }
        }
        self.advance_quit();
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<DocEvent> {
        std::mem::take(&mut self.events)
    }
    /// What the views asked of the host during this store's own calls, such as an undo.
    pub fn take_view_requests(&mut self) -> Vec<(DocId, ViewRequest)> {
        std::mem::take(&mut self.requests)
    }

    // ---------------------------------------------------------------- quit

    /// Quit. No question for a document with unsaved text: its text goes to its autosaved
    /// copy and comes back at the next launch. A blank untitled document simply goes.
    /// `ReadyToQuit` says when the app may exit. If a copy cannot be written, a question
    /// comes first.
    pub fn begin_quit(&mut self) {
        self.quitting = true;
        self.quit_ready_sent = false;
        self.close_queue.clear();
        self.closing_window = None;
        self.panel_queue.clear();
        self.quit_review.clear();
        for d in self.docs.values_mut() {
            d.quit_skip = false;
        }
        let ids: Vec<DocId> = self.docs.keys().copied().collect();
        for doc in ids {
            let mut ctx = ViewContext::default();
            let Some(d) = self.docs.get_mut(&doc) else {
                continue;
            };
            if !d.loading {
                d.view.flush_input(&mut ctx);
            }
            self.forward(doc, ctx);
            self.note_view_changed(doc);
            let Some(d) = self.docs.get(&doc) else {
                continue;
            };
            if d.loading || d.read_only {
                continue;
            }
            if d.is_blank_untitled() || !d.is_dirty() {
                self.remove_autosave(doc);
                continue;
            }
            let current = d.autosave.generation.is_some()
                && d.autosave.written_revision == Some(d.view.revision());
            if !current {
                if d.autosave.running.is_some() {
                    if let Some(d) = self.docs.get_mut(&doc) {
                        d.autosave.again = true;
                    }
                } else {
                    self.start_autosave(doc);
                }
            }
        }
        self.touch_session_now();
        self.advance_quit();
    }

    fn advance_quit(&mut self) {
        if !self.quitting || self.quit_ready_sent {
            return;
        }
        if self
            .questions
            .iter()
            .any(|q| q.usage == QuestionUse::QuitReview)
        {
            return;
        }
        let mut busy = self.saves_running > 0
            || self.session_running
            || self.recent_running
            || self.removals_running > 0
            || !self.closing_views.is_empty();
        let ids: Vec<DocId> = self.docs.keys().copied().collect();
        for doc in ids {
            let Some(d) = self.docs.get(&doc) else {
                continue;
            };
            if d.autosave.running.is_some() || d.view.cut_is_running() {
                busy = true;
                continue;
            }
            if d.loading || d.read_only {
                continue;
            }
            let wants_copy = !d.quit_skip
                && d.is_dirty()
                && !d.is_blank_untitled()
                && !(d.autosave.generation.is_some()
                    && d.autosave.written_revision == Some(d.view.revision()));
            if !wants_copy {
                continue;
            }
            let no_session = self.session.is_none();
            if no_session && !self.restored {
                busy = true;
                continue;
            }
            if !no_session && d.autosave.last_failure.is_none() {
                busy = true;
                self.start_autosave(doc);
                continue;
            }
            // The text has no copy, so nothing ends this quit but the person's answer. A Save
            // they chose counts until it ends. If it fails, this asks again.
            if self.save_is_under_way(doc) {
                busy = true;
                continue;
            }
            let error = DocError::new(
                DocErrorKind::AutosaveFailed,
                d.path.as_deref(),
                d.autosave.last_failure.clone().unwrap_or_else(|| {
                    "QuickEdit has no session folder, so nothing could be kept.".to_owned()
                }),
            );
            self.review_failed_quit_autosave(doc, &error);
            return;
        }
        if busy {
            return;
        }
        if self.session_dirty {
            self.start_session_write();
            if self.session_running {
                return;
            }
        }
        if self.recent_dirty {
            self.start_recent_write();
            return;
        }
        self.quit_ready_sent = true;
        self.events.push(DocEvent::ReadyToQuit);
    }

    /// A Save of this document is running, or waits on a dialog or on a question it raised.
    fn save_is_under_way(&self, doc: DocId) -> bool {
        let raised_by_save = self.questions.iter().any(|q| {
            q.question.doc == doc
                && matches!(
                    q.question.kind,
                    QuestionKind::LossyOverwrite
                        | QuestionKind::ChangedOnDisk
                        | QuestionKind::EncodingNoLongerFits { on_edit: false }
                        | QuestionKind::LockedFile { on_edit: false }
                )
        });
        raised_by_save
            || self
                .docs
                .get(&doc)
                .is_some_and(|d| d.saving || d.reloading || d.unlocking || d.panel_open)
    }

    /// What `SessionEndCopies::write` would need now, or `None` when the last answer still
    /// holds. Its cost follows the number of tabs, not their text.
    pub fn session_end_copies(&mut self) -> Option<SessionEndCopies> {
        let docs_dir = self.session.as_ref()?.dir.join("docs");
        let needed: Vec<DocId> = self
            .docs
            .values()
            .filter(|d| {
                let has_own_copy = d.autosave.generation.is_some()
                    && d.autosave.written_revision == Some(d.view.revision());
                !d.loading
                    && !d.read_only
                    && !d.quit_skip
                    && !has_own_copy
                    && d.is_dirty()
                    && !d.is_blank_untitled()
            })
            .map(|d| d.id)
            .collect();
        let covered: Vec<(DocId, u64)> = needed
            .iter()
            .filter_map(|doc| Some((*doc, self.docs.get(doc)?.view.revision())))
            .collect();
        if covered == self.session_end_covered {
            return None;
        }
        self.session_end_covered = covered;
        let mut items = Vec::with_capacity(needed.len());
        for doc in needed {
            let window = self.window_of(doc).map_or(0, |w| w.0);
            let generation = self.next_generation;
            self.next_generation += 1;
            let Some(d) = self.docs.get(&doc) else {
                continue;
            };
            let mut meta = self.doc_meta(d, window);
            meta.has_autosave = true;
            items.push(SessionEndCopy {
                meta,
                generation,
                previous: d.autosave.generation,
                snapshot: d.view.worker_snapshot(),
            });
        }
        Some(SessionEndCopies { docs_dir, items })
    }

    /// Writes that a process end would cut short.
    fn writes_are_running(&self) -> bool {
        self.saves_running > 0
            || self.session_running
            || self.recent_running
            || self.removals_running > 0
            || !self.closing_views.is_empty()
            || (self.restore_started && !self.restored)
            || self
                .docs
                .values()
                .any(|d| d.autosave.running.is_some() || d.view.cut_is_running())
    }

    /// Runs the quit to its end on this thread, for `WM_ENDSESSION`, where Windows ends the
    /// process when the handler returns. This is the only call here that waits. True when
    /// `ReadyToQuit` came within `limit`.
    pub fn wait_for_quit(&mut self, limit: Duration) -> bool {
        self.session_ending = true;
        let finished = self.run_quit_to_its_end(limit);
        if !self.session_end_failures.is_empty() {
            let lines = std::mem::take(&mut self.session_end_failures);
            if let Err(error) = write_session_end_failures(&self.config.root, &lines) {
                eprintln!("QuickEdit could not keep word of a failed save: {error}");
            }
        }
        finished
    }

    fn run_quit_to_its_end(&mut self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        self.begin_quit();
        loop {
            self.poll(Instant::now());
            if self.quit_ready_sent {
                return true;
            }
            let needs_a_person =
                !self.questions.is_empty() || self.docs.values().any(|d| d.panel_open);
            if needs_a_person && !self.writes_are_running() {
                // Nobody can answer during the end of a session, so what can be written is
                // written, and then this stops. An ordinary question stops nothing by itself:
                // with every copy written, `ReadyToQuit` has come by now.
                if self.session_dirty {
                    self.start_session_write();
                }
                if !self.session_running {
                    return false;
                }
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            if let Ok(completion) = self
                .receiver
                .recv_timeout(left.min(Duration::from_millis(50)))
            {
                self.handle(completion);
            }
        }
    }
}

impl Doc {
    /// Does this document hold text not on disk? A close, a quit and a reload decide by this.
    /// While it restores, its text is in the copy, and the view does not have it yet.
    fn is_dirty(&self) -> bool {
        if self.forced_dirty || (self.loading && self.autosave.generation.is_some()) {
            return true;
        }
        !self.read_only
            && !self.loading
            && (!self.view.is_at_save_point() || self.view.has_queued_edits())
    }

    /// The file is back, and this document had nothing unsaved but the file's absence.
    fn file_state_is_present(&mut self, now: Instant) {
        self.file_state = FileState::Present;
        if self.dirty_from_delete {
            self.dirty_from_delete = false;
            self.forced_dirty = false;
            self.was_dirty = false;
            // A caller has seen that the file holds this text, or that it is the same file, so
            // a copy written meanwhile holds nothing new. The next poll removes it.
            if self.autosave.generation.is_some() {
                self.autosave.due = Some(now);
            } else {
                self.autosave.due = None;
            }
        }
    }

    fn is_blank_untitled(&self) -> bool {
        if self.path.is_some() || self.read_only || self.loading || self.view.has_queued_edits() {
            return false;
        }
        let status = self.view.store_status();
        if status.known_utf16.0 > BLANK_SCAN_LIMIT_UTF16 {
            return false;
        }
        is_blank_text(self.view.worker_snapshot().as_ref())
    }

    fn display_name(&self) -> String {
        if let Some(name) = self
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
        {
            return name;
        }
        if let Some(name) = &self.custom_name {
            return name.clone();
        }
        match self.untitled_number {
            0 | 1 => "Untitled".to_owned(),
            number => format!("Untitled {number}"),
        }
    }

    fn capture_place(&self) -> Place {
        let caret = self.view.caret().0 as u64;
        let selection = self.view.selection();
        let anchor = if selection.start.0 as u64 == caret {
            selection.end.0 as u64
        } else {
            selection.start.0 as u64
        };
        let top = self
            .view
            .visible_range()
            .map_or(self.place.top, |range| range.start.0 as u64);
        Place { anchor, caret, top }
    }

    fn place_if_restored(&self) -> Option<Place> {
        (self.place != Place::default()).then_some(self.place)
    }
}

fn no_thread(what: &str, path: &Path) -> DocError {
    DocError::new(
        DocErrorKind::Other,
        Some(path),
        format!(
            "Windows could not start a thread to {what} {}.",
            path.display()
        ),
    )
}

/// Does this load reach a network share? It may wait there for a long time.
fn load_is_remote(kind: &LoadKind, source: &Source) -> bool {
    let named = match kind {
        LoadKind::RestoreAutosave { path, .. } => path.as_deref(),
        _ => None,
    };
    named.or(source.file()).is_some_and(is_network_path)
}

fn no_window() -> DocError {
    DocError::new(
        DocErrorKind::NoSuchDocument,
        None,
        "That window is no longer open.",
    )
}

fn autosave_time(now: Instant, delay: Duration) -> Option<Instant> {
    (!delay.is_zero()).then(|| now + delay)
}

/// The text the last single edit put in, if it is short enough to check here.
fn last_inserted(view: &EditorView, since: u64) -> Option<String> {
    let ChangesSince::Changes(changes) = view.changes_since(since) else {
        return None;
    };
    let [change] = changes.as_slice() else {
        return None;
    };
    let start = change.change.replaced.start;
    let length = change.change.new_len;
    if length == 0 || length > FIT_CHECK_BYTES {
        return None;
    }
    let mut text = String::with_capacity(length);
    let snapshot = view.worker_snapshot();
    snapshot
        .try_visit_chunks(start..ByteOffset(start.0 + length), &mut |chunk| {
            text.push_str(chunk);
        })
        .ok()?;
    Some(text)
}

fn copy_name(name: &str) -> String {
    match name.rfind('.') {
        Some(at) if at > 0 && at + 1 < name.len() => {
            format!("{} copy{}", &name[..at], &name[at..])
        }
        _ => format!("{name} copy"),
    }
}

/// "foo copy.txt" and 2 give "foo copy 2.txt".
fn numbered_copy_name(base: &str, number: u32) -> String {
    match base.rfind('.') {
        Some(at) if at > 0 && at + 1 < base.len() => {
            format!("{} {number}{}", &base[..at], &base[at..])
        }
        _ => format!("{base} {number}"),
    }
}

/// A store that nobody will use can hold a whole file. Another thread frees it.
fn free_off_thread(loaded: Loaded) {
    let freeing = std::thread::Builder::new()
        .name("qe-docs-free".into())
        .spawn(move || drop(loaded));
    if let Err(error) = freeing {
        eprintln!("QuickEdit: freeing loaded text on this thread, as no thread started: {error}");
    }
}

/// The store may hold the file open for a moment after its view goes.
fn remove_temp_later(path: PathBuf) {
    let removing = std::thread::Builder::new()
        .name("qe-docs-temp".into())
        .spawn(move || {
            for _ in 0..20 {
                match std::fs::remove_file(&path) {
                    Ok(()) => return,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            // The next launch removes it, since this process will have ended.
            remove_quietly(&path);
        });
    if let Err(error) = removing {
        eprintln!("QuickEdit: the next launch removes a temp file: {error}");
    }
}

fn remove_save_temp(target: &Path, temp: PathBuf) {
    let shown = target.display().to_string();
    let target = target.to_path_buf();
    let removing = std::thread::Builder::new()
        .name("qe-docs-temp".into())
        .spawn(move || {
            // The text is still in the window, so this temp file has no other job.
            let Ok(list) = find_leftovers(&target) else {
                return;
            };
            for leftover in list.iter().filter(|l| l.path == temp) {
                if let Err(error) = qe_codec::delete_leftover(
                    leftover,
                    qe_codec::LeftoverDelete::IncludeRecoverable,
                ) {
                    eprintln!("QuickEdit: {error}");
                }
            }
        });
    if let Err(error) = removing {
        eprintln!("QuickEdit: a temp file stays beside {shown}: {error}");
    }
}

fn run_hook(slot: &Mutex<Option<Box<dyn Fn() + Send>>>) {
    if let Ok(mut hook) = slot.lock()
        && let Some(hook) = hook.take()
    {
        hook();
    }
}

fn hold_while(hold: &AtomicBool, reached: &AtomicBool) {
    if hold.load(Ordering::Acquire) {
        reached.store(true, Ordering::Release);
        while hold.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct SaveJob<'a> {
    doc: DocId,
    snapshot: Arc<dyn TextSnapshot>,
    path: &'a Path,
    expected: Option<FileStamp>,
    format: &'a Format,
    others: &'a [(DocId, FileIdentity)],
    copy_source: Option<&'a Path>,
    faults: &'a Faults,
    save_as: bool,
}

/// The bytes of a snapshot in its format. UTF-8 needs one block; others need two for a moment.
fn encoded_bytes(job: &SaveJob<'_>) -> Result<Vec<u8>, SaveTrouble> {
    let trouble = |error: CodecError| match error {
        CodecError::Unencodable(_) => SaveTrouble::DoesNotFit,
        other => SaveTrouble::Other {
            offer_save_as: false,
            error: DocError::from_codec("QuickEdit could not prepare the text of", job.path, other),
        },
    };
    let snapshot = job.snapshot.as_ref();
    let memory = || SaveTrouble::Other {
        offer_save_as: false,
        error: DocError::new(
            DocErrorKind::OutOfMemory,
            Some(job.path),
            "There is not enough memory to prepare this save. The text is still in the window.",
        ),
    };
    if job.format.encoding == TextEncoding::Utf8 {
        let length = snapshot.len_bytes();
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length.0 + 3)
            .map_err(|_| memory())?;
        if job.format.has_bom {
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        let whole = snapshot.visit_chunks(ByteOffset(0)..length, &mut |chunk| {
            bytes.extend_from_slice(chunk.as_bytes());
        });
        if !whole {
            // The map of a file too large to edit refuses to read once the file has gone.
            let error = if snapshot.is_read_only() {
                DocError::new(
                    DocErrorKind::FileNotFound,
                    Some(job.path),
                    format!(
                        "{} has gone, and a file this large is read from its own bytes, so QuickEdit cannot write it again. If it is in the Recycle Bin, restore it there.",
                        job.path.display()
                    ),
                )
            } else {
                DocError::new(
                    DocErrorKind::Other,
                    Some(job.path),
                    "The text could not be read back from memory.",
                )
            };
            return Err(SaveTrouble::Other {
                offer_save_as: false,
                error,
            });
        }
        return Ok(bytes);
    }
    let text = read_all_text(snapshot).map_err(|error| SaveTrouble::Other {
        error,
        offer_save_as: false,
    })?;
    // `encode` cannot report a failed allocation, so this asks for the most it may need.
    let mut probe: Vec<u8> = Vec::new();
    probe
        .try_reserve_exact(text.len().saturating_mul(2).saturating_add(16))
        .map_err(|_| memory())?;
    drop(probe);
    encode(&text, job.format).map_err(trouble)
}

fn run_save(job: &SaveJob<'_>) -> Result<SaveDone, SaveTrouble> {
    let other = |error: DocError, offer_save_as: bool| SaveTrouble::Other {
        error,
        offer_save_as,
    };
    if job.save_as
        && let Ok((base, _)) = file_base(job.path)
        && job
            .others
            .iter()
            .any(|(id, identity)| *id != job.doc && *identity == base.identity)
    {
        return Err(other(
            DocError::new(
                DocErrorKind::OpenInAnotherTab,
                Some(job.path),
                format!(
                    "{} is open in another tab. Close it there, or choose another name.",
                    job.path.display()
                ),
            ),
            true,
        ));
    }
    let code = job.faults.save_error_code.load(Ordering::Relaxed);
    if code != 0 {
        return Err(map_save_error(
            job,
            CodecError::SaveFailed {
                path: job.path.to_path_buf(),
                code,
                original_intact: true,
                leftover: None,
            },
        ));
    }
    let written;
    // A source that has gone leaves only the view's own text to write.
    let source = job.copy_source.filter(|source| source.exists());
    let written_result = if let Some(source) = source {
        // A file too large to edit: its bytes as they are, never its text encoded again.
        let copy = prepare_large_copy(source, job.path, |_, _| {}, || false)
            .map_err(|e| map_save_error(job, e))?;
        written = std::fs::metadata(source).map_or(0, |m| m.len());
        copy.commit().map_err(|e| map_save_error(job, e))?
    } else {
        let bytes = encoded_bytes(job)?;
        written = bytes.len() as u64;
        hold_while(&job.faults.hold_save, &job.faults.save_is_held);
        run_hook(&job.faults.before_swap);
        let saved = match job.expected {
            Some(stamp) => atomic_write_if_unchanged(job.path, &bytes, stamp),
            None => atomic_write(job.path, &bytes),
        };
        drop(bytes);
        match saved {
            Ok(result) => result,
            Err(CodecError::TargetChanged {
                other_version: Some(version),
                ..
            }) => {
                let (base, stamp) = marks(job.path);
                return Ok(SaveDone {
                    written,
                    base,
                    stamp,
                    other_version: Some(version),
                    backup_kept: None,
                });
            }
            Err(error) => return Err(map_save_error(job, error)),
        }
    };
    run_hook(&job.faults.after_swap);
    let (base, stamp) = marks(job.path);
    let backup_kept = match written_result {
        AtomicWriteResult::Replaced => None,
        AtomicWriteResult::ReplacedBackupKept { backup } => Some(backup),
    };
    Ok(SaveDone {
        written,
        base,
        stamp,
        other_version: None,
        backup_kept,
    })
}

fn marks(path: &Path) -> (Option<FileBase>, Option<FileStamp>) {
    (
        file_base(path).ok().map(|(base, _)| base),
        file_stamp(path).ok().flatten(),
    )
}

fn map_save_error(job: &SaveJob<'_>, error: CodecError) -> SaveTrouble {
    let what = "QuickEdit could not save";
    match error {
        CodecError::TargetChanged { new_text, .. } if !job.save_as || job.expected.is_some() => {
            SaveTrouble::Changed { new_text }
        }
        CodecError::Unencodable(_) => SaveTrouble::DoesNotFit,
        CodecError::ReadOnly(_) if !job.save_as => SaveTrouble::Locked,
        error => {
            let error = DocError::from_codec(what, job.path, error);
            let offer_save_as = matches!(
                error.kind,
                DocErrorKind::AccessDenied
                    | DocErrorKind::NotReplaceable
                    | DocErrorKind::ReadOnlyFile
                    | DocErrorKind::DiskFull
                    | DocErrorKind::FileInUse
                    | DocErrorKind::FileNotFound
            );
            SaveTrouble::Other {
                error,
                offer_save_as,
            }
        }
    }
}

fn move_file_on_volume(from: &Path, to: &Path, replace: bool) -> Result<(), std::io::Error> {
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;
    let (source, target) = (crate::identity::wide(from), crate::identity::wide(to));
    let mut flags = MOVEFILE_WRITE_THROUGH;
    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }
    // SAFETY: both names end in a zero and outlive the call.
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(target.as_ptr()), flags) }
        .map_err(|e| crate::identity::os_error(&e))
}

fn move_error(to: &Path, error: &std::io::Error) -> DocError {
    if matches!(error.raw_os_error(), Some(80 | 183)) {
        return DocError::new(
            DocErrorKind::TargetExists,
            Some(to),
            format!("A file named {} already exists.", to.display()),
        );
    }
    DocError::io("QuickEdit could not move the file to", to, error)
}

fn move_file(from: &Path, to: &Path, replace: bool) -> Result<(), DocError> {
    const NOT_SAME_DEVICE: i32 = 17;
    match move_file_on_volume(from, to, replace) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(NOT_SAME_DEVICE) => {
            move_across_volumes(from, to, replace, &|a, b| std::fs::copy(a, b).map(drop))
        }
        Err(error) => Err(move_error(to, &error)),
    }
}

/// Windows's own copy across volumes writes over the target as it goes, so a full disk could
/// lose the file being replaced. This copies under a temp name first, so the old target stays
/// whole until the new bytes are safe. The source goes last.
fn move_across_volumes(
    from: &Path,
    to: &Path,
    replace: bool,
    copy: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), DocError> {
    if !replace && to.exists() {
        return Err(move_error(to, &std::io::Error::from_raw_os_error(183)));
    }
    let name = to.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp = to.with_file_name(format!(".{name}.qe-move-{}", std::process::id()));
    let placed = copy(from, &temp)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&temp)?
                .sync_all()
        })
        .and_then(|()| move_file_on_volume(&temp, to, replace));
    if let Err(error) = placed {
        remove_quietly(&temp);
        return Err(move_error(to, &error));
    }
    std::fs::remove_file(from).map_err(|error| {
        let mut problem = DocError::io(
            "QuickEdit copied the file but could not remove",
            from,
            &error,
        );
        problem.left_on_disk.push(to.to_path_buf());
        problem
    })
}

fn check_file(item: &StatItem) -> StatResult {
    let StatItem {
        doc,
        epoch,
        path,
        identity,
        links,
        hint,
    } = item;
    let moved = |to: PathBuf| -> Option<(PathBuf, FileBase, Option<FileStamp>)> {
        if is_in_recycle_bin(&to) {
            return None;
        }
        let (base, _) = file_base(&to).ok()?;
        Some((to.clone(), base, file_stamp(&to).ok().flatten()))
    };
    match file_base(path) {
        // A file at this name is this document's file, whatever else has moved: an editor that
        // saves by moving the old file aside leaves the new one here.
        Ok((base, false)) => StatResult {
            doc: *doc,
            epoch: *epoch,
            now: Some((base, file_stamp(path).ok().flatten())),
            moved_to: None,
        },
        _ => {
            let from_hint = hint
                .clone()
                .and_then(moved)
                .filter(|found| identity.is_some_and(|own| found.1.identity == own));
            // The number finds any name of the file, so another hard link answers too. That is
            // no move: a name lost with the count of names falling was a deletion.
            let from_number = || {
                identity
                    .and_then(|own| locate_by_identity(own, path))
                    .and_then(moved)
                    .filter(|found| *links == 0 || found.1.links >= *links)
            };
            StatResult {
                doc: *doc,
                epoch: *epoch,
                now: None,
                moved_to: from_hint.or_else(from_number),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_copy_across_volumes_keeps_the_old_target_and_the_source() {
        let dir = std::env::temp_dir().join(format!("qe-docs-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("from.txt"), dir.join("to.txt"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old target").unwrap();
        let full_disk = |_: &Path, partial: &Path| {
            std::fs::write(partial, b"part")?;
            Err(std::io::Error::from_raw_os_error(112))
        };
        let error = move_across_volumes(&from, &to, true, &full_disk).unwrap_err();
        assert_eq!(error.kind, DocErrorKind::DiskFull);
        assert_eq!(std::fs::read(&to).unwrap(), b"old target");
        assert_eq!(std::fs::read(&from).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no temp stays");
        let plain = |a: &Path, b: &Path| std::fs::copy(a, b).map(drop);
        let exists = move_across_volumes(&from, &to, false, &plain).unwrap_err();
        assert_eq!(exists.kind, DocErrorKind::TargetExists);
        move_across_volumes(&from, &to, true, &plain).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
