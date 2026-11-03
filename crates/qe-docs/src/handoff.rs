//! A second launch hands its files to the running QuickEdit, over a named pipe.
//!
//! ```text
//! launch -> hand_off(paths) -> pipe -> HandoffServer thread -> queue -> wake -> UI: take_requests
//!                              <- one byte: taken (the queue has them; no tab is open yet)
//! ```
//!
//! The pipe's name has the user, the Windows session and the integrity level in it, so each
//! level has its own. A launch at one level that finds only another level's instance gets
//! [`HandoffError::DifferentIntegrity`]. The server thread never waits for the UI, and nothing
//! here runs a timer at idle.
use crate::identity::wide;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_NONE, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, INFINITE, OpenProcessToken, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows::core::{PCWSTR, PWSTR};

const MAGIC: u32 = 0x3145_5551; // "QUE1"
const MAX_MESSAGE_UNITS: usize = 1 << 20;
const MAX_PATHS: usize = 4096;
const TAKEN: u8 = 0x51;
/// A running instance answers in far less. Past this it counts as hung.
const CONNECT_WAIT: Duration = Duration::from_millis(1500);
const ANSWER_WAIT: Duration = Duration::from_millis(3000);
/// Less than `ANSWER_WAIT`, so a launch that stalls cannot make the next one give up.
const SERVER_READ_WAIT: Duration = Duration::from_millis(1000);

/// What a launch can learn. Each one says what the launch should do next.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum HandoffError {
    /// An instance runs at another integrity level, such as one started with Run as
    /// administrator. Windows keeps the two apart, so these files did not reach it. Show
    /// this, then run as an instance of your own.
    DifferentIntegrity { ours: String, theirs: String },
    /// An instance holds the pipe and does not answer: it hangs, or it is closing. Show
    /// this, then run as an instance of your own.
    StaleInstance { waited: Duration },
    /// Anything else Windows reported, with its text.
    Failed { what: String },
}
impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DifferentIntegrity { ours, theirs } => write!(
                f,
                "QuickEdit is already running with {theirs} rights, and this launch has {ours} rights, so Windows does not let them share files. This launch opens its own window."
            ),
            Self::StaleInstance { waited } => write!(
                f,
                "A running QuickEdit did not answer in {} ms, so this launch opens its own window.",
                waited.as_millis()
            ),
            Self::Failed { what } => {
                write!(f, "The running QuickEdit could not be reached: {what}")
            }
        }
    }
}
impl std::error::Error for HandoffError {}

/// One launch's request: its files, already absolute. No files means "come forward".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffRequest {
    pub paths: Vec<PathBuf>,
}

/// What [`start`] found out.
pub enum LaunchRole {
    /// No instance ran, so this process is it. Open the paths here. The server is listening.
    Primary(HandoffServer),
    /// The running instance has the files, so this process should exit now.
    HandedOff,
    /// This process must run on its own: show the error, then open the paths here.
    Alone { error: HandoffError },
}

/// The pipe's name for this user, Windows session and level.
#[derive(Clone, Debug)]
pub struct PipeNames {
    /// The base name, which holds the user and the session.
    pub base: String,
    /// This process's level.
    pub level: IntegrityLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrityLevel {
    Low,
    Medium,
    High,
    System,
}
impl IntegrityLevel {
    const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::System];
    fn suffix(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::System => "system",
        }
    }
    fn words(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "normal",
            Self::High => "administrator",
            Self::System => "system",
        }
    }
}

