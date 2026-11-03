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
use windows::Win32::Foundation::{
    APPMODEL_ERROR_NO_PACKAGE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WIN32_ERROR, WPARAM,
};
use windows::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows::Win32::System::Console::{GetStdHandle, STD_HANDLE};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
};
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// A DLL loaded by name comes from System32 only, never the app's folder, the current folder or
/// `PATH`. The app ships no DLLs, so its folder has none to offer, and one found there was put
/// there by someone else. The linker's `/DEPENDENTLOADFLAG` in `build.rs` does the same for the
/// exe's own imports, which load before this runs. COM still finds its servers: it names them by
/// full path. The result is for the caller to show.
pub fn keep_dll_search_to_system32() -> Result<(), String> {
    // SAFETY: plain flags, and no other thread is running yet.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|e| format!("Windows refused the rule for where DLLs load from: {e}"))
}

/// Whether Windows runs this process from a package, as the Store's install does. Only "no
/// package" counts as no: any other answer takes the stricter road, with no flags.
pub fn has_package_identity() -> bool {
    let mut length = 0;
    // SAFETY: with a length of zero and no buffer, this only reports what it would need.
    let answer = unsafe { GetCurrentPackageFullName(&mut length, None) };
    answer_means_a_package(answer)
}

fn answer_means_a_package(answer: WIN32_ERROR) -> bool {
    answer != APPMODEL_ERROR_NO_PACKAGE
}

/// Whether text written to this stream reaches somebody: a pipe or a file. A Windows app started
/// from a terminal has no console, so anything else would vanish.
pub fn stream_has_a_reader(stream: STD_HANDLE) -> bool {
    // SAFETY: a plain query. A missing stream gives an error or a null handle.
    let Ok(handle) = (unsafe { GetStdHandle(stream) }) else {
        return false;
    };
    if handle.is_invalid() {
        return false;
    }
    // SAFETY: a plain query. A stale handle gives the unknown type.
    let kind = unsafe { GetFileType(handle) };
    kind == FILE_TYPE_PIPE || kind == FILE_TYPE_DISK
}

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
    /// The target of the same message from another thread, as a number that can cross to it.
    pub fn bench_step_target(&self) -> isize {
        self.hwnd.0 as isize
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

/// `WakeWindow::post_bench_step`, from another thread.
pub fn post_bench_step_to(target: isize) {
    let hwnd = HWND(target as *mut std::ffi::c_void);
    // SAFETY: Windows checks the handle.
    if let Err(e) = unsafe { PostMessageW(Some(hwnd), WM_APP_BENCH, WPARAM(0), LPARAM(0)) } {
        eprintln!("The bench could not go on: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};

    /// The branch that decides whether flags count. Only "no package" reads flags. A real
    /// package's run gives "buffer too small" here, and root checks that on a real install.
    #[test]
    fn only_the_no_package_answer_lets_flags_count() {
        assert!(!answer_means_a_package(APPMODEL_ERROR_NO_PACKAGE));
        for answer in [ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, WIN32_ERROR(5)] {
            assert!(answer_means_a_package(answer), "{answer:?}");
        }
        let flag = || vec![std::ffi::OsString::from("--hidden")].into_iter();
        let read = Args::parse(flag(), answer_means_a_package(ERROR_INSUFFICIENT_BUFFER));
        let read = read.unwrap();
        assert!(!read.hidden);
        assert_eq!(read.files, [PathBuf::from("--hidden")]);
        let read = Args::parse(flag(), answer_means_a_package(APPMODEL_ERROR_NO_PACKAGE));
        assert!(read.unwrap().hidden);
    }
}
