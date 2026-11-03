//! What lies on disk under `%LOCALAPPDATA%\QuickEdit\session`.
//!
//! ```text
//! session\session.lock        held open while a QuickEdit uses this folder
//! session\session.txt         the windows and tabs, replaced whole in one rename
//! session\docs\<key>.<n>.qea  the unsaved text of one document, generation n
//! session\docs\<key>.<n>.tmp  a generation being written
//! ```
//!
//! A generation is written to its `.tmp` name, flushed, and only then renamed. The older
//! generation goes after that rename, so a kill at any moment leaves at least one whole
//! `.qea`. A `.qea` carries its own description, length and hash, so the text comes back
//! even if `session.txt` is older than it or damaged.
use crate::error::{DocError, DocErrorKind};
use crate::escape::{escape_path, escape_text, unescape_path, unescape_text};
use crate::identity::{FileBase, FileIdentity, process_start, this_process};
use qe_api::{ByteOffset, EncodingChoice, TextEncoding, TextSnapshot};
use qe_codec::{Endian, Format, LineEnding};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const MANIFEST: &str = "session.txt";
const MANIFEST_HEADER: &str = "QuickEditSession 1";
const AUTOSAVE_MAGIC: &str = "QEA1 ";
const TRAILER: &str = "\nQEAEND ";
/// Long enough for the description of any document, with its long path names.
const MAX_HEADER_BYTES: u64 = 256 * 1024;

