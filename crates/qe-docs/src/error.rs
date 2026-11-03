//! Named errors. Each has a kind for code and a plain sentence for the user.
use qe_codec::CodecError;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocErrorKind {
    FileNotFound,
    IsAFolder,
    AccessDenied,
    FileInUse,
    EncodingDoesNotFit,
    TooLarge,
    OutOfMemory,
    DiskFull,
    ReadOnlyFile,
    /// This account may change the file but not replace it. The host offers Save As.
    NotReplaceable,
    TargetExists,
    ChangedOutside,
    /// A save target is already open in another tab.
    OpenInAnotherTab,
    /// The document is loading, reloading or saving, so this must wait.
    Busy,
    /// A file too large to edit is never written over.
    TooLargeToEdit,
    NoSuchDocument,
    /// The file on disk needs the user's help. `left_on_disk` names the copies.
    RecoveryRequired,
    /// The autosaved copy could not be written. The text is still in the window.
    AutosaveFailed,
    /// An autosaved copy failed its check when it was read back.
    AutosaveDamaged,
    SessionDamaged,
    Cancelled,
    Other,
}

/// A failure, with its kind, the file it concerns, and any file of ours left on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocError {
    pub kind: DocErrorKind,
    pub path: Option<PathBuf>,
    pub message: String,
    pub left_on_disk: Vec<PathBuf>,
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for DocError {}

impl DocError {
    pub fn new(kind: DocErrorKind, path: Option<&Path>, message: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.map(Path::to_path_buf),
            message: message.into(),
            left_on_disk: Vec::new(),
        }
    }
    /// A reading or naming failure from `std::io`, in the words of `what`.
    pub(crate) fn io(what: &str, path: &Path, error: &std::io::Error) -> Self {
        let code = error.raw_os_error().unwrap_or(0) as u32;
        let shown = path.display();
        let (kind, reason) = match code {
            2 | 3 => (
                DocErrorKind::FileNotFound,
                "the file or its folder was not found".to_owned(),
            ),
            5 => (
                DocErrorKind::AccessDenied,
                "Windows denied access".to_owned(),
            ),
            32 | 33 => (
                DocErrorKind::FileInUse,
                "another program is using the file".to_owned(),
            ),
            39 | 112 => (DocErrorKind::DiskFull, "the disk is full".to_owned()),
            8 | 14 => (
                DocErrorKind::OutOfMemory,
                "there is not enough memory".to_owned(),
            ),
            _ => (DocErrorKind::Other, format!("{error}")),
        };
        Self::new(kind, Some(path), format!("{what} {shown}: {reason}."))
    }
    pub(crate) fn from_codec(what: &str, path: &Path, error: CodecError) -> Self {
        let left_on_disk = error
            .left_on_disk()
            .into_iter()
            .map(Path::to_path_buf)
            .collect();
        let kind = kind_of(&error);
        let mut out = Self::new(kind, Some(path), format!("{what}: {error}."));
        out.left_on_disk = left_on_disk;
        out
    }
}

fn kind_of(error: &CodecError) -> DocErrorKind {
    match error {
        CodecError::InapplicableEncoding(_)
        | CodecError::Unencodable(_)
        | CodecError::UnavailableEncoding(_)
        | CodecError::InvalidFormat { .. } => DocErrorKind::EncodingDoesNotFit,
        CodecError::TooLarge { .. } => DocErrorKind::TooLarge,
        CodecError::ReadOnly(_) => DocErrorKind::ReadOnlyFile,
        CodecError::AccessDenied { .. } => DocErrorKind::AccessDenied,
        CodecError::SharingViolation { .. } => DocErrorKind::FileInUse,
        CodecError::TargetExists { .. } => DocErrorKind::TargetExists,
        CodecError::RecoveryRequired { .. } => DocErrorKind::RecoveryRequired,
        CodecError::SaveFailed { code: 39 | 112, .. } => DocErrorKind::DiskFull,
        CodecError::SourceChanged { .. } | CodecError::TargetChanged { .. } => {
            DocErrorKind::ChangedOutside
        }
        CodecError::NotReplaceable { .. } => DocErrorKind::NotReplaceable,
        CodecError::Cancelled => DocErrorKind::Cancelled,
        CodecError::CleanupFailed { cause, .. } => kind_of(cause),
        CodecError::Io(e) => match e.raw_os_error() {
            Some(2 | 3) => DocErrorKind::FileNotFound,
            Some(5) => DocErrorKind::AccessDenied,
            Some(32 | 33) => DocErrorKind::FileInUse,
            Some(39 | 112) => DocErrorKind::DiskFull,
            _ => DocErrorKind::Other,
        },
        _ => DocErrorKind::Other,
    }
}
