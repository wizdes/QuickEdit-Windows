//! The app's state and its one event path.
//!
//! ```text
//! window procedures, workers' wakes, settings, the test channel
//!   -> Incoming, in one queue
//!   -> pump: one item at a time, with `App` borrowed
//!   -> Effects: dialogs, window creation that paints, exit, with no borrow held
//! ```
//!
//! A sent message can arrive during any Windows call. It joins the queue, and the outer pump
//! reaches it next. So `App` is never borrowed twice, and no `RefCell` panic can lose text.
use crate::alerts::{AlertDone, AlertRequest, Alerts};
use crate::args::Args;
use crate::channel;
use crate::docs::DocUi;
use crate::ime::Ime;
use crate::panels::{PanelChoice, PanelJob, PdfRun};
use crate::platform::{Signal, WakeWindow, message_loop};
use crate::settings_host::AppSettingsHost;
use crate::timing::{Timing, process_creation};
use crate::ui::Win;
use qe_api::CommandId;
use qe_docs::handoff::{self, HandoffServer, LaunchRole, PipeNames};
use qe_docs::{DocId, DocumentStore, OpenTarget, StoreConfig, Wake, WindowId, default_root};
use qe_layout::DirectWriteLayout;
use qe_settings::{Presentation, SettingChange, SettingsStore, SettingsWindow};
use qe_shell::{ShellEvent, quit_message_loop};
use qe_ui::SharedClipboard;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MessageBoxW, SW_MAXIMIZE, SendMessageW, ShowWindow, WM_PAINT,
};
use windows::core::HSTRING;

/// One thing for the app to do.
pub enum Incoming {
    Shell(WindowId, ShellEvent),
    Signal(Signal),
    Setting(SettingChange),
    Activated(WindowId),
    AppActivated,
    FrameChanged(WindowId, bool),
    Dropped(WindowId, Vec<PathBuf>),
    EndSession,
    SystemSetting,
    TabDrop { doc: DocId, at: POINT },
    Alert(AlertDone),
    Panel(PanelJob, Option<PanelChoice>),
    Run(WindowId, CommandId),
    Quit,
    Note(String),
    Action(WindowId, qe_api::WidgetId, qe_a11y::WidgetAction),
}

/// Work that may send messages, run a dialog or paint, so it runs with no borrow of `App`.
pub enum Effect {
    ShowWindow(WindowId),
    DestroyWindow(WindowId),
    Alert(AlertRequest),
    Panel(PanelJob),
    OpenSettings,
    PaintHidden(WindowId),
    ImeFocus(WindowId, DocId),
    ImeSync(DocId),
    ImeCancel(DocId),
    ImeClose(DocId),
    ImeCommitThenRun(WindowId, DocId, CommandId),
    Exit,
    Beep,
    Reply(String),
    Picture {
        window: WindowId,
        path: PathBuf,
        theme: String,
    },
    FinishSmoke,
    RunBench,
    ImeSyncAll,
    ImeCommitThenQuit,
    ReportWidgets(WindowId),
    ReportInput(WindowId),
    Announce(WindowId, String),
    DropQuestion(qe_docs::QuestionId),
    ChannelAlert(String, Option<String>),
    ChannelSettingsOpen,
    ChannelPumps,
}

pub struct Shared {
    pub state: RefCell<App>,
    pub queue: RefCell<VecDeque<Incoming>>,
    pub settings: SettingsStore,
    pub settings_window: RefCell<Option<SettingsWindow>>,
    pub ime: Rc<Ime>,
    pub alerts: Alerts,
    pub timing: Rc<Timing>,
    pub missed_paint: RefCell<Vec<WindowId>>,
    pub ime_after_frame: Cell<Option<(WindowId, DocId)>>,
    pub pumps: Cell<u64>,
    pub hidden: bool,
    pub channel: bool,
    pub measure: Arc<DirectWriteLayout>,
    pub process_start: Instant,
    pub timings: bool,
}

