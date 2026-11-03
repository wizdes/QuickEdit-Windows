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
//!
//! Each launch that connects gets a short-lived thread of its own, so one that stalls holds up
//! nobody else, and a limit on the whole exchange ends that thread.
//!
//! The name is easy to work out, so a launch trusts nothing because it holds the name. It opens
//! at the identification level, so a server can learn who it is and cannot act as it. It sends
//! nothing until the server's process has this user and this level. A server's pipe has one
//! rule, for its own user. Another program of the same user at the same level has the same
//! rights as QuickEdit, and no check here can tell the two apart.
use crate::error::DocError;
use crate::identity::wide;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND,
    ERROR_INVALID_PARAMETER, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, LocalFree, STILL_ACTIVE, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, INFINITE,
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, ResetEvent, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows::core::{PCWSTR, PWSTR};

const MAGIC: u32 = 0x3145_5551; // "QUE1"
const MAX_MESSAGE_UNITS: usize = 1 << 20;
/// The most files one launch can hand over. Explorer starts one process for a few files, and
/// a long list goes through its own launch's queue instead.
pub const MAX_PATHS: usize = 1024;
/// The most files that wait for the UI. A launch past this runs on its own, and says so.
const MAX_QUEUED_PATHS: usize = 4096;
/// The most requests that wait for the UI, since one with no files still takes a place.
const MAX_QUEUED_REQUESTS: usize = 256;
/// The most launches served at once. Each one ends within two `SERVER_READ_WAIT`s.
const MAX_CLIENT_THREADS: usize = 64;
const TAKEN: u8 = 0x51;
/// A running instance answers in far less. Past this it counts as hung.
const CONNECT_WAIT: Duration = Duration::from_millis(1500);
/// How many times one launch connects when the instance ends under it.
const CONNECT_TRIES: u32 = 3;
const ANSWER_WAIT: Duration = Duration::from_millis(3000);
/// How long one launch has for its whole message, and again for reading its answer.
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
    /// The process that holds the pipe's name is not this user at this level, or Windows would
    /// not say who it is. It was sent nothing. Show this, then run as an instance of your own.
    UntrustedServer { why: String },
    /// The process that held the pipe ended during the connect, on every try. Nothing was sent.
    ServerGone,
    /// The server took the files and answered with a byte that QuickEdit never sends, so it is
    /// not QuickEdit, or not this version. Show this, then run as an instance of your own.
    WrongAnswer { byte: u8 },
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
            Self::UntrustedServer { why } => write!(
                f,
                "Another program holds the name QuickEdit uses to pass files between its windows, so it was sent nothing, and this launch opens its own window. {why}"
            ),
            Self::ServerGone => write!(
                f,
                "The running QuickEdit closed just as this launch reached it, so this launch opens its own window."
            ),
            Self::WrongAnswer { byte } => write!(
                f,
                "A program that answered as QuickEdit gave the answer 0x{byte:02x}, which QuickEdit never sends, so this launch opens its own window."
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
    /// Paths that are not files QuickEdit may open, each with its named error. Show them.
    pub refused: Vec<DocError>,
}

/// What happened to the server itself. Take these on each wake with
/// [`HandoffServer::take_events`], and show them: while the server is down, the next launch
/// runs as an instance of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffEvent {
    /// The server failed, with the reason, and it runs again. It restarts once.
    ServerRestarted { reason: String },
    /// The server failed and did not come back, with the reason. Later launches each run alone.
    ServerStopped { reason: String },
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
        // SAFETY: the pseudo handle is always valid.
        let identity = process_identity(unsafe { GetCurrentProcess() })?;
        let mut session = 0;
        // SAFETY: plain numbers, and `session` outlives the call.
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }
            .map_err(|e| windows_failure("reading the Windows session", &e))?;
        Ok(Self {
            base: format!("QuickEdit-{}-{session}", identity.sid),
            level: identity.level,
        })
    }
    fn pipe(&self, level: IntegrityLevel) -> Vec<u16> {
        let name = format!(r"\\.\pipe\{}-{}", self.base, level.suffix());
        wide(Path::new(&name))
    }
}

/// Who a process runs as: the two things the pipe's name stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    sid: String,
    level: IntegrityLevel,
}

