//! Small Windows parts: the message-only wake window, the message loop, drops and placement.
use crate::timing::Timing;
use qe_ime::TsfThread;
use qe_shell::dispatch_translating;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_APP_WAKE: u32 = WM_APP + 0x70;
const WM_APP_BENCH: u32 = WM_APP + 0x71;
const DEADLINE_TIMER: usize = 1;

type SignalHandler = Rc<dyn Fn(Signal)>;
thread_local! {
    static HANDLER: RefCell<Option<SignalHandler>> = const { RefCell::new(None) };
}

/// What the wake window tells the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Wake,
    Deadline,
    BenchStep,
}

unsafe extern "system" fn wake_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let signal = match msg {
        WM_APP_WAKE => {
            // SAFETY: the pointer lives as long as the window, in `WakeWindow`.
            let posted = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AtomicBool;
            if !posted.is_null() {
                // SAFETY: as above.
                unsafe { (*posted).store(false, Ordering::Release) };
            }
            Some(Signal::Wake)
        }
        WM_APP_BENCH => Some(Signal::BenchStep),
        WM_TIMER if w.0 == DEADLINE_TIMER => {
            // SAFETY: a one-shot timer of this window. A failure here only means it fires again.
            if let Err(e) = unsafe { KillTimer(Some(hwnd), DEADLINE_TIMER) } {
                eprintln!("KillTimer for the store's deadline: {e}");
            }
            Some(Signal::Deadline)
        }
        _ => None,
    };
    if let Some(signal) = signal {
        let handler = HANDLER.with(|h| h.borrow().clone());
        if let Some(handler) = handler {
            handler(signal);
        }
        return LRESULT(0);
    }
    // SAFETY: the default answer for this window's own message.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// A window no one sees. Workers wake the app through it, and it outlives every text window.
pub struct WakeWindow {
    hwnd: HWND,
    target: Arc<AtomicIsize>,
    posted: Arc<AtomicBool>,
}
impl WakeWindow {
    pub fn create() -> Result<Self, String> {
        // SAFETY: plain window creation on this thread.
        unsafe {
            let instance = GetModuleHandleW(PCWSTR::null()).map_err(|e| e.to_string())?;
            let class = w!("QuickEditWake");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wake_proc),
                hInstance: HINSTANCE(instance.0),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("QuickEdit wake"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(HINSTANCE(instance.0)),
                None,
            )
            .map_err(|e| format!("The wake window could not start: {e}"))?;
            let posted = Arc::new(AtomicBool::new(false));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::as_ptr(&posted) as isize);
            Ok(Self {
                hwnd,
                target: Arc::new(AtomicIsize::new(hwnd.0 as isize)),
                posted,
            })
        }
    }
    pub fn set_handler(&self, handler: SignalHandler) {
        HANDLER.with(|h| *h.borrow_mut() = Some(handler));
    }
    /// A call for any thread. Many wakes while one waits count as one.
    pub fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let target = self.target.clone();
        let posted = self.posted.clone();
        Arc::new(move || {
            if posted.swap(true, Ordering::AcqRel) {
                return;
            }
            let raw = target.load(Ordering::Acquire);
            // SAFETY: Windows checks the handle.
            let sent = unsafe {
                PostMessageW(Some(HWND(raw as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0))
            };
            if let Err(e) = sent {
                posted.store(false, Ordering::Release);
                eprintln!("A worker could not wake the window: {e}");
            }
        })
    }
    /// Comes back through the message loop, after what waits in the queue.
    pub fn post_bench_step(&self) {
        // SAFETY: a window of this thread.
        if let Err(e) = unsafe { PostMessageW(Some(self.hwnd), WM_APP_BENCH, WPARAM(0), LPARAM(0)) }
        {
            eprintln!("The bench could not go on: {e}");
        }
    }
    /// Arms the store's one-shot timer. `None` means no timer at all, which is the idle state.
    pub fn set_deadline(&self, milliseconds: Option<u32>) -> Result<(), String> {
        // SAFETY: plain timer calls on this thread's window.
        unsafe {
            match milliseconds {
                Some(ms) => {
                    if SetTimer(Some(self.hwnd), DEADLINE_TIMER, ms.max(1), None) == 0 {
                        return Err(format!(
                            "SetTimer failed: {}",
                            windows::core::Error::from_thread()
                        ));
                    }
                }
                None => {
                    // No timer set is the usual answer here, and it costs nothing.
                    let _ = KillTimer(Some(self.hwnd), DEADLINE_TIMER);
                }
            }
        }
        Ok(())
    }
}

/// The message loop. It uses plain `GetMessageW` until the first focus has started TSF, and
/// then TSF's pump, so that TSF costs nothing for a run that never takes the keyboard.
pub fn message_loop(
    tsf: &RefCell<Option<Rc<TsfThread>>>,
    timing: &Timing,
    broken: &Cell<bool>,
) -> Result<i32, String> {
    let mut msg = MSG::default();
    loop {
        if broken.get() {
            return Err("a test broke the message loop".to_owned());
        }
        let thread = tsf.borrow().clone();
        if let Some(thread) = thread {
            if !thread.get_message(&mut msg).map_err(|e| e.to_string())? {
                return Ok(msg.wParam.0 as i32);
            }
            if thread.key_was_taken(&msg) {
                continue;
            }
        } else {
            // SAFETY: `msg` lives through the call.
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if got.0 == -1 {
                return Err(windows::core::Error::from_thread().to_string());
            }
            if got.0 == 0 {
                return Ok(msg.wParam.0 as i32);
            }
        }
        if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
            timing.key_message_arrived();
        }
        dispatch_translating(&msg);
    }
}

/// The paths of a `WM_DROPFILES`. The drop handle ends here.
pub fn dropped_paths(w: WPARAM) -> Vec<PathBuf> {
    let drop = HDROP(w.0 as *mut _);
    let mut paths = Vec::new();
    // SAFETY: the handle comes with the message, and each buffer lives through its call.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        for index in 0..count {
            let length = DragQueryFileW(drop, index, None) as usize;
            let mut wide = vec![0u16; length + 1];
            DragQueryFileW(drop, index, Some(&mut wide));
            wide.truncate(length);
            // A Windows file name need not be valid Unicode, and it must still open.
            paths.push(PathBuf::from(OsString::from_wide(&wide)));
        }
        DragFinish(drop);
    }
    paths
}

/// The screen place of the pointer now.
pub fn cursor_pos() -> Result<POINT, String> {
    let mut at = POINT::default();
    // SAFETY: `at` lives through the call.
    unsafe { GetCursorPos(&mut at) }.map_err(|e| e.to_string())?;
    Ok(at)
}