pub struct App {
    pub store: DocumentStore,
    pub windows: BTreeMap<WindowId, Win>,
    pub docs: HashMap<DocId, DocUi>,
    pub effects: Vec<Effect>,
    pub settings: SettingsStore,
    pub measure: Arc<DirectWriteLayout>,
    pub wake: Wake,
    pub wake_window: Rc<WakeWindow>,
    pub server: Option<HandoffServer>,
    pub restored: bool,
    pub ready_to_quit: bool,
    pub hidden: bool,
    pub stub_spell: bool,
    pub clipboard: Option<SharedClipboard>,
    pub timing: Rc<Timing>,
    pub ime: Rc<Ime>,
    pub me: Weak<Shared>,
    pub deadline_set: Option<Instant>,
    /// Every note shown so far, newest last, for the test channel.
    pub notes: Vec<String>,
    /// Notes that came before there was a window to show them in.
    pub waiting_notes: Vec<String>,
    /// File dialogs in tests wait here for the test channel.
    pub test_panels: VecDeque<PanelJob>,
    pub channel_on: bool,
    pub pdf_runs: Vec<PdfRun>,
    /// A Save dialog that waits for `EncodingFit`.
    pub panels_waiting: Vec<qe_docs::SavePanelRequest>,
    pub smoke: bool,
    pub smoke_done: bool,
    pub bench: Option<crate::bench::Bench>,
    pub tab_drag: Rc<Cell<Option<(WindowId, DocId)>>>,
    pub shown_questions: std::collections::HashSet<qe_docs::QuestionId>,
    /// Documents with an open question.
    pub asking: std::collections::HashSet<DocId>,
}

/// A hidden run with no folder of its own must not touch the user's session or hand files to
/// the user's running copy, so it gets a folder that goes when it ends.
fn data_root(args: &mut Args) -> Result<(PathBuf, bool), String> {
    match &args.root {
        Some(root) => Ok((root.clone(), false)),
        None if args.hidden => {
            let root =
                std::env::temp_dir().join(format!("QuickEdit-hidden-{}", std::process::id()));
            args.root = Some(root.clone());
            Ok((root, true))
        }
        None => Ok((default_root()?, false)),
    }
}

