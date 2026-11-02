//! Moves work to the window's thread.
//!
//! UIA calls a provider on its own worker thread. The editor lives on the window's thread, so
//! each piece of work that touches the host crosses there in a sent message, with a limit on
//! the wait. The work owns everything it uses, so a caller can give up and return while the
//! work is still running, and nothing it uses goes away. Actions that may open a dialog go
//! there in a posted message instead, so their caller returns at once.
//!
//! No lock is held while work runs. A forged message from another program finds no entry in
//! `WAITING`, and does nothing.
use crate::{
    com::{gone, not_yet},
    host::{UiThreadOnly, UiaHost},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::c_void,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    thread::ThreadId,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{
            PostMessageW, RegisterWindowMessageW, SMTO_ERRORONEXIT, SMTO_NORMAL,
            SendMessageTimeoutW,
        },
    },
    core::{Result, w},
};

/// How long a call waits for a busy window before it tells the client to ask again.
const WAIT_MS: u32 = 2000;
static WAIT_NOW: AtomicU32 = AtomicU32::new(WAIT_MS);

/// A test can shorten the wait, because each busy case otherwise takes the whole of it.
#[doc(hidden)]
pub fn set_wait_for_tests(milliseconds: u32) {
    WAIT_NOW.store(milliseconds, Ordering::Relaxed);
}

/// Work for the window's thread. It moves between threads only through `WAITING`, and it
/// runs or drops on one thread at a time.
struct Task(Box<dyn FnOnce()>);
// SAFETY: a task's captures are this crate's COM objects, whose counts are atomic, and values
// behind `Arc`. Each runs on the window's thread. If dropped elsewhere, it holds no host,
// because the host is only reached through `WindowThread` on the window's thread.
unsafe impl Send for Task {}

struct Entry {
    window: isize,
    /// True for posted work, which nobody waits for.
    posted: bool,
    task: Task,
}

fn waiting() -> &'static Mutex<HashMap<u64, Entry>> {
    static WAITING: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    WAITING.get_or_init(Mutex::default)
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn call_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("QuickEditUiaHostCall")) })
}

/// The window procedure's half. Returns `Some` if this message carried work. It takes the
/// work out before it runs it, so a later message for the same work does nothing, and no lock
/// is held while it runs. That work may pump messages, and this may run again inside it.
pub(crate) fn dispatch(message: u32, wparam: WPARAM) -> Option<LRESULT> {
    if message < 0xC000 || message != call_message() {
        return None;
    }
    let entry = waiting()
        .lock()
        .ok()
        .and_then(|mut table| table.remove(&(wparam.0 as u64)));
    if let Some(entry) = entry {
        (entry.task.0)();
    }
    Some(LRESULT(1))
}

/// How much work is waiting for the window's thread. Tests read this to prove none is left.
#[doc(hidden)]
pub fn waiting_work() -> usize {
    waiting().lock().map_or(0, |table| table.len())
}

/// Counts the calls that crossed threads. Tests read this to prove UIA came from elsewhere.
pub fn crossed_calls() -> usize {
    CROSSED.load(Ordering::Relaxed)
}
static CROSSED: AtomicUsize = AtomicUsize::new(0);

/// One window's way to its thread, and the only holder of its host.
pub(crate) struct WindowThread {
    thread: ThreadId,
    window: isize,
    /// New calls from other threads now say "gone" at once, so closing never waits for them.
    closing: AtomicBool,
    host: UiThreadOnly<RefCell<Option<Arc<dyn UiaHost>>>>,
}

