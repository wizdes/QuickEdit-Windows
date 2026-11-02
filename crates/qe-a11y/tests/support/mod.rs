//! A hidden window with a real `EditorView` and a host written the way the README tells
//! the shell to write it. Each test runs on its own thread, so each has its own windows.
#![allow(dead_code)]
use qe_a11y::{Accessibility, UiThreadOnly, UiaHost, UiaSlot, WidgetAction};
use qe_api::{
    Dip, Rect, View, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
    testing::FixedTextMeasurer,
};
use qe_editor::EditorView;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::ThreadId,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{Accessibility::*, WindowsAndMessaging::*},
    },
    core::{BSTR, IUnknown, Interface, PCWSTR, w},
};

#[derive(Default)]
pub struct Counts {
    pub editor_calls: AtomicUsize,
    pub widget_calls: AtomicUsize,
    pub made: AtomicUsize,
    pub actions: Mutex<Vec<(WidgetId, WidgetAction)>>,
    /// Posted actions that failed after their call had said yes, with the error code.
    pub failed: Mutex<Vec<(WidgetId, WidgetAction, i32)>>,
    /// The thread each `Ui` ended on. A leak leaves this empty.
    pub ended_on: Mutex<Vec<ThreadId>>,
}

pub type OnAction = Box<dyn Fn(WidgetId, &WidgetAction)>;

pub struct Ui {
    pub view: RefCell<EditorView>,
    pub tree: RefCell<Widget>,
    pub document: RefCell<Option<Widget>>,
    pub view_id: Cell<Option<u64>>,
    pub font: RefCell<Option<(String, f64)>>,
    pub dpi: Cell<Option<u32>>,
    pub checkable: RefCell<Vec<WidgetId>>,
    /// What an action does, as the shell's action handler would.
    pub on_action: RefCell<Option<Rc<OnAction>>>,
    pub counts: Arc<Counts>,
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.counts
            .ended_on
            .lock()
            .expect("ended_on lock")
            .push(std::thread::current().id());
    }
}

pub struct TestHost {
    pub ui: UiThreadOnly<Rc<Ui>>,
    pub counts: Arc<Counts>,
}

impl UiaHost for TestHost {
    fn widgets(&self) -> Option<Widget> {
        self.counts.widget_calls.fetch_add(1, Ordering::SeqCst);
        Some(self.ui.get()?.tree.try_borrow().ok()?.clone())
    }
    fn action(&self, id: WidgetId, action: WidgetAction) -> windows::core::Result<()> {
        self.counts
            .actions
            .lock()
            .expect("actions lock")
            .push((id, action.clone()));
        if let Some(ui) = self.ui.get() {
            let handler = ui.on_action.borrow().clone();
            if let Some(handler) = handler {
                handler(id, &action);
            }
        }
        Ok(())
    }
    fn document_widget(&self) -> Option<Widget> {
        self.ui.get()?.document.try_borrow().ok()?.clone()
    }
    fn view_id(&self) -> Option<u64> {
        self.ui.get()?.view_id.get()
    }
    fn with_editor(&self, body: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool {
        self.counts.editor_calls.fetch_add(1, Ordering::SeqCst);
        let Some(ui) = self.ui.get() else {
            return false;
        };
        let Ok(mut view) = ui.view.try_borrow_mut() else {
            return false;
        };
        let mut ctx = ViewContext::default();
        body(&mut view, &mut ctx);
        true
    }
    fn font(&self) -> Option<(String, f64)> {
        self.ui.get()?.font.try_borrow().ok()?.clone()
    }
    fn dpi(&self) -> Option<u32> {
        self.ui.get()?.dpi.get()
    }
    fn is_checkable(&self, id: WidgetId) -> bool {
        self.ui
            .get()
            .is_some_and(|ui| ui.checkable.try_borrow().is_ok_and(|ids| ids.contains(&id)))
    }
    fn action_failed(&self, id: WidgetId, action: &WidgetAction, error: &windows::core::Error) {
        self.counts
            .failed
            .lock()
            .expect("failed lock")
            .push((id, action.clone(), error.code().0));
    }
}

pub fn widget(id: u64, role: WidgetRole, name: &str, x: f32, children: Vec<Widget>) -> Widget {
    Widget {
        id: WidgetId(id),
        bounds: Rect {
            x: Dip(x),
            y: Dip(0.0),
            width: Dip(40.0),
            height: Dip(20.0),
        },
        role,
        name: name.into(),
        value: None,
        states: WidgetStates {
            enabled: true,
            ..Default::default()
        },
        selection: None,
        range_value: None,
        description: None,
        children,
    }
}

pub fn document_widget() -> Widget {
    let mut document = widget(1000, WidgetRole::Group, "Untitled", 0.0, Vec::new());
    document.bounds = Rect {
        x: Dip(0.0),
        y: Dip(40.0),
        width: Dip(400.0),
        height: Dip(300.0),
    };
    document.states.focused = true;
    document
}

pub fn root_with(children: Vec<Widget>) -> Widget {
    let mut root = widget(1, WidgetRole::Group, "QuickEdit", 0.0, children);
    root.bounds = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(800.0),
        height: Dip(600.0),
    };
    root
}