impl PipeNames {
    /// The real names, from this process's token.
    pub fn for_this_process() -> Result<Self, HandoffError> {
        let fail = |what: &str, error: &windows::core::Error| HandoffError::Failed {
            what: format!("{what}: {error}"),
        };
        let mut token = HANDLE::default();
        // SAFETY: the process handle is the pseudo handle, and `token` outlives the call.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(|e| fail("reading this process's token", &e))?;
        let result = (|| {
            let user = token_bytes(token, TokenUser)
                .map_err(|e| fail("reading the user from the token", &e))?;
            // SAFETY: Windows filled the buffer with a TOKEN_USER, and it is aligned for it.
            let user_sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
            let mut text = PWSTR::null();
            // SAFETY: the SID lives in `user`, and `text` is freed below.
            unsafe { ConvertSidToStringSidW(user_sid, &mut text) }
                .map_err(|e| fail("reading the user's name", &e))?;
            // SAFETY: Windows gave a string that ends in a zero.
            let sid = unsafe { text.to_string() }.unwrap_or_default();
            // SAFETY: the pointer came from the call above and is freed once.
            unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
            let label = token_bytes(token, TokenIntegrityLevel)
                .map_err(|e| fail("reading the integrity level", &e))?;
            // SAFETY: Windows filled the buffer with a TOKEN_MANDATORY_LABEL.
            let label_sid = unsafe { (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid };
            // SAFETY: the SID is valid while `label` lives, and has at least one part.
            let rid = unsafe {
                let parts = *GetSidSubAuthorityCount(label_sid);
                *GetSidSubAuthority(label_sid, u32::from(parts) - 1)
            };
            let level = match rid {
                0..=0x0fff => IntegrityLevel::Low,
                0x1000..=0x2fff => IntegrityLevel::Medium,
                0x3000..=0x3fff => IntegrityLevel::High,
                _ => IntegrityLevel::System,
            };
            let mut session = 0;
            // SAFETY: plain numbers, and `session` outlives the call.
            unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }
                .map_err(|e| fail("reading the Windows session", &e))?;
            Ok(Self {
                base: format!("QuickEdit-{sid}-{session}"),
                level,
            })
        })();
        // SAFETY: the token is open, and this is its only close.
        if let Err(error) = unsafe { CloseHandle(token) } {
            eprintln!("QuickEdit: closing the token failed: {error}");
        }
        result
    }
    fn pipe(&self, level: IntegrityLevel) -> Vec<u16> {
        let name = format!(r"\\.\pipe\{}-{}", self.base, level.suffix());
        wide(Path::new(&name))
    }
}

fn token_bytes(
    token: HANDLE,
    class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
) -> windows::core::Result<Vec<u64>> {
    let mut size = 0;
    // SAFETY: this first call only asks for the size, and is expected to fail.
    let asked = unsafe { GetTokenInformation(token, class, None, 0, &mut size) };
    drop(asked);
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    // SAFETY: the buffer holds `size` bytes and outlives the call.
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    }?;
    Ok(buffer)
}

/// Makes each path absolute from this launch's folder, since the server has its own. No disk.
pub fn absolute_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|path| std::path::absolute(path).unwrap_or_else(|_| path.clone()))
        .collect()
}

fn encode_message(paths: &[PathBuf]) -> Vec<u8> {
    let mut units: Vec<u16> = Vec::new();
    for path in paths {
        units.extend(path.as_os_str().encode_wide());
        units.push(0);
    }
    let mut out = Vec::with_capacity(16 + units.len() * 2);
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&(paths.len() as u32).to_le_bytes());
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// `None` for anything but a whole, well-formed message.
fn decode_message(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if word(0)? != MAGIC {
        return None;
    }
    let count = word(4)? as usize;
    let units = word(8)? as usize;
    if count > MAX_PATHS || units > MAX_MESSAGE_UNITS || bytes.len() != 12 + units * 2 {
        return None;
    }
    let (pairs, _) = bytes[12..].as_chunks::<2>();
    let body: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
    if count == 0 {
        return body.is_empty().then(Vec::new);
    }
    if body.last() != Some(&0) {
        return None;
    }
    let paths: Vec<PathBuf> = body[..body.len() - 1]
        .split(|unit| *unit == 0)
        .map(|part| PathBuf::from(OsString::from_wide(part)))
        .collect();
    (paths.len() == count && paths.iter().all(|p| !p.as_os_str().is_empty())).then_some(paths)
}

