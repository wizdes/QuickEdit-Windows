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
    /// The file changed behind this document's text, and the person has not answered yet.
    pub(crate) changed_while_away: bool,
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
        "key={} window={} number={} enc={} bom={} endian={} ending={} lossy={} anchor={} caret={} top={} prevent={} page={} ro={} auto={} away={}",
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
        flag(meta.changed_while_away),
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
            links: 0,
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
        changed_while_away: on("away"),
    })
}

#[cfg(test)]
pub(crate) fn encode_manifest(windows: &[WindowMeta], docs: &[DocMeta]) -> String {
    encode_session(windows, docs, &BTreeMap::new())
}

/// `dead`: for each document key, the copies below this generation hold nothing to bring
/// back. Their text is on disk or the person chose to discard it, but a removal has not
/// been seen to finish. A copy at or above it was written later, and it is live.
pub(crate) fn encode_session(
    windows: &[WindowMeta],
    docs: &[DocMeta],
    dead: &BTreeMap<String, u64>,
) -> String {
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
    for (key, below) in dead {
        out.push_str(&format!("dead key={key} below={below}\n"));
    }
    out.push_str(&format!("end {} {}\n", windows.len(), docs.len()));
    out
}

#[cfg(test)]
pub(crate) fn decode_manifest(text: &str) -> Option<(Vec<WindowMeta>, Vec<DocMeta>)> {
    decode_session(text).map(|list| (list.windows, list.docs))
}

/// One folder's `session.txt`.
#[derive(Default)]
struct SessionList {
    windows: Vec<WindowMeta>,
    docs: Vec<DocMeta>,
    dead: BTreeMap<String, u64>,
    /// The file is there, and Windows would not let it be read.
    unread: bool,
}

fn decode_session(text: &str) -> Option<SessionList> {
    let mut lines = text.lines();
    if lines.next() != Some(MANIFEST_HEADER) {
        return None;
    }
    let (mut windows, mut docs) = (Vec::new(), Vec::new());
    let mut dead = BTreeMap::new();
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
        } else if let Some(rest) = line.strip_prefix("dead ") {
            let map = fields(rest);
            let key = map.get("key")?;
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            dead.insert((*key).to_owned(), map.get("below")?.parse::<u64>().ok()?);
        } else {
            let rest = line.strip_prefix("end ")?;
            let mut counts = rest.split(' ').map(str::parse::<usize>);
            if counts.next()?.ok()? != windows.len() || counts.next()?.ok()? != docs.len() {
                return None;
            }
            ended = true;
        }
    }
    ended.then_some(SessionList {
        windows,
        docs,
        dead,
        unread: false,
    })
}

/// Another program, such as a virus scanner, can hold a file for a moment.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33)) && tries < 10 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
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
        rename_with_retry(&temp, &target)
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
    rename_with_retry(&temp, &target).map_err(fail)?;
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

/// Removes every generation of one document below `below`. A `.damaged` file is not one: a
/// message has named it to the user as staying. Errors name the file.
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
        if !(name.ends_with(".qea") || name.ends_with(".tmp")) {
            continue;
        }
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
                "QuickEdit could not remove an old autosaved copy, and tries again at the next launch. The copy will not come back:",
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

