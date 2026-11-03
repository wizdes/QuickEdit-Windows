//! Reading a file or an autosaved copy, on a worker thread.
use crate::autosave::read_generation;
use crate::error::{DocError, DocErrorKind};
use crate::identity::{FileBase, file_base, this_process};
use qe_api::{ByteOffset, EncodingChoice, TextEncoding, TextSnapshot};
use qe_buffer::{MappedStore, Notify, PieceStore};
use qe_codec::{CodecError, FileStamp, Format, LineEnding, decode_file, file_stamp};
use qe_editor::LoadedFile;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const PREFIX_BYTES: usize = 64 * 1024;
const READ_TRIES: u32 = 3;
/// A tree of 4 KiB leaves costs about one part in this much more than its text.
const TREE_COST_DIVISOR: usize = 16;

/// The size rules. Tests lower them, so that a small file can use the large paths.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) read_only: u64,
}

pub(crate) struct Body {
    pub(crate) file: LoadedFile,
    pub(crate) format: Format,
    pub(crate) byte_count: u64,
    pub(crate) base: Option<FileBase>,
    pub(crate) stamp: Option<FileStamp>,
    /// A UTF-8 copy of a large file in another encoding, removed with the document.
    pub(crate) temp_copy: Option<PathBuf>,
    /// What an autosaved copy says of itself, when the text came from one.
    pub(crate) copy_head: Option<CopyHead>,
}

/// What an autosaved copy knew about the file when it was written.
#[derive(Clone, Copy)]
pub(crate) struct CopyHead {
    pub(crate) base: Option<FileBase>,
    pub(crate) changed_while_away: bool,
}

pub(crate) enum Loaded {
    Body(Body),
    /// A reload found the same text. The caret, the scroll and undo stay.
    SameText {
        format: Format,
        byte_count: u64,
        base: Option<FileBase>,
        stamp: Option<FileStamp>,
    },
}

/// What a worker reads for a load.
pub(crate) enum Source {
    File {
        path: PathBuf,
        choice: EncodingChoice,
        /// The text on screen, so a reload can tell a date-only change.
        compare: Option<Arc<dyn TextSnapshot>>,
        /// The first pass over a large file found bytes that are not UTF-8.
        force_copy: bool,
    },
    /// Newest first. The first whole copy wins.
    Autosave { generations: Vec<PathBuf> },
    /// A copy of text in memory, for Duplicate.
    Snapshot {
        snapshot: Arc<dyn TextSnapshot>,
        format: Format,
    },
    /// The same large file again, for a copy with no file of its own.
    Mapped { path: PathBuf, format: Format },
}

impl Source {
    /// The file this reads from disk, if it is one the user named.
    pub(crate) fn file(&self) -> Option<&Path> {
        match self {
            Self::File { path, .. } | Self::Mapped { path, .. } => Some(path),
            Self::Autosave { .. } | Self::Snapshot { .. } => None,
        }
    }
}

pub(crate) struct LoadJob {
    pub(crate) source: Source,
    pub(crate) limits: Limits,
    pub(crate) large_dir: PathBuf,
    pub(crate) notify: Notify,
    pub(crate) cancel: Arc<AtomicBool>,
}

pub(crate) struct LoadResult {
    pub(crate) loaded: Result<Loaded, DocError>,
    /// Newer autosaved copies that failed their check, with what was wrong.
    pub(crate) problems: Vec<DocError>,
}

pub(crate) fn ending_text(ending: LineEnding) -> &'static str {
    match ending {
        LineEnding::Lf => "\n",
        LineEnding::CrLf => "\r\n",
        LineEnding::Cr => "\r",
    }
}

/// `PieceStore::new` copies the text and cannot report a failed allocation, which would
/// end the app and lose every other tab's text. So this asks for that much first.
fn room_for_tree(path: Option<&Path>, bytes: usize) -> Result<(), DocError> {
    let needed = bytes.saturating_add(bytes / TREE_COST_DIVISOR);
    let mut probe: Vec<u8> = Vec::new();
    probe.try_reserve_exact(needed).map_err(|_| {
        DocError::new(
            DocErrorKind::OutOfMemory,
            path,
            format!(
                "There is not enough memory to hold {} MiB of text.",
                bytes >> 20
            ),
        )
    })
}

fn same_text(snapshot: &dyn TextSnapshot, text: &str) -> bool {
    if snapshot.is_read_only() || snapshot.len_bytes().0 != text.len() {
        return false;
    }
    let mut at = 0;
    let mut same = true;
    let whole = snapshot.visit_chunks_while(ByteOffset(0)..ByteOffset(text.len()), &mut |chunk| {
        same = text.as_bytes().get(at..at + chunk.len()) == Some(chunk.as_bytes());
        at += chunk.len();
        same
    });
    whole && same
}