impl WindowThread {
    /// Call on the window's thread.
    pub(crate) fn new(window: isize, host: Arc<dyn UiaHost>) -> Self {
        Self {
            thread: std::thread::current().id(),
            window,
            closing: AtomicBool::new(false),
            host: UiThreadOnly::new(RefCell::new(Some(host))),
        }
    }
    pub(crate) fn is_window_thread(&self) -> bool {
        std::thread::current().id() == self.thread
    }
    /// The host, on the window's thread, while the window is open. No reference to it ever
    /// exists on another thread, so its last reference always ends on the window's thread.
    pub(crate) fn host_here(&self) -> Result<Arc<dyn UiaHost>> {
        self.host
            .get()
            .and_then(|cell| cell.try_borrow().ok().and_then(|host| host.clone()))
            .ok_or_else(gone)
    }
    pub(crate) fn check_open(&self) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            if self.is_window_thread() {
                return self.host_here().map(drop);
            }
            return Err(gone());
        }
        Ok(())
    }
    /// From now on, calls from other threads say "gone" without crossing.
    pub(crate) fn begin_close(&self) {
        self.closing.store(true, Ordering::Release);
    }
    /// Ends the host here, and drops any posted work this window never reached.
    pub(crate) fn finish_close(&self) {
        let mut dropped = Vec::new();
        if let Ok(mut table) = waiting().lock() {
            let ids: Vec<u64> = table
                .iter()
                .filter(|(_, e)| e.window == self.window && e.posted)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                dropped.extend(table.remove(&id));
            }
        }
        drop(dropped);
        if let Some(cell) = self.host.get()
            && let Ok(mut host) = cell.try_borrow_mut()
        {
            let last = host.take();
            drop(host);
            drop(last);
        }
    }
    /// Runs `body` on the window's thread and gives its answer. If the window stays busy past
    /// the wait, this says to ask again, whatever `body` was going to answer, and `body`
    /// either never runs or finishes with nobody listening.
    pub(crate) fn on_ui<T: 'static>(
        self: &Arc<Self>,
        body: impl FnOnce(&Arc<dyn UiaHost>) -> Result<T> + 'static,
    ) -> Result<T> {
        self.check_open()?;
        if self.is_window_thread() {
            return body(&self.host_here()?);
        }
        CROSSED.fetch_add(1, Ordering::Relaxed);
        let answer: Arc<Mutex<Option<Answered<T>>>> = Arc::new(Mutex::new(None));
        let slot = answer.clone();
        let this = self.clone();
        let id = next_id();
        let task = Task(Box::new(move || {
            let out = this.host_here().and_then(|host| body(&host));
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(Answered(out));
            }
        }));
        {
            let mut table = waiting().lock().map_err(|_| gone())?;
            table.insert(
                id,
                Entry {
                    window: self.window,
                    posted: false,
                    task,
                },
            );
        }
        unsafe {
            SendMessageTimeoutW(
                HWND(self.window as *mut c_void),
                call_message(),
                WPARAM(id as usize),
                LPARAM(0),
                SMTO_NORMAL | SMTO_ERRORONEXIT,
                WAIT_NOW.load(Ordering::Relaxed),
                None,
            )
        };
        // If it has not begun, it never will. The work is dropped here.
        let unstarted = waiting().lock().ok().and_then(|mut t| t.remove(&id));

        drop(unstarted);
        let finished = answer.lock().ok().and_then(|mut slot| slot.take());
        match finished {
            Some(Answered(out)) => out,
            None if self.closing.load(Ordering::Acquire) => Err(gone()),
            None => Err(not_yet("The window is busy. Ask again soon.")),
        }
    }
    /// Queues `body` to run on the window's thread after this call returns.
    pub(crate) fn post(
        self: &Arc<Self>,
        body: impl FnOnce(&Arc<dyn UiaHost>) + 'static,
    ) -> Result<()> {
        self.check_open()?;
        let this = self.clone();
        let id = next_id();
        let task = Task(Box::new(move || {
            if let Ok(host) = this.host_here() {
                body(&host);
            }
        }));
        {
            let mut table = waiting().lock().map_err(|_| gone())?;
            table.insert(
                id,
                Entry {
                    window: self.window,
                    posted: true,
                    task,
                },
            );
        }
        let sent = unsafe {
            PostMessageW(
                Some(HWND(self.window as *mut c_void)),
                call_message(),
                WPARAM(id as usize),
                LPARAM(0),
            )
        };
        if let Err(error) = sent {
            if let Ok(mut table) = waiting().lock() {
                table.remove(&id);
            }
            return Err(error);
        }
        Ok(())
    }
}

/// An answer on its way back to the waiting thread.
struct Answered<T>(Result<T>);
// SAFETY: answers are strings, numbers and this crate's COM objects, whose counts are atomic.
// One thread at a time touches one, through the lock around it.
unsafe impl<T> Send for Answered<T> {}