fn windows_failure(what: &str, error: &windows::core::Error) -> HandoffError {
    HandoffError::Failed {
        what: format!("{what}: {error}"),
    }
}

/// Sends `paths` to a running instance at this level, if one is there. `Ok(false)` means
/// none is running at any level. Up to about 4.5 seconds when an instance hangs; a launch
/// has no UI to hold up yet.
pub fn hand_off(names: &PipeNames, paths: &[PathBuf]) -> Result<bool, HandoffError> {
    let pipe = names.pipe(names.level);
    let started = Instant::now();
    let file = loop {
        // SAFETY: the name ends in a zero and outlives the call.
        let opened = unsafe {
            CreateFileW(
                PCWSTR(pipe.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        match opened {
            Ok(handle) => break Owned(handle),
            Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => {
                return other_level_check(names);
            }
            Err(error) if error.code() == ERROR_PIPE_BUSY.to_hresult() => {
                let left = CONNECT_WAIT.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(HandoffError::StaleInstance {
                        waited: started.elapsed(),
                    });
                }
                // SAFETY: the name outlives the call.
                // A failed wait says only that the time ran out or the pipe went. The loop
                // asks again and learns which.
                let _ = unsafe { WaitNamedPipeW(PCWSTR(pipe.as_ptr()), left.as_millis() as u32) };
            }
            Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => {
                return Err(HandoffError::DifferentIntegrity {
                    ours: names.level.words().to_owned(),
                    theirs: "other".to_owned(),
                });
            }
            Err(error) => return Err(windows_failure("opening the pipe", &error)),
        }
    };
    // So that the instance may bring its window forward.
    let mut server = 0;
    // SAFETY: the pipe is open and `server` outlives the call.
    if unsafe { GetNamedPipeServerProcessId(file.0, &mut server) }.is_ok()
        // SAFETY: a plain number.
        && let Err(error) = unsafe { AllowSetForegroundWindow(server) }
    {
        // Only the window's coming forward depends on this.
        eprintln!("QuickEdit: AllowSetForegroundWindow failed: {error}");
    }
    let message = encode_message(paths);
    overlapped_write(file.0, &message, ANSWER_WAIT).map_err(|failure| match failure {
        Waited::TimedOut(waited) => HandoffError::StaleInstance { waited },
        Waited::Failed(error) => windows_failure("sending the files", &error),
    })?;
    let mut answer = [0u8; 1];
    let got =
        overlapped_read(file.0, &mut answer, ANSWER_WAIT).map_err(|failure| match failure {
            Waited::TimedOut(waited) => HandoffError::StaleInstance { waited },
            Waited::Failed(error) => windows_failure("reading the answer", &error),
        })?;
    if got != 1 || answer[0] != TAKEN {
        return Err(HandoffError::Failed {
            what: "the running QuickEdit refused the message".into(),
        });
    }
    Ok(true)
}

fn other_level_check(names: &PipeNames) -> Result<bool, HandoffError> {
    for level in IntegrityLevel::ALL {
        if level == names.level {
            continue;
        }
        let pipe = names.pipe(level);
        // SAFETY: the name outlives the call. Zero uses the pipe's default wait only when one
        // exists, and returns at once when there is none.
        let exists = unsafe { WaitNamedPipeW(PCWSTR(pipe.as_ptr()), 1) };
        let found = exists.as_bool()
            || windows::core::Error::from_thread().code() != ERROR_FILE_NOT_FOUND.to_hresult();
        if found {
            return Err(HandoffError::DifferentIntegrity {
                ours: names.level.words().to_owned(),
                theirs: level.words().to_owned(),
            });
        }
    }
    Ok(false)
}

struct Owned(HANDLE);
// SAFETY: a Windows handle may be used and closed from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above; the calls used on it from several threads are safe for handles.
unsafe impl Sync for Owned {}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: this type holds the only copy of the handle.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            eprintln!("QuickEdit: closing a pipe handle failed: {error}");
        }
    }
}