/// FNV-1a over 64 bits, a byte at a time, so where the chunks fall makes no difference.
#[derive(Clone, Copy)]
pub(crate) struct Fnv(u64);
impl Fnv {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    pub(crate) fn add(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }
    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

/// Which encoding a reload and a revert use for this document. Automatic is a real choice,
/// apart from no choice at all, which follows the Open setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenChoice {
    FromSetting,
    Chosen(EncodingChoice),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WindowFrame {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShowState {
    #[default]
    Normal,
    Maximized,
    FullScreen,
}

/// One document's description, in `session.txt` and at the head of its `.qea`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocMeta {
    pub(crate) key: String,
    pub(crate) window: u64,
    pub(crate) path: Option<PathBuf>,
    pub(crate) name: Option<String>,
    pub(crate) untitled_number: u32,
    pub(crate) format: Format,
    pub(crate) open_choice: OpenChoice,
    pub(crate) anchor: u64,
    pub(crate) caret: u64,
    pub(crate) top: u64,
    pub(crate) base: Option<FileBase>,
    pub(crate) prevent_editing: bool,
    pub(crate) wrap_to_page: bool,
    pub(crate) read_only: bool,
    pub(crate) copy_source: Option<PathBuf>,
    pub(crate) has_autosave: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowMeta {
    pub(crate) id: u64,
    pub(crate) frame: WindowFrame,
    pub(crate) show: ShowState,
    pub(crate) active: Option<String>,
}

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn encode_meta(meta: &DocMeta) -> String {
    let mut line = format!(
        "key={} window={} number={} enc={} bom={} endian={} ending={} lossy={} anchor={} caret={} top={} prevent={} page={} ro={} auto={}",
        meta.key,
        meta.window,
        meta.untitled_number,
        escape_text(meta.format.encoding.name()),
        flag(meta.format.has_bom),
        match meta.format.endian {
            None => "-",
            Some(Endian::Little) => "l",
            Some(Endian::Big) => "b",
        },
        match meta.format.main_line_ending {
            LineEnding::Lf => "lf",
            LineEnding::CrLf => "crlf",
            LineEnding::Cr => "cr",
        },
        flag(meta.format.lossy),
        meta.anchor,
        meta.caret,
        meta.top,
        flag(meta.prevent_editing),
        flag(meta.wrap_to_page),
        flag(meta.read_only),
        flag(meta.has_autosave),
    );
    line.push_str(" open=");
    line.push_str(&match meta.open_choice {
        OpenChoice::FromSetting => "setting".to_owned(),
        OpenChoice::Chosen(EncodingChoice::Automatic) => "auto".to_owned(),
        OpenChoice::Chosen(EncodingChoice::Specific(e)) => format!("e:{}", escape_text(e.name())),
    });
    if let Some(path) = &meta.path {
        line.push_str(&format!(" path={}", escape_path(path)));
    }
    if let Some(name) = &meta.name {
        line.push_str(&format!(" name={}", escape_text(name)));
    }
    if let Some(path) = &meta.copy_source {
        line.push_str(&format!(" source={}", escape_path(path)));
    }
    if let Some(base) = &meta.base {
        line.push_str(&format!(
            " size={} written={} vol={} id={:x} locked={}",
            base.size,
            base.written,
            base.identity.volume,
            base.identity.file,
            flag(base.read_only)
        ));
    }
    line
}

fn fields(line: &str) -> BTreeMap<&str, &str> {
    line.split(' ').filter_map(|f| f.split_once('=')).collect()
}

fn encoding_by_name(name: &str) -> Option<TextEncoding> {
    TextEncoding::ALL.iter().copied().find(|e| e.name() == name)
}

fn decode_meta(line: &str) -> Option<DocMeta> {
    let map = fields(line);
    let number = |key: &str| map.get(key)?.parse::<u64>().ok();
    let on = |key: &str| map.get(key).is_some_and(|v| *v == "1");
    let encoding = encoding_by_name(&unescape_text(map.get("enc")?)?)?;
    let endian = match *map.get("endian")? {
        "-" => None,
        "l" => Some(Endian::Little),
        "b" => Some(Endian::Big),
        _ => return None,
    };
    let ending = match *map.get("ending")? {
        "lf" => LineEnding::Lf,
        "crlf" => LineEnding::CrLf,
        "cr" => LineEnding::Cr,
        _ => return None,
    };
    let format = Format::new(encoding, on("bom"), endian, ending, on("lossy")).ok()?;
    let open_choice = match *map.get("open")? {
        "setting" => OpenChoice::FromSetting,
        "auto" => OpenChoice::Chosen(EncodingChoice::Automatic),
        other => OpenChoice::Chosen(EncodingChoice::Specific(encoding_by_name(&unescape_text(
            other.strip_prefix("e:")?,
        )?)?)),
    };
    let base = match (number("size"), number("written"), number("vol")) {
        (Some(size), Some(written), Some(volume)) => Some(FileBase {
            size,
            written,
            identity: FileIdentity {
                volume,
                file: u128::from_str_radix(map.get("id")?, 16).ok()?,
            },
            read_only: on("locked"),
        }),
        _ => None,
    };
    let key = (*map.get("key")?).to_owned();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(DocMeta {
        key,
        window: number("window")?,
        path: match map.get("path") {
            Some(text) => Some(unescape_path(text)?),
            None => None,
        },
        name: match map.get("name") {
            Some(text) => Some(unescape_text(text)?),
            None => None,
        },
        untitled_number: u32::try_from(number("number")?).ok()?,
        format,
        open_choice,
        anchor: number("anchor")?,
        caret: number("caret")?,
        top: number("top")?,
        base,
        prevent_editing: on("prevent"),
        wrap_to_page: on("page"),
        read_only: on("ro"),
        copy_source: match map.get("source") {
            Some(text) => Some(unescape_path(text)?),
            None => None,
        },
        has_autosave: on("auto"),
    })
}

pub(crate) fn encode_manifest(windows: &[WindowMeta], docs: &[DocMeta]) -> String {
    let mut out = String::from(MANIFEST_HEADER);
    out.push('\n');
    for window in windows {
        out.push_str(&format!(
            "window id={} x={} y={} w={} h={} show={}",
            window.id,
            window.frame.x,
            window.frame.y,
            window.frame.width,
            window.frame.height,
            match window.show {
                ShowState::Normal => "normal",
                ShowState::Maximized => "max",
                ShowState::FullScreen => "full",
            },
        ));
        if let Some(active) = &window.active {
            out.push_str(&format!(" active={active}"));
        }
        out.push('\n');
    }
    for doc in docs {
        out.push_str("doc ");
        out.push_str(&encode_meta(doc));
        out.push('\n');
    }
    out.push_str(&format!("end {} {}\n", windows.len(), docs.len()));
    out
}

pub(crate) fn decode_manifest(text: &str) -> Option<(Vec<WindowMeta>, Vec<DocMeta>)> {
    let mut lines = text.lines();
    if lines.next() != Some(MANIFEST_HEADER) {
        return None;
    }
    let (mut windows, mut docs) = (Vec::new(), Vec::new());
    let mut ended = false;
    for line in lines {
        if let Some(rest) = line.strip_prefix("window ") {
            let map = fields(rest);
            let int = |key: &str| map.get(key)?.parse::<i32>().ok();
            windows.push(WindowMeta {
                id: map.get("id")?.parse().ok()?,
                frame: WindowFrame {
                    x: int("x")?,
                    y: int("y")?,
                    width: int("w")?,
                    height: int("h")?,
                },
                show: match *map.get("show")? {
                    "max" => ShowState::Maximized,
                    "full" => ShowState::FullScreen,
                    _ => ShowState::Normal,
                },
                active: map.get("active").map(|s| (*s).to_owned()),
            });
        } else if let Some(rest) = line.strip_prefix("doc ") {
            docs.push(decode_meta(rest)?);
        } else {
            let rest = line.strip_prefix("end ")?;
            let mut counts = rest.split(' ').map(str::parse::<usize>);
            if counts.next()?.ok()? != windows.len() || counts.next()?.ok()? != docs.len() {
                return None;
            }
            ended = true;
        }
    }
    ended.then_some((windows, docs))
}

/// Replaces `session.txt` whole. A reader finds the old file or the new one.
pub(crate) fn write_manifest(dir: &Path, text: &str) -> Result<(), DocError> {
    let (pid, _) = this_process();
    let temp = dir.join(format!("session.tmp-{pid}"));
    let target = dir.join(MANIFEST);
    let what = "QuickEdit could not save its list of windows at";
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Another program, such as a virus scanner, can hold a file for a moment.
        let mut tries = 0;
        loop {
            match std::fs::rename(&temp, &target) {
                Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33)) && tries < 10 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                other => return other,
            }
        }
    })();
    result.map_err(|e| {
        if let Err(gone) = std::fs::remove_file(&temp)
            && gone.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("QuickEdit: could not remove {}: {gone}", temp.display());
        }
        DocError::io(what, &target, &e)
    })
}

