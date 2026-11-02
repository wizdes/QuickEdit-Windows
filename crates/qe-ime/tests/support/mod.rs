//! Shared by the test files. Each test binary uses only some of this.
#![allow(dead_code)]
use qe_api::{Dip, Rect, View, ViewRequest};
use qe_editor::EditorView;
use qe_ime::{ImeHost, SharedViewHost, TextStoreHandle, TsfThread};
use qe_layout::DirectWriteLayout;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, POINT, RECT},
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            TextServices::{
                ITextStoreACP, ITextStoreACPSink, ITextStoreACPSink_Impl, TEXT_STORE_LOCK_FLAGS,
                TEXT_STORE_TEXT_CHANGE_FLAGS, TS_LF_READ, TS_LF_READWRITE, TS_LF_SYNC,
                TS_TEXTCHANGE, TsLayoutCode,
            },
            WindowsAndMessaging::{
                CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW,
                WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{ComObject, GUID, Interface, PCWSTR, Result, implement, w},
};

pub fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

pub fn area(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}

pub fn settle(view: &mut EditorView) {
    let mut ctx = qe_api::ViewContext::default();
    view.poll_layout(&mut ctx);
    for _ in 0..5000 {
        if !view.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
        view.poll_layout(&mut ctx);
    }
    panic!("the view did not settle");
}

pub fn laid_out(text: &str) -> EditorView {
    let mut view = EditorView::from_text(text).unwrap();
    view.layout(area(600.0, 300.0), &DirectWriteLayout::new().unwrap());
    settle(&mut view);
    view
}

/// A hidden window: it is never shown.
pub struct HiddenWindow(pub HWND);
impl HiddenWindow {
    pub fn new() -> Self {
        unsafe extern "system" fn proc(
            hwnd: HWND,
            message: u32,
            w: windows::Win32::Foundation::WPARAM,
            l: windows::Win32::Foundation::LPARAM,
        ) -> windows::Win32::Foundation::LRESULT {
            unsafe { DefWindowProcW(hwnd, message, w, l) }
        }
        unsafe {
            let instance: HMODULE = GetModuleHandleW(PCWSTR::null()).unwrap();
            let class = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance.into(),
                lpszClassName: w!("QeImeTestHidden"),
                ..Default::default()
            };
            // A second call on another test's thread finds the class there, which is fine.
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("QeImeTestHidden"),
                w!("hidden"),
                WS_OVERLAPPEDWINDOW,
                100,
                200,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            Self(hwnd)
        }
    }
}
impl Drop for HiddenWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) }.unwrap();
    }
}

/// A view, a host for it, and what the host was told.
pub struct Fixture {
    pub view: Rc<RefCell<EditorView>>,
    pub window: HiddenWindow,
    pub requests: Rc<RefCell<Vec<ViewRequest>>>,
    pub problems: Rc<RefCell<Vec<String>>>,
    /// How many times the store asked for a `sync` because the view was busy.
    pub busy: Rc<Cell<u32>>,
    /// What the host says about the window, which these tests never really show.
    pub shown: Rc<Cell<bool>>,
    pub host: Rc<SharedViewHost>,
}
impl Fixture {
    pub fn new(view: EditorView) -> Self {
        sta();
        let view = Rc::new(RefCell::new(view));
        let window = HiddenWindow::new();
        let requests: Rc<RefCell<Vec<ViewRequest>>> = Rc::default();
        let problems: Rc<RefCell<Vec<String>>> = Rc::default();
        let busy: Rc<Cell<u32>> = Rc::default();
        let shown = Rc::new(Cell::new(true));
        let host = Rc::new(SharedViewHost {
            view: view.clone(),
            hwnd: window.0,
            dpi: Rc::new(Cell::new(96)),
            on_requests: {
                let requests = requests.clone();
                Box::new(move |new| requests.borrow_mut().extend(new))
            },
            on_problem: {
                let problems = problems.clone();
                Box::new(move |new| problems.borrow_mut().push(new))
            },
            on_sync_needed: {
                let busy = busy.clone();
                Box::new(move || busy.set(busy.get() + 1))
            },
            shown: {
                let shown = shown.clone();
                Box::new(move || shown.get())
            },
        });
        Self {
            view,
            window,
            requests,
            problems,
            busy,
            shown,
            host,
        }
    }
    pub fn text(&self) -> String {
        self.view.borrow().text().unwrap()
    }
    pub fn no_problems(&self) {
        assert_eq!(self.problems.borrow().as_slice(), &[] as &[String]);
    }
}