type HostEntry = (Arc<dyn UiaHost>, Arc<Counts>);

thread_local! {
    static SLOTS: RefCell<HashMap<isize, Rc<UiaSlot>>> = RefCell::new(HashMap::new());
    static HOSTS: RefCell<HashMap<isize, HostEntry>> = RefCell::new(HashMap::new());
}

/// The README's window procedure: the slot's call first, then the shell's own messages.
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let key = hwnd.0 as isize;
    let slot = SLOTS.with(|slots| slots.borrow_mut().entry(key).or_default().clone());
    let answer = slot.on_window_message(hwnd, msg, w, l, || {
        HOSTS.with(|hosts| {
            let hosts = hosts.borrow();
            let (host, counts) = hosts.get(&key).expect("a host for this window");
            counts.made.fetch_add(1, Ordering::SeqCst);
            host.clone()
        })
    });
    if let Some(answer) = answer {
        return answer;
    }
    if msg == WM_DESTROY {
        slot.shutdown().expect("shutdown in WM_DESTROY");
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

static REAL_MOVE_LIMIT: AtomicBool = AtomicBool::new(false);

/// Keeps the real limit on moves. Other test windows widen it, because they check exact counts,
/// and a pause on a busy machine would stop a move short. `tests/move_limit.rs` tests the limit.
pub fn keep_the_real_move_limit() {
    REAL_MOVE_LIMIT.store(true, Ordering::Relaxed);
    qe_a11y::text::set_move_limit_for_tests(qe_a11y::text::MOVE_BUDGET);
}

pub struct TestWindow {
    pub hwnd: HWND,
    pub ui: Rc<Ui>,
    pub counts: Arc<Counts>,
    pub host: Arc<dyn UiaHost>,
}

impl TestWindow {
    pub fn new(view: EditorView) -> Self {
        Self::with_tree(view, root_with(Vec::new()))
    }
    pub fn from_text(text: &str) -> Self {
        Self::new(EditorView::from_text(text).expect("a view"))
    }
    pub fn with_tree(view: EditorView, tree: Widget) -> Self {
        if !REAL_MOVE_LIMIT.load(Ordering::Relaxed) {
            qe_a11y::text::set_move_limit_for_tests(Duration::from_secs(5));
        }
        static CLASS: Once = Once::new();
        CLASS.call_once(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                lpszClassName: w!("QeA11yTestWindow"),
                ..Default::default()
            };
            assert_ne!(
                unsafe { RegisterClassW(&class) },
                0,
                "RegisterClassW failed"
            );
        });
        // No WS_VISIBLE and no ShowWindow, so this never appears on screen.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("QeA11yTestWindow"),
                w!("QuickEdit test"),
                // A popup has no title bar, so UIA adds no Close or Minimize buttons to the tree.
                WS_POPUP,
                10,
                10,
                800,
                600,
                None,
                None,
                None,
                None,
            )
        }
        .expect("CreateWindowExW failed");
        let counts = Arc::new(Counts::default());
        let ui = Rc::new(Ui {
            view: RefCell::new(view),
            tree: RefCell::new(tree),
            document: RefCell::new(Some(document_widget())),
            view_id: Cell::new(Some(7)),
            font: RefCell::new(Some(("Cascadia Mono".into(), 10.0))),
            dpi: Cell::new(None),
            checkable: RefCell::new(Vec::new()),
            on_action: RefCell::new(None),
            counts: counts.clone(),
        });
        let host: Arc<dyn UiaHost> = Arc::new(TestHost {
            ui: UiThreadOnly::new(ui.clone()),
            counts: counts.clone(),
        });
        HOSTS.with(|hosts| {
            hosts
                .borrow_mut()
                .insert(hwnd.0 as isize, (host.clone(), counts.clone()));
        });
        Self {
            hwnd,
            ui,
            counts,
            host,
        }
    }
    /// Makes a provider apart from the window's slot, for tests that call COM directly.
    pub fn accessibility(&self) -> Accessibility {
        Accessibility::new(self.hwnd, self.host.clone())
    }
    pub fn lay_out(&self, width: f32, height: f32) {
        let measure = FixedTextMeasurer {
            char_width: Dip(8.0),
            height: Dip(16.0),
            baseline: Dip(12.0),
        };
        let bounds = Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(width),
            height: Dip(height),
        };
        self.ui.view.borrow_mut().layout(bounds, &measure);
        self.settle();
    }
    /// Waits for the layout workers, without any new layout call.
    pub fn settle(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut ctx = ViewContext::default();
            let mut view = self.ui.view.borrow_mut();
            view.poll_layout(&mut ctx);
            if view.visible_range().is_ok() && !view.pending_layout() {
                return;
            }
            assert!(Instant::now() < deadline, "layout did not settle");
            drop(view);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn slot(&self) -> Rc<UiaSlot> {
        SLOTS.with(|slots| {
            slots
                .borrow_mut()
                .entry(self.hwnd.0 as isize)
                .or_default()
                .clone()
        })
    }
    pub fn with_slot<T>(&self, body: impl FnOnce(&Accessibility) -> T) -> Option<T> {
        self.slot().current().map(|ui| body(&ui))
    }
    /// Destroys the window, so WM_DESTROY shuts down UIA for it, as in the shell.
    pub fn close(&self) {
        let key = self.hwnd.0 as isize;
        HOSTS.with(|hosts| hosts.borrow_mut().remove(&key));
        unsafe { DestroyWindow(self.hwnd) }.expect("DestroyWindow failed");
        SLOTS.with(|slots| slots.borrow_mut().remove(&key));
    }
    /// Runs the window's messages until `done` says stop.
    pub fn pump_until(&self, what: &str, seconds: u64, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            pump();
            if done() {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// What the host's actions have been told so far, after posted actions have run.
    pub fn actions_after_pump(&self) -> Vec<(WidgetId, WidgetAction)> {
        pump();
        self.counts.actions.lock().expect("actions").clone()
    }
    /// FindText as UIA calls it: from another thread, with this window's messages running.
    /// `Ok(None)` is the real null.
    pub fn find_off_thread(
        &self,
        range: &ITextRangeProvider,
        query: &str,
        backward: bool,
        ignore_case: bool,
    ) -> windows::core::Result<Option<ITextRangeProvider>> {
        let range = SendIt(range.clone());
        let query = query.to_owned();
        let (say, heard) = mpsc::channel();
        std::thread::spawn(move || {
            let range = range;
            let answer = unsafe {
                range
                    .0
                    .FindText(&BSTR::from(query.as_str()), backward, ignore_case)
            };
            say.send(SendIt(answer)).expect("report");
        });
        let mut got = None;
        self.pump_until("FindText on another thread", 120, || {
            if let Ok(answer) = heard.try_recv() {
                got = Some(answer);
            }
            got.is_some()
        });
        match got.expect("an answer").0 {
            Ok(found) => Ok(Some(found)),
            Err(error) if error.code().0 == 0 => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Moves a value to another thread in a test.
pub struct SendIt<T>(pub T);
// SAFETY: tests move this crate's COM objects, whose counts are atomic and whose calls move to
// the window's thread, and their answers.
unsafe impl<T> Send for SendIt<T> {}

pub fn pump() {
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        let _ = unsafe { TranslateMessage(&message) };
        unsafe { DispatchMessageW(&message) };
    }
}

pub fn text_of(simple: &IRawElementProviderSimple) -> ITextProvider2 {
    unsafe { simple.GetPatternProvider(UIA_TextPatternId) }
        .expect("the text pattern")
        .cast()
        .expect("ITextProvider2")
}

pub fn document_range(simple: &IRawElementProviderSimple) -> ITextRangeProvider {
    unsafe { text_of(simple).DocumentRange() }.expect("DocumentRange")
}

/// Assert a call gave S_OK with a null result, which the wrapper reports as an empty error.
pub fn assert_null<T>(answer: windows::core::Result<T>, what: &str) {
    match answer {
        Ok(_) => panic!("{what}: expected a null result, got an object"),
        Err(error) => assert_eq!(
            error.code().0,
            0,
            "{what}: expected S_OK with null, got {error}"
        ),
    }
}

pub const GONE: i32 = UIA_E_ELEMENTNOTAVAILABLE as i32;
pub const NOT_YET: i32 = UIA_E_TIMEOUT as i32;

pub fn assert_code<T>(answer: windows::core::Result<T>, code: i32, what: &str) {
    match answer {
        Ok(_) => panic!("{what}: expected error 0x{code:08x}, got success"),
        Err(error) => assert_eq!(error.code().0, code, "{what}: wrong error: {error}"),
    }
}

pub fn unknown_is_not_supported(value: &IUnknown) -> bool {
    let reserved = unsafe { UiaGetReservedNotSupportedValue() }.expect("the reserved value");
    value.as_raw() == reserved.as_raw()
}

pub fn no_name() -> PCWSTR {
    PCWSTR::null()
}

/// Position of a range's ends, through the COM calls a client has.
pub fn ends(range: &ITextRangeProvider, doc: &ITextRangeProvider) -> (usize, usize) {
    let at = |end| unsafe {
        range
            .CompareEndpoints(end, doc, TextPatternRangeEndpoint_Start)
            .expect("CompareEndpoints") as usize
    };
    (
        at(TextPatternRangeEndpoint_Start),
        at(TextPatternRangeEndpoint_End),
    )
}

/// A range at these positions, moved there with the calls a client has.
pub fn range_at(
    simple: &IRawElementProviderSimple,
    start: usize,
    end: usize,
) -> ITextRangeProvider {
    let doc = document_range(simple);
    let range = unsafe { doc.Clone() }.expect("Clone");
    unsafe {
        range
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &doc,
                TextPatternRangeEndpoint_Start,
            )
            .expect("collapse");
    }
    // Character steps would stop on clusters. This helper needs exact units, so it uses a
    // find-free walk: a caret range is placed by selecting through the editor by the caller
    // when a position is inside a cluster.
    let mut at = 0;
    while at < end {
        let moved = unsafe {
            range.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1)
        }
        .expect("move end");
        assert_eq!(moved, 1, "ran out of text walking to {end}");
        at = ends(&range, &doc).1;
    }
    assert_eq!(at, end, "{end} is not a character stop");
    let walk = unsafe { range.Clone() }.expect("Clone");
    unsafe {
        walk.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &doc,
            TextPatternRangeEndpoint_Start,
        )
    }
    .expect("collapse");
    let mut at = 0;
    while at < start {
        let moved =
            unsafe { walk.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1) }
                .expect("move end");
        assert_eq!(moved, 1, "ran out of text walking to {start}");
        at = ends(&walk, &doc).1;
    }
    assert_eq!(at, start, "{start} is not a character stop");
    unsafe {
        range.MoveEndpointByRange(
            TextPatternRangeEndpoint_Start,
            &walk,
            TextPatternRangeEndpoint_End,
        )
    }
    .expect("set start");
    range
}

/// Several test threads with UIA clients at once in one process make UIA fail with E_FAIL or an
/// empty tree. The app has one UI thread, so these tests take turns.
pub fn one_ui_thread() -> std::sync::MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