pub(crate) fn generation_name(key: &str, generation: u64) -> String {
    format!("{key}.{generation}.qea")
}

/// What one autosave must write.
pub(crate) struct AutosaveWrite<'a> {
    pub(crate) docs_dir: &'a Path,
    pub(crate) meta: &'a DocMeta,
    pub(crate) generation: u64,
    pub(crate) previous: Option<u64>,
    pub(crate) snapshot: &'a dyn TextSnapshot,
    pub(crate) cancel: &'a AtomicBool,
    /// Called with the running byte count. An error stops the write, as a full disk would.
    pub(crate) progress: &'a dyn Fn(u64) -> std::io::Result<()>,
}

/// Writes one generation. `Ok(false)` means it was cancelled and left nothing.
pub(crate) fn write_generation(job: &AutosaveWrite<'_>) -> Result<bool, DocError> {
    let name = generation_name(&job.meta.key, job.generation);
    let target = job.docs_dir.join(&name);
    let temp = target.with_extension("tmp");
    let what = "QuickEdit could not write the autosaved copy at";
    let fail = |error: std::io::Error| {
        if let Err(gone) = std::fs::remove_file(&temp)
            && gone.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("QuickEdit: could not remove {}: {gone}", temp.display());
        }
        let mut out = DocError::io(what, &target, &error);
        if out.kind != DocErrorKind::DiskFull {
            out.kind = DocErrorKind::AutosaveFailed;
        }
        out
    };
    std::fs::create_dir_all(job.docs_dir).map_err(fail)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(fail)?;
    let mut buffer = Vec::with_capacity(1 << 20);
    let head = format!("{AUTOSAVE_MAGIC}{}\n", encode_meta(job.meta));
    buffer.extend_from_slice(head.as_bytes());
    let mut hash = Fnv::new();
    let mut written = 0u64;
    let mut failure: Option<std::io::Error> = None;
    let length = job.snapshot.len_bytes();
    let read_all = job
        .snapshot
        .visit_chunks_while(ByteOffset(0)..length, &mut |chunk| {
            if job.cancel.load(Ordering::Acquire) {
                return false;
            }
            hash.add(chunk.as_bytes());
            written += chunk.len() as u64;
            if buffer.len() + chunk.len() > buffer.capacity() {
                if let Err(error) = file.write_all(&buffer) {
                    failure = Some(error);
                    return false;
                }
                buffer.clear();
            }
            if chunk.len() >= buffer.capacity() {
                if let Err(error) = file.write_all(chunk.as_bytes()) {
                    failure = Some(error);
                    return false;
                }
            } else {
                buffer.extend_from_slice(chunk.as_bytes());
            }
            if let Err(error) = (job.progress)(written) {
                failure = Some(error);
                return false;
            }
            true
        });
    if let Some(error) = failure {
        drop(file);
        return Err(fail(error));
    }
    if job.cancel.load(Ordering::Acquire) {
        drop(file);
        std::fs::remove_file(&temp).map_err(fail)?;
        return Ok(false);
    }
    if !read_all || written != length.0 as u64 {
        drop(file);
        return Err(fail(std::io::Error::other(
            "the text could not be read back from memory",
        )));
    }
    let tail = format!("{TRAILER}{written} {:016x}\n", hash.value());
    buffer.extend_from_slice(tail.as_bytes());
    let finished = file.write_all(&buffer).and_then(|()| file.sync_all());
    drop(file);
    finished.map_err(fail)?;
    std::fs::rename(&temp, &target).map_err(fail)?;
    if let Some(previous) = job.previous
        && previous != job.generation
    {
        let old = job.docs_dir.join(generation_name(&job.meta.key, previous));
        if let Err(error) = std::fs::remove_file(&old)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // The new copy is safe. The next restore prefers it and removes the old one.
            eprintln!("QuickEdit: could not remove {}: {error}", old.display());
        }
    }
    Ok(true)
}

