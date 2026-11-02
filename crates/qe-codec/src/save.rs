//! Safe file writes: the new bytes reach the final name all at once or not at all.
//! The README next to `Cargo.toml` explains the rules and the recovery files.
use crate::CodecError;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_DIRECTORY, ERROR_FILE_EXISTS,
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_LOCK_VIOLATION, ERROR_NOT_SUPPORTED,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, ERROR_UNABLE_TO_REMOVE_REPLACED, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetFileSecurityW, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, SetSecurityDescriptorControl,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_DATA, GetFileInformationByHandle, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    OPEN_EXISTING, REPLACE_FILE_FLAGS, ReplaceFileW,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::PCWSTR;

/// About 10 tries over 500 ms, for a scanner or indexer that holds a file briefly.
const TRIES: u32 = 10;
const TRY_DELAY: Duration = Duration::from_millis(50);
const NAME_ATTEMPTS: u32 = 64;
const COPY_CHUNK: usize = 1024 * 1024;
const STILL_ACTIVE_CODE: u32 = 259;

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn win32_code(error: &windows::core::Error) -> u32 {
    (error.code().0 as u32) & 0xffff
}
fn io_code(error: &std::io::Error) -> u32 {
    error.raw_os_error().map_or(0, |code| code as u32)
}
/// The `\\?\` form, which raw calls need for paths over 260 characters.
fn verbatim(path: &Path) -> PathBuf {
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    let text = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    if units.starts_with(&text(r"\\?\")) {
        return path.to_path_buf();
    }
    let mut out = if units.starts_with(&text(r"\\")) {
        let mut out = text(r"\\?\UNC\");
        out.extend_from_slice(&units[2..]);
        out
    } else {
        let mut out = text(r"\\?\");
        out.extend_from_slice(&units);
        out
    };
    out.shrink_to_fit();
    OsString::from_wide(&out).into()
}
/// The path for messages: the plain form when it is short enough to work as typed.
fn friendly(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.len() >= 248 {
        return path.to_path_buf();
    }
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\")
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// What a leftover's name records about its target: a hash of the target's name.
/// Case does not count, as on NTFS.
fn name_hash(name: &std::ffi::OsStr) -> u32 {
    let lower = name.to_string_lossy().to_lowercase();
    lower.encode_utf16().fold(0x811c_9dc5u32, |hash, unit| {
        (hash ^ u32::from(unit)).wrapping_mul(0x0100_0193)
    })
}
fn leftover_name(hash: u32, pid: u32, random: u64, extension: &str) -> String {
    format!(".qe-{hash:08x}-{pid}-{random:016x}.{extension}")
}
struct ParsedName {
    hash: u32,
    pid: u32,
    kind: LeftoverKind,
}
fn parse_leftover_name(name: &str) -> Option<ParsedName> {
    let rest = name.strip_prefix(".qe-")?;
    let (rest, kind) = if let Some(rest) = rest.strip_suffix(".tmp") {
        (rest, LeftoverKind::Temp)
    } else {
        (rest.strip_suffix(".bak")?, LeftoverKind::Backup)
    };
    let mut parts = rest.split('-');
    let (hash, pid, random) = (parts.next()?, parts.next()?, parts.next()?);
    let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    if parts.next().is_some() || !hex(hash, 8) || !hex(random, 16) {
        return None;
    }
    if !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(ParsedName {
        hash: u32::from_str_radix(hash, 16).ok()?,
        pid: pid.parse().ok()?,
        kind,
    })
}

/// The steps of a save that tests must be able to replace.
trait SaveEnv {
    fn replace(&mut self, target: &Path, temp: &Path, backup: &Path) -> Result<(), u32> {
        let (target, temp, backup) = (wide_path(target), wide_path(temp), wide_path(backup));
        // SAFETY: the three paths end in a zero and outlive the call; the other
        // arguments are the documented empty values.
        unsafe {
            ReplaceFileW(
                PCWSTR(target.as_ptr()),
                PCWSTR(temp.as_ptr()),
                PCWSTR(backup.as_ptr()),
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        }
        .map_err(|e| win32_code(&e))
    }
    fn move_no_replace(&mut self, from: &Path, to: &Path) -> Result<(), u32> {
        let (from, to) = (wide_path(from), wide_path(to));
        // SAFETY: both paths end in a zero and outlive the call. Without
        // MOVEFILE_REPLACE_EXISTING the call refuses an existing `to`.
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|e| win32_code(&e))
    }
    fn random(&mut self) -> u64 {
        RandomState::new().build_hasher().finish()
    }
    /// The temp file exists and no byte has been written.
    fn temp_created(&mut self, _temp: &Path) {}
    /// All bytes are on disk and the temp file is closed.
    fn before_commit(&mut self, _temp: &Path) {}
    fn sleep(&mut self, time: Duration) {
        std::thread::sleep(time);
    }
}
struct RealEnv;
impl SaveEnv for RealEnv {}

/// How to tell whether a file is still the file it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileState {
    volume: u32,
    index: u64,
    size: u64,
    written: u64,
}
impl FileState {
    fn same_file(&self, other: &Self) -> bool {
        self.volume == other.volume && self.index == other.index
    }
}
/// `None` when nothing is at that path. An open for no access works even while
/// another program holds the file with no sharing.
fn file_state(path: &Path) -> Result<Option<FileState>, u32> {
    let name = wide_path(path);
    // SAFETY: the path ends in a zero and outlives the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let handle = match handle {
        Ok(handle) => handle,
        Err(e) => {
            let code = win32_code(&e);
            let missing = code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0;
            return if missing { Ok(None) } else { Err(code) };
        }
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` is open and `info` is a live struct of the right type.
    let result = unsafe { GetFileInformationByHandle(handle, &mut info) };
    // SAFETY: `handle` is open and is not used again.
    if let Err(e) = unsafe { CloseHandle(handle) } {
        return Err(win32_code(&e));
    }
    result.map_err(|e| win32_code(&e))?;
    let join = |high: u32, low: u32| (u64::from(high) << 32) | u64::from(low);
    Ok(Some(FileState {
        volume: info.dwVolumeSerialNumber,
        index: join(info.nFileIndexHigh, info.nFileIndexLow),
        size: join(info.nFileSizeHigh, info.nFileSizeLow),
        written: join(
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        ),
    }))
}

/// The target's access rules, ready to give to a new file.
struct AccessRules {
    // u64 gives the alignment a security descriptor needs.
    descriptor: Vec<u64>,
}
impl AccessRules {
    fn pointer(&mut self) -> PSECURITY_DESCRIPTOR {
        PSECURITY_DESCRIPTOR(self.descriptor.as_mut_ptr().cast())
    }
}
/// Reads the target's access rules. `None` on a file system that has none, such
/// as FAT or exFAT, where a new file cannot be wider than the target. The rules
/// are marked protected so that Windows keeps them exactly and does not mix in
/// rules inherited from the folder.
fn read_access_rules(path: &Path) -> Result<Option<AccessRules>, u32> {
    let name = wide_path(path);
    let mut needed = 0u32;
    // SAFETY: the path ends in a zero and outlives the call; with no buffer the call
    // only reports the size.
    let done = unsafe {
        GetFileSecurityW(
            PCWSTR(name.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &mut needed,
        )
    };
    if !done.as_bool() && needed == 0 {
        // SAFETY: the call takes no arguments.
        let code = unsafe { GetLastError() }.0;
        return if code == ERROR_NOT_SUPPORTED.0 || code == ERROR_INVALID_FUNCTION.0 {
            Ok(None)
        } else {
            Err(code)
        };
    }
    let mut rules = AccessRules {
        descriptor: vec![0u64; (needed as usize).div_ceil(8).max(1)],
    };
    let pointer = rules.pointer();
    // SAFETY: as above, and `descriptor` holds at least `needed` bytes.
    let done = unsafe {
        GetFileSecurityW(
            PCWSTR(name.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(pointer),
            needed,
            &mut needed,
        )
    };
    if !done.as_bool() {
        // SAFETY: the call takes no arguments.
        return Err(unsafe { GetLastError() }.0);
    }
    // SAFETY: `pointer` holds the valid descriptor that the call just wrote.
    unsafe { SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED) }
        .map_err(|e| win32_code(&e))?;
    Ok(Some(rules))
}

/// Deletes a file of ours and returns its path if it could not go. A scanner may
/// hold a new file for a moment, so this tries a few times.
fn delete_ours(path: &Path) -> Option<PathBuf> {
    for attempt in 0..3 {
        match fs::remove_file(path) {
            Ok(()) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) if attempt < 2 => std::thread::sleep(TRY_DELAY),
            Err(_) => {}
        }
    }
    Some(friendly(path))
}

/// Maps a Windows code to the error for a save whose original is in the state
/// that `intact` reports.
fn save_error(code: u32, path: &Path, intact: bool, leftover: Option<PathBuf>) -> CodecError {
    let path = friendly(path);
    if intact {
        if code == ERROR_ACCESS_DENIED.0 {
            return CodecError::AccessDenied {
                path,
                code,
                leftover,
            };
        }
        if code == ERROR_SHARING_VIOLATION.0 || code == ERROR_LOCK_VIOLATION.0 {
            return CodecError::SharingViolation {
                path,
                code,
                leftover,
            };
        }
    }
    CodecError::SaveFailed {
        path,
        code,
        original_intact: intact,
        leftover,
    }
}

/// Opens the target for `access`, with all sharing, and returns the failure code.
fn probe_access(path: &Path, access: u32) -> Option<u32> {
    let name = wide_path(path);
    // SAFETY: the path ends in a zero and outlives the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    match handle {
        Ok(handle) => {
            // SAFETY: `handle` is open and is not used again.
            unsafe { CloseHandle(handle) }.err().map(|e| win32_code(&e))
        }
        Err(e) => Some(win32_code(&e)),
    }
}

enum Resolved {
    Existing(PathBuf),
    New(PathBuf),
}
/// Finds the real file behind `path`. A link is followed, so that a save writes
/// its target and keeps the link. The result is in the `\\?\` form.
fn resolve_target(path: &Path) -> Result<Resolved, CodecError> {
    let absolute = std::path::absolute(path)?;
    match fs::symlink_metadata(&absolute) {
        Ok(meta) if meta.file_type().is_symlink() => match fs::canonicalize(&absolute) {
            Ok(real) => Ok(Resolved::Existing(real)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => resolve_dangling_link(&absolute),
            Err(e) => Err(save_error(io_code(&e), &absolute, true, None)),
        },
        // Some file system drivers cannot report a final path. The name as given
        // still works, in `\\?\` form; only a parent link stays unresolved.
        Ok(_) => Ok(Resolved::Existing(
            fs::canonicalize(&absolute).unwrap_or_else(|_| verbatim(&absolute)),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new_file_in_real_folder(&absolute)
            .map_err(|e| {
                save_error(
                    io_code(&e),
                    absolute.parent().unwrap_or(&absolute),
                    true,
                    None,
                )
            }),
        Err(e) => Err(save_error(io_code(&e), &absolute, true, None)),
    }
}
fn new_file_in_real_folder(absolute: &Path) -> std::io::Result<Resolved> {
    let (Some(folder), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return Err(std::io::Error::from_raw_os_error(
            ERROR_PATH_NOT_FOUND.0 as i32,
        ));
    };
    let folder = fs::canonicalize(folder).unwrap_or_else(|_| verbatim(folder));
    if !folder.is_dir() {
        return Err(std::io::Error::from_raw_os_error(
            ERROR_PATH_NOT_FOUND.0 as i32,
        ));
    }
    Ok(Resolved::New(folder.join(name)))
}
/// A link to nothing. The save creates the file the link names if that file's
/// folder exists; otherwise the error names both paths.
fn resolve_dangling_link(link: &Path) -> Result<Resolved, CodecError> {
    let mut current = link.to_path_buf();
    for _ in 0..64 {
        let Ok(next) = fs::read_link(&current) else {
            break;
        };
        let joined = match current.parent() {
            Some(folder) => folder.join(next),
            None => next,
        };
        current = std::path::absolute(joined)?;
    }
    new_file_in_real_folder(&current).map_err(|_| CodecError::DanglingLink {
        link: friendly(link),
        target: friendly(&current),
    })
}
/// The checks that come before any byte is written. Returns the state to compare
/// against later, and the rules a temp file must have.
fn preflight_existing(target: &Path) -> Result<(FileState, Option<AccessRules>), CodecError> {
    let meta = fs::metadata(target).map_err(|e| save_error(io_code(&e), target, true, None))?;
    if meta.is_dir() {
        return Err(save_error(ERROR_DIRECTORY.0, target, true, None));
    }
    if meta.permissions().readonly() {
        return Err(CodecError::ReadOnly(friendly(target)));
    }
    // A rule that denies writing must stop a save, though ReplaceFileW alone would
    // succeed. A sharing violation here is not final: the retries come later.
    if let Some(code) = probe_access(target, FILE_WRITE_DATA.0)
        && code != ERROR_SHARING_VIOLATION.0
        && code != ERROR_LOCK_VIOLATION.0
    {
        return Err(save_error(code, target, true, None));
    }
    let state = match file_state(target) {
        Ok(Some(state)) => state,
        Ok(None) => return Err(save_error(ERROR_FILE_NOT_FOUND.0, target, true, None)),
        Err(code) => return Err(save_error(code, target, true, None)),
    };
    let rules = read_access_rules(target).map_err(|code| save_error(code, target, true, None))?;
    Ok((state, rules))
}
fn sibling_with_new_name(
    target: &Path,
    extension: &str,
    env: &mut impl SaveEnv,
    mut create: impl FnMut(&Path) -> Result<(), u32>,
) -> Result<PathBuf, CodecError> {
    let folder = target.parent().unwrap_or(target);
    let hash = name_hash(target.file_name().unwrap_or_default());
    let mut last = ERROR_FILE_EXISTS.0;
    for _ in 0..NAME_ATTEMPTS {
        let name = leftover_name(hash, std::process::id(), env.random(), extension);
        let candidate = folder.join(name);
        match create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(code) if code == ERROR_FILE_EXISTS.0 || code == ERROR_ALREADY_EXISTS.0 => {
                last = code;
            }
            Err(code) => return Err(save_error(code, folder, true, None)),
        }
    }
    Err(save_error(last, folder, true, None))
}
/// Creates a new file with the given rules in place from its first moment.
fn create_new_with_rules(path: &Path, rules: Option<&mut AccessRules>) -> Result<File, u32> {
    let name = wide_path(path);
    let attributes = rules.map(|rules| SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: rules.pointer().0,
        bInheritHandle: false.into(),
    });
    // SAFETY: the path ends in a zero, and `attributes` and the descriptor it names
    // outlive the call.
    let handle: HANDLE = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ,
            attributes.as_ref().map(|a| a as *const SECURITY_ATTRIBUTES),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| win32_code(&e))?;
    // SAFETY: the handle was just opened for this file and nothing else owns it.
    Ok(unsafe { File::from_raw_handle(handle.0) })
}
fn create_temp(
    target: &Path,
    mut rules: Option<AccessRules>,
    env: &mut impl SaveEnv,
) -> Result<(PathBuf, File), CodecError> {
    let mut opened = None;
    let path = sibling_with_new_name(target, "tmp", env, |candidate| {
        opened = Some(create_new_with_rules(candidate, rules.as_mut())?);
        Ok(())
    })?;
    env.temp_created(&path);
    let file = opened.ok_or_else(|| save_error(ERROR_FILE_NOT_FOUND.0, &path, true, None))?;
    Ok((path, file))
}
/// ERROR_UNABLE_TO_REMOVE_REPLACED hides the cause. This finds it.
fn real_cause(code: u32, target: &Path) -> u32 {
    if code == ERROR_UNABLE_TO_REMOVE_REPLACED.0
        && let Some(cause) = probe_access(target, DELETE.0)
    {
        return cause;
    }
    code
}
/// Puts a finished temp file over an existing target, with the original kept
/// aside under a reserved name until the swap has worked.
fn commit_replace(
    target: &Path,
    temp: &Path,
    before: FileState,
    env: &mut impl SaveEnv,
) -> Result<AtomicWriteResult, CodecError> {
    let backup = sibling_with_new_name(target, "bak", env, |candidate| {
        create_new_with_rules(candidate, None).map(drop)
    });
    let backup = match backup {
        Ok(backup) => backup,
        Err(error) => {
            let leftover = delete_ours(temp);
            return Err(with_leftover(error, leftover));
        }
    };
    let placeholder = file_state(&backup).ok().flatten();
    let result = retry_replace(target, temp, &backup, env);
    match result {
        Ok(()) => Ok(match delete_ours(&backup) {
            None => AtomicWriteResult::Replaced,
            Some(backup) => AtomicWriteResult::ReplacedBackupKept { backup },
        }),
        Err(code) => Err(after_failed_replace(
            target,
            temp,
            &backup,
            placeholder,
            before,
            code,
            env,
        )),
    }
}
fn retry_replace(
    target: &Path,
    temp: &Path,
    backup: &Path,
    env: &mut impl SaveEnv,
) -> Result<(), u32> {
    let mut tries = 0;
    loop {
        match env.replace(target, temp, backup) {
            Ok(()) => return Ok(()),
            Err(code) if is_passing(code) && tries + 1 < TRIES => {
                tries += 1;
                env.sleep(TRY_DELAY);
            }
            Err(code) => return Err(code),
        }
    }
}
/// Codes that a scanner or indexer can cause for a moment. A lasting access rule
/// was already caught before the temp file was made.
fn is_passing(code: u32) -> bool {
    code == ERROR_SHARING_VIOLATION.0
        || code == ERROR_LOCK_VIOLATION.0
        || code == ERROR_UNABLE_TO_REMOVE_REPLACED.0
        || code == ERROR_ACCESS_DENIED.0
}
fn with_leftover(error: CodecError, found: Option<PathBuf>) -> CodecError {
    match error {
        CodecError::AccessDenied { path, code, .. } => CodecError::AccessDenied {
            path,
            code,
            leftover: found,
        },
        CodecError::SharingViolation { path, code, .. } => CodecError::SharingViolation {
            path,
            code,
            leftover: found,
        },
        CodecError::SaveFailed {
            path,
            code,
            original_intact,
            ..
        } => CodecError::SaveFailed {
            path,
            code,
            original_intact,
            leftover: found,
        },
        other => other,
    }
}
/// Looks at the disk to learn what the failed replace did, and says only what it
/// found. The temp file goes only when the original is known to be safe.
fn after_failed_replace(
    target: &Path,
    temp: &Path,
    backup: &Path,
    placeholder: Option<FileState>,
    before: FileState,
    code: u32,
    env: &mut impl SaveEnv,
) -> CodecError {
    let now = file_state(target);
    let aside = file_state(backup);
    let original_is_aside = matches!(aside, Ok(Some(state)) if state.same_file(&before));
    let drop_placeholder = |aside: &Result<Option<FileState>, u32>| -> Option<PathBuf> {
        match (aside, placeholder) {
            (Ok(Some(state)), Some(reserved)) if state.same_file(&reserved) => delete_ours(backup),
            _ => None,
        }
    };
    if matches!(now, Ok(Some(state)) if state == before) {
        let temp_left = delete_ours(temp);
        let leftover = temp_left.or(drop_placeholder(&aside));
        return save_error(real_cause(code, target), target, true, leftover);
    }
    if original_is_aside && matches!(now, Ok(None)) {
        // The original was moved aside and the new file did not take its name.
        let restored = env.move_no_replace(backup, target).is_ok()
            && matches!(file_state(target), Ok(Some(state)) if state == before);
        if restored {
            let leftover = delete_ours(temp);
            return save_error(code, target, true, leftover);
        }
    }
    if original_is_aside {
        return CodecError::RecoveryRequired {
            original: friendly(target),
            backup: friendly(backup),
            new_text: Some(friendly(temp)),
            code,
        };
    }
    // The original is neither where it was nor aside, or another program changed it.
    // The temp file stays: it may be the only copy of this text on disk.
    // An empty reservation that cannot go now is harmless: `find_leftovers` lists it
    // and does not offer it.
    let _ = drop_placeholder(&aside);
    CodecError::SaveFailed {
        path: friendly(target),
        code,
        original_intact: false,
        leftover: Some(friendly(temp)),
    }
}
/// Puts a finished temp file at a name where nothing was. It never replaces.
fn commit_new(target: &Path, temp: &Path, env: &mut impl SaveEnv) -> Result<(), CodecError> {
    let mut tries = 0;
    let code = loop {
        match env.move_no_replace(temp, target) {
            Ok(()) => return Ok(()),
            Err(code) if is_passing(code) && tries + 1 < TRIES => {
                tries += 1;
                env.sleep(TRY_DELAY);
            }
            Err(code) => break code,
        }
    };
    let leftover = delete_ours(temp);
    if code == ERROR_ALREADY_EXISTS.0 || code == ERROR_FILE_EXISTS.0 {
        return Err(CodecError::TargetExists {
            path: friendly(target),
            leftover,
        });
    }
    Err(save_error(code, target, true, leftover))
}
/// A failure while the temp file was being written: the target has not been
/// touched by this save, and a check confirms it.
fn after_failed_write(
    error: std::io::Error,
    target: &Path,
    temp: &Path,
    before: Option<FileState>,
) -> CodecError {
    let leftover = delete_ours(temp);
    let intact = match before {
        Some(before) => matches!(file_state(target), Ok(Some(state)) if state == before),
        None => true,
    };
    save_error(io_code(&error), target, intact, leftover)
}

#[derive(Debug, PartialEq, Eq)]
pub enum AtomicWriteResult {
    /// The target now holds the new bytes.
    Replaced,
    /// The target now holds the new bytes, but the backup of the old bytes could
    /// not be deleted. It is safe to delete.
    ReplacedBackupKept { backup: PathBuf },
}
/// Writes `bytes` to `path` so that the file holds either all the old bytes or all
/// the new ones. A failed save is always an error.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<AtomicWriteResult, CodecError> {
    atomic_write_with(path, bytes, &mut RealEnv)
}
fn atomic_write_with(
    path: &Path,
    bytes: &[u8],
    env: &mut impl SaveEnv,
) -> Result<AtomicWriteResult, CodecError> {
    let (target, before, rules) = match resolve_target(path)? {
        Resolved::Existing(target) => {
            let (state, rules) = preflight_existing(&target)?;
            (target, Some(state), rules)
        }
        Resolved::New(target) => (target, None, None),
    };
    let (temp, mut file) = create_temp(&target, rules, env)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        return Err(after_failed_write(error, &target, &temp, before));
    }
    env.before_commit(&temp);
    match before {
        Some(before) => commit_replace(&target, &temp, before, env),
        None => commit_new(&target, &temp, env).map(|()| AtomicWriteResult::Replaced),
    }
}

/// A complete copy under a temp name in the target's folder. Nothing is at the
/// final name until `commit`. Dropping it deletes the temp file.
#[derive(Debug)]
pub struct PreparedCopy {
    temp: PathBuf,
    target: PathBuf,
    /// What the target was when the copy was prepared. `None` for a new name.
    before: Option<FileState>,
    done: bool,
}
impl PreparedCopy {
    pub fn temp_path(&self) -> &Path {
        &self.temp
    }
    /// Puts the copy at the final name, all at once. A target that existed at
    /// `prepare_large_copy` is replaced, with the same guarantees as `atomic_write`.
    /// A new name stays new: a file that appeared meanwhile gives `TargetExists`.
    pub fn commit(mut self) -> Result<AtomicWriteResult, CodecError> {
        self.done = true;
        let env = &mut RealEnv;
        match self.before {
            Some(before) => commit_replace(&self.target, &self.temp, before, env),
            None => commit_new(&self.target, &self.temp, env).map(|()| AtomicWriteResult::Replaced),
        }
    }
}
impl Drop for PreparedCopy {
    fn drop(&mut self) {
        if !self.done {
            // `find_leftovers` reports this file if it cannot go now.
            let _ = delete_ours(&self.temp);
        }
    }
}
fn copy_failure(error: &std::io::Error, copy: &PreparedCopy) -> CodecError {
    let intact = match copy.before {
        Some(before) => matches!(file_state(&copy.target), Ok(Some(state)) if state == before),
        None => true,
    };
    save_error(io_code(error), &copy.target, intact, None)
}
fn prepare_copy(
    input: &Path,
    dest: &Path,
    must_be_new: bool,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &mut dyn FnMut() -> bool,
    env: &mut impl SaveEnv,
) -> Result<PreparedCopy, CodecError> {
    let mut source = File::open(input)?;
    let total = source.metadata()?.len();
    let (target, before, rules) = match resolve_target(dest)? {
        Resolved::Existing(target) => {
            if must_be_new {
                return Err(CodecError::TargetExists {
                    path: friendly(&target),
                    leftover: None,
                });
            }
            let (state, rules) = preflight_existing(&target)?;
            (target, Some(state), rules)
        }
        Resolved::New(target) => (target, None, None),
    };
    let (temp, file) = create_temp(&target, rules, env)?;
    let prepared = PreparedCopy {
        temp,
        target,
        before,
        done: false,
    };
    // Declared after `prepared`, so on an early return the file closes before
    // `prepared` deletes it.
    let mut output = file;
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut done = 0;
    loop {
        if cancel() {
            return Err(CodecError::Cancelled);
        }
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        output
            .write_all(&buffer[..n])
            .map_err(|e| copy_failure(&e, &prepared))?;
        done += n as u64;
        progress(done, total);
    }
    output.sync_all().map_err(|e| copy_failure(&e, &prepared))?;
    Ok(prepared)
}
/// Copies `input` to a temp name beside `dest`, ready for `PreparedCopy::commit`.
/// This is the safe way to Save As a file too large to hold in memory, over an
/// existing file or to a new name. It writes nothing under the final name.
pub fn prepare_large_copy(
    input: &Path,
    dest: &Path,
    mut progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<PreparedCopy, CodecError> {
    prepare_copy(input, dest, false, &mut progress, &mut cancel, &mut RealEnv)
}
/// Copies `input` to a new file at `dest`. A file already at `dest` gives
/// `TargetExists`. A crash cannot leave a partial file under the final name.
pub fn copy_large_original(
    input: &Path,
    dest: &Path,
    mut progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<(), CodecError> {
    prepare_copy(input, dest, true, &mut progress, &mut cancel, &mut RealEnv)?
        .commit()
        .map(drop)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftoverKind {
    /// New text of a save that never finished. It may be partial.
    Temp,
    /// The old bytes of a target, moved aside during a save.
    Backup,
}
/// One file left behind by a save that did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    pub path: PathBuf,
    pub kind: LeftoverKind,
    pub size: u64,
    pub modified: SystemTime,
    /// The document this file most likely belongs to. The name holds only a hash
    /// of the document's name, so this is `None` when no file in the folder matches
    /// and no document was named in the call.
    pub likely_target: Option<PathBuf>,
    /// True when no file matching the document's name is there now.
    pub target_missing: bool,
    /// True when the process that made this file still runs. Its save may still be
    /// going on, so the file must be left alone for now.
    pub owner_running: bool,
    /// True when the app should offer recovery to the user. See the README.
    pub offer_recovery: bool,
}
fn process_is_running(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    // SAFETY: the call takes plain values and the handle is closed below.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            let mut code = 0u32;
            // SAFETY: `handle` is open and `code` is a live u32.
            let asked = unsafe { GetExitCodeProcess(handle, &mut code) };
            // SAFETY: `handle` is open and is not used again.
            let closed = unsafe { CloseHandle(handle) };
            closed.is_err() || asked.is_err() || code == STILL_ACTIVE_CODE
        }
        // Denied means a process with this number exists but belongs to someone else.
        Err(e) => win32_code(&e) == ERROR_ACCESS_DENIED.0,
    }
}
/// Finds the temp and backup files of ours in a folder, or the ones that belong to
/// one document if `folder_or_path` is a file or a path where a file used to be.
/// It deletes nothing and changes nothing. Files with other names are ignored.
pub fn find_leftovers(folder_or_path: &Path) -> Result<Vec<Leftover>, CodecError> {
    let absolute = std::path::absolute(folder_or_path)?;
    let (folder, document) = if absolute.is_dir() {
        (absolute, None)
    } else {
        let real = match resolve_target(&absolute)? {
            Resolved::Existing(real) | Resolved::New(real) => real,
        };
        let folder = real
            .parent()
            .ok_or_else(|| save_error(ERROR_PATH_NOT_FOUND.0, &real, true, None))?
            .to_path_buf();
        (folder, Some(real))
    };
    let wanted = document.as_deref().and_then(Path::file_name).map(name_hash);
    let mut found = Vec::new();
    let mut documents: HashMap<u32, PathBuf> = HashMap::new();
    for entry in fs::read_dir(&folder)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        match parse_leftover_name(&text) {
            Some(parsed) if wanted.is_none_or(|hash| hash == parsed.hash) => {
                let meta = entry.metadata()?;
                found.push((entry.path(), parsed, meta.len(), meta.modified()?));
            }
            Some(_) => {}
            None => {
                documents
                    .entry(name_hash(&name))
                    .or_insert_with(|| entry.path());
            }
        }
    }
    let mut leftovers = Vec::with_capacity(found.len());
    for (path, parsed, size, modified) in found {
        let present = documents.get(&parsed.hash).cloned();
        let target_missing = present.is_none();
        let likely_target = present.or_else(|| document.clone()).map(|p| friendly(&p));
        let owner_running = process_is_running(parsed.pid);
        let useful = match parsed.kind {
            LeftoverKind::Backup => size > 0,
            LeftoverKind::Temp => size > 0 && target_missing,
        };
        leftovers.push(Leftover {
            path: friendly(&path),
            kind: parsed.kind,
            size,
            modified,
            likely_target,
            target_missing,
            owner_running,
            offer_recovery: useful && !owner_running,
        });
    }
    leftovers.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(leftovers)
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;
