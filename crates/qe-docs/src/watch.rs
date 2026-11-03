//! Hears about changes to the folders of open files, with no timer.
//!
//! One thread for each folder sits in `WaitForMultipleObjects`, so it costs no CPU until
//! Windows has something to say. Why this and not only a check at window activation: the
//! Mac's file presenter reloads a clean document while another app has the focus, and a
//! check at activation could not. Activation still runs a check as well, for the places
//! this cannot hear, such as some network shares.
use crate::identity::{os_error, wide};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::core::PCWSTR;

/// What a folder's thread heard.
#[derive(Debug)]
pub(crate) enum FolderNews {
    /// These names in the folder changed.
    Names(Vec<OsString>),
    /// A name changed to another name in the folder.
    Renamed { old: OsString, new: OsString },
    /// Too much happened to list. Check every open file there.
    Everything,
    /// This folder cannot be watched, with the reason. A check at activation still works.
    CannotWatch(String),
}

struct Event(HANDLE);
// SAFETY: a Windows event handle may be used from any thread.
unsafe impl Send for Event {}
// SAFETY: as above; `SetEvent` may be called from several threads at once.
unsafe impl Sync for Event {}
impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: this type holds the only copy of the handle.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            eprintln!("QuickEdit: closing an event failed: {error}");
        }
    }
}
fn new_event() -> std::io::Result<Event> {
    // SAFETY: no name and no attributes. Manual reset, not set.
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map(Event)
        .map_err(|e| os_error(&e))
}

/// Dropping this stops the thread, which does not make the UI thread wait.
pub(crate) struct FolderWatch {
    pub(crate) folder: PathBuf,
    stop: std::sync::Arc<Event>,
}
impl Drop for FolderWatch {
    fn drop(&mut self) {
        // SAFETY: the event outlives the call, because of this `Arc`.
        if let Err(error) = unsafe { SetEvent(self.stop.0) } {
            eprintln!("QuickEdit: stopping a folder watch failed: {error}");
        }
    }
}

/// Starts a thread. A folder it cannot open is reported through `news`.
pub(crate) fn watch_folder(
    folder: &Path,
    before_listening: impl FnOnce() + Send + 'static,
    news: impl Fn(&Path, FolderNews) + Send + 'static,
) -> std::io::Result<FolderWatch> {
    let stop = std::sync::Arc::new(new_event()?);
    let thread_stop = stop.clone();
    let path = folder.to_path_buf();
    std::thread::Builder::new()
        .name("qe-docs-watch".into())
        .spawn(move || {
            before_listening();
            if let Err(error) = run(&path, &thread_stop, &news) {
                news(&path, FolderNews::CannotWatch(error.to_string()));
            }
        })?;
    Ok(FolderWatch {
        folder: folder.to_path_buf(),
        stop,
    })
}

const BUFFER_BYTES: usize = 64 * 1024;

fn run(folder: &Path, stop: &Event, news: &impl Fn(&Path, FolderNews)) -> std::io::Result<()> {
    let name = wide(folder);
    // SAFETY: the name ends in a zero and outlives the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|e| os_error(&e))?;
    let handle = Event(handle);
    let ready = new_event()?;
    // 32-bit alignment is what the call asks for.
    let mut buffer = vec![0u32; BUFFER_BYTES / 4];
    let mut listening = false;
    loop {
        let mut overlapped = OVERLAPPED {
            hEvent: ready.0,
            ..Default::default()
        };
        // SAFETY: the buffer and `overlapped` stay in place until the wait below ends or
        // the cancel below has finished.
        unsafe {
            ReadDirectoryChangesW(
                handle.0,
                buffer.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_CREATION,
                None,
                Some(&mut overlapped),
                None,
            )
        }
        .map_err(|e| os_error(&e))?;
        // A change between the read and this moment had no one to hear it. So once the
        // watch stands, every open file in this folder gets one look.
        if !listening {
            listening = true;
            news(folder, FolderNews::Everything);
        }
        let handles = [ready.0, stop.0];
        // SAFETY: both handles are open. No timeout: this thread sleeps until one is set.
        let woke = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        if woke != WAIT_OBJECT_0 {
            // SAFETY: the handle is open. Then wait for the kernel to let go of the buffer.
            let cancelled = unsafe { CancelIoEx(handle.0, Some(&overlapped)) };
            let mut count = 0;
            if cancelled.is_ok() {
                // SAFETY: `overlapped` is the one passed above.
                let ended = unsafe { GetOverlappedResult(handle.0, &overlapped, &mut count, true) };
                drop(ended);
            }
            return Ok(());
        }
        let mut count = 0;
        // SAFETY: the event says the call ended, so this does not wait.
        match unsafe { GetOverlappedResult(handle.0, &overlapped, &mut count, false) } {
            Ok(()) if count == 0 => news(folder, FolderNews::Everything),
            Ok(()) => report(folder, &buffer, count as usize, news),
            Err(error) if error.code() == WIN32_ERROR(ERROR_NOTIFY_ENUM_DIR.0).to_hresult() => {
                news(folder, FolderNews::Everything);
            }
            Err(error) => return Err(os_error(&error)),
        }
        // SAFETY: the event is open. It is set again by the next call's end.
        unsafe { windows::Win32::System::Threading::ResetEvent(ready.0) }
            .map_err(|e| os_error(&e))?;
    }
}

fn report(folder: &Path, buffer: &[u32], count: usize, news: &impl Fn(&Path, FolderNews)) {
    let base = buffer.as_ptr().cast::<u8>();
    let mut at = 0usize;
    let mut names = Vec::new();
    let mut old_name: Option<OsString> = None;
    let header = std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);
    while at + header <= count {
        // SAFETY: `at` is inside the `count` bytes Windows filled, on a record's start, and
        // Windows keeps records aligned to 32 bits.
        let record = unsafe { &*base.add(at).cast::<FILE_NOTIFY_INFORMATION>() };
        let units = record.FileNameLength as usize / 2;
        if at + header + record.FileNameLength as usize > count {
            break;
        }
        // SAFETY: the check above says all of the name lies in the filled bytes, and the
        // name starts on a 16-bit edge in a record on a 32-bit edge.
        let name =
            unsafe { std::slice::from_raw_parts(base.add(at + header).cast::<u16>(), units) };
        let name = OsString::from_wide(name);
        if record.Action == FILE_ACTION_RENAMED_OLD_NAME {
            old_name = Some(name);
        } else if record.Action == FILE_ACTION_RENAMED_NEW_NAME {
            if let Some(old) = old_name.take() {
                news(folder, FolderNews::Renamed { old, new: name });
            } else {
                names.push(name);
            }
        } else {
            names.push(name);
        }
        if record.NextEntryOffset == 0 {
            break;
        }
        at += record.NextEntryOffset as usize;
    }
    // A rename out of this folder has no new name here.
    names.extend(old_name);
    if !names.is_empty() {
        names.sort();
        names.dedup();
        news(folder, FolderNews::Names(names));
    }
}