/// Removes every generation of one document. Errors name the file, since an autosaved
/// copy that stays can come back at the next launch.
pub(crate) fn remove_generations(docs_dir: &Path, key: &str, below: u64) -> Result<(), DocError> {
    let entries = match std::fs::read_dir(docs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(DocError::io(
                "QuickEdit could not read the autosave folder",
                docs_dir,
                &error,
            ));
        }
    };
    let prefix = format!("{key}.");
    let mut first_error = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let older = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split('.').next())
            .and_then(|generation| generation.parse::<u64>().ok())
            .is_some_and(|generation| generation < below);
        if older
            && let Err(error) = std::fs::remove_file(entry.path())
            && error.kind() != std::io::ErrorKind::NotFound
            && first_error.is_none()
        {
            first_error = Some(DocError::io(
                "QuickEdit could not remove the autosaved copy, so it may come back at the next launch:",
                &entry.path(),
                &error,
            ));
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The text of one `.qea`, whole and checked.
pub(crate) fn read_generation(path: &Path) -> Result<(DocMeta, String), DocError> {
    let damaged = |why: &str| {
        DocError::new(
            DocErrorKind::AutosaveDamaged,
            Some(path),
            format!(
                "The autosaved copy at {} is damaged: {why}.",
                path.display()
            ),
        )
    };
    let what = "QuickEdit could not read the autosaved copy at";
    let mut file = File::open(path).map_err(|e| DocError::io(what, path, &e))?;
    let size = file
        .metadata()
        .map_err(|e| DocError::io(what, path, &e))?
        .len();
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size as usize)
        .map_err(|_| damaged_memory(path, size))?;
    file.read_to_end(&mut bytes)
        .map_err(|e| DocError::io(what, path, &e))?;
    let head_end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .ok_or_else(|| damaged("it has no description line"))?;
    let head = std::str::from_utf8(&bytes[..head_end]).map_err(|_| damaged("bad description"))?;
    let meta = head
        .strip_prefix(AUTOSAVE_MAGIC)
        .and_then(decode_meta)
        .ok_or_else(|| damaged("bad description"))?;
    let trailer_at = bytes
        .windows(TRAILER.len())
        .rposition(|w| w == TRAILER.as_bytes())
        .filter(|at| *at + 1 > head_end)
        .ok_or_else(|| damaged("it has no end mark"))?;
    let trailer = std::str::from_utf8(&bytes[trailer_at + TRAILER.len()..])
        .map_err(|_| damaged("bad end mark"))?;
    let mut parts = trailer.trim_end_matches('\n').split(' ');
    let length: u64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| damaged("bad end mark"))?;
    let expected = parts
        .next()
        .and_then(|p| u64::from_str_radix(p, 16).ok())
        .ok_or_else(|| damaged("bad end mark"))?;
    let start = head_end + 1;
    if trailer_at < start || (trailer_at - start) as u64 != length {
        return Err(damaged("its length is wrong"));
    }
    let mut hash = Fnv::new();
    hash.add(&bytes[start..trailer_at]);
    if hash.value() != expected {
        return Err(damaged("its check value is wrong"));
    }
    bytes.truncate(trailer_at);
    bytes.drain(..start);
    let text = String::from_utf8(bytes).map_err(|_| damaged("its text is not UTF-8"))?;
    Ok((meta, text))
}

