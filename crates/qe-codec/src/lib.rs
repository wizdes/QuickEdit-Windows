//! Plain-text byte conversion and safe file writes for QuickEdit.
//!
//! The README next to this crate's `Cargo.toml` explains the save rules, the
//! recovery files and the known limits.
mod save;
mod text;

use qe_api::TextEncoding;
use std::path::PathBuf;

pub use save::{
    AtomicWriteResult, Leftover, LeftoverKind, PreparedCopy, atomic_write, copy_large_original,
    find_leftovers, prepare_large_copy,
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
/// In the save errors, `code` is the Windows error code. `leftover` names a file
/// of ours that could not be deleted; `find_leftovers` also finds it later.
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
            Self::Io(e) => e.fmt(f),
            Self::Cancelled => write!(f, "operation cancelled"),
        }
    }
}
impl std::error::Error for CodecError {}
impl From<std::io::Error> for CodecError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