enum Waited {
    TimedOut(Duration),
    Failed(windows::core::Error),
}

fn finish_overlapped(
    handle: HANDLE,
    overlapped: &mut OVERLAPPED,
    limit: Duration,
    event: &Owned,
) -> Result<u32, Waited> {
    let started = Instant::now();
    // SAFETY: the event is open.
    let woke = unsafe { WaitForSingleObject(event.0, limit.as_millis() as u32) };
    if woke == WAIT_TIMEOUT {
        // SAFETY: the handle is open. The wait below lets the kernel drop the buffer.
        let cancelled = unsafe { CancelIoEx(handle, Some(overlapped)) };
        if cancelled.is_ok() {
            let mut count = 0;
            // SAFETY: `overlapped` belongs to the call that was cancelled.
            let ended = unsafe { GetOverlappedResult(handle, overlapped, &mut count, true) };
            drop(ended);
        }
        return Err(Waited::TimedOut(started.elapsed()));
    }
    let mut count = 0;
    // SAFETY: the event says the call ended.
    unsafe { GetOverlappedResult(handle, overlapped, &mut count, false) }
        .map_err(Waited::Failed)?;
    Ok(count)
}

fn overlapped_write(handle: HANDLE, bytes: &[u8], limit: Duration) -> Result<(), Waited> {
    // SAFETY: no name and no attributes.
    let event =
        Owned(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(Waited::Failed)?);
    let mut sent = 0usize;
    while sent < bytes.len() {
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: the bytes and `overlapped` stay in place until `finish_overlapped` returns.
        let started =
            unsafe { WriteFile(handle, Some(&bytes[sent..]), None, Some(&mut overlapped)) };
        if let Err(error) = started
            && error.code() != ERROR_IO_PENDING.to_hresult()
        {
            return Err(Waited::Failed(error));
        }
        sent += finish_overlapped(handle, &mut overlapped, limit, &event)? as usize;
    }
    Ok(())
}

fn overlapped_read(handle: HANDLE, bytes: &mut [u8], limit: Duration) -> Result<usize, Waited> {
    // SAFETY: no name and no attributes.
    let event =
        Owned(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(Waited::Failed)?);
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    // SAFETY: the bytes and `overlapped` stay in place until `finish_overlapped` returns.
    let started = unsafe { ReadFile(handle, Some(bytes), None, Some(&mut overlapped)) };
    if let Err(error) = started
        && error.code() != ERROR_IO_PENDING.to_hresult()
    {
        return Err(Waited::Failed(error));
    }
    finish_overlapped(handle, &mut overlapped, limit, &event).map(|n| n as usize)
}

/// The running instance's end of the pipe. Dropping it stops the thread with no wait.
pub struct HandoffServer {
    queue: Arc<Mutex<Vec<HandoffRequest>>>,
    stop: Arc<Owned>,
}