/// Shows an error when there is nothing else to show it with. Hidden runs print it only.
pub fn fatal_box(hidden: bool, text: &str) {
    eprintln!("{text}");
    if !hidden {
        // SAFETY: both strings live through the call, and this thread has a message queue.
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(text),
                &HSTRING::from("QuickEdit"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

pub fn run() -> i32 {
    let args = match Args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(problem) => {
            eprintln!("{problem}");
            return 2;
        }
    };
    if args.help {
        println!("{}", Args::help_text());
        return 0;
    }
    let hidden = args.hidden;
    match start(args) {
        Ok(code) => code,
        Err(problem) => {
            fatal_box(hidden, &format!("QuickEdit could not start: {problem}"));
            1
        }
    }
}

fn start(mut args: Args) -> Result<i32, String> {
    let process_start = process_creation().unwrap_or_else(|problem| {
        if args.timings {
            eprintln!("{problem}. The times below count from the start of main.");
        }
        Instant::now()
    });
    let (root, throwaway) = data_root(&mut args)?;
    let wake_window = Rc::new(WakeWindow::create()?);
    let wake: Wake = wake_window.waker();

    // The handoff goes first: a second launch ends here, before it loads anything else.
    let mut names = PipeNames::for_this_process().map_err(|e| e.to_string())?;
    if let Some(given) = &args.root {
        // A test's own folder means its own instance.
        let tag: u64 = given
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
        names.base = format!("{}-{tag:016x}", names.base);
    }
    let files = handoff::absolute_paths(&args.files);
    let role = handoff::start(&names, &files, wake.clone());
    let (server, alone_problem) = match role {
        LaunchRole::HandedOff => {
            if args.timings {
                eprintln!(
                    "second launch: handed off after {:.3} ms from process creation",
                    process_start.elapsed().as_secs_f64() * 1000.0
                );
            }
            return Ok(0);
        }
        LaunchRole::Primary(server) => (Some(server), None),
        LaunchRole::Alone { error } => (None, Some(error.to_string())),
    };

    std::fs::create_dir_all(&root)
        .map_err(|e| format!("the folder {} could not be made: {e}", root.display()))?;
    let settings = SettingsStore::open(root.join("settings.txt"));
    let mut config = StoreConfig::from_settings(&settings, wake.clone());
    config.root = root.clone();
    let mut store = DocumentStore::new(config);
    let mut first_problem = None;
    store.begin_restore();
    // The store holds these until the restore ends. Then they join the last used window.
    if !files.is_empty()
        && let Err(e) = store.open_paths(&files, OpenTarget::LastUsedWindow, None)
    {
        first_problem = Some(e.message);
    }

    let measure = Arc::new(DirectWriteLayout::new().map_err(|e| e.to_string())?);
    let timing = Rc::new(Timing::default());
    let ime = Rc::new(Ime::default());
    let clipboard: Option<SharedClipboard> = args.channel.then(|| {
        let memory: SharedClipboard = qe_ui::MemoryClipboard::shared();
        memory
    });
    let channel_on = args.channel;
    let shared = Rc::new_cyclic(|me: &Weak<Shared>| Shared {
        state: RefCell::new(App {
            store,
            windows: BTreeMap::new(),
            docs: HashMap::new(),
            effects: Vec::new(),
            settings: settings.clone(),
            measure: measure.clone(),
            wake: wake.clone(),
            wake_window: wake_window.clone(),
            server,
            restored: false,
            ready_to_quit: false,
            hidden: args.hidden,
            stub_spell: args.stub_spell,
            clipboard,
            timing: timing.clone(),
            ime: ime.clone(),
            me: me.clone(),
            deadline_set: None,
            notes: Vec::new(),
            waiting_notes: Vec::new(),
            test_panels: VecDeque::new(),
            channel_on,
            pdf_runs: Vec::new(),
            panels_waiting: Vec::new(),
            smoke: args.smoke,
            smoke_done: false,
            bench: (args.bench_keys > 0)
                .then(|| crate::bench::Bench::new(args.bench_keys, args.bench_middle)),
            tab_drag: Rc::new(Cell::new(None)),
            shown_questions: std::collections::HashSet::new(),
            asking: std::collections::HashSet::new(),
        }),
        queue: RefCell::new(VecDeque::new()),
        settings: settings.clone(),
        settings_window: RefCell::new(None),
        ime: ime.clone(),
        alerts: Alerts::default(),
        timing: timing.clone(),
        missed_paint: RefCell::new(Vec::new()),
        ime_after_frame: Cell::new(None),
        pumps: Cell::new(0),
        hidden: args.hidden,
        channel: args.channel,
        measure: measure.clone(),
        process_start,
        timings: args.timings,
    });

    // Wakes from any thread reach one handler.
    let weak = Rc::downgrade(&shared);
    wake_window.set_handler(Rc::new(move |signal| {
        if let Some(shared) = weak.upgrade() {
            push(&shared, Incoming::Signal(signal));
        }
    }));

    // Settings: the first problems, the listener, and writes that end while Settings is closed.
    if let Some(problem) = settings.load_problem() {
        queue_note(&shared, problem);
    }
    if let Some(problem) = alone_problem.or(first_problem) {
        queue_note(&shared, problem);
    }
    let weak = Rc::downgrade(&shared);
    settings.subscribe(move |change| {
        if let Some(shared) = weak.upgrade() {
            push(&shared, Incoming::Setting(change.clone()));
        }
    });
    let write_wake = wake.clone();
    settings.add_write_wake(Arc::new(move || write_wake()));

    if args.channel {
        channel::start(&shared);
    }
    // The session read has probably ended by now, and its wake is in the queue.
    pump(&shared);
    let code = message_loop(&shared.ime.thread)?;
    if shared.timings {
        shared.timing.report("this run", process_start);
    }
    if throwaway {
        drop(shared);
        if let Err(e) = std::fs::remove_dir_all(&root) {
            eprintln!("{} stayed behind: {e}", root.display());
        }
    }
    Ok(code)
}

/// Adds an item and works through the queue, unless the queue's owner is already at work.
pub fn push(shared: &Rc<Shared>, item: Incoming) {
    shared.queue.borrow_mut().push_back(item);
    pump(shared);
}
pub fn queue_note(shared: &Rc<Shared>, text: String) {
    push(shared, Incoming::Note(text));
}

pub fn pump(shared: &Rc<Shared>) {
    shared.pumps.set(shared.pumps.get() + 1);
    loop {
        let Ok(mut app) = shared.state.try_borrow_mut() else {
            // The owner of this borrow is lower on this stack, and it goes on to the queue.
            return;
        };
        let item = shared.queue.borrow_mut().pop_front();
        if let Some(item) = item {
            app.handle(item);
        }
        app.after_event();
        let effects = std::mem::take(&mut app.effects);
        drop(app);
        if effects.is_empty() && shared.queue.borrow().is_empty() {
            break;
        }
        for effect in effects {
            run_effect(shared, effect);
        }
    }
    let missed = std::mem::take(&mut *shared.missed_paint.borrow_mut());
    if !missed.is_empty()
        && let Ok(app) = shared.state.try_borrow()
    {
        for window in missed {
            if let Some(win) = app.windows.get(&window)
                && let Err(e) = win.handle.invalidate_content()
            {
                eprintln!("repainting after a busy moment: {e}");
            }
        }
    }
}

fn paint_hidden(shared: &Rc<Shared>, window: WindowId) {
    let hwnd = shared
        .state
        .try_borrow()
        .ok()
        .and_then(|app| app.windows.get(&window).map(|win| win.hwnd));
    if let Some(hwnd) = hwnd {
        // SAFETY: a window of this thread. A hidden window gets no WM_PAINT from Windows.
        unsafe {
            SendMessageW(hwnd, WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
        }
    }
}

fn window_slot(shared: &Rc<Shared>, window: WindowId) -> Option<Rc<qe_a11y::UiaSlot>> {
    let app = shared.state.try_borrow().ok()?;
    Some(app.windows.get(&window)?.slot.clone())
}

fn run_effect(shared: &Rc<Shared>, effect: Effect) {
    match effect {
        Effect::ShowWindow(window) => {
            let found = shared.state.try_borrow().ok().and_then(|app| {
                app.windows
                    .get(&window)
                    .map(|win| (win.handle.clone(), win.show_state))
            });
            if let Some((handle, state)) = found {
                if shared.hidden {
                    paint_hidden(shared, window);
                } else if let Err(e) = handle.show() {
                    queue_note(shared, format!("A window could not show: {e}"));
                } else if state == qe_docs::ShowState::Maximized
                    && let Ok(hwnd) = handle.hwnd()
                {
                    // SAFETY: a window of this thread.
                    let _ = unsafe { ShowWindow(HWND(hwnd), SW_MAXIMIZE) };
                } else if state == qe_docs::ShowState::FullScreen
                    && let Err(e) = handle.toggle_full_screen()
                {
                    queue_note(shared, format!("Full screen could not come back: {e}"));
                }
                if let Some((window, doc)) = shared.ime_after_frame.take() {
                    shared.ime.focus(shared, window, doc);
                }
            }
        }
        Effect::DestroyWindow(window) => {
            let win = shared
                .state
                .try_borrow_mut()
                .ok()
                .and_then(|mut app| app.windows.remove(&window));
            if let Some(win) = win {
                win.close();
            }
        }
        Effect::Alert(request) => shared.alerts.request(shared, request),
        Effect::Panel(job) => crate::panels::show(shared, job),
        Effect::OpenSettings => open_settings(shared),
        Effect::PaintHidden(window) => paint_hidden(shared, window),
        Effect::ImeFocus(window, doc) => {
            // The first frame comes before TSF loads. Showing the window ends with that frame.
            if shared.timing.first_frame.get().is_none() {
                shared.ime_after_frame.set(Some((window, doc)));
            } else {
                shared.ime.focus(shared, window, doc);
            }
        }
        Effect::ImeSync(doc) => shared.ime.sync(doc),
        Effect::ImeCancel(doc) => shared.ime.cancel(doc),
        Effect::ImeClose(doc) => shared.ime.close(doc),
        Effect::ImeCommitThenRun(window, doc, command) => {
            shared.ime.commit(doc);
            shared
                .queue
                .borrow_mut()
                .push_back(Incoming::Run(window, command));
        }
        Effect::Exit => exit(shared),
        Effect::Beep => {
            // SAFETY: a plain call. A failed beep has nothing to report to.
            let _ = unsafe { windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_OK) };
        }
        Effect::Reply(text) => channel::reply(&text),
        Effect::Picture {
            window,
            path,
            theme,
        } => channel::picture(shared, window, &path, &theme),
        Effect::RunBench => crate::bench::run(shared),
        Effect::ImeCommitThenQuit => {
            shared.ime.commit_all();
            shared.queue.borrow_mut().push_back(Incoming::Quit);
        }
        Effect::ReportWidgets(window) => {
            if let Some(slot) = window_slot(shared, window)
                && let Err(e) = slot.widgets_changed()
            {
                queue_note(
                    shared,
                    format!("A screen reader could not be told of a change: {e}"),
                );
            }
        }
        Effect::ReportInput(window) => {
            if let Some(slot) = window_slot(shared, window)
                && let Err(e) = slot.after_input()
            {
                queue_note(
                    shared,
                    format!("A screen reader could not be told of a change: {e}"),
                );
            }
        }
        Effect::Announce(window, text) => {
            if let Some(slot) = window_slot(shared, window)
                && let Err(e) = slot.announce(&text)
            {
                queue_note(
                    shared,
                    format!("A screen reader could not be told \"{text}\": {e}"),
                );
            }
        }
        Effect::DropQuestion(id) => shared.alerts.drop_question(shared, id),
        Effect::ImeSyncAll => shared.ime.sync_all(),
        Effect::ChannelAlert(verb, word) => channel::alert_verb(shared, &verb, word.as_deref()),
        Effect::ChannelSettingsOpen => {
            let open = shared
                .settings_window
                .borrow()
                .as_ref()
                .is_some_and(SettingsWindow::is_open);
            channel::reply(&open.to_string());
        }
        Effect::ChannelPumps => channel::reply(&format!(
            "{} {}",
            shared.pumps.get(),
            shared.timing.presents.get()
        )),
        Effect::FinishSmoke => {
            println!("hidden smoke passed");
            push(shared, Incoming::Note(String::new()));
        }
    }
}

fn open_settings(shared: &Rc<Shared>) {
    let mut slot = shared.settings_window.borrow_mut();
    let window = slot.get_or_insert_with(|| {
        SettingsWindow::new(
            shared.settings.clone(),
            Rc::new(AppSettingsHost {
                shared: Rc::downgrade(shared),
            }),
        )
    });
    let presentation = if shared.hidden {
        Presentation::Hidden
    } else {
        Presentation::Shown
    };
    let opened = window.open_as(presentation);
    drop(slot);
    if let Err(problem) = opened {
        queue_note(shared, format!("Settings could not open: {problem}"));
    }
}

/// The end of the app. Settings closes first, because closing commits a typed size.
fn exit(shared: &Rc<Shared>) {
    if let Some(mut window) = shared.settings_window.borrow_mut().take() {
        window.close();
    }
    if let Err(problem) = shared.settings.flush() {
        fatal_box(shared.hidden, &problem);
    } else if let Some(problem) = shared.settings.write_error() {
        fatal_box(shared.hidden, &problem);
    }
    shared.ime.close_all();
    quit_message_loop(0);
}
