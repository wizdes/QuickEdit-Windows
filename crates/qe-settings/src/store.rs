//! The settings store. It lives on the UI thread and is cheap to clone.
//!
//! Reading happens at `open`, on the caller's thread: one small file, under 1 ms in the
//! release test. A change stays in memory at once. A short-lived thread then looks at the
//! file and edits it, so the UI thread never waits on the disk. No thread runs at idle.
//! The one exception is after `flush`, which means the app is exiting: see `flush`.
use crate::font_dialog::{LateFunction, late_function};
use crate::format::{
    Edit, MAX_FILE_BYTES, Parsed, StoredValue, Unsaved, apply_edits, default_of, encode,
    encoding_by_name, parse, parse_font, value_problem,
};
use crate::keys::{Refusal, kind_refusal, menu_path_text, menu_place, opens_submenu};
use qe_api::{CommandId, EncodingChoice, KeyBinding, Modifiers, SettingKey, TextEncoding};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use windows::Win32::Globalization::GetUserDefaultGeoName;
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::FOLDERID_LocalAppData;
use windows::core::{PCWSTR, s, w};

/// The file's name inside `%LOCALAPPDATA%\QuickEdit\`.
pub const SETTINGS_FILE_NAME: &str = "settings.txt";

/// The app's settings file, in `%LOCALAPPDATA%\\QuickEdit\\`. Under MSIX, Windows redirects
/// that folder by itself, whichever way the path was found.
///
/// The variable comes first because it costs microseconds. `SHGetKnownFolderPath` took
/// 6.4 ms cold on this PC, and it would load `shell32` at launch (design-spec section 4,
/// rule 6). It runs only if the variable is missing or not a full path.
pub fn default_settings_path() -> Result<PathBuf, String> {
    let folder = local_folder_from_variable(std::env::var_os("LOCALAPPDATA"))
        .map_or_else(known_local_folder, Ok)?;
    Ok(folder.join("QuickEdit").join(SETTINGS_FILE_NAME))
}
fn local_folder_from_variable(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}
type KnownFolderCall = unsafe extern "system" fn(
    *const windows::core::GUID,
    u32,
    windows::Win32::Foundation::HANDLE,
    *mut windows::core::PWSTR,
) -> windows::core::HRESULT;
fn known_local_folder() -> Result<PathBuf, String> {
    let failed = |why: String| format!("Windows did not give the local data folder: {why}");
    let address = late_function(w!("shell32.dll"), s!("SHGetKnownFolderPath")).map_err(failed)?;
    // SAFETY: this is the signature that `shlobj_core.h` gives this name.
    let call = unsafe { std::mem::transmute::<LateFunction, KnownFolderCall>(address) };
    let mut path = windows::core::PWSTR::null();
    // SAFETY: the id is a valid static, and the out pointer is valid for the call.
    unsafe {
        call(
            &FOLDERID_LocalAppData,
            0,
            windows::Win32::Foundation::HANDLE::default(),
            &mut path,
        )
    }
    .ok()
    .map_err(|e| failed(e.to_string()))?;
    // SAFETY: on success the call returns a valid string with a terminating zero.
    let text = unsafe { path.to_string() };
    // SAFETY: the string came from the call above, which allocates with this allocator.
    unsafe { CoTaskMemFree(Some(path.0 as *const _)) };
    text.map(PathBuf::from).map_err(|e| failed(e.to_string()))
}

/// Something that changed. Listeners run on the UI thread, after the change is in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingChange {
    /// A setting has a new value.
    Value(SettingKey),
    /// A shortcut changed. The menus and the key lookup follow `key_of`.
    Keys,
    /// A write failed. The text is for the user. Changes stay in memory and stay unsaved.
    WriteFailed(String),
    /// A save found a new problem with the file, or found an old one gone. `load_problem`
    /// says the same.
    FileProblem(Option<String>),
    /// A write worked after one that failed.
    WriteRecovered,
}
/// Why a value was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingError(pub String);
impl std::fmt::Display for SettingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for SettingError {}

/// Paper for Wrap to Page and PDFs. It has no control, as on the Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperSize {
    Letter,
    A4,
}
/// Row I26: Letter in the US and Canada, else A4. A region not known means A4.
pub fn paper_for_region(region: Option<&str>) -> PaperSize {
    match region {
        Some("US" | "CA") => PaperSize::Letter,
        _ => PaperSize::A4,
    }
}
fn user_region() -> Option<String> {
    let mut name = [0u16; 16];
    // SAFETY: the buffer is valid, and the call takes its length from the slice.
    let length = unsafe { GetUserDefaultGeoName(&mut name) };
    let length = usize::try_from(length).ok()?.checked_sub(1)?;
    String::from_utf16(name.get(..length)?).ok()
}

/// A plain font choice: its family and size in points.
#[derive(Clone, Debug, PartialEq)]
pub struct FontChoice {
    pub family: String,
    pub points: f32,
}
impl FontChoice {
    /// The setting's text, such as `Cascadia Mono 10`.
    pub fn setting_text(&self) -> String {
        format!("{} {}", self.family, self.points)
    }
    /// What Settings shows beside the Change button.
    pub fn display_text(&self) -> String {
        format!("{} {} pt", self.family, self.points)
    }
}

/// A callback that a write's end runs on the writer's thread. It posts a message; the
/// UI thread then calls `poll_writes`.
pub type WriteWake = Arc<dyn Fn() + Send + Sync>;
type Listener = Rc<RefCell<dyn FnMut(&SettingChange)>>;