/// A host whose view the test can make busy, as a view in use further up the stack is.
pub struct GateHost {
    pub inner: Rc<SharedViewHost>,
    pub shut: Rc<Cell<bool>>,
}
impl ImeHost for GateHost {
    fn with_view(&self, f: &mut dyn FnMut(&mut EditorView, &mut qe_api::ViewContext)) -> bool {
        !self.shut.get() && self.inner.with_view(f)
    }
    fn to_screen(&self, rect: Rect) -> Option<RECT> {
        self.inner.to_screen(rect)
    }
    fn to_window(&self, point: POINT) -> Option<qe_api::Point> {
        self.inner.to_window(point)
    }
    fn hwnd(&self) -> HWND {
        self.inner.hwnd()
    }
    fn problem(&self, message: String) {
        self.inner.problem(message);
    }
    fn sync_needed(&self) {
        self.inner.sync_needed();
    }
    fn is_shown(&self) -> bool {
        self.inner.is_shown()
    }
}

/// Starts TSF. If this account cannot, these tests cannot say anything, so they stop and
/// say why. A review saw `0x80004005` in an account with no signed-in desktop session.
pub fn start_tsf() -> TsfThread {
    TsfThread::start().unwrap_or_else(|e| {
        panic!(
            "TSF did not start, so this test proves nothing here: {e}. This test needs an \
             account with a signed-in desktop session."
        )
    })
}

pub fn sta() {
    // Each test runs on its own thread, and never leaves the apartment. That costs nothing.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Text(i32, i32, i32),
    Selection,
    Layout,
    Status(u32),
}

pub type OnLock = Box<dyn FnOnce(u32) -> Result<()>>;

