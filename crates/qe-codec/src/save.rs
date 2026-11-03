//! Safe file writes: the new bytes reach the final name all at once or not at all.
//! The README next to `Cargo.toml` explains the rules and the recovery files.
use crate::CodecError;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_CANT_RESOLVE_FILENAME,
    ERROR_DIRECTORY, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION,
    ERROR_LOCK_VIOLATION, ERROR_NOT_A_REPARSE_POINT, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND,
    ERROR_SHARING_VIOLATION, ERROR_UNABLE_TO_REMOVE_REPLACED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetFileSecurityW, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    SetFileSecurityW,
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
use windows::core::{HSTRING, PCWSTR};

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
/// The plain form of a path when it is short enough to work as typed. It works on the
/// path's own units, because a Windows name need not be valid Unicode. Going through
/// text would swap such a unit for U+FFFD and name a file that does not exist.
fn friendly(path: &Path) -> PathBuf {
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.len() >= 248 {
        return path.to_path_buf();
    }
    let text = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    if let Some(rest) = units.strip_prefix(&text(r"\\?\UNC\")[..]) {
        let mut out = text(r"\\");
        out.extend_from_slice(rest);
        return OsString::from_wide(&out).into();
    }
    match units.strip_prefix(&text(r"\\?\")[..]) {
        Some(rest) => OsString::from_wide(rest).into(),
        None => path.to_path_buf(),
    }
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
    fn write_temp(&mut self, file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
        file.write_all(bytes)
    }
    fn flush_temp(&mut self, file: &mut File) -> std::io::Result<()> {
        file.sync_all()
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct FileState {
    volume: u32,
    index: u64,
    size: u64,
    written: u64,
}
impl FileState {
    fn same_file(&self, other: &Self) -> bool {
        self.volume == other.volume && self.index == other.index
    }
    /// Same size and same time. This survives a rename on a file system whose file
    /// numbers come from a folder slot, where `same_file` does not.
    fn same_content_stamp(&self, other: &Self) -> bool {
        self.size == other.size && self.written == other.written
    }
}
fn handle_state(handle: HANDLE) -> Result<FileState, u32> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` is open and `info` is a live struct of the right type.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(|e| win32_code(&e))?;
    let join = |high: u32, low: u32| (u64::from(high) << 32) | u64::from(low);
    Ok(FileState {
        volume: info.dwVolumeSerialNumber,
        index: join(info.nFileIndexHigh, info.nFileIndexLow),
        size: join(info.nFileSizeHigh, info.nFileSizeLow),
        written: join(
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        ),
    })
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
    let state = handle_state(handle);
    // SAFETY: `handle` is open and is not used again.
    if let Err(e) = unsafe { CloseHandle(handle) } {
        return Err(win32_code(&e));
    }
    state.map(Some)
}
/// The path for an error, but only if something is there. A check that fails is not
/// proof that the file went, so that case still names it.
fn if_on_disk(path: &Path) -> Option<PathBuf> {
    match file_state(path) {
        Ok(None) => None,
        Ok(Some(_)) | Err(_) => Some(friendly(path)),
    }
}

/// A file being read from start to end by a copy or a decode. Any change to it gives
/// `SourceChanged`, so a caller cannot use a mix of two versions, or a partial one.
///
/// Reads stop as soon as the file is longer or shorter than it was at the open, so
/// a file that grows cannot use up memory or carry a progress count past its total.
pub(crate) struct WatchedSource {
    file: File,
    /// The `\\?\` form, so that the check works on any path `File::open` accepts.
    path: PathBuf,
    start: FileState,
    position: u64,
}
impl WatchedSource {
    pub(crate) fn open(input: &Path) -> Result<Self, CodecError> {
        let file = File::open(input)?;
        let handle = HANDLE(file.as_raw_handle());
        let start =
            handle_state(handle).map_err(|code| std::io::Error::from_raw_os_error(code as i32))?;
        Ok(Self {
            file,
            path: verbatim(&std::path::absolute(input)?),
            start,
            position: 0,
        })
    }
    pub(crate) fn size(&self) -> u64 {
        self.start.size
    }
    pub(crate) fn shown_path(&self) -> PathBuf {
        friendly(&self.path)
    }
    fn changed(&self) -> CodecError {
        CodecError::SourceChanged {
            path: self.shown_path(),
            leftover: None,
        }
    }
    pub(crate) fn seek_to(&mut self, position: u64) -> Result<(), CodecError> {
        self.file.seek(SeekFrom::Start(position))?;
        self.position = position;
        Ok(())
    }
    /// Reads at most `buffer.len()` bytes. Zero means the end of an unchanged file.
    pub(crate) fn read(&mut self, buffer: &mut [u8]) -> Result<usize, CodecError> {
        let n = self.file.read(buffer)?;
        let now = self.position + n as u64;
        if now > self.start.size || (n == 0 && now != self.start.size) {
            return Err(self.changed());
        }
        self.position = now;
        if n == 0 {
            self.confirm_unchanged()?;
        }
        Ok(n)
    }
    /// Compares the size, the time and the file behind the name with the open.
    pub(crate) fn confirm_unchanged(&self) -> Result<(), CodecError> {
        let held = handle_state(HANDLE(self.file.as_raw_handle()));
        let named = file_state(&self.path);
        if held == Ok(self.start) && named == Ok(Some(self.start)) {
            Ok(())
        } else {
            Err(self.changed())
        }
    }
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
/// as FAT or exFAT, where a new file cannot be wider than the target. The rules go
/// to the new file as they are. Rules marked as inherited stay so, and a list with
/// its inheritance removed stays protected, so the new file follows its folder
/// exactly when the old one did.
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
    Ok(Some(rules))
}

/// Gives the owner of a file of ours the full right to it. A temp file carries a copy
/// of the document's rules, and those may not let this user delete it. The creator
/// owns the file and may always change its rules. Returns false if that failed.
fn allow_owner_to_delete(path: &Path) -> bool {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the strings outlive the calls, and the descriptor is freed once.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from("D:P(A;;FA;;;OW)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .is_err()
        {
            return false;
        }
        let set = SetFileSecurityW(
            PCWSTR(wide_path(path).as_ptr()),
            DACL_SECURITY_INFORMATION,
            descriptor,
        )
        .as_bool();
        LocalFree(Some(HLOCAL(descriptor.0)));
        set
    }
}
/// Deletes a file of ours and returns the Windows code if it could not go. A scanner
/// may hold a new file for a moment, so this tries a few times. If its own rules are
/// the obstacle, this changes them first. The file holds nothing the caller still wants.
fn try_delete(path: &Path) -> Result<(), u32> {
    let mut widened = false;
    let mut attempt = 0;
    loop {
        let error = match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => e,
        };
        if io_code(&error) == ERROR_ACCESS_DENIED.0 && !widened {
            widened = true;
            if allow_owner_to_delete(path) {
                continue;
            }
        }
        attempt += 1;
        if attempt == 3 {
            return Err(io_code(&error));
        }
        std::thread::sleep(TRY_DELAY);
    }
}
/// `try_delete`, for a caller that only needs the path of a file that stayed.
fn delete_ours(path: &Path) -> Option<PathBuf> {
    try_delete(path).err().map(|_| friendly(path))
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
    let missing = |target: &Path| CodecError::DanglingLink {
        link: friendly(link),
        target: friendly(target),
    };
    let mut current = link.to_path_buf();
    for _ in 0..64 {
        let next = match fs::read_link(&current) {
            Ok(next) => next,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || io_code(&e) == ERROR_NOT_A_REPARSE_POINT.0 =>
            {
                return new_file_in_real_folder(&current).map_err(|_| missing(&current));
            }
            Err(e) => return Err(save_error(io_code(&e), &current, true, None)),
        };
        // Windows starts from the real folder that holds the link and removes each `..`
        // in the link's text as text, even after a folder that is itself a link. Measured
        // on this PC: with `hop` a link to a far folder, a link with target `hop\..\a.txt`
        // opens the `a.txt` beside the link. `canonicalize` gives a `\\?\` path, and `join`
        // on such a path removes `..` the same way. It keeps an absolute target as it is.
        let joined = match current.parent() {
            Some(folder) => match fs::canonicalize(folder) {
                Ok(real) => real.join(next),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(missing(&folder.join(next)));
                }
                Err(e) => return Err(save_error(io_code(&e), folder, true, None)),
            },
            None => next,
        };
        current = joined;
    }
    Err(save_error(ERROR_CANT_RESOLVE_FILENAME.0, link, true, None))
}
/// The checks that come before any byte is written. Returns the state to compare
/// against later, and the rules a temp file must have.
fn preflight_existing(target: &Path) -> Result<(FileState, Option<AccessRules>), CodecError> {
    check_access(target)?;
    let state = match file_state(target) {
        Ok(Some(state)) => state,
        Ok(None) => return Err(save_error(ERROR_FILE_NOT_FOUND.0, target, true, None)),
        Err(code) => return Err(save_error(code, target, true, None)),
    };
    let rules = read_access_rules(target).map_err(|code| save_error(code, target, true, None))?;
    Ok((state, rules))
}
/// Whether this account may save over `target` now. A prepared copy can wait a long
/// time, so a commit asks again.
fn check_access(target: &Path) -> Result<(), CodecError> {
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
    // ReplaceFileW must rename the target. Without that right the swap can never work,
    // so this stops here, before a temp file exists. Saving in place instead could lose
    // the text in a crash, so there is no such route.
    if let Some(code) = probe_access(target, DELETE.0)
        && code != ERROR_SHARING_VIOLATION.0
        && code != ERROR_LOCK_VIOLATION.0
    {
        return Err(if code == ERROR_ACCESS_DENIED.0 {
            CodecError::NotReplaceable {
                path: friendly(target),
                code,
            }
        } else {
            save_error(code, target, true, None)
        });
    }
    Ok(())
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
/// What a failed swap does with a finished temp file that is not needed for safety.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TempOnFailure {
    /// The caller still has the text, as `atomic_write`'s caller does.
    Delete,
    /// The temp file may be the only copy, as with a prepared copy whose source is gone.
    /// The error names it, or `PreparedCopy` gives it back.
    Keep,
}
impl TempOnFailure {
    /// What the error must add about `temp`.
    fn apply(self, temp: &Path) -> Option<PathBuf> {
        match self {
            Self::Delete => delete_ours(temp),
            Self::Keep => None,
        }
    }
}
/// What a swap needs to know about the finished temp file.
#[derive(Clone, Copy)]
struct Commit<'a> {
    target: &'a Path,
    temp: &'a Path,
    temp_state: FileState,
    on_failure: TempOnFailure,
}
impl Commit<'_> {
    fn temp_changed(&self) -> CodecError {
        CodecError::TempChanged {
            path: friendly(self.target),
            temp: if_on_disk(self.temp),
        }
    }
    fn temp_is_as_written(&self) -> bool {
        matches!(file_state(self.temp), Ok(Some(state)) if state == self.temp_state)
    }
}
/// Puts a finished temp file over an existing target, with the original kept
/// aside under a reserved name until the swap has worked.
fn commit_replace(
    commit: Commit<'_>,
    before: FileState,
    env: &mut impl SaveEnv,
) -> Result<AtomicWriteResult, CodecError> {
    let Commit {
        target,
        temp,
        on_failure,
        ..
    } = commit;
    // A rule or mark added since the save began counts. If the file itself changed,
    // `retry_replace` reports that, which says more.
    if matches!(file_state(target), Ok(Some(state)) if state == before)
        && let Err(error) = check_access(target)
    {
        return Err(with_leftover(error, on_failure.apply(temp)));
    }
    let backup = sibling_with_new_name(target, "bak", env, |candidate| {
        create_new_with_rules(candidate, None).map(drop)
    });
    let backup = match backup {
        Ok(backup) => backup,
        Err(error) => return Err(with_leftover(error, on_failure.apply(temp))),
    };
    let placeholder = file_state(&backup).ok().flatten();
    match retry_replace(commit, &backup, before, env) {
        Swap::Done => after_good_replace(target, &backup, before),
        Swap::TargetChanged => {
            let error = CodecError::TargetChanged {
                path: friendly(target),
                new_text: if_on_disk(temp),
                other_version: None,
            };
            Err(with_leftover(error, delete_ours(&backup)))
        }
        Swap::TempChanged => Err(with_leftover(commit.temp_changed(), delete_ours(&backup))),
        Swap::Failed(code) => Err(after_failed_replace(
            commit,
            &backup,
            placeholder,
            before,
            code,
            env,
        )),
    }
}
/// The swap worked. What now has the backup's name is what the target held at the
/// swap. If that is not the file this save started from, another program wrote in
/// the instant after the last check, and the backup is the only copy of its bytes.
fn after_good_replace(
    target: &Path,
    backup: &Path,
    before: FileState,
) -> Result<AtomicWriteResult, CodecError> {
    match file_state(backup) {
        Ok(Some(state)) if state.same_content_stamp(&before) => {}
        Ok(None) => return Ok(AtomicWriteResult::Replaced),
        // Unknown means it stays, and the caller hears about it.
        Err(_) => {
            return Ok(AtomicWriteResult::ReplacedBackupKept {
                backup: friendly(backup),
            });
        }
        Ok(Some(_)) => {
            return Err(CodecError::TargetChanged {
                path: friendly(target),
                new_text: None,
                other_version: Some(friendly(backup)),
            });
        }
    }
    Ok(match delete_ours(backup) {
        None => AtomicWriteResult::Replaced,
        Some(backup) => AtomicWriteResult::ReplacedBackupKept { backup },
    })
}
enum Swap {
    Done,
    /// The target was not what this save started from, and no swap was tried.
    TargetChanged,
    /// The temp file was not what this save wrote, and no swap was tried.
    TempChanged,
    Failed(u32),
}
fn retry_replace(
    commit: Commit<'_>,
    backup: &Path,
    before: FileState,
    env: &mut impl SaveEnv,
) -> Swap {
    let Commit { target, temp, .. } = commit;
    let mut tries = 0;
    let mut last_code = None;
    loop {
        // Checked before every try: a retry waits long enough for another program to
        // finish its own save. A failed try may itself have changed these files, and
        // only `after_failed_replace` can tell, so after a try the code is what counts.
        match file_state(target) {
            Ok(Some(state)) if state == before => {}
            Ok(_) => return last_code.map_or(Swap::TargetChanged, Swap::Failed),
            Err(code) => return Swap::Failed(last_code.unwrap_or(code)),
        }
        if !commit.temp_is_as_written() {
            return last_code.map_or(Swap::TempChanged, Swap::Failed);
        }
        match env.replace(target, temp, backup) {
            Ok(()) => return Swap::Done,
            Err(code) if is_passing(code) && tries + 1 < TRIES => {
                tries += 1;
                last_code = Some(code);
                env.sleep(TRY_DELAY);
            }
            Err(code) => return Swap::Failed(code),
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
/// Adds a file of ours that stayed on disk to an error. An error with no free place
/// for it is wrapped, so a second file or an error with no such field loses nothing.
pub(crate) fn with_leftover(error: CodecError, found: Option<PathBuf>) -> CodecError {
    let Some(found) = found else {
        return error;
    };
    match error {
        CodecError::AccessDenied {
            path,
            code,
            leftover: None,
        } => CodecError::AccessDenied {
            path,
            code,
            leftover: Some(found),
        },
        CodecError::SharingViolation {
            path,
            code,
            leftover: None,
        } => CodecError::SharingViolation {
            path,
            code,
            leftover: Some(found),
        },
        CodecError::SaveFailed {
            path,
            code,
            original_intact,
            leftover: None,
        } => CodecError::SaveFailed {
            path,
            code,
            original_intact,
            leftover: Some(found),
        },
        CodecError::SourceChanged {
            path,
            leftover: None,
        } => CodecError::SourceChanged {
            path,
            leftover: Some(found),
        },
        CodecError::TargetExists {
            path,
            leftover: None,
        } => CodecError::TargetExists {
            path,
            leftover: Some(found),
        },
        other => CodecError::CleanupFailed {
            cause: Box::new(other),
            leftover: found,
        },
    }
}
/// Looks at the disk to learn what the failed replace did, and says only what it
/// found. The temp file goes only when the original is known to be safe.
///
/// The backup started as an empty reservation of ours, and nothing else writes to it,
/// so bytes there always came from the target. File numbers are not enough to tell:
/// on FAT and exFAT a number may come from a folder slot. So a backup is deleted only
/// when it is empty and is still the reservation.
fn after_failed_replace(
    commit: Commit<'_>,
    backup: &Path,
    placeholder: Option<FileState>,
    before: FileState,
    code: u32,
    env: &mut impl SaveEnv,
) -> CodecError {
    let Commit {
        target,
        temp,
        on_failure,
        ..
    } = commit;
    let now = file_state(target);
    let aside = file_state(backup);
    let original_is_aside = matches!(
        aside,
        Ok(Some(state)) if state.size > 0 || state.same_content_stamp(&before)
    );
    // The backup's path, if it must stay or could not go.
    let drop_placeholder = |aside: &Result<Option<FileState>, u32>| -> Option<PathBuf> {
        match (aside, placeholder) {
            (Ok(None), _) => None,
            (Ok(Some(state)), Some(reserved)) if state.size == 0 && state.same_file(&reserved) => {
                delete_ours(backup)
            }
            _ => Some(friendly(backup)),
        }
    };
    if matches!(now, Ok(Some(state)) if state == before) {
        let temp_left = on_failure.apply(temp);
        let backup_left = drop_placeholder(&aside);
        let error = save_error(real_cause(code, target), target, true, backup_left);
        return with_leftover(error, temp_left);
    }
    if original_is_aside && matches!(now, Ok(None)) && env.move_no_replace(backup, target).is_ok() {
        // The file that was moved aside is back under its name, so no backup exists now.
        return if matches!(file_state(target), Ok(Some(state)) if state.same_content_stamp(&before))
        {
            save_error(code, target, true, on_failure.apply(temp))
        } else {
            // It holds what another program wrote, and the temp file stays.
            CodecError::TargetChanged {
                path: friendly(target),
                new_text: if_on_disk(temp),
                other_version: None,
            }
        };
    }
    if original_is_aside {
        return CodecError::RecoveryRequired {
            original: friendly(target),
            backup: friendly(backup),
            new_text: if_on_disk(temp),
            code,
        };
    }
    // The original is neither where it was nor aside, or another program changed it.
    // The temp file stays: it may be the only copy of this text on disk.
    let error = CodecError::SaveFailed {
        path: friendly(target),
        code,
        original_intact: false,
        leftover: if_on_disk(temp),
    };
    with_leftover(error, drop_placeholder(&aside))
}
/// Puts a finished temp file at a name where nothing was. It never replaces.
fn commit_new(commit: Commit<'_>, env: &mut impl SaveEnv) -> Result<(), CodecError> {
    let Commit {
        target,
        temp,
        on_failure,
        ..
    } = commit;
    if !commit.temp_is_as_written() {
        return Err(commit.temp_changed());
    }
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
    let leftover = on_failure.apply(temp);
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
///
/// The target's state is compared just before the swap. If another program changed it
/// after this call began, the result is `TargetChanged`, and no version is lost.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<AtomicWriteResult, CodecError> {
    atomic_write_with(path, bytes, &mut RealEnv)
}
/// `atomic_write` that also refuses if the file is not what `expected` recorded, for
/// a change at any time since then, such as since the document was opened. Nothing
/// is written in that case, so `TargetChanged` names no file: the text is still with
/// the caller. A file that was deleted since then also gives `TargetChanged`.
pub fn atomic_write_if_unchanged(
    path: &Path,
    bytes: &[u8],
    expected: FileStamp,
) -> Result<AtomicWriteResult, CodecError> {
    atomic_write_expecting(path, bytes, Some(expected), &mut RealEnv)
}
fn atomic_write_with(
    path: &Path,
    bytes: &[u8],
    env: &mut impl SaveEnv,
) -> Result<AtomicWriteResult, CodecError> {
    atomic_write_expecting(path, bytes, None, env)
}
fn changed_since_stamp(target: &Path) -> CodecError {
    CodecError::TargetChanged {
        path: friendly(target),
        new_text: None,
        other_version: None,
    }
}
fn atomic_write_expecting(
    path: &Path,
    bytes: &[u8],
    expected: Option<FileStamp>,
    env: &mut impl SaveEnv,
) -> Result<AtomicWriteResult, CodecError> {
    let (target, before, rules) = match resolve_target(path)? {
        Resolved::Existing(target) => {
            let (state, rules) = preflight_existing(&target)?;
            if expected.is_some_and(|stamp| stamp.0 != state) {
                return Err(changed_since_stamp(&target));
            }
            (target, Some(state), rules)
        }
        Resolved::New(target) => {
            if expected.is_some() {
                return Err(changed_since_stamp(&target));
            }
            (target, None, None)
        }
    };
    let (temp, mut file) = create_temp(&target, rules, env)?;
    let written = env
        .write_temp(&mut file, bytes)
        .and_then(|()| env.flush_temp(&mut file));
    drop(file);
    if let Err(error) = written {
        return Err(after_failed_write(error, &target, &temp, before));
    }
    // After the close, which can move the file's time.
    let temp_state = match file_state(&temp) {
        Ok(Some(state)) => state,
        Ok(None) => {
            let missing = std::io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND.0 as i32);
            return Err(after_failed_write(missing, &target, &temp, before));
        }
        Err(code) => {
            let failed = std::io::Error::from_raw_os_error(code as i32);
            return Err(after_failed_write(failed, &target, &temp, before));
        }
    };
    env.before_commit(&temp);
    let commit = Commit {
        target: &target,
        temp: &temp,
        temp_state,
        on_failure: TempOnFailure::Delete,
    };
    match before {
        Some(before) => commit_replace(commit, before, env),
        None => commit_new(commit, env).map(|()| AtomicWriteResult::Replaced),
    }
}
/// What a file was at one moment: which file, its size and its last write time. Keep
/// the stamp from when a document was opened and from each save, and pass it to
/// `atomic_write_if_unchanged`. Take a new one after every save, because a save
/// changes all of these.
///
/// A program that keeps its handle open can write without moving the time until it
/// closes the handle. A write of the same size inside one step of the file time can
/// go unseen: about 16 ms on NTFS, 2 seconds on FAT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp(FileState);
/// The stamp of the real file behind `path`, or `None` when no file is there. An error
/// means that the path's folder is missing or the check itself failed.
pub fn file_stamp(path: &Path) -> Result<Option<FileStamp>, CodecError> {
    let target = match resolve_target(path)? {
        Resolved::Existing(target) | Resolved::New(target) => target,
    };
    file_state(&target)
        .map(|state| state.map(FileStamp))
        .map_err(|code| save_error(code, &target, true, None))
}

/// A failed `PreparedCopy::try_commit`.
#[derive(Debug)]
pub struct CommitFailure {
    pub error: CodecError,
    /// The copy, ready for another try, when its temp file is still as it was written.
    /// Dropping this deletes the temp file, so keep it if the source may be gone.
    pub copy: Option<PreparedCopy>,
}
/// A complete copy under a temp name in the target's folder. Nothing is at the
/// final name until `commit`. Dropping it deletes the temp file. While it waits, it
/// holds the temp file so that no other program can write to it or rename it.
#[derive(Debug)]
pub struct PreparedCopy {
    temp: PathBuf,
    target: PathBuf,
    /// What the target was when the copy was prepared. `None` for a new name.
    before: Option<FileState>,
    /// What the temp file was when the copy was finished.
    temp_state: FileState,
    holder: Option<File>,
    done: bool,
}
/// Opens the finished temp file for reading and refuses others the right to write,
/// rename or delete it. If that open fails, for example while a scanner has the file,
/// the copy goes on without it: the commit still compares the file's state.
fn hold_temp(temp: &Path) -> Option<File> {
    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(temp)
        .ok()
}
impl PreparedCopy {
    pub fn temp_path(&self) -> &Path {
        &self.temp
    }
    /// Puts the copy at the final name, all at once. A target that existed at
    /// `prepare_large_copy` is replaced, with the same guarantees as `atomic_write`.
    /// A new name stays new: a file that appeared meanwhile gives `TargetExists`.
    ///
    /// The checks run again here: the target's state, the read-only mark, the rights to
    /// write and replace, and the temp file's own state. A changed temp file gives
    /// `TempChanged`, and the target is not touched.
    ///
    /// A target that another program changed since `prepare_large_copy` gives
    /// `TargetChanged`. The target keeps that program's bytes, and the copy stays at
    /// `new_text`. The caller decides what to do with it.
    ///
    /// After any failure the temp file stays on disk, because the source may be gone and
    /// this may be the only copy. `CodecError::left_on_disk` names it. To try again
    /// without a new copy, use `try_commit`.
    pub fn commit(self) -> Result<AtomicWriteResult, CodecError> {
        self.try_commit().map_err(|failure| failure.keep_and_name())
    }
    /// `commit` that gives the copy back after a failure, so the caller can try again.
    pub fn try_commit(self) -> Result<AtomicWriteResult, Box<CommitFailure>> {
        self.attempt(None, &mut RealEnv)
    }
    /// `commit` that also refuses if the target is not what `expected` recorded, as
    /// in `atomic_write_if_unchanged`. The copy stays at `new_text` in that case.
    pub fn commit_if_unchanged(self, expected: FileStamp) -> Result<AtomicWriteResult, CodecError> {
        self.try_commit_if_unchanged(expected)
            .map_err(|failure| failure.keep_and_name())
    }
    /// `commit_if_unchanged` that gives the copy back after a failure.
    pub fn try_commit_if_unchanged(
        self,
        expected: FileStamp,
    ) -> Result<AtomicWriteResult, Box<CommitFailure>> {
        self.attempt(Some(expected), &mut RealEnv)
    }
    fn attempt(
        mut self,
        expected: Option<FileStamp>,
        env: &mut impl SaveEnv,
    ) -> Result<AtomicWriteResult, Box<CommitFailure>> {
        // ReplaceFileW cannot work while anything, this included, holds the file.
        self.holder = None;
        env.before_commit(&self.temp);
        let commit = Commit {
            target: &self.target,
            temp: &self.temp,
            temp_state: self.temp_state,
            on_failure: TempOnFailure::Keep,
        };
        let result = if expected.is_some_and(|stamp| self.before != Some(stamp.0)) {
            Err(CodecError::TargetChanged {
                path: friendly(&self.target),
                new_text: if_on_disk(&self.temp),
                other_version: None,
            })
        } else {
            match self.before {
                Some(before) => commit_replace(commit, before, env),
                None => commit_new(commit, env).map(|()| AtomicWriteResult::Replaced),
            }
        };
        match result {
            Ok(result) => {
                self.done = true;
                Ok(result)
            }
            Err(error) => {
                let usable = commit.temp_is_as_written();
                self.done = !usable;
                let copy = usable.then(|| {
                    self.holder = hold_temp(&self.temp);
                    self
                });
                Err(Box::new(CommitFailure { error, copy }))
            }
        }
    }
    /// Throws the copy away and says if the temp file could not go. Dropping a
    /// `PreparedCopy` does the same, but a drop cannot report a failure.
    pub fn discard(mut self) -> Result<(), CodecError> {
        self.done = true;
        self.holder = None;
        try_delete(&self.temp).map_err(|code| CodecError::SaveFailed {
            path: friendly(&self.temp),
            code,
            original_intact: true,
            leftover: Some(friendly(&self.temp)),
        })
    }
    /// Removes the temp file after a failed copy, and adds it to `error` if it stayed.
    fn fail_with(mut self, error: CodecError) -> CodecError {
        self.done = true;
        self.holder = None;
        with_leftover(error, delete_ours(&self.temp))
    }
}
impl CommitFailure {
    /// The error alone, with the temp file left on disk and named in it.
    fn keep_and_name(self) -> CodecError {
        let Some(mut copy) = self.copy else {
            return self.error;
        };
        copy.done = true;
        copy.holder = None;
        let temp = friendly(&copy.temp);
        if self.error.left_on_disk().contains(&temp.as_path()) {
            self.error
        } else {
            with_leftover(self.error, Some(temp))
        }
    }
}
impl Drop for PreparedCopy {
    fn drop(&mut self) {
        if !self.done {
            self.holder = None;
            // A drop cannot report. `find_leftovers` lists this file if it stayed, and
            // `discard` is the call for a caller that wants to know now.
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
    let mut source = WatchedSource::open(input)?;
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
    let mut prepared = PreparedCopy {
        temp,
        target,
        before,
        temp_state: FileState::default(),
        holder: None,
        done: false,
    };
    // `file` is closed when this call returns, so the temp file can then be deleted.
    if let Err(error) = copy_into(&mut source, file, &prepared, progress, cancel, env) {
        return Err(prepared.fail_with(error));
    }
    // After the close, which can move the file's time.
    match file_state(&prepared.temp) {
        Ok(Some(state)) => prepared.temp_state = state,
        Ok(None) => {
            let error = save_error(ERROR_FILE_NOT_FOUND.0, &prepared.temp, true, None);
            return Err(prepared.fail_with(error));
        }
        Err(code) => {
            let error = save_error(code, &prepared.temp, true, None);
            return Err(prepared.fail_with(error));
        }
    }
    prepared.holder = hold_temp(&prepared.temp);
    Ok(prepared)
}
fn copy_into(
    source: &mut WatchedSource,
    mut output: File,
    prepared: &PreparedCopy,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &mut dyn FnMut() -> bool,
    env: &mut impl SaveEnv,
) -> Result<(), CodecError> {
    let total = source.size();
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
        env.write_temp(&mut output, &buffer[..n])
            .map_err(|e| copy_failure(&e, prepared))?;
        done += n as u64;
        progress(done, total);
    }
    if total == 0 {
        progress(0, 0);
    }
    env.flush_temp(&mut output)
        .map_err(|e| copy_failure(&e, prepared))?;
    source.confirm_unchanged()
}
/// Copies `input` to a temp name beside `dest`, ready for `PreparedCopy::commit`.
/// This is the safe way to Save As a file too large to hold in memory, over an
/// existing file or to a new name. It writes nothing under the final name.
///
/// `progress` gets `(done, total)`. `total` is the size of `input` at the start, and
/// `done` never passes it. An empty file reports `(0, 0)` once, as its finish, so a
/// caller that shows a percentage must handle a total of zero.
///
/// If `input` changes during the copy, the result is `SourceChanged` and the partial
/// copy is deleted: a longer or shorter file, a new write time, or another file under
/// that name. A failure that could not delete the temp file names it in the error.
pub fn prepare_large_copy(
    input: &Path,
    dest: &Path,
    mut progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<PreparedCopy, CodecError> {
    prepare_copy(input, dest, false, &mut progress, &mut cancel, &mut RealEnv)
}
/// Copies `input` to a new file at `dest`. A file already at `dest` gives
/// `TargetExists`. A crash cannot leave a partial file under the final name. Progress
/// and a changed `input` work as in `prepare_large_copy`.
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

/// True when the folder of `leftover` holds a file with that hash that is not one of ours.
fn document_is_there(leftover: &Path, hash: u32) -> Result<bool, CodecError> {
    let folder = leftover
        .parent()
        .ok_or_else(|| save_error(ERROR_PATH_NOT_FOUND.0, leftover, true, None))?;
    for entry in fs::read_dir(folder)? {
        let name = entry?.file_name();
        if parse_leftover_name(&name.to_string_lossy()).is_none() && name_hash(&name) == hash {
            return Ok(true);
        }
    }
    Ok(false)
}
/// How far `delete_leftover` may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftoverDelete {
    /// Refuses a file that holds text the user could still recover.
    KeepRecoverable,
    /// Deletes that text too. Use this only after the user has chosen it.
    IncludeRecoverable,
}
/// Deletes one file that `find_leftovers` listed, when the user asks. A file already
/// gone counts as done. It gives `LeftoverKept` and leaves the file alone if the name
/// is not one of ours, if the file changed since it was listed, if another running
/// process made it, or, under `KeepRecoverable`, if it may hold the user's text.
///
/// This process's own files are allowed, so do not call this during a save of the
/// same document.
pub fn delete_leftover(leftover: &Leftover, how: LeftoverDelete) -> Result<(), CodecError> {
    let path = verbatim(&std::path::absolute(&leftover.path)?);
    let kept = |reason| CodecError::LeftoverKept {
        path: leftover.path.clone(),
        reason,
    };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let parsed = parse_leftover_name(&name).ok_or_else(|| kept("it is not a file of ours"))?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() || meta.len() != leftover.size || meta.modified()? != leftover.modified {
        return Err(kept("it changed after it was listed"));
    }
    if parsed.pid != std::process::id() && process_is_running(parsed.pid) {
        return Err(kept("the program that made it is still running"));
    }
    // Looked up now: the list may be old, and the document may have gone since.
    let recoverable = match parsed.kind {
        LeftoverKind::Backup => meta.len() > 0,
        LeftoverKind::Temp => meta.len() > 0 && !document_is_there(&path, parsed.hash)?,
    };
    if recoverable && how == LeftoverDelete::KeepRecoverable {
        return Err(kept("it may hold text the user can still recover"));
    }
    try_delete(&path).map_err(|code| CodecError::SaveFailed {
        path: leftover.path.clone(),
        code,
        original_intact: true,
        leftover: Some(leftover.path.clone()),
    })
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;
