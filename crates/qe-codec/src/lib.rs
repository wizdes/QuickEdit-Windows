//! Plain-text byte conversion and safe file writes for QuickEdit.
//!
//! The README next to this crate's `Cargo.toml` explains the save rules, the
//! recovery files and the known limits.
mod save;
mod text;

use qe_api::TextEncoding;
use std::path::PathBuf;

pub use save::{
    AtomicWriteResult, CommitFailure, FileStamp, Leftover, LeftoverDelete, LeftoverKind,
    PreparedCopy, SaveHooks, atomic_write, atomic_write_guarded, atomic_write_guarded_with,
    atomic_write_if_unchanged, copy_large_original, delete_leftover, file_holds_bytes, file_stamp,
    find_leftovers, prepare_large_copy, stamp_after_swap,
};
pub use text::{can_encode, decode, decode_file, encode, is_available, transcode_to_utf8_file};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}
/// How a file's text is stored. Code outside this crate gets one from `decode`,
/// `Format::new_document`, `Format::changed_to` or `Format::new`; it cannot write
/// one out as a struct literal. `encode` refuses a value whose fields disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Format {
    pub encoding: TextEncoding,
    pub has_bom: bool,
    pub endian: Option<Endian>,
    pub main_line_ending: LineEnding,
    pub lossy: bool,
}
impl Format {
    pub fn new_document() -> Self {
        Self {
            encoding: TextEncoding::Utf8,
            has_bom: false,
            endian: None,
            main_line_ending: LineEnding::Lf,
            lossy: false,
        }
    }
    /// A format from stored values, such as a session restored at launch. Values
    /// that disagree give `InvalidFormat`: an `endian` on anything but UTF-16 or
    /// UTF-32, an `endian` against a fixed-order encoding, or a BOM on a legacy page.
    pub fn new(
        encoding: TextEncoding,
        has_bom: bool,
        endian: Option<Endian>,
        main_line_ending: LineEnding,
        lossy: bool,
    ) -> Result<Self, CodecError> {
        let format = Self {
            encoding,
            has_bom,
            endian,
            main_line_ending,
            lossy,
        };
        format.check()?;
        Ok(format)
    }
    pub(crate) fn check(&self) -> Result<(), CodecError> {
        let fixed = match self.encoding {
            TextEncoding::Utf16Le | TextEncoding::Utf32Le => Some(Endian::Little),
            TextEncoding::Utf16Be | TextEncoding::Utf32Be => Some(Endian::Big),
            _ => None,
        };
        let what = if text::unicode_endian(self.encoding).is_none() && self.endian.is_some() {
            "only UTF-16 and UTF-32 have a byte order"
        } else if fixed.is_some() && self.endian.is_some() && fixed != self.endian {
            "the byte order disagrees with the encoding"
        } else if self.has_bom
            && self.encoding != TextEncoding::Utf8
            && text::unicode_endian(self.encoding).is_none()
        {
            "a legacy encoding has no BOM"
        } else {
            return Ok(());
        };
        Err(CodecError::InvalidFormat { what })
    }
    pub fn changed_to(&self, encoding: TextEncoding) -> Self {
        if encoding == self.encoding {
            return self.clone();
        }
        Self {
            encoding,
            has_bom: matches!(encoding, TextEncoding::Utf16 | TextEncoding::Utf32),
            endian: text::unicode_endian(encoding),
            main_line_ending: self.main_line_ending,
            lossy: false,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    pub text: String,
    pub format: Format,
}

/// Every way a decode, an encode or a save can fail.
///
/// In the save errors, `code` is the Windows error code. `leftover` names a file of
/// ours that stayed on disk, because it could not be deleted or because a failed
/// `PreparedCopy::commit` keeps its text; `find_leftovers` also finds it later. A second
/// such file is added by `CleanupFailed`, and `left_on_disk` lists them all.
///
/// Paths in these errors are exact, even for a name that is not valid Unicode. The
/// messages show such a name with a stand-in character.
#[derive(Debug)]
pub enum CodecError {
    InapplicableEncoding(TextEncoding),
    Unencodable(TextEncoding),
    UnavailableEncoding(TextEncoding),
    /// The data does not fit in memory. The encoding is fine.
    TooLarge {
        what: &'static str,
    },
    /// The target has the read-only attribute. Nothing changed.
    ReadOnly(PathBuf),
    /// Windows refused access to this file or folder. The target was not changed.
    AccessDenied {
        path: PathBuf,
        code: u32,
        leftover: Option<PathBuf>,
    },
    /// Another program held the target for the whole retry time. The target was
    /// checked and was not changed.
    SharingViolation {
        path: PathBuf,
        code: u32,
        leftover: Option<PathBuf>,
    },
    /// A save to a new name found a file there. That file was not touched.
    TargetExists {
        path: PathBuf,
        leftover: Option<PathBuf>,
    },
    /// The path is a link whose target's folder does not exist.
    DanglingLink {
        link: PathBuf,
        target: PathBuf,
    },
    /// The original is no longer at `original`; its bytes are at `backup`. The
    /// app must tell the user. `new_text` holds the text of this save, if it is
    /// still on disk.
    RecoveryRequired {
        original: PathBuf,
        backup: PathBuf,
        new_text: Option<PathBuf>,
        code: u32,
    },
    /// Any other save failure. `original_intact` is true only after a check of
    /// the target, or when there was no original to harm.
    SaveFailed {
        path: PathBuf,
        code: u32,
        original_intact: bool,
        leftover: Option<PathBuf>,
    },
    /// The file being read changed while it was read: its size, its time, or which
    /// file the name points to. Nothing from it was used. `leftover` is as above.
    SourceChanged {
        path: PathBuf,
        leftover: Option<PathBuf>,
    },
    /// Another program changed the target during this save, or since the stamp the
    /// caller passed. No version was lost. If `other_version` is `None`, the target
    /// still holds the other program's bytes, and `new_text` holds this save's text
    /// if it reached the disk. If `other_version` is set, the change came in the last
    /// instant: the target holds this save's text and that file holds the other
    /// program's bytes.
    TargetChanged {
        path: PathBuf,
        new_text: Option<PathBuf>,
        other_version: Option<PathBuf>,
    },
    /// This account may change the file but may not delete or rename it, so a safe
    /// save cannot put a new file there. Nothing was written. The app should offer
    /// Save As.
    NotReplaceable {
        path: PathBuf,
        code: u32,
    },
    /// The fields of a `Format` disagree. Nothing was written.
    InvalidFormat {
        what: &'static str,
    },
    /// `cause` is what went wrong, and a file of ours also stayed on disk.
    CleanupFailed {
        cause: Box<CodecError>,
        leftover: PathBuf,
    },
    /// Something changed the finished temp file before the swap, so this save refused
    /// to use it. The target was not changed. `temp` is that file, if it is still there.
    TempChanged {
        path: PathBuf,
        temp: Option<PathBuf>,
    },
    /// `delete_leftover` refused: the file may still be in use, it changed since it
    /// was listed, or it holds text the user could still recover.
    LeftoverKept {
        path: PathBuf,
        reason: &'static str,
    },
    Io(std::io::Error),
    Cancelled,
}
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InapplicableEncoding(e) => {
                write!(f, "could not be opened using text encoding {}", e.name())
            }
            Self::Unencodable(e) => write!(f, "text cannot be saved using {}", e.name()),
            Self::UnavailableEncoding(e) => write!(
                f,
                "text encoding {} is not available on this Windows system",
                e.name()
            ),
            Self::TooLarge { what } => write!(f, "the file is too large: {what}"),
            Self::ReadOnly(p) => write!(f, "read-only target: {}", p.display()),
            Self::AccessDenied { path, code, .. } => write!(
                f,
                "access denied to {} (Windows error {code}); the file on disk was not changed",
                path.display()
            ),
            Self::SharingViolation { path, code, .. } => write!(
                f,
                "another program is using {} (Windows error {code}); the file on disk was not changed",
                path.display()
            ),
            Self::TargetExists { path, .. } => {
                write!(f, "a file appeared at {} during the save", path.display())
            }
            Self::DanglingLink { link, target } => write!(
                f,
                "the link {} points to {}, whose folder does not exist",
                link.display(),
                target.display()
            ),
            Self::RecoveryRequired {
                original,
                backup,
                code,
                ..
            } => write!(
                f,
                "could not restore {} after a failed save (Windows error {code}); \
                 the original bytes are at {}",
                original.display(),
                backup.display()
            ),
            Self::SaveFailed {
                path,
                code,
                original_intact,
                ..
            } => write!(
                f,
                "could not save {} (Windows error {code}); {}",
                path.display(),
                if *original_intact {
                    "the file on disk was not changed"
                } else {
                    "the file on disk may have changed"
                }
            ),
            Self::SourceChanged { path, .. } => write!(
                f,
                "{} changed while it was being read; try again",
                path.display()
            ),
            Self::TargetChanged { path, .. } => write!(
                f,
                "another program changed {} during the save; both versions were kept",
                path.display()
            ),
            Self::NotReplaceable { path, code } => write!(
                f,
                "cannot safely save {} (Windows error {code}): this account may edit the file \
                 but not replace it; the file on disk was not changed; use Save As",
                path.display()
            ),
            Self::InvalidFormat { what } => write!(f, "invalid text format: {what}"),
            Self::CleanupFailed { cause, leftover } => write!(
                f,
                "{cause}; also, a file of ours stayed at {}",
                leftover.display()
            ),
            Self::TempChanged { path, .. } => write!(
                f,
                "the new text for {} was changed before the save finished; \
                 the file on disk was not changed",
                path.display()
            ),
            Self::LeftoverKept { path, reason } => {
                write!(f, "{} was not deleted: {reason}", path.display())
            }
            Self::Io(e) => e.fmt(f),
            Self::Cancelled => write!(f, "operation cancelled"),
        }
    }
}
impl CodecError {
    /// Every file of ours that this error says is still on disk.
    pub fn left_on_disk(&self) -> Vec<&std::path::Path> {
        let mut files: Vec<&std::path::Path> = Vec::new();
        match self {
            Self::AccessDenied { leftover, .. }
            | Self::SharingViolation { leftover, .. }
            | Self::TargetExists { leftover, .. }
            | Self::SaveFailed { leftover, .. }
            | Self::SourceChanged { leftover, .. } => files.extend(leftover.as_deref()),
            Self::TargetChanged {
                new_text,
                other_version,
                ..
            } => {
                files.extend(new_text.as_deref());
                files.extend(other_version.as_deref());
            }
            Self::RecoveryRequired {
                backup, new_text, ..
            } => {
                files.extend(new_text.as_deref());
                files.push(backup.as_path());
            }
            Self::TempChanged { temp, .. } => files.extend(temp.as_deref()),
            Self::CleanupFailed { cause, leftover } => {
                files.extend(cause.left_on_disk());
                files.push(leftover.as_path());
            }
            _ => {}
        }
        files
    }
}
impl std::error::Error for CodecError {}
impl From<std::io::Error> for CodecError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
