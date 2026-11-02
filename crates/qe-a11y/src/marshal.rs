//! Moves host calls to the window's thread.
//!
//! UIA calls a provider on its own worker thread. The editor lives on the window's thread, so
//! each host call from another thread goes there through a sent message and waits for the
//! answer. A forged message from another program finds no entry in `WAITING`, and does nothing.
use crate::host::{UiaHost, WidgetAction};
use qe_api::{ViewContext, Widget, WidgetId};
use qe_editor::EditorView;
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread::ThreadId,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{
            RegisterWindowMessageW, SMTO_ERRORONEXIT, SMTO_NORMAL, SendMessageTimeoutW,
        },
    },
    core::{Result, w},
};

/// How long a call waits for a busy window before it tells the client to ask again.
const WAIT_MS: u32 = 2000;

/// A call waiting for the window's thread. The pointer is to the caller's stack, so it is
/// only used while the lock is held, and the caller empties it before it returns.
struct Waiting(Mutex<Option<*mut (dyn FnMut() + 'static)>>);
// SAFETY: the pointer is used by one thread at a time, under the lock, while its owner waits.
unsafe impl Send for Waiting {}
unsafe impl Sync for Waiting {}

fn waiting() -> &'static Mutex<HashMap<u64, Arc<Waiting>>> {
    static WAITING: OnceLock<Mutex<HashMap<u64, Arc<Waiting>>>> = OnceLock::new();
    WAITING.get_or_init(Mutex::default)
}

fn call_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("QuickEditUiaHostCall")) })
}

/// The window procedure's half. Returns `Some` if this message was a host call.
pub(crate) fn dispatch(message: u32, wparam: WPARAM) -> Option<LRESULT> {
    if message < 0xC000 || message != call_message() {
        return None;
    }
    let entry = waiting()
        .lock()
        .ok()
        .and_then(|table| table.get(&(wparam.0 as u64)).cloned());
    if let Some(entry) = entry
        && let Ok(slot) = entry.0.lock()
        && let Some(run) = *slot
    {
        // SAFETY: the owner of this closure cannot return while this lock is held, and it
        // empties the slot before it returns, so the pointer is live here.
        unsafe { (*run)() };
    }
    Some(LRESULT(1))
}

/// Runs `body` on the window's thread and waits for it. False if the window did not take it.
fn run_on_window(window: isize, body: &mut dyn FnMut()) -> bool {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    // SAFETY: this only forgets the lifetime. The entry is emptied below before `body` ends.
    let pointer: *mut (dyn FnMut() + 'static) =
        unsafe { std::mem::transmute::<&mut dyn FnMut(), *mut (dyn FnMut() + 'static)>(body) };
    let entry = Arc::new(Waiting(Mutex::new(Some(pointer))));
    match waiting().lock() {
        Ok(mut table) => {
            table.insert(id, entry.clone());
        }
        Err(_) => return false,
    }
    let mut answer = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            HWND(window as *mut c_void),
            call_message(),
            WPARAM(id as usize),
            LPARAM(0),
            SMTO_NORMAL | SMTO_ERRORONEXIT,
            WAIT_MS,
            Some(&mut answer),
        )
    };
    if let Ok(mut table) = waiting().lock() {
        table.remove(&id);
    }
    // This waits for a run in progress, and stops one that has not begun.
    let emptied = match entry.0.lock() {
        Ok(mut slot) => slot.take().is_some() || true,
        // A panic on the window's thread aborts the app, so this cannot happen in release.
        Err(_) => std::process::abort(),
    };
    emptied && sent.0 != 0 && answer == 1
}

/// Counts the calls that crossed threads. Tests read this to prove UIA came from elsewhere.
pub fn crossed_calls() -> usize {
    CROSSED.load(Ordering::Relaxed)
}
static CROSSED: AtomicUsize = AtomicUsize::new(0);

/// A host whose calls always run on the window's thread.
pub(crate) struct OnWindowThread {
    pub(crate) inner: Arc<dyn UiaHost>,
    pub(crate) thread: ThreadId,
    pub(crate) window: isize,
}

impl OnWindowThread {
    fn call<T>(&self, body: impl FnOnce(&dyn UiaHost) -> T) -> Option<T> {
        if std::thread::current().id() == self.thread {
            return Some(body(self.inner.as_ref()));
        }
        CROSSED.fetch_add(1, Ordering::Relaxed);
        let mut body = Some(body);
        let mut out = None;
        let inner = self.inner.as_ref();
        run_on_window(self.window, &mut || {
            if let Some(body) = body.take() {
                out = Some(body(inner));
            }
        });
        out
    }
}

impl UiaHost for OnWindowThread {

    fn widgets(&self) -> Option<Widget> {
        self.call(|host| host.widgets()).flatten()
    }
    fn action(&self, id: WidgetId, action: WidgetAction) -> Result<()> {
        self.call(|host| host.action(id, action))
            .unwrap_or_else(|| Err(crate::com::not_yet("The window is busy. Ask again soon.")))
    }
    fn document_widget(&self) -> Option<Widget> {
        self.call(|host| host.document_widget()).flatten()
    }
    fn view_id(&self) -> Option<u64> {
        self.call(|host| host.view_id()).flatten()
    }
    fn with_editor(&self, body: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool {
        self.call(|host| host.with_editor(body)).unwrap_or(false)
    }
    fn font(&self) -> Option<(String, f64)> {
        self.call(|host| host.font()).flatten()
    }
}

impl OnWindowThread {
    /// Runs `body` on the window's thread. A window that stays busy past the wait gives the
    /// error that tells a client to ask again, which is not the same as "gone".
    pub(crate) fn call_now<T>(&self, body: impl FnOnce(&dyn UiaHost) -> Result<T>) -> Result<T> {
        self.call(body)
            .unwrap_or_else(|| Err(crate::com::not_yet("The window is busy. Ask again soon.")))
    }
    pub(crate) fn widgets_now(&self) -> Result<Widget> {
        self.call_now(|host| host.widgets().ok_or_else(crate::com::gone))
    }
}