fn damaged_memory(path: &Path, size: u64) -> DocError {
    DocError::new(
        DocErrorKind::OutOfMemory,
        Some(path),
        format!(
            "There is not enough memory to read the autosaved copy at {} ({} MiB). It stays on disk.",
            path.display(),
            size >> 20
        ),
    )
}

fn read_generation_head(path: &Path) -> Option<DocMeta> {
    let file = File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(MAX_HEADER_BYTES).read_to_end(&mut head).ok()?;
    let end = head.iter().position(|b| *b == b'\n')?;
    decode_meta(
        std::str::from_utf8(&head[..end])
            .ok()?
            .strip_prefix(AUTOSAVE_MAGIC)?,
    )
}

/// This QuickEdit's hold on a session folder. A second one takes the next folder.
pub(crate) struct SessionLock {
    pub(crate) dir: PathBuf,
    _file: File,
}

fn try_lock(dir: &Path) -> Result<Option<File>, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(dir.join("session.lock"))
    {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.raw_os_error() == Some(32) => Ok(None),
        Err(error) => Err(error),
    }
}

fn session_folder(root: &Path, index: u32) -> PathBuf {
    if index == 1 {
        root.join("session")
    } else {
        root.join(format!("session-{index}"))
    }
}

/// One document as the restore found it.
pub(crate) struct RestoreDoc {
    pub(crate) meta: DocMeta,
    /// Its `.qea` files, newest first.
    pub(crate) generations: Vec<(u64, PathBuf)>,
}

pub(crate) struct RestorePlan {
    pub(crate) lock: SessionLock,
    pub(crate) windows: Vec<WindowMeta>,
    pub(crate) docs: Vec<RestoreDoc>,
    pub(crate) problems: Vec<DocError>,
    pub(crate) recent: Vec<PathBuf>,
    pub(crate) next_generation: u64,
}