/// Bytes at the start of a file that can be shown from the file itself: no BOM to hide,
/// and valid UTF-8 as far as they go. The line ending comes from them as well, since a
/// read-only file never gets a new line.
fn plain_utf8_prefix(path: &Path, choice: EncodingChoice) -> std::io::Result<Option<LineEnding>> {
    match choice {
        EncodingChoice::Automatic | EncodingChoice::Specific(TextEncoding::Utf8) => {}
        EncodingChoice::Specific(_) => return Ok(None),
    }
    let mut bytes = Vec::with_capacity(PREFIX_BYTES);
    std::fs::File::open(path)?
        .take(PREFIX_BYTES as u64)
        .read_to_end(&mut bytes)?;
    let has_bom = [
        &[0xEF, 0xBB, 0xBF][..],
        &[0xFF, 0xFE][..],
        &[0xFE, 0xFF][..],
        &[0x00, 0x00, 0xFE, 0xFF][..],
    ]
    .iter()
    .any(|bom| bytes.starts_with(bom));
    if has_bom {
        return Ok(None);
    }
    match std::str::from_utf8(&bytes) {
        Ok(_) => {}
        // A character cut off by the end of the prefix is fine.
        Err(error) if error.error_len().is_none() => {}
        Err(_) => return Ok(None),
    }
    let crlf = bytes.windows(2).filter(|w| w == b"\r\n").count();
    let lf = bytes.iter().filter(|b| **b == b'\n').count() - crlf;
    let cr = bytes.iter().filter(|b| **b == b'\r').count() - crlf;
    Ok(Some(if lf >= crlf && lf >= cr {
        LineEnding::Lf
    } else if crlf >= cr {
        LineEnding::CrLf
    } else {
        LineEnding::Cr
    }))
}

static NEXT_COPY: AtomicU64 = AtomicU64::new(1);

fn large_copy_name(dir: &Path) -> PathBuf {
    let (pid, start) = this_process();
    dir.join(format!(
        "{pid}-{start}-{}.utf8",
        NEXT_COPY.fetch_add(1, Ordering::Relaxed)
    ))
}

fn open_mapped(path: &Path, notify: &Notify) -> Result<MappedStore, DocError> {
    MappedStore::open_with_notify(path, notify.clone())
        .map_err(|e| DocError::io("QuickEdit could not open", path, &e))
}

fn load_large(
    job: &LoadJob,
    path: &Path,
    choice: EncodingChoice,
    force_copy: bool,
    size: u64,
) -> Result<Body, DocError> {
    let what = "QuickEdit could not open";
    let cancelled = || job.cancel.load(Ordering::Acquire);
    let plain = if force_copy {
        None
    } else {
        plain_utf8_prefix(path, choice).map_err(|e| DocError::io(what, path, &e))?
    };
    let (base, stamp) = file_marks(path);
    if let Some(ending) = plain {
        let format = Format::new(TextEncoding::Utf8, false, None, ending, false)
            .map_err(|e| DocError::from_codec(what, path, e))?;
        let store = open_mapped(path, &job.notify)?;
        return Ok(Body {
            file: LoadedFile::ReadOnly(store),
            format,
            byte_count: size,
            base,
            stamp,
            temp_copy: None,
            copy_head: None,
        });
    }
    // Another encoding, or a BOM: the mapped store shows a file's own UTF-8 bytes only, so
    // a UTF-8 copy goes beside the session and costs disk, not memory.
    std::fs::create_dir_all(&job.large_dir)
        .map_err(|e| DocError::io("QuickEdit could not make its folder", &job.large_dir, &e))?;
    let copy = large_copy_name(&job.large_dir);
    let format = qe_codec::transcode_to_utf8_file(path, &copy, choice, |_, _| {}, cancelled)
        .map_err(|e| {
            remove_quietly(&copy);
            DocError::from_codec(what, path, e)
        })?;
    let store = open_mapped(&copy, &job.notify).inspect_err(|_| remove_quietly(&copy))?;
    Ok(Body {
        file: LoadedFile::ReadOnly(store),
        format,
        byte_count: size,
        base,
        stamp,
        temp_copy: Some(copy),
        copy_head: None,
    })
}

pub(crate) fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("QuickEdit: could not remove {}: {error}", path.display());
    }
}

fn file_marks(path: &Path) -> (Option<FileBase>, Option<FileStamp>) {
    (
        file_base(path).ok().map(|(base, _)| base),
        file_stamp(path).ok().flatten(),
    )
}