/// `Ok(None)`: the file was read, and it has no good description. An error: it was not read.
fn read_generation_head(path: &Path) -> std::io::Result<Option<DocMeta>> {
    let file = File::open(path)?;
    let mut head = Vec::new();
    file.take(MAX_HEADER_BYTES).read_to_end(&mut head)?;
    let Some(end) = head.iter().position(|b| *b == b'\n') else {
        return Ok(None);
    };
    Ok(std::str::from_utf8(&head[..end])
        .ok()
        .and_then(|line| line.strip_prefix(AUTOSAVE_MAGIC))
        .and_then(decode_meta))
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
    /// Copies that stay dead until a later launch can remove them.
    pub(crate) dead: BTreeMap<String, u64>,
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

fn keep_broken_copy(path: &Path, target: PathBuf) -> Option<PathBuf> {
    match std::fs::copy(path, &target) {
        Ok(_) => Some(target),
        Err(error) => {
            eprintln!("QuickEdit: could not copy {}: {error}", path.display());
            None
        }
    }
}

/// `broken_copy`: where a damaged list goes, which stays when this folder is merged away.
fn read_session(dir: &Path, broken_copy: PathBuf, problems: &mut Vec<DocError>) -> SessionList {
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
            return SessionList {
                unread: true,
                ..Default::default()
            };
        }
    };
    match std::str::from_utf8(&bytes).ok().and_then(decode_session) {
        Some(parsed) => parsed,
        None => {
            let kept = keep_broken_copy(&path, broken_copy);
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
    let own = read_session(
        &lock.dir,
        lock.dir.join("session.broken.txt"),
        &mut problems,
    );
    let (mut windows, mut docs, mut dead) = (own.windows, own.docs, own.dead);
    let docs_dir = lock.dir.join("docs");

    // Sessions of other copies that have ended join this one. Their files move first, then
    // this session's list is written, and only then does their folder go, with its lock held
    // to the end so that a third launch cannot take it. A kill in between finds the moved
    // files as autosaved copies with no row, or finds rows this list has already, which a
    // key tells apart. A file that will not move keeps its row and its folder for next time.
    let mut merged: Vec<MergedFolder> = Vec::new();
    for other in 1..=64 {
        let dir = session_folder(root, other);
        if dir == lock.dir || !dir.join("session.lock").exists() {
            continue;
        }
        let Ok(Some(held)) = try_lock(&dir) else {
            continue;
        };
        let broken = free_name(&lock.dir, &format!("session-{other}.broken"), "txt");
        let theirs = read_session(&dir, broken, &mut problems);
        if let Err(error) = std::fs::create_dir_all(&docs_dir) {
            problems.push(DocError::io(what, &docs_dir, &error));
        }
        let moved = move_session_files(&dir, &docs_dir, &lock.dir, other, &mut problems);
        let offset = u64::from(other) * 1_000_000;
        let (stuck, joining): (Vec<DocMeta>, Vec<DocMeta>) = theirs
            .docs
            .into_iter()
            .partition(|d| moved.stuck_keys.contains(&d.key));
        let joining: Vec<DocMeta> = joining
            .into_iter()
            .filter(|d| !docs.iter().any(|mine| mine.key == d.key))
            .collect();
        for window in &theirs.windows {
            let id = window.id + offset;
            let has_row = joining.iter().any(|d| d.window == window.id);
            if has_row && !windows.iter().any(|w| w.id == id) {
                let mut joined = window.clone();
                joined.id = id;
                windows.push(joined);
            }
        }
        docs.extend(joining.into_iter().map(|mut d| {
            d.window += offset;
            d
        }));
        for (key, below) in theirs.dead {
            let entry = dead.entry(key).or_insert(below);
            *entry = below.max(*entry);
        }
        let stuck_windows = theirs
            .windows
            .into_iter()
            .filter(|w| stuck.iter().any(|d| d.window == w.id))
            .collect();
        merged.push(MergedFolder {
            dir,
            held,
            whole: moved.whole && !theirs.unread,
            stuck,
            stuck_windows,
        });
    }

    let mut generations = list_generations(&docs_dir);
    let next_generation = generations
        .values()
        .flat_map(|list| list.iter().map(|(generation, _)| *generation))
        .max()
        .map_or(1, |newest| newest + 1)
        .max(dead.values().copied().max().unwrap_or(1));
    let dead = remove_dead_copies(&mut generations, dead, &mut problems);
    let mut out: Vec<RestoreDoc> = Vec::with_capacity(docs.len());
    for meta in docs {
        let files = generations.remove(&meta.key).unwrap_or_default();
        if meta.has_autosave && files.is_empty() && !dead.contains_key(&meta.key) {
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
    for (key, mut files) in generations {
        let Some(head) = usable_head(&mut files, &mut problems) else {
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
        match write_manifest(&lock.dir, &encode_session(&windows, &doc_rows, &dead)) {
            Ok(()) => {
                for folder in merged {
                    finish_merged_folder(folder, &mut problems);
                }
            }
            // Every folder stays, and the next launch tells its rows apart by their keys.
            Err(error) => problems.push(error),
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
        dead,
    })
}

/// A folder this launch has read, with its lock still held.
struct MergedFolder {
    dir: PathBuf,
    held: File,
    /// Every file left it, so it can go.
    whole: bool,
    stuck: Vec<DocMeta>,
    stuck_windows: Vec<WindowMeta>,
}

struct MovedFiles {
    whole: bool,
    /// The documents that have a file still in the old folder.
    stuck_keys: std::collections::BTreeSet<String>,
}

/// A name in `dir` that nothing has yet, so that a move or a copy cannot replace a file.
fn free_name(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{extension}"));
    if !first.exists() {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("{stem}-{n}.{extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

fn could_not_move(path: &Path, error: &std::io::Error) -> DocError {
    let mut problem = DocError::io("QuickEdit could not move the file at", path, error);
    problem.message.push_str(
        " It stays there, and QuickEdit tries again at the next launch. Its text is safe.",
    );
    problem.left_on_disk.push(path.to_path_buf());
    problem
}

/// Moves all that a folder holds, not only its copies: a `.damaged` file or a damaged list
/// has been named to the user as staying on disk.
fn move_session_files(
    dir: &Path,
    docs_dir: &Path,
    main_dir: &Path,
    index: u32,
    problems: &mut Vec<DocError>,
) -> MovedFiles {
    let mut moved = MovedFiles {
        whole: true,
        stuck_keys: Default::default(),
    };
    let from = dir.join("docs");
    match std::fs::read_dir(&from) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                // A `.tmp` was never finished. The `.qea` beside it is the last good copy.
                if name.ends_with(".tmp") {
                    if let Err(error) = std::fs::remove_file(&path) {
                        moved.whole = false;
                        eprintln!("QuickEdit: could not remove {}: {error}", path.display());
                    }
                    continue;
                }
                let target = docs_dir.join(entry.file_name());
                let result = if target.exists() {
                    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
                } else {
                    rename_with_retry(&path, &target)
                };
                if let Err(error) = result {
                    problems.push(could_not_move(&path, &error));
                    moved.whole = false;
                    if let Some((key, _)) = name.split_once('.') {
                        moved.stuck_keys.insert(key.to_owned());
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            problems.push(DocError::io(
                "QuickEdit could not read the autosave folder",
                &from,
                &error,
            ));
            moved.whole = false;
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        moved.whole = false;
        return moved;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "docs" || name == "session.lock" || name == MANIFEST {
            continue;
        }
        if name.starts_with("session.tmp-") {
            if let Err(error) = std::fs::remove_file(&path) {
                moved.whole = false;
                eprintln!("QuickEdit: could not remove {}: {error}", path.display());
            }
            continue;
        }
        let (stem, extension) = name.rsplit_once('.').unwrap_or((&name, "kept"));
        let target = free_name(main_dir, &format!("session-{index}.{stem}"), extension);
        if let Err(error) = rename_with_retry(&path, &target) {
            problems.push(could_not_move(&path, &error));
            moved.whole = false;
        }
    }
    moved
}

/// Copies below a dead mark go now. One that will not go stays dead for the next launch.
fn remove_dead_copies(
    generations: &mut BTreeMap<String, Vec<(u64, PathBuf)>>,
    dead: BTreeMap<String, u64>,
    problems: &mut Vec<DocError>,
) -> BTreeMap<String, u64> {
    let mut still = BTreeMap::new();
    for (key, below) in dead {
        let Some(list) = generations.get_mut(&key) else {
            continue;
        };
        let (gone, live): (Vec<_>, Vec<_>) = std::mem::take(list)
            .into_iter()
            .partition(|(generation, _)| *generation < below);
        *list = live;
        if list.is_empty() {
            generations.remove(&key);
        }
        for (_, path) in gone {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                still.insert(key.clone(), below);
                problems.push(DocError::io(
                    "QuickEdit could not remove an old autosaved copy, and tries again at the next launch. The copy will not come back:",
                    &path,
                    &error,
                ));
            }
        }
    }
    still
}

/// The description of a copy with no row. A file with a bad head goes to `.damaged` once and
/// is named once. A file Windows would not open stays as it is, to be read next time.
fn usable_head(files: &mut Vec<(u64, PathBuf)>, problems: &mut Vec<DocError>) -> Option<DocMeta> {
    let mut found = None;
    files.retain(|(_, path)| {
        if found.is_some() {
            return true;
        }
        match read_generation_head(path) {
            Ok(Some(meta)) => {
                found = Some(meta);
                true
            }
            Ok(None) => {
                let mut problem = DocError::new(
                    DocErrorKind::AutosaveDamaged,
                    Some(path),
                    format!(
                        "The autosaved copy at {} is damaged, so it cannot come back. It stays on disk.",
                        path.display()
                    ),
                );
                let kept = path.with_extension("damaged");
                match std::fs::rename(path, &kept) {
                    Ok(()) => problem.left_on_disk.push(kept),
                    Err(error) => {
                        eprintln!("QuickEdit: could not rename {}: {error}", path.display());
                        problem.left_on_disk.push(path.clone());
                    }
                }
                problems.push(problem);
                false
            }
            Err(error) => {
                let mut problem = DocError::io("QuickEdit could not read the autosaved copy at", path, &error);
                problem.message.push_str(" It stays on disk, and the next launch tries again.");
                problem.left_on_disk.push(path.clone());
                problems.push(problem);
                false
            }
        }
    });
    found
}

fn finish_merged_folder(folder: MergedFolder, problems: &mut Vec<DocError>) {
    let MergedFolder {
        dir,
        held,
        whole,
        stuck,
        stuck_windows,
    } = folder;
    if !whole {
        // Only what did not move stays on its list, so a later launch brings back no row
        // this launch already took.
        let text = encode_session(&stuck_windows, &stuck, &BTreeMap::new());
        if let Err(error) = write_manifest(&dir, &text) {
            problems.push(error);
        }
        return;
    }
    // The list goes first: with it gone, no row of this folder can come back.
    let list = dir.join(MANIFEST);
    let cleared = match std::fs::remove_file(&list) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err((list, error)),
        _ => {
            let docs = dir.join("docs");
            match std::fs::remove_dir(&docs) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err((docs, error)),
                _ => Ok(()),
            }
        }
    };
    drop(held);
    let cleared = cleared.and_then(|()| {
        let lock = dir.join("session.lock");
        std::fs::remove_file(&lock).map_err(|error| (lock, error))?;
        std::fs::remove_dir(&dir).map_err(|error| (dir.clone(), error))
    });
    if let Err((path, error)) = cleared {
        let mut problem = DocError::io(
            "QuickEdit could not remove an old session folder at",
            &path,
            &error,
        );
        problem
            .message
            .push_str(" Nothing of yours is lost, and the next launch tries again.");
        problems.push(problem);
    }
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
        rename_with_retry(&temp, &target)
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
                links: 0,
            }),
            prevent_editing: true,
            wrap_to_page: true,
            read_only: false,
            copy_source: Some(PathBuf::from(r"C:\big file")),
            has_autosave: true,
            changed_while_away: true,
        }
    }

    #[test]
    fn a_dead_mark_comes_back_and_a_bad_one_is_refused() {
        let dead = BTreeMap::from([("ab12".to_owned(), 7u64)]);
        let text = encode_session(&[], &[meta()], &dead);
        assert_eq!(decode_session(&text).unwrap().dead, dead);
        assert!(decode_session(&text.replace("below=7", "below=x")).is_none());
        assert!(decode_session(&text.replace("key=ab12 below", "key=a/b below")).is_none());
    }

    #[test]
    fn a_short_hold_on_a_file_does_not_stop_its_rename() {
        let dir = std::env::temp_dir().join(format!("qe-docs-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("a"), dir.join("b"));
        std::fs::write(&from, b"x").unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&from)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            drop(held);
        });
        rename_with_retry(&from, &to).unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"x");
        std::fs::remove_dir_all(&dir).unwrap();
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