fn list_generations(docs_dir: &Path) -> BTreeMap<String, Vec<(u64, PathBuf)>> {
    let mut found: BTreeMap<String, Vec<(u64, PathBuf)>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(docs_dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // A `.tmp` was never finished. The `.qea` beside it is the last good copy.
        if name.ends_with(".tmp") {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!("QuickEdit: could not remove {}: {error}", path.display());
            }
            continue;
        }
        let Some(stem) = name.strip_suffix(".qea") else {
            continue;
        };
        let Some((key, generation)) = stem.rsplit_once('.') else {
            continue;
        };
        let Ok(generation) = generation.parse::<u64>() else {
            continue;
        };
        found
            .entry(key.to_owned())
            .or_default()
            .push((generation, path));
    }
    for list in found.values_mut() {
        list.sort_by_key(|(generation, _)| std::cmp::Reverse(*generation));
    }
    found
}

fn keep_broken_copy(path: &Path, name: &str) -> Option<PathBuf> {
    let target = path.with_file_name(name);
    match std::fs::copy(path, &target) {
        Ok(_) => Some(target),
        Err(error) => {
            eprintln!("QuickEdit: could not copy {}: {error}", path.display());
            None
        }
    }
}

fn read_session(dir: &Path, problems: &mut Vec<DocError>) -> (Vec<WindowMeta>, Vec<DocMeta>) {
    let path = dir.join(MANIFEST);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Default::default(),
        Err(error) => {
            problems.push(DocError::io(
                "QuickEdit could not read its list of windows at",
                &path,
                &error,
            ));
            return Default::default();
        }
    };
    match std::str::from_utf8(&bytes).ok().and_then(decode_manifest) {
        Some(parsed) => parsed,
        None => {
            let kept = keep_broken_copy(&path, "session.broken.txt");
            let mut problem = DocError::new(
                DocErrorKind::SessionDamaged,
                Some(&path),
                "QuickEdit's list of windows is damaged. Unsaved text comes back from its autosaved copies; windows and caret places do not.",
            );
            problem.left_on_disk.extend(kept);
            problems.push(problem);
            Default::default()
        }
    }
}

/// Removes temp files of large documents whose QuickEdit has ended.
fn remove_stale_large_files(root: &Path, own: (u32, u64)) {
    let folder = root.join("large");
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut parts = name.split('-');
        let pid = parts.next().and_then(|p| p.parse::<u32>().ok());
        let start = parts.next().and_then(|p| p.parse::<u64>().ok());
        let alive = match (pid, start) {
            (Some(pid), Some(start)) => (pid, start) == own || process_start(pid) == Some(start),
            _ => false,
        };
        if !alive && let Err(error) = std::fs::remove_file(entry.path()) {
            eprintln!(
                "QuickEdit: could not remove {}: {error}",
                entry.path().display()
            );
        }
    }
}

/// A list's temp file names its process, and only a killed process leaves one. A temp of a
/// running copy in the shared root costs that copy one failed write, which it names and retries.
fn remove_stale_list_temps(dir: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix(prefix)
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if process_start(pid).is_none()
            && let Err(error) = std::fs::remove_file(entry.path())
        {
            eprintln!(
                "QuickEdit: could not remove {}: {error}",
                entry.path().display()
            );
        }
    }
}