fn load_file(
    job: &LoadJob,
    path: &Path,
    choice: EncodingChoice,
    compare: Option<&Arc<dyn TextSnapshot>>,
    force_copy: bool,
) -> Result<Loaded, DocError> {
    let what = "QuickEdit could not open";
    let (base, is_folder) = file_base(path).map_err(|e| DocError::io(what, path, &e))?;
    if is_folder {
        return Err(DocError::new(
            DocErrorKind::IsAFolder,
            Some(path),
            format!("{} is a folder, not a file.", path.display()),
        ));
    }
    if base.size > job.limits.read_only {
        return load_large(job, path, choice, force_copy, base.size).map(Loaded::Body);
    }
    let mut tries = 0;
    let decoded = loop {
        tries += 1;
        let stamp = file_stamp(path).map_err(|e| DocError::from_codec(what, path, e))?;
        let cancel = || job.cancel.load(Ordering::Acquire);
        match decode_file(path, choice, |_, _| {}, cancel) {
            Ok(decoded) => break (decoded, stamp),
            // Another program is writing. Its next change brings a new reload as well.
            Err(CodecError::SourceChanged { .. }) if tries < READ_TRIES => {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            Err(error) => return Err(DocError::from_codec(what, path, error)),
        }
    };
    let (decoded, stamp) = decoded;
    // The file may have grown past the limit since its size was read.
    let (base, _) = file_base(path).map_err(|e| DocError::io(what, path, &e))?;
    if base.size > job.limits.read_only {
        drop(decoded);
        return load_large(job, path, choice, force_copy, base.size).map(Loaded::Body);
    }
    if let Some(shown) = compare
        && same_text(shown.as_ref(), &decoded.text)
    {
        return Ok(Loaded::SameText {
            format: decoded.format,
            byte_count: base.size,
            base: Some(base),
            stamp,
        });
    }
    room_for_tree(Some(path), decoded.text.len())?;
    let store = PieceStore::new(&decoded.text);
    let ending = ending_text(decoded.format.main_line_ending);
    Ok(Loaded::Body(Body {
        file: LoadedFile::Editable { store, ending },
        format: decoded.format,
        byte_count: base.size,
        base: Some(base),
        stamp,
        temp_copy: None,
        copy_head: None,
    }))
}

/// Text from an autosaved copy. A damaged copy stays on disk under a new name, and the next
/// older one is tried.
fn load_autosave(
    generations: &[PathBuf],
    problems: &mut Vec<DocError>,
) -> Result<Loaded, DocError> {
    let mut last = None;
    for path in generations {
        match read_generation(path) {
            Ok((meta, text)) => {
                room_for_tree(Some(path), text.len())?;
                let store = PieceStore::new(&text);
                let ending = ending_text(meta.format.main_line_ending);
                return Ok(Loaded::Body(Body {
                    file: LoadedFile::Editable { store, ending },
                    byte_count: text.len() as u64,
                    format: meta.format,
                    base: None,
                    stamp: None,
                    temp_copy: None,
                    copy_head: Some(CopyHead {
                        base: meta.base,
                        changed_while_away: meta.changed_while_away,
                    }),
                }));
            }
            Err(mut problem) => {
                if problem.kind == DocErrorKind::AutosaveDamaged {
                    let kept = path.with_extension("damaged");
                    match std::fs::rename(path, &kept) {
                        Ok(()) => problem.left_on_disk.push(kept),
                        Err(error) => {
                            eprintln!("QuickEdit: could not rename {}: {error}", path.display());
                        }
                    }
                }
                last = Some(problem.clone());
                problems.push(problem);
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        DocError::new(
            DocErrorKind::AutosaveDamaged,
            None,
            "No autosaved copy was found.",
        )
    }))
}

pub(crate) fn read_all_text(snapshot: &dyn TextSnapshot) -> Result<String, DocError> {
    let length = snapshot.len_bytes();
    let mut text = String::new();
    text.try_reserve_exact(length.0).map_err(|_| {
        DocError::new(
            DocErrorKind::OutOfMemory,
            None,
            format!(
                "There is not enough memory to copy {} MiB of text.",
                length.0 >> 20
            ),
        )
    })?;
    if !snapshot.visit_chunks(ByteOffset(0)..length, &mut |chunk| text.push_str(chunk)) {
        return Err(DocError::new(
            DocErrorKind::Other,
            None,
            "The text could not be read back from memory.",
        ));
    }
    Ok(text)
}

pub(crate) fn run_load(job: &LoadJob) -> LoadResult {
    let mut problems = Vec::new();
    // Closed while it waited: no disk and no network for a document nobody will see.
    if job.cancel.load(Ordering::Acquire) {
        return LoadResult {
            loaded: Err(DocError::new(
                DocErrorKind::Cancelled,
                None,
                "The document closed before its text was read.",
            )),
            problems,
        };
    }
    let loaded = match &job.source {
        Source::File {
            path,
            choice,
            compare,
            force_copy,
        } => load_file(job, path, *choice, compare.as_ref(), *force_copy),
        Source::Autosave { generations } => load_autosave(generations, &mut problems),
        Source::Snapshot { snapshot, format } => {
            read_all_text(snapshot.as_ref()).and_then(|text| {
                room_for_tree(None, text.len())?;
                let store = PieceStore::new(&text);
                Ok(Loaded::Body(Body {
                    byte_count: text.len() as u64,
                    file: LoadedFile::Editable {
                        store,
                        ending: ending_text(format.main_line_ending),
                    },
                    format: format.clone(),
                    base: None,
                    stamp: None,
                    temp_copy: None,
                    copy_head: None,
                }))
            })
        }
        Source::Mapped { path, format } => open_mapped(path, &job.notify).map(|store| {
            let byte_count = file_base(path).map_or(0, |(base, _)| base.size);
            Loaded::Body(Body {
                file: LoadedFile::ReadOnly(store),
                format: format.clone(),
                byte_count,
                base: None,
                stamp: None,
                temp_copy: None,
                copy_head: None,
            })
        }),
    };
    LoadResult { loaded, problems }
}