fn process_identity(process: HANDLE) -> Result<Identity, HandoffError> {
    let mut token = HANDLE::default();
    // SAFETY: the caller gives an open process handle, and `token` outlives the call.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
        .map_err(|e| windows_failure("reading a process's token", &e))?;
    let result = token_identity(token);
    // SAFETY: the token is open, and this is its only close.
    if let Err(error) = unsafe { CloseHandle(token) } {
        eprintln!("QuickEdit: closing the token failed: {error}");
    }
    result
}

fn token_identity(token: HANDLE) -> Result<Identity, HandoffError> {
    let user = token_bytes(token, TokenUser)
        .map_err(|e| windows_failure("reading the user from the token", &e))?;
    // SAFETY: Windows filled the buffer with a TOKEN_USER, and it is aligned for it.
    let user_sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = PWSTR::null();
    // SAFETY: the SID lives in `user`, and `text` is freed below.
    unsafe { ConvertSidToStringSidW(user_sid, &mut text) }
        .map_err(|e| windows_failure("reading the user's name", &e))?;
    // SAFETY: Windows gave a string that ends in a zero.
    let sid = unsafe { text.to_string() }.unwrap_or_default();
    // SAFETY: the pointer came from the call above and is freed once.
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    let label = token_bytes(token, TokenIntegrityLevel)
        .map_err(|e| windows_failure("reading the integrity level", &e))?;
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
    Ok(Identity { sid, level })
}

/// The check a real launch makes on the process that holds the pipe: the same user and the same
/// level as this one. Windows refusing to say who it is counts as a failure of the check.
#[doc(hidden)]
pub fn server_is_this_user_and_level(server: u32) -> Result<(), HandoffError> {
    let refuse = |why: String| HandoffError::UntrustedServer { why };
    // SAFETY: the pseudo handle is always valid.
    let ours = process_identity(unsafe { GetCurrentProcess() })?;
    // SAFETY: a plain number. The handle is closed by `Owned`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, server) }
        .map(Owned)
        .map_err(|e| {
            // This is what Windows says for a process id that nothing holds any more. A process
            // of another user gives "access denied".
            if e.code() == ERROR_INVALID_PARAMETER.to_hresult() {
                HandoffError::ServerGone
            } else {
                refuse(format!("Windows would not open process {server}: {e}"))
            }
        })?;
    // Windows keeps an ended process's record while anything holds a handle on it, so its token
    // may still read as this user's. It can answer nobody.
    if process_has_ended(process.0) {
        return Err(HandoffError::ServerGone);
    }
    let theirs = process_identity(process.0).map_err(|e| {
        if process_has_ended(process.0) {
            HandoffError::ServerGone
        } else {
            refuse(format!("Windows would not read process {server}: {e}"))
        }
    })?;
    identities_match(&ours, &theirs).map_err(refuse)
}

fn process_has_ended(process: HANDLE) -> bool {
    let mut code = 0;
    // SAFETY: the caller gives an open process handle, and `code` outlives the call.
    let read = unsafe { GetExitCodeProcess(process, &mut code) };
    read.is_ok() && code != STILL_ACTIVE.0 as u32
}

fn identities_match(ours: &Identity, theirs: &Identity) -> Result<(), String> {
    if ours.sid != theirs.sid {
        return Err(format!(
            "It runs as user {}, and this launch runs as {}.",
            theirs.sid, ours.sid
        ));
    }
    if ours.level != theirs.level {
        return Err(format!(
            "It runs with {} rights, and this launch has {} rights.",
            theirs.level.words(),
            ours.level.words()
        ));
    }
    Ok(())
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
    hand_off_checked(names, paths, &server_is_this_user_and_level)
}

/// `hand_off` with the check on the server given by the caller, so that a test can make it fail.
/// The check gets the server's process id before any of this launch's files go out.
#[doc(hidden)]
pub fn hand_off_checked(
    names: &PipeNames,
    paths: &[PathBuf],
    check: &dyn Fn(u32) -> Result<(), HandoffError>,
) -> Result<bool, HandoffError> {
    if paths.len() > MAX_PATHS {
        return Err(HandoffError::Failed {
            what: format!(
                "{} files are too many to pass at once. The limit is {MAX_PATHS}",
                paths.len()
            ),
        });
    }
    // An instance that closes as this launch connects has been sent nothing yet, so the whole
    // connect is safe to try again. The next try usually finds no pipe, or the next instance.
    let mut tries = 1;
    loop {
        match connect_and_send(names, paths, check) {
            Err(HandoffError::ServerGone) if tries < CONNECT_TRIES => tries += 1,
            result => return result,
        }
    }
}