/// Takes a session folder and reads what the last run left, from this folder and from any
/// other folder no running QuickEdit holds. It reads only descriptions, never text.
pub(crate) fn build_restore_plan(root: &Path) -> Result<RestorePlan, DocError> {
    let what = "QuickEdit could not use its session folder at";
    let mut lock = None;
    let mut index = 1;
    while lock.is_none() {
        let dir = session_folder(root, index);
        match try_lock(&dir) {
            Ok(Some(file)) => {
                lock = Some(SessionLock { dir, _file: file });
            }
            Ok(None) => index += 1,
            Err(error) => return Err(DocError::io(what, &dir, &error)),
        }
        if index > 64 {
            return Err(DocError::new(
                DocErrorKind::Other,
                Some(root),
                "More than 64 copies of QuickEdit seem to be running, so this one cannot keep a session.",
            ));
        }
    }
    let Some(lock) = lock else {
        return Err(DocError::new(DocErrorKind::Other, Some(root), "no session"));
    };
    let mut problems = Vec::new();
    remove_stale_large_files(root, this_process());
    remove_stale_list_temps(&lock.dir, "session.tmp-");
    remove_stale_list_temps(root, "recent.tmp-");
    let (mut windows, mut docs) = read_session(&lock.dir, &mut problems);
    let docs_dir = lock.dir.join("docs");

    // Sessions of other copies that have ended join this one. Their files move first, then
    // this session's list is written, and only then does their list go. A kill in between
    // finds the moved files as autosaved copies with no row, which come back all the same.
    let mut merged: Vec<PathBuf> = Vec::new();
    for other in 1..=64 {
        let dir = session_folder(root, other);
        if dir == lock.dir || !dir.join("session.lock").exists() {
            continue;
        }
        let Ok(Some(held)) = try_lock(&dir) else {
            continue;
        };
        let (their_windows, their_docs) = read_session(&dir, &mut problems);
        let offset = u64::from(other) * 1_000_000;
        windows.extend(their_windows.into_iter().map(|mut w| {
            w.id += offset;
            w
        }));
        docs.extend(their_docs.into_iter().map(|mut d| {
            d.window += offset;
            d
        }));
        if let Err(error) = std::fs::create_dir_all(&docs_dir) {
            problems.push(DocError::io(what, &docs_dir, &error));
        }
        for list in list_generations(&dir.join("docs")).into_values() {
            for (_, path) in list {
                if let Some(name) = path.file_name()
                    && let Err(error) = std::fs::rename(&path, docs_dir.join(name))
                {
                    problems.push(DocError::io(
                        "QuickEdit could not move the autosaved copy at",
                        &path,
                        &error,
                    ));
                }
            }
        }
        drop(held);
        merged.push(dir);
    }

    let mut generations = list_generations(&docs_dir);
    let next_generation = generations
        .values()
        .flat_map(|list| list.iter().map(|(generation, _)| *generation))
        .max()
        .unwrap_or(0)
        + 1;
    let mut out: Vec<RestoreDoc> = Vec::with_capacity(docs.len());
    for meta in docs {
        let files = generations.remove(&meta.key).unwrap_or_default();
        if meta.has_autosave && files.is_empty() {
            problems.push(DocError::new(
                DocErrorKind::AutosaveDamaged,
                meta.path.as_deref(),
                match &meta.path {
                    Some(path) => format!(
                        "The autosaved copy of {} is missing, so the file comes back as it is on disk.",
                        path.display()
                    ),
                    None => "The autosaved copy of an untitled document is missing, so that document cannot come back.".to_owned(),
                },
            ));
            if meta.path.is_none() {
                continue;
            }
        }
        out.push(RestoreDoc {
            meta,
            generations: files,
        });
    }
    // A copy with no row: a kill came between its rename and the list, or the list is
    // damaged. Its own description brings it back, in the last window.
    let fallback = windows.last().map(|w| w.id);
    for (key, files) in generations {
        let Some(head) = files
            .iter()
            .find_map(|(_, path)| read_generation_head(path))
        else {
            for (_, path) in &files {
                problems.push(DocError::new(
                    DocErrorKind::AutosaveDamaged,
                    Some(path),
                    format!(
                        "The autosaved copy at {} cannot be read. It stays on disk.",
                        path.display()
                    ),
                ));
            }
            continue;
        };
        let mut meta = head;
        meta.key = key;
        if !windows.iter().any(|w| w.id == meta.window) {
            meta.window = fallback.unwrap_or(meta.window);
            if fallback.is_none() {
                windows.push(WindowMeta {
                    id: meta.window,
                    frame: WindowFrame::default(),
                    show: ShowState::Normal,
                    active: None,
                });
            }
        }
        out.push(RestoreDoc {
            meta,
            generations: files,
        });
    }
    let doc_rows: Vec<DocMeta> = out.iter().map(|d| d.meta.clone()).collect();
    if !merged.is_empty() {
        write_manifest(&lock.dir, &encode_manifest(&windows, &doc_rows))?;
        for dir in merged {
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                eprintln!(
                    "QuickEdit: could not remove the old session folder {}: {error}",
                    dir.display()
                );
            }
        }
    }
    let recent = read_recent(root, &mut problems);
    Ok(RestorePlan {
        lock,
        windows,
        docs: out,
        problems,
        recent,
        next_generation,
    })
}

