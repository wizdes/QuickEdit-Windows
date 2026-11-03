//! Which real file a path names, where a moved file went, and a process's birth time.
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, ExtendedFileIdType, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_READONLY, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_128, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::PCWSTR;

/// One real file: its volume and its file number. Two paths with the same identity are
/// the same file, whatever links or letter case they use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub(crate) volume: u64,
    pub(crate) file: u128,
}

/// What a file was when the document last matched it, in a form that outlives the
/// process: the session keeps it to learn whether the file changed while QuickEdit
/// was not running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileBase {
    pub(crate) size: u64,
    pub(crate) written: u64,
    pub(crate) identity: FileIdentity,
    pub(crate) read_only: bool,
}

pub(crate) fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

pub(crate) fn os_error(error: &windows::core::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(error.code().0 & 0xffff)
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this type holds the only copy of an open handle.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            eprintln!("QuickEdit: closing a file handle failed: {error}");
        }
    }
}

fn open_for_query(path: &Path) -> std::io::Result<Handle> {
    let name = wide(path);
    // SAFETY: the name ends in a zero and outlives the call. Access 0 reads only metadata.
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
    handle.map(Handle).map_err(|e| os_error(&e))
}

fn base_of_handle(handle: HANDLE) -> std::io::Result<(FileBase, bool)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open and `info` outlives the call.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(|e| os_error(&e))?;
    let mut id = FILE_ID_INFO::default();
    // SAFETY: the buffer is the size the class needs and outlives the call.
    let long_id = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            std::ptr::from_mut(&mut id).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    let identity = match long_id {
        Ok(()) => FileIdentity {
            volume: id.VolumeSerialNumber,
            file: u128::from_le_bytes(id.FileId.Identifier),
        },
        // Some file systems have no long number. The short one is what they offer.
        Err(_) => FileIdentity {
            volume: u64::from(info.dwVolumeSerialNumber),
            file: (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
        },
    };
    let base = FileBase {
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        written: (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(info.ftLastWriteTime.dwLowDateTime),
        identity,
        read_only: info.dwFileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0,
    };
    Ok((
        base,
        info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
    ))
}

/// The state of the real file behind `path`, and whether it is a folder.
pub(crate) fn file_base(path: &Path) -> std::io::Result<(FileBase, bool)> {
    let handle = open_for_query(path)?;
    base_of_handle(handle.0)
}

/// The path Windows reports for a handle, with its `\\?\` prefix taken off.
fn final_path(handle: HANDLE) -> std::io::Result<PathBuf> {
    let mut buffer = vec![0u16; 1024];
    loop {
        // SAFETY: the buffer outlives the call and its length is passed.
        let got =
            unsafe { GetFinalPathNameByHandleW(handle, &mut buffer, VOLUME_NAME_DOS) } as usize;
        if got == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if got < buffer.len() {
            buffer.truncate(got);
            break;
        }
        buffer.resize(got + 1, 0);
    }
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    if let Some(rest) = buffer.strip_prefix(unc.as_slice()) {
        let mut out: Vec<u16> = r"\\".encode_utf16().collect();
        out.extend_from_slice(rest);
        return Ok(PathBuf::from(OsString::from_wide(&out)));
    }
    let rest = buffer.strip_prefix(prefix.as_slice()).unwrap_or(&buffer);
    Ok(PathBuf::from(OsString::from_wide(rest)))
}

/// Where a file with this identity is now, by its number, on the same volume as `near`.
/// `None` when it is gone or Windows cannot say. A file in the Recycle Bin comes back with
/// a path inside `$Recycle.Bin`, which the caller treats as deleted.
pub(crate) fn locate_by_identity(identity: FileIdentity, near: &Path) -> Option<PathBuf> {
    let hint = near.ancestors().skip(1).find_map(|folder| {
        if folder.as_os_str().is_empty() {
            return None;
        }
        open_for_query(folder).ok()
    })?;
    let descriptor = FILE_ID_DESCRIPTOR {
        dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: ExtendedFileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 {
            ExtendedFileId: FILE_ID_128 {
                Identifier: identity.file.to_le_bytes(),
            },
        },
    };
    // SAFETY: the hint is open, and the descriptor outlives the call.
    let found = unsafe {
        OpenFileById(
            hint.0,
            &descriptor,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    }
    .ok()?;
    let found = Handle(found);
    let (base, _) = base_of_handle(found.0).ok()?;
    if base.identity != identity {
        return None;
    }
    final_path(found.0).ok()
}

pub(crate) fn is_in_recycle_bin(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("$Recycle.Bin")
    })
}

/// This process's number and start time. Together they name it even when the number is
/// used again later.
pub(crate) fn this_process() -> (u32, u64) {
    // SAFETY: the call takes no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    (pid, process_start(pid).unwrap_or(0))
}

/// When a process started, or `None` when it is gone.
pub(crate) fn process_start(pid: u32) -> Option<u64> {
    // SAFETY: plain numbers go in, and `Handle` closes what comes out.
    let handle =
        Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut created = FILETIME::default();
    let (mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the handle is open and the four times outlive the call.
    unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