impl HandoffServer {
    /// The requests that came since the last call, oldest first. Never waits on a person.
    pub fn take_requests(&self) -> Vec<HandoffRequest> {
        match self.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}
impl Drop for HandoffServer {
    fn drop(&mut self) {
        // SAFETY: the event is open while this `Arc` lives.
        if let Err(error) = unsafe { SetEvent(self.stop.0) } {
            eprintln!("QuickEdit: stopping the handoff thread failed: {error}");
        }
    }
}

fn create_pipe(pipe: &[u16], first: bool) -> windows::core::Result<Owned> {
    let mut flags = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: the name ends in a zero and outlives the call.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(pipe.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(flags.0),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(windows::core::Error::from_thread());
    }
    Ok(Owned(handle))
}

/// Takes the pipe's name and starts listening. `Ok(None)`: another process holds the name.
pub fn listen(
    names: &PipeNames,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<Option<HandoffServer>, HandoffError> {
    let pipe = names.pipe(names.level);
    let first = match create_pipe(&pipe, true) {
        Ok(handle) => handle,
        // The first-instance flag fails with access denied when the name is taken.
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => return Ok(None),
        Err(error) => return Err(windows_failure("creating the pipe", &error)),
    };
    // SAFETY: no name and no attributes.
    let stop = Arc::new(Owned(
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|e| windows_failure("making an event", &e))?,
    ));
    let queue = Arc::new(Mutex::new(Vec::new()));
    let (thread_queue, thread_stop) = (queue.clone(), stop.clone());
    std::thread::Builder::new()
        .name("qe-docs-handoff".into())
        .spawn(move || serve(first, pipe, &thread_queue, &thread_stop, wake.as_ref()))
        .map_err(|e| HandoffError::Failed {
            what: format!("starting the handoff thread: {e}"),
        })?;
    Ok(Some(HandoffServer { queue, stop }))
}

/// A launch's whole job: hand off, or become the instance. Two launches at once are safe,
/// because only one can create the pipe, and the other then hands off to it.
pub fn start(
    names: &PipeNames,
    paths: &[PathBuf],
    wake: Arc<dyn Fn() + Send + Sync>,
) -> LaunchRole {
    start_with(names, paths, wake, &|| {})
}

/// `start`, with a call between finding no instance and taking the name, so that a test can
/// be the other launch that wins the race at that moment.
#[doc(hidden)]
pub fn start_with(
    names: &PipeNames,
    paths: &[PathBuf],
    wake: Arc<dyn Fn() + Send + Sync>,
    between: &dyn Fn(),
) -> LaunchRole {
    let paths = absolute_paths(paths);
    // A loser of the race may arrive before the winner listens, so it tries a few times.
    let deadline = Instant::now() + CONNECT_WAIT;
    loop {
        match hand_off(names, &paths) {
            Ok(true) => return LaunchRole::HandedOff,
            Ok(false) => {}
            Err(error) => return LaunchRole::Alone { error },
        }
        between();
        match listen(names, wake.clone()) {
            Ok(Some(server)) => return LaunchRole::Primary(server),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) => {
                return LaunchRole::Alone {
                    error: HandoffError::StaleInstance {
                        waited: CONNECT_WAIT,
                    },
                };
            }
            Err(error) => return LaunchRole::Alone { error },
        }
    }
}