/// A sink that stands in for TSF. A lock runs the closure that the test gave it.
#[implement(ITextStoreACPSink)]
pub struct FakeSink {
    pub heard: RefCell<Vec<Heard>>,
    pub on_lock: RefCell<Option<OnLock>>,
    pub lock_flags: RefCell<Vec<u32>>,
    pub fail_text: Cell<u32>,
}
impl ITextStoreACPSink_Impl for FakeSink_Impl {
    fn OnTextChange(
        &self,
        _flags: TEXT_STORE_TEXT_CHANGE_FLAGS,
        change: *const TS_TEXTCHANGE,
    ) -> Result<()> {
        if self.fail_text.get() > 0 {
            self.fail_text.set(self.fail_text.get() - 1);
            return Err(windows::Win32::Foundation::E_FAIL.into());
        }
        let c = unsafe { *change };
        self.heard
            .borrow_mut()
            .push(Heard::Text(c.acpStart, c.acpOldEnd, c.acpNewEnd));
        Ok(())
    }
    fn OnSelectionChange(&self) -> Result<()> {
        self.heard.borrow_mut().push(Heard::Selection);
        Ok(())
    }
    fn OnLayoutChange(&self, _code: TsLayoutCode, _view: u32) -> Result<()> {
        self.heard.borrow_mut().push(Heard::Layout);
        Ok(())
    }
    fn OnStatusChange(&self, flags: u32) -> Result<()> {
        self.heard.borrow_mut().push(Heard::Status(flags));
        Ok(())
    }
    fn OnAttrsChange(&self, _: i32, _: i32, _: u32, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnLockGranted(&self, flags: TEXT_STORE_LOCK_FLAGS) -> Result<()> {
        self.lock_flags.borrow_mut().push(flags.0);
        let next = self.on_lock.borrow_mut().take();
        match next {
            Some(next) => next(flags.0),
            None => Ok(()),
        }
    }
    fn OnStartEditTransaction(&self) -> Result<()> {
        Ok(())
    }
    fn OnEndEditTransaction(&self) -> Result<()> {
        Ok(())
    }
}

/// Calls the store's COM interface directly, as TSF would.
pub struct Direct {
    pub handle: TextStoreHandle,
    pub store: ITextStoreACP,
    pub sink: ComObject<FakeSink>,
    pub fixture: Fixture,
}
pub const READ: u32 = TS_LF_READ.0;
pub const WRITE: u32 = TS_LF_READWRITE.0;
pub const SYNC: u32 = TS_LF_SYNC;
impl Direct {
    pub fn new(view: EditorView) -> Self {
        let fixture = Fixture::new(view);
        let handle = TextStoreHandle::new(fixture.host.clone());
        let store = handle.text_store();
        let sink = ComObject::new(FakeSink {
            heard: RefCell::default(),
            on_lock: RefCell::default(),
            lock_flags: RefCell::default(),
            fail_text: Cell::new(0),
        });
        let as_sink: ITextStoreACPSink = sink.to_interface();
        unsafe { store.AdviseSink(&ITextStoreACPSink::IID, &as_sink, 0x1f) }.unwrap();
        Self {
            handle,
            store,
            sink,
            fixture,
        }
    }
    /// Runs `f` under a sync lock of this kind, and hands back what it returns.
    pub fn locked<R: 'static>(
        &self,
        kind: u32,
        f: impl FnOnce(&ITextStoreACP) -> R + 'static,
    ) -> R {
        let out: Rc<RefCell<Option<R>>> = Rc::default();
        let store = self.store.clone();
        let slot = out.clone();
        *self.sink.on_lock.borrow_mut() = Some(Box::new(move |_| {
            *slot.borrow_mut() = Some(f(&store));
            Ok(())
        }));
        // The host syncs after its edits, and when the store asks after a lock.
        self.handle.sync();
        let session = unsafe { self.store.RequestLock(kind | SYNC) }.unwrap();
        assert!(session.is_ok(), "the lock was refused: {session:?}");
        let answer = out.borrow_mut().take().expect("the lock never ran");
        self.handle.sync();
        answer
    }
    pub fn read_all(&self) -> String {
        self.locked(READ, |store| read(store, 0, -1))
    }
    pub fn take_heard(&self) -> Vec<Heard> {
        std::mem::take(&mut *self.sink.heard.borrow_mut())
    }
}

/// `GetText` for a range, as one string.
pub fn read(store: &ITextStoreACP, start: i32, end: i32) -> String {
    let mut buffer = [0_u16; 512];
    let mut got = 0;
    let mut runs = [Default::default(); 2];
    let mut run_count = 0;
    let mut next = 0;
    unsafe {
        store
            .GetText(
                start,
                end,
                &mut buffer,
                &mut got,
                &mut runs,
                &mut run_count,
                &mut next,
            )
            .unwrap();
    }
    String::from_utf16_lossy(&buffer[..got as usize])
}

/// A new layout that has not finished, so that a key has something to wait for.
pub fn lay_out_without_waiting(view: &mut EditorView, width: f32, height: f32) {
    View::layout(
        view,
        area(width, height),
        &DirectWriteLayout::new().unwrap(),
    );
}

/// Makes `View` usable in tests that lay a view out again.
pub fn lay_out(view: &mut EditorView, width: f32, height: f32) {
    View::layout(
        view,
        area(width, height),
        &DirectWriteLayout::new().unwrap(),
    );
    settle(view);
}