fn connect_and_send(
    names: &PipeNames,
    paths: &[PathBuf],
    check: &dyn Fn(u32) -> Result<(), HandoffError>,
) -> Result<bool, HandoffError> {
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
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
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
    let mut server = 0;
    // SAFETY: the pipe is open and `server` outlives the call.
    unsafe { GetNamedPipeServerProcessId(file.0, &mut server) }.map_err(|e| {
        if e.code() == ERROR_BROKEN_PIPE.to_hresult()
            || e.code() == ERROR_PIPE_NOT_CONNECTED.to_hresult()
        {
            HandoffError::ServerGone
        } else {
            HandoffError::UntrustedServer {
                why: format!("Windows would not say which process it is: {e}"),
            }
        }
    })?;
    check(server)?;
    // So that the instance may bring its window forward.
    // SAFETY: a plain number.
    if let Err(error) = unsafe { AllowSetForegroundWindow(server) } {
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
    if got != 1 {
        return Err(HandoffError::Failed {
            what: "the running QuickEdit refused the message".into(),
        });
    }
    if answer[0] != TAKEN {
        return Err(HandoffError::WrongAnswer { byte: answer[0] });
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
    events: Arc<Mutex<Vec<HandoffEvent>>>,
    stop: Arc<Owned>,
    broken: Arc<Owned>,
    clients: Arc<AtomicUsize>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl HandoffServer {
    /// The requests that came since the last call, oldest first. Never waits on a person.
    pub fn take_requests(&self) -> Vec<HandoffRequest> {
        match self.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
    /// What happened to the server since the last call. See [`HandoffEvent`].
    pub fn take_events(&self) -> Vec<HandoffEvent> {
        match self.events.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
    /// The app is about to end. The pipe closes, the launches being served finish, and what
    /// they asked comes back here. After this, a new launch finds no pipe and starts as the
    /// instance. A launch that heard "taken" is in the answer, so its files are never lost.
    /// This waits up to `limit`, so call it only when no window waits on this thread.
    pub fn stop_and_take(&self, limit: Duration) -> Vec<HandoffRequest> {
        // SAFETY: the event is open while this `Arc` lives.
        if let Err(error) = unsafe { SetEvent(self.stop.0) } {
            eprintln!("QuickEdit: stopping the handoff thread failed: {error}");
        }
        let thread = match self.thread.lock() {
            Ok(mut thread) => thread.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(thread) = thread
            && thread.join().is_err()
        {
            eprintln!("QuickEdit: the handoff thread stopped with a failure");
        }
        let end = Instant::now() + limit;
        while self.clients.load(Ordering::Acquire) > 0 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(1));
        }
        let left = self.clients.load(Ordering::Acquire);
        if left > 0 {
            eprintln!("QuickEdit: {left} launch(es) were still being served when the app ended");
        }
        self.take_requests()
    }
    /// Makes the server fail as a Windows error would, for a test.
    #[doc(hidden)]
    pub fn break_for_test(&self) {
        // SAFETY: the event is open while this `Arc` lives.
        if let Err(error) = unsafe { SetEvent(self.broken.0) } {
            eprintln!("QuickEdit: breaking the handoff thread failed: {error}");
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

/// One rule, for one user, and no rules from a parent. An elevated process also marks its pipe
/// with its level, because a pipe with no mark counts as normal level, and a normal program of
/// the same user could then write to it. Its text is what a test reads back.
fn pipe_rights_text(identity: &Identity) -> String {
    let label = match identity.level {
        IntegrityLevel::Low | IntegrityLevel::Medium => "",
        IntegrityLevel::High => "S:(ML;;NW;;;HI)",
        IntegrityLevel::System => "S:(ML;;NW;;;SI)",
    };
    format!("D:P(A;;GA;;;{}){label}", identity.sid)
}

fn pipe_rights(identity: &Identity) -> Vec<u16> {
    pipe_rights_text(identity)
        .encode_utf16()
        .chain([0])
        .collect()
}

fn create_pipe(pipe: &[u16], rights: &[u16], first: bool) -> windows::core::Result<Owned> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the text ends in a zero, and `descriptor` is freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(rights.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
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
            Some(&raw const attributes),
        )
    };
    let made = if handle == INVALID_HANDLE_VALUE {
        Err(windows::core::Error::from_thread())
    } else {
        Ok(Owned(handle))
    };
    // SAFETY: Windows made this copy for this call, and it is freed once.
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    made
}

/// Takes the pipe's name and starts listening. `Ok(None)`: another process holds the name.
pub fn listen(
    names: &PipeNames,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<Option<HandoffServer>, HandoffError> {
    let pipe = names.pipe(names.level);
    // SAFETY: the pseudo handle is always valid.
    let rights = pipe_rights(&process_identity(unsafe { GetCurrentProcess() })?);
    let first = match create_pipe(&pipe, &rights, true) {
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
    // SAFETY: no name and no attributes.
    let broken = Arc::new(Owned(
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|e| windows_failure("making an event", &e))?,
    ));
    let queue = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let clients = Arc::new(AtomicUsize::new(0));
    let shared = Shared {
        pipe,
        rights,
        queue: queue.clone(),
        events: events.clone(),
        stop: stop.clone(),
        broken: broken.clone(),
        wake,
        clients: clients.clone(),
    };
    let thread = std::thread::Builder::new()
        .name("qe-docs-handoff".into())
        .spawn(move || supervise(first, &shared))
        .map_err(|e| HandoffError::Failed {
            what: format!("starting the handoff thread: {e}"),
        })?;
    Ok(Some(HandoffServer {
        queue,
        events,
        stop,
        broken,
        clients,
        thread: Mutex::new(Some(thread)),
    }))
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

/// `start`, with the check on the server given by the caller, for a test.
#[doc(hidden)]
pub fn start_checked(
    names: &PipeNames,
    paths: &[PathBuf],
    wake: Arc<dyn Fn() + Send + Sync>,
    check: &dyn Fn(u32) -> Result<(), HandoffError>,
) -> LaunchRole {
    start_inner(names, paths, wake, &|| {}, check)
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
    start_inner(names, paths, wake, between, &server_is_this_user_and_level)
}

fn start_inner(
    names: &PipeNames,
    paths: &[PathBuf],
    wake: Arc<dyn Fn() + Send + Sync>,
    between: &dyn Fn(),
    check: &dyn Fn(u32) -> Result<(), HandoffError>,
) -> LaunchRole {
    let paths = absolute_paths(paths);
    // A loser of the race may arrive before the winner listens, so it tries a few times.
    let deadline = Instant::now() + CONNECT_WAIT;
    loop {
        match hand_off_checked(names, &paths, check) {
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

/// What the server's threads share.
struct Shared {
    pipe: Vec<u16>,
    rights: Vec<u16>,
    queue: Arc<Mutex<Vec<HandoffRequest>>>,
    events: Arc<Mutex<Vec<HandoffEvent>>>,
    stop: Arc<Owned>,
    broken: Arc<Owned>,
    wake: Arc<dyn Fn() + Send + Sync>,
    clients: Arc<AtomicUsize>,
}

impl Shared {
    fn tell(&self, event: HandoffEvent) {
        match self.events.lock() {
            Ok(mut events) => events.push(event),
            Err(poisoned) => poisoned.into_inner().push(event),
        }
        (self.wake)();
    }
}

/// Runs the server. The first failure brings it back once. A failure then reaches the user.
fn supervise(first: Owned, shared: &Shared) {
    let mut listening = first;
    let mut restarted = false;
    loop {
        let reason = match serve(listening, shared) {
            Ok(()) => return,
            Err(reason) => reason,
        };
        if restarted {
            shared.tell(HandoffEvent::ServerStopped { reason });
            return;
        }
        restarted = true;
        // A launch being served still holds the name, so then no other process can have taken
        // it, and only then may the new pipe skip the first-instance rule.
        let first = shared.clients.load(Ordering::Acquire) == 0;
        match create_pipe(&shared.pipe, &shared.rights, first) {
            Ok(pipe) => {
                listening = pipe;
                shared.tell(HandoffEvent::ServerRestarted { reason });
            }
            Err(error) => {
                shared.tell(HandoffEvent::ServerStopped {
                    reason: format!("{reason}. It could not start again: {error}"),
                });
                return;
            }
        }
    }
}

/// `Ok` when asked to stop. An error says why this thread cannot go on.
fn serve(mut listening: Owned, shared: &Shared) -> Result<(), String> {
    // SAFETY: no name and no attributes.
    let ready = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
        Ok(event) => Owned(event),
        Err(error) => {
            return Err(format!(
                "the handoff thread could not make an event: {error}"
            ));
        }
    };
    loop {
        // SAFETY: the event is open. It is set again when the next launch connects.
        unsafe { ResetEvent(ready.0) }.map_err(|e| format!("the handoff event failed: {e}"))?;
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
                unsafe { DisconnectNamedPipe(listening.0) }
                    .map_err(|e| format!("the handoff pipe could not reset: {e}"))?;
                continue;
            }
            Err(error) => return Err(format!("the handoff pipe failed: {error}")),
        };
        if !connected {
            let handles = [ready.0, shared.stop.0, shared.broken.0];
            // SAFETY: all are open. No timeout: this thread sleeps until a launch comes.
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
                if woke == WAIT_EVENT(WAIT_OBJECT_0.0 + 2) {
                    // SAFETY: the event is open.
                    unsafe { ResetEvent(shared.broken.0) }
                        .map_err(|e| format!("the handoff event failed: {e}"))?;
                    return Err("the handoff pipe was told to fail".to_owned());
                }
                return Ok(());
            }
        }
        // A new instance must exist before this one is busy, or a launch would find no pipe
        // and think it is the first.
        let next = create_pipe(&shared.pipe, &shared.rights, false)
            .map_err(|e| format!("the handoff pipe could not grow: {e}"))?;
        let client = std::mem::replace(&mut listening, next);
        serve_on_its_own_thread(client, shared);
    }
}

struct ClientCount(Arc<AtomicUsize>);
impl Drop for ClientCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn serve_on_its_own_thread(client: Owned, shared: &Shared) {
    let running = shared.clients.fetch_add(1, Ordering::AcqRel);
    let count = ClientCount(shared.clients.clone());
    if running >= MAX_CLIENT_THREADS {
        eprintln!("QuickEdit: too many launches at once, so one was dropped");
        return;
    }
    let (queue, wake) = (shared.queue.clone(), shared.wake.clone());
    let started = std::thread::Builder::new()
        .name("qe-docs-handoff-client".into())
        .spawn(move || {
            let _count = count;
            serve_client(&client, &queue, wake.as_ref());
        });
    if let Err(error) = started {
        eprintln!("QuickEdit: a launch was dropped, as no thread started for it: {error}");
    }
}

/// Paths that are not files a document may hold are named, not opened. Nothing here reads a
/// disk or a network.
fn split_request(paths: Vec<PathBuf>) -> HandoffRequest {
    let mut request = HandoffRequest {
        paths: Vec::with_capacity(paths.len()),
        refused: Vec::new(),
    };
    for path in paths {
        // This server's folder is not the launch's, so a relative path has no meaning here.
        if !path.is_absolute() {
            request.refused.push(DocError::new(
                crate::error::DocErrorKind::PathRefused,
                Some(&path),
                format!(
                    "QuickEdit cannot open {}: this is not a complete file path.",
                    path.display()
                ),
            ));
            continue;
        }
        match crate::paths::check_open_path(&path) {
            Ok(_) => request.paths.push(path),
            Err(error) => request.refused.push(error),
        }
    }
    request
}

/// False when the queue is full, so the launch must not hear that it was taken.
fn queue_request(queue: &Mutex<Vec<HandoffRequest>>, request: HandoffRequest) -> bool {
    let mut list = match queue.lock() {
        Ok(list) => list,
        Err(poisoned) => poisoned.into_inner(),
    };
    let waiting: usize = list.iter().map(|r| r.paths.len()).sum();
    if list.len() >= MAX_QUEUED_REQUESTS || waiting + request.paths.len() > MAX_QUEUED_PATHS {
        return false;
    }
    list.push(request);
    true
}

fn serve_client(
    client: &Owned,
    queue: &Mutex<Vec<HandoffRequest>>,
    wake: &(dyn Fn() + Send + Sync),
) {
    match read_request(client.0) {
        Some(paths) => {
            if queue_request(queue, split_request(paths)) {
                wake();
                if overlapped_write(client.0, &[TAKEN], SERVER_READ_WAIT).is_err() {
                    eprintln!("QuickEdit: a launch left before it heard the answer");
                }
                // A disconnect would drop an answer not yet read. The launch reads it and
                // closes, which ends this read. A launch that does neither has this long.
                let mut gone = [0u8; 1];
                let waited = overlapped_read(client.0, &mut gone, SERVER_READ_WAIT);
                drop(waited);
            } else {
                eprintln!("QuickEdit: too many files wait to open, so a launch was refused");
            }
        }
        None => eprintln!("QuickEdit: a bad or slow handoff message was dropped"),
    }
    // SAFETY: the handle is open.
    if let Err(error) = unsafe { DisconnectNamedPipe(client.0) } {
        eprintln!("QuickEdit: disconnecting a launch failed: {error}");
    }
}

/// The whole message within `SERVER_READ_WAIT`, however it is cut into pieces.
fn read_request(client: HANDLE) -> Option<Vec<PathBuf>> {
    let deadline = Instant::now() + SERVER_READ_WAIT;
    let read_all = |bytes: &mut [u8]| -> Option<()> {
        let mut got = 0;
        while got < bytes.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let count = overlapped_read(client, &mut bytes[got..], left).ok()?;
            if count == 0 {
                return None;
            }
            got += count;
        }
        Some(())
    };
    let mut head = [0u8; 12];
    read_all(&mut head)?;
    let units = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
    if units > MAX_MESSAGE_UNITS {
        return None;
    }
    let mut message = head.to_vec();
    message.resize(12 + units * 2, 0);
    read_all(&mut message[12..])?;
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
    fn another_user_or_another_level_fails_the_identity_comparison() {
        let ours = Identity {
            sid: "S-1-5-21-1".into(),
            level: IntegrityLevel::Medium,
        };
        assert_eq!(identities_match(&ours, &ours.clone()), Ok(()));
        let user = Identity {
            sid: "S-1-5-21-2".into(),
            ..ours.clone()
        };
        let shown = identities_match(&ours, &user).unwrap_err();
        assert!(shown.contains("S-1-5-21-2"), "{shown}");
        let level = Identity {
            level: IntegrityLevel::High,
            ..ours.clone()
        };
        let shown = identities_match(&ours, &level).unwrap_err();
        assert!(shown.contains("administrator"), "{shown}");
    }
    #[test]
    fn a_pipe_is_marked_with_its_level_only_when_elevated() {
        let identity = |level| Identity {
            sid: "S-1-5-21-1".into(),
            level,
        };
        for level in [IntegrityLevel::Low, IntegrityLevel::Medium] {
            assert_eq!(
                pipe_rights_text(&identity(level)),
                "D:P(A;;GA;;;S-1-5-21-1)"
            );
        }
        assert_eq!(
            pipe_rights_text(&identity(IntegrityLevel::High)),
            "D:P(A;;GA;;;S-1-5-21-1)S:(ML;;NW;;;HI)"
        );
        assert_eq!(
            pipe_rights_text(&identity(IntegrityLevel::System)),
            "D:P(A;;GA;;;S-1-5-21-1)S:(ML;;NW;;;SI)"
        );
    }
    /// Windows must read the mark as a level rule, not ignore it. The text goes through the same
    /// conversion as a real pipe's, with no pipe, as this test may run at normal level, where
    /// Windows refuses a mark above the process's own level.
    #[test]
    fn windows_reads_the_mark_back_from_the_text() {
        use windows::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
        use windows::Win32::Security::LABEL_SECURITY_INFORMATION;
        for (level, want) in [
            (IntegrityLevel::High, "S:(ML;;NW;;;HI)"),
            (IntegrityLevel::System, "S:(ML;;NW;;;SI)"),
        ] {
            let rights = pipe_rights(&Identity {
                sid: "S-1-5-21-1".into(),
                level,
            });
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            let mut text = windows::core::PWSTR::null();
            // SAFETY: the text ends in a zero, and each thing Windows gives is freed once.
            let read = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(rights.as_ptr()),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    None,
                )
                .unwrap();
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    SDDL_REVISION_1,
                    LABEL_SECURITY_INFORMATION,
                    &mut text,
                    None,
                )
                .unwrap();
                let read = text.to_string().unwrap();
                LocalFree(Some(HLOCAL(text.0.cast())));
                LocalFree(Some(HLOCAL(descriptor.0)));
                read
            };
            assert_eq!(read, want);
        }
    }
    #[test]
    fn real_names_come_from_the_token() {
        let names = PipeNames::for_this_process().unwrap();
        assert!(names.base.starts_with("QuickEdit-S-1-"), "{}", names.base);
        let _ = names.pipe(names.level);
    }
}