fn serve(
    mut listening: Owned,
    pipe: Vec<u16>,
    queue: &Mutex<Vec<HandoffRequest>>,
    stop: &Owned,
    wake: &(dyn Fn() + Send + Sync),
) {
    // SAFETY: no name and no attributes.
    let ready = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
        Ok(event) => Owned(event),
        Err(error) => {
            eprintln!("QuickEdit: the handoff thread could not make an event: {error}");
            return;
        }
    };
    loop {
        let mut overlapped = OVERLAPPED {
            hEvent: ready.0,
            ..Default::default()
        };
        // SAFETY: `overlapped` stays in place until the wait below ends.
        let connecting = unsafe { ConnectNamedPipe(listening.0, Some(&mut overlapped)) };
        let connected = match connecting {
            Ok(()) => true,
            Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => true,
            Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => false,
            // A launch came and went before this call. The pipe must be reset for the next.
            Err(error) if error.code() == ERROR_NO_DATA.to_hresult() => {
                // SAFETY: the handle is open.
                if let Err(error) = unsafe { DisconnectNamedPipe(listening.0) } {
                    eprintln!("QuickEdit: the handoff pipe could not reset: {error}");
                    return;
                }
                continue;
            }
            Err(error) => {
                eprintln!("QuickEdit: the handoff pipe failed: {error}");
                return;
            }
        };
        if !connected {
            let handles = [ready.0, stop.0];
            // SAFETY: both are open. No timeout: this thread sleeps until a launch comes.
            let woke = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
            if woke != WAIT_OBJECT_0 {
                // SAFETY: the handle is open. The wait lets the kernel drop `overlapped`.
                if unsafe { CancelIoEx(listening.0, Some(&overlapped)) }.is_ok() {
                    let mut count = 0;
                    // SAFETY: `overlapped` belongs to the cancelled call.
                    let ended =
                        unsafe { GetOverlappedResult(listening.0, &overlapped, &mut count, true) };
                    drop(ended);
                }
                return;
            }
        }
        // A new instance must exist before this one is busy, or a launch would find no pipe
        // and think it is the first.
        let next = match create_pipe(&pipe, false) {
            Ok(next) => next,
            Err(error) => {
                eprintln!("QuickEdit: the handoff pipe could not grow: {error}");
                return;
            }
        };
        let client = std::mem::replace(&mut listening, next);
        match read_request(client.0) {
            Some(paths) => {
                match queue.lock() {
                    Ok(mut list) => list.push(HandoffRequest { paths }),
                    Err(poisoned) => poisoned.into_inner().push(HandoffRequest { paths }),
                }
                wake();
                if let Err(_failure) = overlapped_write(client.0, &[TAKEN], SERVER_READ_WAIT) {
                    eprintln!("QuickEdit: a launch left before it heard the answer");
                }
                // SAFETY: the handle is open. This waits only for the client to read.
                if let Err(error) = unsafe { FlushFileBuffers(client.0) } {
                    eprintln!("QuickEdit: the handoff answer did not flush: {error}");
                }
            }
            None => eprintln!("QuickEdit: a bad or slow handoff message was dropped"),
        }
        // SAFETY: the handle is open.
        if let Err(error) = unsafe { DisconnectNamedPipe(client.0) } {
            eprintln!("QuickEdit: disconnecting a launch failed: {error}");
        }
        drop(client);
    }
}

fn read_request(client: HANDLE) -> Option<Vec<PathBuf>> {
    let mut head = [0u8; 12];
    let mut got = 0;
    while got < head.len() {
        let count = overlapped_read(client, &mut head[got..], SERVER_READ_WAIT).ok()?;
        if count == 0 {
            return None;
        }
        got += count;
    }
    let units = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
    if units > MAX_MESSAGE_UNITS {
        return None;
    }
    let mut message = head.to_vec();
    message.resize(12 + units * 2, 0);
    let mut got = 12;
    while got < message.len() {
        let count = overlapped_read(client, &mut message[got..], SERVER_READ_WAIT).ok()?;
        if count == 0 {
            return None;
        }
        got += count;
    }
    decode_message(&message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn messages_round_trip_and_bad_ones_are_refused() {
        let paths = vec![
            PathBuf::from(r"C:\a b\\u{4e2d}.txt"),
            PathBuf::from(r"C:\c"),
        ];
        let bytes = encode_message(&paths);
        assert_eq!(decode_message(&bytes).unwrap(), paths);
        assert_eq!(
            decode_message(&encode_message(&[])).unwrap(),
            Vec::<PathBuf>::new()
        );
        assert!(decode_message(&bytes[..bytes.len() - 2]).is_none(), "cut");
        let mut wrong = bytes.clone();
        wrong[0] ^= 1;
        assert!(decode_message(&wrong).is_none(), "magic");
        let mut count = bytes.clone();
        count[4] = 9;
        assert!(decode_message(&count).is_none(), "count");
        assert!(decode_message(&[]).is_none());
    }
    #[test]
    fn real_names_come_from_the_token() {
        let names = PipeNames::for_this_process().unwrap();
        assert!(names.base.starts_with("QuickEdit-S-1-"), "{}", names.base);
        let _ = names.pipe(names.level);
    }
}