fn read_recent(root: &Path, problems: &mut Vec<DocError>) -> Vec<PathBuf> {
    let path = root.join(crate::recent::RECENT_FILE);
    let text = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            problems.push(DocError::io(
                "QuickEdit could not read Open Recent at",
                &path,
                &error,
            ));
            return Vec::new();
        }
    };
    let (list, bad) = crate::recent::decode_recent(&text);
    if bad > 0 {
        problems.push(DocError::new(
            DocErrorKind::Other,
            Some(&path),
            format!("{bad} lines of Open Recent could not be read and were dropped."),
        ));
    }
    list
}

/// Writes `recent.txt` the same way as `session.txt`.
pub(crate) fn write_recent(root: &Path, text: &str) -> Result<(), DocError> {
    let target = root.join(crate::recent::RECENT_FILE);
    let (pid, _) = this_process();
    let temp = root.join(format!("recent.tmp-{pid}"));
    let result = (|| {
        std::fs::create_dir_all(root)?;
        let mut file = File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)
    })();
    result.map_err(|e| DocError::io("QuickEdit could not save Open Recent at", &target, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> DocMeta {
        DocMeta {
            key: "ab12".into(),
            window: 3,
            path: Some(PathBuf::from(r"C:\a b\c=d%.txt")),
            name: Some("My copy.txt".into()),
            untitled_number: 2,
            format: Format::new(
                TextEncoding::Utf16Be,
                true,
                Some(Endian::Big),
                LineEnding::CrLf,
                true,
            )
            .unwrap(),
            open_choice: OpenChoice::Chosen(EncodingChoice::Specific(TextEncoding::Utf8)),
            anchor: 5,
            caret: 9,
            top: 2,
            base: Some(FileBase {
                size: 10,
                written: 20,
                identity: FileIdentity {
                    volume: 30,
                    file: 40,
                },
                read_only: true,
            }),
            prevent_editing: true,
            wrap_to_page: true,
            read_only: false,
            copy_source: Some(PathBuf::from(r"C:\big file")),
            has_autosave: true,
        }
    }

    #[test]
    fn a_manifest_comes_back_field_for_field() {
        let windows = vec![WindowMeta {
            id: 3,
            frame: WindowFrame {
                x: -5,
                y: 6,
                width: 700,
                height: 800,
            },
            show: ShowState::Maximized,
            active: Some("ab12".into()),
        }];
        let docs = vec![meta()];
        let text = encode_manifest(&windows, &docs);
        assert_eq!(decode_manifest(&text).unwrap(), (windows, docs));
    }

    #[test]
    fn a_cut_or_changed_manifest_is_refused() {
        let text = encode_manifest(&[], &[meta()]);
        assert!(decode_manifest(&text[..text.len() / 2]).is_none(), "cut");
        assert!(decode_manifest(&text.replace("end 0 1", "end 0 2")).is_none());
        assert!(decode_manifest(&text.replace("doc key", "dog key")).is_none());
        assert!(decode_manifest("").is_none());
        assert!(decode_manifest(&text.replace("enc=UTF-16", "enc=Klingon")).is_none());
    }
}