#[derive(Default)]
struct WriteSlot {
    /// A save is wanted. The path, and the whole text of memory for a file with no lines.
    wanted: Option<(PathBuf, String)>,
    running: bool,
    finished: Option<Result<(), String>>,
    /// Changes that have not reached the disk. A failed save leaves them here.
    unsaved: Unsaved,
    /// What a save found in a file that changed. `poll_writes` takes it on the UI thread.
    found: Option<FoundFile>,
}
/// What a save learned from the disk, for memory and for the message.
struct FoundFile {
    /// The file's settings, or `None` when memory stays as it is.
    parsed: Option<Parsed>,
    problem: Option<String>,
}
/// What this store last saw on disk. Only the writer's thread uses it after `open`.
#[derive(Default)]
struct KnownFile {
    /// The bytes this store last read or wrote. `None`: no file, or it could not be read.
    bytes: Option<Vec<u8>>,
    /// `open` could not read this file, or could not copy it. The first good look counts
    /// as new, so that memory and the message catch up.
    blocked: bool,
    /// The file at `bytes` has a problem, and no save has confirmed a flushed copy yet.
    copy_owed: bool,
    /// The newest copy, for the message.
    copy: Option<PathBuf>,
}
#[derive(Default)]
struct Writer {
    slot: Mutex<WriteSlot>,
    done: Condvar,
    wakes: Mutex<Vec<(u64, WriteWake)>>,
    known: Mutex<KnownFile>,
    /// Counts the writer's looks at the disk, so a test can see queued saves merge.
    #[cfg(test)]
    attempts: AtomicU32,
    /// Makes the next thread fail to start, so a test can see that failure.
    #[cfg(test)]
    fail_start: std::sync::atomic::AtomicBool,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl Writer {
    /// Adds the change and asks for a save, in one step, so a save never holds a change
    /// without its text. A write in progress finishes first, and a request behind it is
    /// replaced, so a burst of changes makes at most two writes. `Err` means the thread
    /// did not start: the failure is in `finished`, and the change stays unsaved.
    fn queue(
        self: &Arc<Self>,
        path: PathBuf,
        text: String,
        change: impl FnOnce(&mut Unsaved),
    ) -> Result<(), ()> {
        let mut slot = lock(&self.slot);
        change(&mut slot.unsaved);
        slot.wanted = Some((path, text));
        if slot.running {
            return Ok(());
        }
        slot.running = true;
        let me = self.clone();
        let started = self.start_thread(move || me.run());
        if let Err(error) = started {
            slot.running = false;
            slot.wanted = None;
            // It replaces any older result, so an old success cannot clear this failure.
            slot.finished = Some(Err(format!(
                "Settings were not saved: the writer could not start: {error}"
            )));
            return Err(());
        }
        Ok(())
    }
    fn start_thread(&self, work: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_start.swap(false, Ordering::Relaxed) {
            return Err(std::io::Error::other("this test said so"));
        }
        std::thread::Builder::new()
            .name("settings-writer".into())
            .spawn(work)
            .map(drop)
    }
    fn run(&self) {
        loop {
            let (path, text, unsaved) = {
                let mut slot = lock(&self.slot);
                let Some((path, text)) = slot.wanted.take() else {
                    slot.running = false;
                    self.done.notify_all();
                    return;
                };
                (path, text, slot.unsaved.clone())
            };
            let result = self.save(&path, &text, &unsaved);
            {
                let mut slot = lock(&self.slot);
                slot.finished = Some(result.map(|found| {
                    slot.unsaved.clear_through(unsaved.last());
                    if found.is_some() {
                        slot.found = found;
                    }
                }));
            }
            let wakes: Vec<WriteWake> = lock(&self.wakes).iter().map(|w| w.1.clone()).collect();
            for wake in wakes {
                wake();
            }
        }
    }
    /// One save. It looks at the disk first, so what a person or another QuickEdit wrote
    /// stays. A write that lands between this read and the rename, a few milliseconds, is
    /// still lost: Windows has no rename that first checks the target.
    fn save(
        &self,
        path: &Path,
        whole_text: &str,
        unsaved: &Unsaved,
    ) -> Result<Option<FoundFile>, String> {
        const TRIES: u32 = 20;
        const PAUSE: std::time::Duration = std::time::Duration::from_millis(25);
        #[cfg(test)]
        self.attempts.fetch_add(1, Ordering::Relaxed);
        let not_saved = |why: String| format!("Settings were not saved: {why}");
        let mut known = lock(&self.known);
        let bytes = match read_disk(path, TRIES, PAUSE) {
            Disk::Bytes(bytes) => bytes,
            Disk::TooLong if known.blocked => return Err(not_saved(too_long_at_open(path))),
            Disk::TooLong => {
                return Err(not_saved(format!(
                    "the settings file {} is over 1 MB long, so it was not replaced. Move or rename it to save settings again.",
                    path.display()
                )));
            }
            Disk::Unreadable(error) if known.blocked => {
                return Err(not_saved(unreadable_at_open(path, &error)));
            }
            Disk::Unreadable(error) => {
                return Err(not_saved(format!(
                    "the settings file {} could not be read, so it was not replaced: {error}",
                    path.display()
                )));
            }
            // A first run, or the file was moved away. Memory is all there is.
            Disk::Missing => {
                replace_file(path, whole_text.as_bytes()).map_err(not_saved)?;
                let was_known = known.bytes.is_some() || known.blocked;
                *known = KnownFile {
                    bytes: Some(whole_text.as_bytes().to_vec()),
                    ..KnownFile::default()
                };
                return Ok(was_known.then_some(FoundFile {
                    parsed: None,
                    problem: None,
                }));
            }
        };
        let changed = known.blocked || known.bytes.as_deref() != Some(&bytes);
        let (mark, lines) = split_text(&bytes);
        let why = match lines {
            Some(lines) => parse(lines).problem(),
            None => Some(NOT_TEXT.to_owned()),
        };
        // The copy counts only once its bytes are checked and flushed. The original stays
        // until the swap below, so a copy that went missing can always be made again.
        if why.is_some() && (changed || known.copy_owed) {
            let copy = keep_copy(path, &bytes, true).map_err(|error| {
                not_saved(format!(
                    "the settings file {} has a problem, no copy of it could be kept ({error}), and so it was not replaced.",
                    path.display()
                ))
            })?;
            known.copy = Some(copy);
        }
        let edited = match lines {
            Some(lines) => [mark, apply_edits(lines, unsaved).as_bytes()].concat(),
            None => whole_text.as_bytes().to_vec(),
        };
        if edited.len() as u64 > MAX_FILE_BYTES {
            return Err(not_saved(format!(
                "this save would make {} over 1 MB long, so the file stays as it was.",
                path.display()
            )));
        }
        if edited != bytes {
            replace_file(path, &edited).map_err(not_saved)?;
        }
        let found_waits = lock(&self.slot).found.is_some();
        let found = (changed || found_waits)
            .then(|| found_after_save(path, &edited, lines.is_none(), why, known.copy.as_deref()));
        *known = KnownFile {
            bytes: Some(edited),
            copy: known.copy.take(),
            ..KnownFile::default()
        };
        Ok(found)
    }
    fn wait(&self) {
        let mut slot = lock(&self.slot);
        while slot.running {
            slot = self
                .done
                .wait(slot)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}
/// What a save tells memory about a file that changed. Text is read again, because the
/// save just edited it. A file that was not text is now memory's own, so memory stays.
fn found_after_save(
    path: &Path,
    edited: &[u8],
    was_not_text: bool,
    why: Option<String>,
    copy: Option<&Path>,
) -> FoundFile {
    let tell = |why: &str, parsed_nothing: bool| {
        let start = if was_not_text {
            "QuickEdit kept its settings and saved them"
        } else if parsed_nothing {
            "QuickEdit started with default settings"
        } else {
            "The settings that could not be read have their defaults"
        };
        problem_with_copy(why, start, copy.unwrap_or(path))
    };
    if was_not_text {
        return FoundFile {
            parsed: None,
            problem: why.as_deref().map(|why| tell(why, false)),
        };
    }
    let parsed = parse(split_text(edited).1.unwrap_or_default());
    let problem = parsed
        .problem()
        .map(|why| tell(&shortened_why(&why), parsed.is_empty()));
    FoundFile {
        parsed: Some(parsed),
        problem,
    }
}
fn problem_with_copy(why: &str, start: &str, copy: &Path) -> String {
    format!(
        "The settings file has a problem: {why}. {start}. A copy of the file is at {}.",
        copy.display()
    )
}

/// The long-path form, `\\\\?\\C:\\...`, because `MoveFileExW` stops at about 260 characters
/// with a plain path. Rust's own file calls do this by themselves.
fn wide_long_path(path: &Path) -> Vec<u16> {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let plain: Vec<u16> = full.as_os_str().encode_wide().collect();
    let text = |value: &str| value.encode_utf16().collect::<Vec<u16>>();
    let mut out = if plain.starts_with(&text(r"\\?\")) || plain.starts_with(&text(r"\\.\")) {
        plain
    } else if plain.starts_with(&text(r"\\")) {
        [text(r"\\?\UNC\"), plain[2..].to_vec()].concat()
    } else {
        [text(r"\\?\"), plain].concat()
    };
    out.push(0);
    out
}
fn temp_beside(path: &Path) -> Result<(PathBuf, File), String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let folder = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map_or_else(|| "settings".into(), |n| n.to_string_lossy().into_owned());
    for _ in 0..16 {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = folder.join(format!("{name}.tmp-{}-{number}", std::process::id()));
        match File::create_new(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("no free temporary name beside the file".into())
}
/// Windows' codes for a file that another program holds for a moment, such as a virus
/// scanner, or another QuickEdit reading or swapping it.
fn is_passing_hold(code: u32) -> bool {
    const ACCESS_DENIED: u32 = 5;
    const SHARING_VIOLATION: u32 = 32;
    const LOCK_VIOLATION: u32 = 33;
    matches!(code, ACCESS_DENIED | SHARING_VIOLATION | LOCK_VIOLATION)
}
/// A temp file this old has no writer left: a live save holds one for milliseconds.
const STALE_TEMP: std::time::Duration = std::time::Duration::from_secs(60);
fn is_temp_name(name: &str, file_name: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    name.strip_prefix(file_name)
        .and_then(|rest| rest.strip_prefix(".tmp-"))
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(process, number)| digits(process) && digits(number))
}
/// Removes what a crash left behind, by the exact name `temp_beside` makes. A failure
/// here costs one small file, which the next save tries again, so it stops no save.
fn remove_stale_temps(path: &Path) {
    let (Some(folder), Some(file_name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let file_name = file_name.to_string_lossy();
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if !is_temp_name(&entry.file_name().to_string_lossy(), &file_name) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|found| found.modified())
            .is_ok_and(|time| time.elapsed().is_ok_and(|age| age >= STALE_TEMP));
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}
/// The file changes in one step or not at all: a temp file beside it, written and flushed,
/// then one rename over it. A reader finds the old file or the new one, and a crash leaves
/// one of them whole. `qe_codec` is not used only because `qe-settings` has no line for it
/// in `Cargo.lock`, and its `ReplaceFileW` leaves a moment with no file at this name.
///
/// `sync_all` and `MOVEFILE_WRITE_THROUGH` are here for power loss. Killing a process cannot
/// show a lost flush, so no test proves them.
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map_or_else(|| SETTINGS_FILE_NAME.into(), |n| n.to_string_lossy());
    let fail = |why: String| format!("{} could not be replaced: {why}", path.display());
    match fs::metadata(path) {
        Ok(found) if found.permissions().readonly() => {
            return Err(format!("{name} is read-only."));
        }
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(fail(error.to_string())),
        _ => {}
    }
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(|e| fail(e.to_string()))?;
    }
    remove_stale_temps(path);
    let (temp, mut file) = temp_beside(path).map_err(fail)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let swapped = written
        .map_err(|e| e.to_string())
        .and_then(|()| swap_in(path, &temp));
    if let Err(why) = swapped {
        return match fs::remove_file(&temp) {
            Ok(()) => Err(fail(why)),
            Err(left) => Err(fail(format!(
                "{why} The temporary file {} could not be removed: {left}",
                temp.display()
            ))),
        };
    }
    Ok(())
}
/// One rename, tried again for about half a second while another program holds the file.
/// `ACCESS_DENIED` counts as a hold, because virus scanners give it, so a lasting one costs
/// that half second too. A failure leaves the target as it was. This runs on the writer's
/// thread.
fn swap_in(path: &Path, temp: &Path) -> Result<(), String> {
    const TRIES: u32 = 20;
    let (from, to) = (wide_long_path(temp), wide_long_path(path));
    let mut last = String::new();
    for _ in 0..TRIES {
        // SAFETY: both strings end in a zero and live through the call.
        let done = unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        match done {
            Ok(()) => return Ok(()),
            Err(error) if is_passing_hold((error.code().0 as u32) & 0xffff) => {
                last = error.message();
            }
            Err(error) => return Err(error.message()),
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err(last)
}

struct State {
    path: PathBuf,
    /// Only the settings that differ from their defaults.
    values: Vec<(SettingKey, StoredValue)>,
    overrides: Vec<(CommandId, Option<KeyBinding>)>,
    /// The file has a shortcut line for a command this version does not know.
    unknown_shortcut: bool,
    /// Each command's key now, by `CommandId as usize`.
    resolved: Vec<Option<KeyBinding>>,
    load_problem: Option<String>,
    /// `open` could not read or copy the file, so a save may not replace it as it stands.
    /// It shows at once. The writer looks again at each save and clears it if the cause
    /// has gone.
    save_block: Option<String>,
    write_error: Option<String>,
}
impl State {
    fn resolve(&mut self) {
        let overrides = &self.overrides;
        self.resolved = CommandId::ALL
            .iter()
            .map(|command| {
                if let Some((_, own)) = overrides.iter().find(|(seen, _)| seen == command) {
                    return *own;
                }
                // A default key the user gave another command stays with that command.
                command.default_key().filter(|fallback| {
                    !overrides
                        .iter()
                        .any(|(other, key)| other != command && *key == Some(*fallback))
                })
            })
            .collect();
    }
    fn stored(&self, key: SettingKey) -> Option<&StoredValue> {
        self.values
            .iter()
            .find(|(seen, _)| *seen == key)
            .map(|(_, value)| value)
    }
    fn encoded(&self) -> String {
        encode(&self.values, &self.overrides)
    }
    /// Takes in what a file holds.
    fn set_memory(&mut self, parsed: Parsed) {
        self.values = parsed.values;
        self.overrides = parsed.keys;
        self.unknown_shortcut = parsed.unknown_shortcut;
        self.values
            .retain(|(key, value)| *value != default_of(*key));
        self.overrides
            .retain(|(command, key)| *key != command.default_key());
        self.resolve();
    }
}
struct Inner {
    state: RefCell<State>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    next_listener: RefCell<u64>,
    events: RefCell<VecDeque<SettingChange>>,
    dispatching: RefCell<bool>,
    writer: Arc<Writer>,
    /// `flush` has run, so the app is exiting and each later save is waited for.
    flushed: Cell<bool>,
    settling: Cell<bool>,
}
/// A store that goes away may be the app's last act, so its last save is waited for.
impl Drop for Inner {
    fn drop(&mut self) {
        self.writer.wait();
    }
}

/// Settings and keys, shared by the app's windows on the UI thread.
#[derive(Clone)]
pub struct SettingsStore(Rc<Inner>);

/// What reading found.
struct Loaded {
    parsed: Parsed,
    problem: Option<String>,
    block: Option<String>,
    copy: Option<PathBuf>,
    bytes: Option<Vec<u8>>,
}
/// How many copies of broken files stay. Past this, the oldest of them go.
const MAX_COPIES: usize = 5;
/// How much of what was wrong goes in the message. The copy holds all of it.
const MAX_WHY_CHARS: usize = 400;
/// The copies this code made, oldest first: `settings.broken.txt` is 1, and then
/// `settings.broken-2.txt` and on. No other name counts.
fn copies_beside(path: &Path, stem: &str) -> Result<Vec<(u32, PathBuf)>, String> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let first = format!("{stem}.broken.txt");
    let start = format!("{stem}.broken-");
    let mut copies = Vec::new();
    for entry in fs::read_dir(folder).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let number = if *name == *first {
            Some(1)
        } else {
            name.strip_prefix(&start)
                .and_then(|rest| rest.strip_suffix(".txt"))
                .filter(|digits| !digits.starts_with('0'))
                .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|digits| digits.parse::<u32>().ok())
                .filter(|number| *number >= 2)
        };
        if let Some(number) = number
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            copies.push((number, entry.path()));
        }
    }
    copies.sort();
    Ok(copies)
}
fn holds_these_bytes(copy: &Path, bytes: &[u8]) -> bool {
    fs::metadata(copy).is_ok_and(|found| found.len() == bytes.len() as u64)
        && fs::read(copy).is_ok_and(|held| held == bytes)
}
fn flush_file(path: &Path) -> std::io::Result<()> {
    OpenOptions::new().write(true).open(path)?.sync_all()
}
/// A copy of the broken file beside it, whole, or an error. The same content as the newest
/// copy makes no new one, so launching again and again adds nothing. The launch thread does
/// not flush (`must_flush` false). The writer does, before any save replaces the original,
/// and until then the original is still there. A copy it cannot flush, such as a read-only
/// one, does not count, so it makes a new one.
fn keep_copy(path: &Path, bytes: &[u8], must_flush: bool) -> Result<PathBuf, String> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map_or_else(|| "settings".into(), |n| n.to_string_lossy().into_owned());
    let copies = copies_beside(path, &stem)?;
    if let Some((_, newest)) = copies.last()
        && holds_these_bytes(newest, bytes)
        && (!must_flush || flush_file(newest).is_ok())
    {
        return Ok(newest.clone());
    }
    // After the highest number, the count starts again from 1. Any free name will do.
    let next = |number: u32| number.checked_add(1).unwrap_or(1);
    let mut number = copies.last().map_or(1, |(number, _)| next(*number));
    for _ in 0..16 {
        let name = if number == 1 {
            format!("{stem}.broken.txt")
        } else {
            format!("{stem}.broken-{number}.txt")
        };
        let copy = folder.join(name);
        match File::create_new(&copy) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes) {
                    drop(file);
                    // A part of a copy is not a copy. If this fails too, it says so.
                    return Err(match fs::remove_file(&copy) {
                        Ok(()) => error.to_string(),
                        Err(left) => format!("{error}; {} stays: {left}", copy.display()),
                    });
                }
                // Past the limit the oldest go. A copy that cannot go now (it may be open)
                // goes at a later launch, and it stops nothing.
                let over = (copies.len() + 1).saturating_sub(MAX_COPIES);
                for (_, old) in copies.iter().take(over) {
                    let _ = fs::remove_file(old);
                }
                if must_flush {
                    drop(file);
                    flush_file(&copy).map_err(|error| error.to_string())?;
                    if !holds_these_bytes(&copy, bytes) {
                        return Err(format!("{} does not hold the file", copy.display()));
                    }
                }
                return Ok(copy);
            }
            // A folder with this name gives access denied, not "already exists".
            Err(error)
                if error.kind() == ErrorKind::AlreadyExists
                    || fs::symlink_metadata(&copy).is_ok() =>
            {
                number = next(number);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("no free name for a copy".into())
}
fn shortened_why(why: &str) -> String {
    match why.char_indices().nth(MAX_WHY_CHARS) {
        Some((end, _)) => format!("{}\u{2026}", &why[..end]),
        None => why.to_owned(),
    }
}
fn broken(path: &Path, bytes: Vec<u8>, why: &str, parsed: Parsed) -> Loaded {
    let start = if parsed.is_empty() {
        "QuickEdit started with default settings"
    } else {
        "The settings that could not be read have their defaults"
    };
    let why = shortened_why(why);
    match keep_copy(path, &bytes, false) {
        Ok(copy) => Loaded {
            parsed,
            problem: Some(problem_with_copy(&why, start, &copy)),
            block: None,
            copy: Some(copy),
            bytes: Some(bytes),
        },
        Err(error) => {
            let problem = format!(
                "The settings file has a problem: {why}. {start}. No copy could be kept ({error}), so changes will not be saved over it."
            );
            Loaded {
                parsed,
                block: Some(format!("Settings were not saved: {problem}")),
                problem: Some(problem),
                copy: None,
                bytes: Some(bytes),
            }
        }
    }
}
/// What is at the path now.
enum Disk {
    Missing,
    TooLong,
    Unreadable(std::io::Error),
    Bytes(Vec<u8>),
}
/// Reads up to the limit, and `None` means there was more. A file may grow after its size
/// was checked, and a part of a file is not the file.
fn read_capped(reader: impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    Read::take(reader, MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes))
}
/// Reads the file, and tries again for a moment while another program holds it. Only a
/// file that is really missing is a first run.
fn read_disk(path: &Path, tries: u32, pause: std::time::Duration) -> Disk {
    let mut tried = 1;
    let mut file = loop {
        match File::open(path) {
            Ok(file) => break file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Disk::Missing,
            Err(error)
                if tried < tries
                    && error
                        .raw_os_error()
                        .is_some_and(|code| is_passing_hold(code as u32)) =>
            {
                tried += 1;
                std::thread::sleep(pause);
            }
            Err(error) => return Disk::Unreadable(error),
        }
    };
    match file.metadata() {
        Ok(found) if found.len() > MAX_FILE_BYTES => return Disk::TooLong,
        Ok(_) => {}
        Err(error) => return Disk::Unreadable(error),
    }
    match read_capped(&mut file) {
        Ok(Some(bytes)) => Disk::Bytes(bytes),
        Ok(None) => Disk::TooLong,
        Err(error) => Disk::Unreadable(error),
    }
}
const NOT_TEXT: &str = "it is not UTF-8 text";
/// The byte order mark, if there is one, and the text after it, if it is text.
fn split_text(bytes: &[u8]) -> (&[u8], Option<&str>) {
    let rest = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mark = &bytes[..bytes.len() - rest.len()];
    (mark, std::str::from_utf8(rest).ok())
}
fn unreadable_at_open(path: &Path, error: &std::io::Error) -> String {
    format!(
        "The settings file {} could not be read: {error}. QuickEdit started with default settings, and changes will not be saved over the file.",
        path.display()
    )
}
/// A file this long is not read at launch, because copying it could take any time, and a
/// part of it is not a copy. So it stays where it is and is not replaced.
fn too_long_at_open(path: &Path) -> String {
    format!(
        "The settings file is over 1 MB long, so it was not read. QuickEdit started with default settings, and changes will not be saved over the file. Move or rename {} to save settings again.",
        path.display()
    )
}
fn read(path: &Path) -> Loaded {
    let blocked = |problem: String| Loaded {
        parsed: Parsed::default(),
        block: Some(format!("Settings were not saved: {problem}")),
        problem: Some(problem),
        copy: None,
        bytes: None,
    };
    let bytes = match read_disk(path, 6, std::time::Duration::from_millis(10)) {
        Disk::Missing => {
            return Loaded {
                parsed: Parsed::default(),
                problem: None,
                block: None,
                copy: None,
                bytes: None,
            };
        }
        Disk::TooLong => return blocked(too_long_at_open(path)),
        Disk::Unreadable(error) => return blocked(unreadable_at_open(path, &error)),
        Disk::Bytes(bytes) => bytes,
    };
    let Some(text) = split_text(&bytes).1 else {
        return broken(path, bytes, NOT_TEXT, Parsed::default());
    };
    let parsed = parse(text);
    match parsed.problem() {
        Some(why) => broken(path, bytes, &why, parsed),
        None => Loaded {
            parsed,
            problem: None,
            block: None,
            copy: None,
            bytes: Some(bytes),
        },
    }
}

impl SettingsStore {
    /// Reads the file at `path`. A missing file gives the defaults. A file with a
    /// problem is kept beside itself, and `load_problem` says so for the caller to show.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let loaded = read(&path);
        let known = KnownFile {
            blocked: loaded.block.is_some(),
            copy_owed: loaded.problem.is_some() && loaded.bytes.is_some(),
            copy: loaded.copy,
            bytes: loaded.bytes,
        };
        let mut state = State {
            path,
            values: Vec::new(),
            overrides: Vec::new(),
            unknown_shortcut: false,
            resolved: Vec::new(),
            load_problem: loaded.problem,
            save_block: loaded.block,
            write_error: None,
        };
        state.set_memory(loaded.parsed);
        let writer = Writer {
            known: Mutex::new(known),
            ..Writer::default()
        };
        Self(Rc::new(Inner {
            state: RefCell::new(state),
            listeners: RefCell::default(),
            next_listener: RefCell::default(),
            events: RefCell::default(),
            dispatching: RefCell::default(),
            writer: Arc::new(writer),
            flushed: Cell::default(),
            settling: Cell::default(),
        }))
    }
    pub fn path(&self) -> PathBuf {
        self.0.state.borrow().path.clone()
    }
    /// What is wrong with the file, in words for the user: what `open` found, until a save
    /// looks at a file that changed. `SettingChange::FileProblem` says when that happens.
    pub fn load_problem(&self) -> Option<String> {
        self.0.state.borrow().load_problem.clone()
    }
    /// Why the last write failed. Its changes stay unsaved. The next good write clears it.
    pub fn write_error(&self) -> Option<String> {
        self.0.state.borrow().write_error.clone()
    }

    // Reading.

    pub fn get(&self, key: SettingKey) -> StoredValue {
        self.0
            .state
            .borrow()
            .stored(key)
            .cloned()
            .unwrap_or_else(|| default_of(key))
    }
    /// `None` when this setting is not a yes or no.
    pub fn get_bool(&self, key: SettingKey) -> Option<bool> {
        match self.get(key) {
            StoredValue::Bool(value) => Some(value),
            _ => None,
        }
    }
    /// `None` when this setting is not a number.
    pub fn get_integer(&self, key: SettingKey) -> Option<i64> {
        match self.get(key) {
            StoredValue::Integer(value) => Some(value),
            _ => None,
        }
    }
    /// `None` when this setting is not text.
    pub fn get_text(&self, key: SettingKey) -> Option<String> {
        match self.get(key) {
            StoredValue::Text(value) => Some(value),
            _ => None,
        }
    }
    /// The plain font. The default is Cascadia Mono 10.
    pub fn font(&self) -> FontChoice {
        let text = self.get_text(SettingKey::PlainTextFont).unwrap_or_default();
        match parse_font(&text) {
            Some((family, points)) => FontChoice {
                family: family.to_owned(),
                points,
            },
            None => FontChoice {
                family: "Cascadia Mono".into(),
                points: 10.0,
            },
        }
    }
    pub fn encoding_choice(&self, key: SettingKey) -> EncodingChoice {
        self.get_text(key)
            .and_then(|name| encoding_by_name(&name))
            .map_or(EncodingChoice::Automatic, EncodingChoice::Specific)
    }
    /// The list that encoding menus offer, in its saved order. With nothing saved it is
    /// `TextEncoding::DEFAULT_LIST`, the Mac's list (row I42).
    pub fn enabled_encodings(&self) -> Vec<TextEncoding> {
        let state = self.0.state.borrow();
        match state.stored(SettingKey::EnabledEncodings) {
            Some(StoredValue::Text(text)) => text.split(',').filter_map(encoding_by_name).collect(),
            _ => TextEncoding::DEFAULT_LIST.to_vec(),
        }
    }
    /// The saved paper, or the one for the user's region.
    pub fn paper_size(&self) -> PaperSize {
        if let Some(StoredValue::Text(text)) = self.0.state.borrow().stored(SettingKey::PaperSize) {
            return if text == "Letter" {
                PaperSize::Letter
            } else {
                PaperSize::A4
            };
        }
        paper_for_region(user_region().as_deref())
    }

    // Writing.

    /// Refuses a value with the wrong type or outside its limits. The window shows the
    /// reason. No event when the value is the same, and no write unless a change to this
    /// setting is still unsaved: then this tries that save again.
    pub fn set(&self, key: SettingKey, value: StoredValue) -> Result<(), SettingError> {
        if key == SettingKey::KeyboardShortcuts {
            return Err(SettingError(
                "Shortcuts change through set_key and reset_key.".into(),
            ));
        }
        if let Some(problem) = value_problem(key, &value) {
            return Err(SettingError(problem));
        }
        {
            let mut state = self.0.state.borrow_mut();
            let now = state
                .stored(key)
                .cloned()
                .unwrap_or_else(|| default_of(key));
            if now == value {
                drop(state);
                if lock(&self.0.writer.slot).unsaved.holds_setting(key) {
                    self.save(|_| {});
                }
                return Ok(());
            }
            state.values.retain(|(seen, _)| *seen != key);
            if value != default_of(key) {
                state.values.push((key, value.clone()));
            }
        }
        self.save(|unsaved| unsaved.add(Edit::Setting(key, value)));
        self.emit(SettingChange::Value(key));
        Ok(())
    }
    pub fn set_bool(&self, key: SettingKey, value: bool) -> Result<(), SettingError> {
        self.set(key, StoredValue::Bool(value))
    }
    pub fn set_integer(&self, key: SettingKey, value: i64) -> Result<(), SettingError> {
        self.set(key, StoredValue::Integer(value))
    }
    pub fn set_text(&self, key: SettingKey, value: impl Into<String>) -> Result<(), SettingError> {
        self.set(key, StoredValue::Text(value.into()))
    }
    pub fn set_font(&self, font: &FontChoice) -> Result<(), SettingError> {
        self.set_text(SettingKey::PlainTextFont, font.setting_text())
    }
    pub fn set_enabled_encodings(&self, list: &[TextEncoding]) -> Result<(), SettingError> {
        let names: Vec<&str> = list.iter().map(|e| e.name()).collect();
        self.set_text(SettingKey::EnabledEncodings, names.join(","))
    }
    pub fn set_encoding_choice(
        &self,
        key: SettingKey,
        choice: EncodingChoice,
    ) -> Result<(), SettingError> {
        let text = match choice {
            EncodingChoice::Automatic => "Automatic",
            EncodingChoice::Specific(encoding) => encoding.name(),
        };
        self.set_text(key, text)
    }

    // Keys.

    /// The command's key now. It fits `qe_ui::menu_bar_groups_with_keys`:
    /// `menu_bar_groups_with_keys(&|c| store.key_of(c))`.
    pub fn key_of(&self, command: CommandId) -> Option<KeyBinding> {
        self.0
            .state
            .borrow()
            .resolved
            .get(command as usize)
            .copied()
            .flatten()
    }
    /// The command this key press runs.
    pub fn command_for_key(&self, key: KeyBinding) -> Option<CommandId> {
        let state = self.0.state.borrow();
        state
            .resolved
            .iter()
            .position(|held| *held == Some(key))
            .and_then(|index| CommandId::ALL.get(index).copied())
    }
    /// The same lookup, from what `InputKind::KeyDown` carries. A chord with a Windows
    /// key runs no command.
    pub fn command_for_press(&self, virtual_key: u16, modifiers: Modifiers) -> Option<CommandId> {
        if modifiers.win {
            return None;
        }
        self.command_for_key(KeyBinding::new(
            virtual_key,
            modifiers.ctrl,
            modifiers.alt,
            modifiers.shift,
        ))
    }
    /// The row has a change, or lost its default because another command took that key.
    pub fn is_key_changed(&self, command: CommandId) -> bool {
        let state = self.0.state.borrow();
        state.overrides.iter().any(|(seen, _)| *seen == command)
            || state.resolved.get(command as usize).copied().flatten() != command.default_key()
    }
    fn clash(&self, key: KeyBinding, command: CommandId) -> Option<Refusal> {
        let other = self
            .command_for_key(key)
            .filter(|other| *other != command)?;
        Some(Refusal::Clash {
            key,
            name: menu_place(other).name,
            menu_path: menu_path_text(other),
        })
    }
    /// Why this key may not go to this command, or `None`. A command's own default is
    /// never refused for its kind, as on the Mac.
    pub fn refusal_for(&self, key: KeyBinding, command: CommandId) -> Option<Refusal> {
        if opens_submenu(command) {
            return Some(Refusal::OpensSubmenu(menu_place(command).name));
        }
        if command.default_key() != Some(key)
            && let Some(refused) = kind_refusal(key)
        {
            return Some(refused);
        }
        self.clash(key, command)
    }
    /// Gives the command this key, or refuses with the reason.
    pub fn set_key(&self, command: CommandId, key: KeyBinding) -> Result<(), Refusal> {
        if let Some(refused) = self.refusal_for(key, command) {
            return Err(refused);
        }
        self.write_override(command, Some(key));
        Ok(())
    }
    /// The command gets no key.
    pub fn clear_key(&self, command: CommandId) {
        self.write_override(command, None);
    }
    /// Puts the command back on its default key, unless another command has that key
    /// now. Then the reset is refused with the clash, and both keys stay.
    pub fn reset_key(&self, command: CommandId) -> Result<(), Refusal> {
        if let Some(fallback) = command.default_key()
            && let Some(refused) = self.clash(fallback, command)
        {
            return Err(refused);
        }
        self.write_override(command, command.default_key());
        Ok(())
    }
    fn write_override(&self, command: CommandId, key: Option<KeyBinding>) {
        if opens_submenu(command) {
            return;
        }
        {
            let mut state = self.0.state.borrow_mut();
            let before = state.overrides.clone();
            state.overrides.retain(|(seen, _)| *seen != command);
            if key != command.default_key() {
                state.overrides.push((command, key));
            }
            if before == state.overrides {
                drop(state);
                if lock(&self.0.writer.slot).unsaved.holds_key(command) {
                    self.save(|_| {});
                }
                return;
            }
            state.resolve();
        }
        self.save(|unsaved| unsaved.add(Edit::Key(command, key)));
        self.emit(SettingChange::Keys);
    }
    /// Restore Keyboard Defaults: every change goes, also ones for commands this
    /// version does not know.
    pub fn restore_keyboard_defaults(&self) {
        if self.drop_key_changes() {
            self.save(Unsaved::drop_all_shortcuts);
            self.emit(SettingChange::Keys);
        }
    }
    fn drop_key_changes(&self) -> bool {
        let mut state = self.0.state.borrow_mut();
        let changed = !state.overrides.is_empty() || state.unknown_shortcut;
        state.overrides.clear();
        state.unknown_shortcut = false;
        state.resolve();
        changed
    }
    /// Restore All Defaults, as on the Mac: every setting and every shortcut goes back.
    /// The encodings list stays, because the Mac's has no registered value. Keys this
    /// version does not know stay too, apart from shortcuts.
    pub fn restore_all(&self) {
        let keys_changed = self.drop_key_changes();
        let changed: Vec<SettingKey> = {
            let mut state = self.0.state.borrow_mut();
            let changed = state
                .values
                .iter()
                .map(|(key, _)| *key)
                .filter(|key| *key != SettingKey::EnabledEncodings)
                .collect();
            state
                .values
                .retain(|(key, _)| *key == SettingKey::EnabledEncodings);
            changed
        };
        if keys_changed || !changed.is_empty() {
            let edits: Vec<Edit> = changed
                .iter()
                .map(|key| Edit::Setting(*key, default_of(*key)))
                .collect();
            self.save(|unsaved| {
                if keys_changed {
                    unsaved.drop_all_shortcuts();
                }
                for edit in edits {
                    unsaved.add(edit);
                }
            });
        }
        for key in changed {
            self.emit(SettingChange::Value(key));
        }
        if keys_changed {
            self.emit(SettingChange::Keys);
        }
    }

    // Events.

    /// Runs `listener` for each change until `unsubscribe`. It may call the store.
    pub fn subscribe(&self, listener: impl FnMut(&SettingChange) + 'static) -> u64 {
        let id = {
            let mut next = self.0.next_listener.borrow_mut();
            *next += 1;
            *next
        };
        self.0
            .listeners
            .borrow_mut()
            .push((id, Rc::new(RefCell::new(listener))));
        id
    }
    pub fn unsubscribe(&self, id: u64) {
        self.0
            .listeners
            .borrow_mut()
            .retain(|(seen, _)| *seen != id);
    }
    fn emit(&self, change: SettingChange) {
        self.0.events.borrow_mut().push_back(change);
        if self.0.dispatching.replace(true) {
            return;
        }
        loop {
            let next = self.0.events.borrow_mut().pop_front();
            let Some(change) = next else { break };
            let listeners: Vec<(u64, Listener)> = self.0.listeners.borrow().clone();
            for (id, listener) in listeners {
                let live = self
                    .0
                    .listeners
                    .borrow()
                    .iter()
                    .any(|(seen, _)| *seen == id);
                if live && let Ok(mut listener) = listener.try_borrow_mut() {
                    (*listener)(&change);
                }
            }
        }
        self.0.dispatching.replace(false);
    }

    // The writer.

    /// Runs `wake` on the writer's thread after each write. It posts a message, and the
    /// message's handler calls `poll_writes`. Returns an id for `remove_write_wake`.
    pub fn add_write_wake(&self, wake: WriteWake) -> u64 {
        let id = {
            let mut next = self.0.next_listener.borrow_mut();
            *next += 1;
            *next
        };
        lock(&self.0.writer.wakes).push((id, wake));
        id
    }
    pub fn remove_write_wake(&self, id: u64) {
        lock(&self.0.writer.wakes).retain(|(seen, _)| *seen != id);
    }
    /// Marks the change as unsaved and asks the writer for a save. A block that `open` found
    /// shows at once, and the writer looks again.
    fn save(&self, change: impl FnOnce(&mut Unsaved)) {
        let (path, text, block) = {
            let state = self.0.state.borrow();
            (
                state.path.clone(),
                state.encoded(),
                state.save_block.clone(),
            )
        };
        if let Some(block) = block {
            self.note_write(Err(block));
        }
        if self.0.writer.queue(path, text, change).is_err() {
            self.poll_writes();
        }
        if self.0.flushed.get() {
            self.settle();
        }
    }
    /// Turns a finished write into a `write_error` and an event, and takes in what a save
    /// found in a file that changed. No wait.
    pub fn poll_writes(&self) {
        let (finished, found) = {
            let mut slot = lock(&self.0.writer.slot);
            let found = slot.found.take().map(|mut found| {
                if let Some(parsed) = &mut found.parsed {
                    slot.unsaved.apply_to(parsed);
                }
                found
            });
            (slot.finished.take(), found)
        };
        if let Some(found) = found {
            self.take_found(found);
        }
        if let Some(result) = finished {
            self.note_write(result);
        }
    }
    fn take_found(&self, found: FoundFile) {
        let mut events = Vec::new();
        {
            let mut state = self.0.state.borrow_mut();
            if let Some(parsed) = found.parsed {
                let before: Vec<StoredValue> = SettingKey::ALL
                    .iter()
                    .map(|key| {
                        state
                            .stored(*key)
                            .cloned()
                            .unwrap_or_else(|| default_of(*key))
                    })
                    .collect();
                let keys_before = state.resolved.clone();
                state.set_memory(parsed);
                for (key, old) in SettingKey::ALL.iter().zip(before) {
                    let new = state
                        .stored(*key)
                        .cloned()
                        .unwrap_or_else(|| default_of(*key));
                    if new != old {
                        events.push(SettingChange::Value(*key));
                    }
                }
                if keys_before != state.resolved {
                    events.push(SettingChange::Keys);
                }
            }
            if state.load_problem != found.problem {
                state.load_problem.clone_from(&found.problem);
                events.push(SettingChange::FileProblem(found.problem));
            }
        }
        for event in events {
            self.emit(event);
        }
    }
    fn note_write(&self, result: Result<(), String>) {
        let event = {
            let mut state = self.0.state.borrow_mut();
            match result {
                Ok(()) => {
                    state.save_block = None;
                    state
                        .write_error
                        .take()
                        .map(|_| SettingChange::WriteRecovered)
                }
                Err(message) => {
                    state.write_error = Some(message.clone());
                    Some(SettingChange::WriteFailed(message))
                }
            }
        };
        if let Some(event) = event {
            self.emit(event);
        }
    }
    /// Waits until the writer is idle, with its results taken in. A listener may save during
    /// this, and that save is waited for too. `false` means listeners would not stop saving.
    fn settle(&self) -> bool {
        const ROUNDS: usize = 32;
        if self.0.settling.replace(true) {
            return true;
        }
        let mut calm = false;
        for _ in 0..ROUNDS {
            self.0.writer.wait();
            self.poll_writes();
            let slot = lock(&self.0.writer.slot);
            if !slot.running && slot.finished.is_none() && slot.found.is_none() {
                calm = true;
                break;
            }
        }
        self.0.settling.set(false);
        calm
    }
    /// For the app's exit and for tests: never while typing. It tries any change that failed
    /// to save once more, waits, and returns an error if any change is still unsaved.
    ///
    /// After this the app counts as exiting: each later save is waited for on the calling
    /// thread, so it reaches the disk or sets `write_error` before the call returns. A
    /// process exit can then lose nothing.
    pub fn flush(&self) -> Result<(), String> {
        self.0.flushed.set(true);
        let retry = {
            let slot = lock(&self.0.writer.slot);
            !slot.running && !slot.unsaved.is_empty()
        };
        if retry {
            let (path, text) = {
                let state = self.0.state.borrow();
                (state.path.clone(), state.encoded())
            };
            let _ = self.0.writer.queue(path, text, |_| {});
        }
        let calm = self.settle();
        if let Some(error) = self.write_error() {
            return Err(error);
        }
        if !calm || !lock(&self.0.writer.slot).unsaved.is_empty() {
            return Err(
                "Settings were not saved: changes were still being made when QuickEdit stopped waiting."
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch_path(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("qe-settings-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder.join(SETTINGS_FILE_NAME)
    }
    /// The second review: a file that grows after its size check must still count as long.
    #[test]
    fn a_read_stops_one_byte_past_the_limit() {
        let limit = MAX_FILE_BYTES as usize;
        let read = |length: usize| read_capped(std::io::repeat(b'x').take(length as u64));
        assert_eq!(read(limit).unwrap().map(|b| b.len()), Some(limit));
        assert_eq!(read(limit + 1).unwrap(), None);
        assert_eq!(read(limit * 3).unwrap(), None);
    }
    /// The second review: a burst behind a save in progress makes one more save, not 300.
    #[test]
    fn a_burst_of_changes_behind_a_held_file_makes_at_most_two_saves() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = scratch_path("burst");
        fs::write(&path, "TabWidth=4\r\n").unwrap();
        let store = SettingsStore::open(&path);
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        for width in 1..=300 {
            store
                .set_integer(SettingKey::WindowWidthCharacters, width)
                .unwrap();
        }
        drop(held);
        store.flush().unwrap();
        let attempts = store.0.writer.attempts.load(Ordering::Relaxed);
        assert!(attempts <= 2, "{attempts} saves");
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "TabWidth=4\r\nWindowWidthCharacters=300\r\n");
        drop(store);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    /// The second review, finding 11: a thread that cannot start is an error, even with an
    /// old success still waiting for a poll.
    #[test]
    fn a_stale_success_does_not_clear_a_failure_to_start() {
        let path = scratch_path("stale-ok");
        let store = SettingsStore::open(&path);
        store.set_integer(SettingKey::TabWidth, 3).unwrap();
        store.0.writer.wait();
        assert_eq!(
            lock(&store.0.writer.slot).finished,
            Some(Ok(())),
            "a success nothing has polled"
        );
        store.0.writer.fail_start.store(true, Ordering::Relaxed);
        store.set_integer(SettingKey::TabWidth, 4).unwrap();
        let error = store.write_error().expect("the failure shows at once");
        assert!(error.contains("could not start"), "{error}");
        store.poll_writes();
        assert_eq!(store.write_error().as_deref(), Some(error.as_str()));
        // The change is still unsaved, so the next try writes it.
        store.flush().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "TabWidth=4\r\n");
        drop(store);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn the_variable_counts_only_as_a_full_path() {
        assert_eq!(local_folder_from_variable(None), None);
        assert_eq!(local_folder_from_variable(Some("".into())), None);
        assert_eq!(local_folder_from_variable(Some("relative\\x".into())), None);
        assert_eq!(
            local_folder_from_variable(Some("C:\\Users\\a\\AppData\\Local".into())),
            Some(PathBuf::from("C:\\Users\\a\\AppData\\Local"))
        );
    }
    #[test]
    fn the_late_call_finds_the_same_folder_as_the_variable() {
        // The fallback path, run for real: it loads shell32 in this test process only.
        let known = known_local_folder().unwrap();
        assert_eq!(known.file_name().unwrap(), "Local");
    }
}
