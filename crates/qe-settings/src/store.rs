//! The settings store. It lives on the UI thread and is cheap to clone.
//!
//! Reading happens once, at `open`, on the caller's thread: one small file, under 1 ms
//! in the release test. A change stays in memory at once, and a short-lived thread
//! writes the file, so the UI thread never waits on the disk. No thread runs at idle.
use crate::font_dialog::{LateFunction, late_function};
use crate::format::{
    MAX_FILE_BYTES, Parsed, SHORTCUT_PREFIX, StoredValue, default_of, encode, encoding_by_name,
    parse, parse_font, value_problem,
};
use crate::keys::{Refusal, kind_refusal, menu_path_text, menu_place, opens_submenu};
use qe_api::{CommandId, EncodingChoice, KeyBinding, Modifiers, SettingKey, TextEncoding};
use std::cell::RefCell;
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
    /// A write failed. The text is for the user. Changes stay in memory.
    WriteFailed(String),
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
    pending: Option<(PathBuf, String)>,
    running: bool,
    finished: Option<Result<(), String>>,
}
#[derive(Default)]
struct Writer {
    slot: Mutex<WriteSlot>,
    done: Condvar,
    wakes: Mutex<Vec<(u64, WriteWake)>>,
    /// The copy of a broken file. It is flushed here, off the launch thread, before the
    /// first save may replace the original.
    copy_to_flush: Mutex<Option<PathBuf>>,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl Writer {
    /// Takes the newest text. A write in progress finishes first, and any text queued
    /// behind it is replaced, so a burst of changes makes at most two writes.
    fn queue(self: &Arc<Self>, path: PathBuf, text: String) -> Result<(), String> {
        let mut slot = lock(&self.slot);
        slot.pending = Some((path, text));
        if slot.running {
            return Ok(());
        }
        slot.running = true;
        let me = self.clone();
        let started = std::thread::Builder::new()
            .name("settings-writer".into())
            .spawn(move || me.run());
        if let Err(error) = started {
            slot.running = false;
            slot.pending = None;
            return Err(format!("the writer could not start: {error}"));
        }
        Ok(())
    }
    fn run(&self) {
        loop {
            let job = lock(&self.slot).pending.take();
            let Some((path, text)) = job else {
                let mut slot = lock(&self.slot);
                if slot.pending.is_some() {
                    continue;
                }
                slot.running = false;
                self.done.notify_all();
                return;
            };
            let result = self
                .flush_copy()
                .and_then(|()| replace_file(&path, text.as_bytes()));
            lock(&self.slot).finished = Some(result);
            let wakes: Vec<WriteWake> = lock(&self.wakes).iter().map(|w| w.1.clone()).collect();
            for wake in wakes {
                wake();
            }
        }
    }
    fn flush_copy(&self) -> Result<(), String> {
        let copy = lock(&self.copy_to_flush).clone();
        let Some(copy) = copy else {
            return Ok(());
        };
        OpenOptions::new()
            .write(true)
            .open(&copy)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                format!(
                    "Settings were not saved: the copy of the old file at {} is not safe: {error}",
                    copy.display()
                )
            })?;
        *lock(&self.copy_to_flush) = None;
        Ok(())
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

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
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
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map_or_else(|| SETTINGS_FILE_NAME.into(), |n| n.to_string_lossy());
    let fail = |why: String| format!("Settings were not saved: {why}");
    match fs::metadata(path) {
        Ok(found) if found.permissions().readonly() => {
            return Err(fail(format!("{name} is read-only.")));
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
/// A failure leaves the target as it was. This runs on the writer's thread.
fn swap_in(path: &Path, temp: &Path) -> Result<(), String> {
    const TRIES: u32 = 20;
    let (from, to) = (wide(temp), wide(path));
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
    kept: Vec<String>,
    /// Each command's key now, by `CommandId as usize`.
    resolved: Vec<Option<KeyBinding>>,
    load_problem: Option<String>,
    /// A broken file with no copy beside it must not be replaced. This says why.
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
        encode(&self.values, &self.overrides, &self.kept)
    }
}
struct Inner {
    state: RefCell<State>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    next_listener: RefCell<u64>,
    events: RefCell<VecDeque<SettingChange>>,
    dispatching: RefCell<bool>,
    writer: Arc<Writer>,
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
/// A copy of the broken file beside it, whole, or an error. The same content as the newest
/// copy makes no new one, so launching again and again adds nothing. There is no flush
/// here, on the launch thread: the writer flushes the copy before any save replaces the
/// original, and until then the original is still there.
fn keep_copy(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map_or_else(|| "settings".into(), |n| n.to_string_lossy().into_owned());
    let copies = copies_beside(path, &stem)?;
    if let Some((_, newest)) = copies.last()
        && holds_these_bytes(newest, bytes)
    {
        return Ok(newest.clone());
    }
    let mut number = copies
        .last()
        .map_or(1, |(number, _)| number.saturating_add(1));
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
                return Ok(copy);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                number = number.saturating_add(1);
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
fn broken(path: &Path, bytes: &[u8], why: &str, kept_values: Option<Parsed>) -> Loaded {
    let start = if kept_values.is_some() {
        "The settings that could not be read have their defaults"
    } else {
        "QuickEdit started with default settings"
    };
    let parsed = kept_values.unwrap_or_default();
    let why = shortened_why(why);
    match keep_copy(path, bytes) {
        Ok(copy) => Loaded {
            parsed,
            problem: Some(format!(
                "The settings file has a problem: {why}. {start}. A copy of the file is at {}.",
                copy.display()
            )),
            block: None,
            copy: Some(copy),
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
            }
        }
    }
}
/// Opens the file, and tries again for a moment while another program holds it. Only a
/// file that is really missing is a first run.
fn open_for_reading(path: &Path) -> std::io::Result<File> {
    const TRIES: u32 = 6;
    let mut tries = 1;
    loop {
        match File::open(path) {
            Err(error)
                if tries < TRIES
                    && error
                        .raw_os_error()
                        .is_some_and(|code| is_passing_hold(code as u32)) =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            other => return other,
        }
    }
}
fn read(path: &Path) -> Loaded {
    let empty = |problem: Option<String>, block: Option<String>| Loaded {
        parsed: Parsed::default(),
        problem,
        block,
        copy: None,
    };
    let blocked = |problem: String| {
        empty(
            Some(problem.clone()),
            Some(format!("Settings were not saved: {problem}")),
        )
    };
    let unreadable = |error: std::io::Error| {
        blocked(format!(
            "The settings file could not be read: {error}. QuickEdit started with default settings, and changes will not be saved over the file."
        ))
    };
    // A file this long is not one this app wrote. Copying it could take any time at launch,
    // and a part of it is not a copy, so it stays where it is, not read, and is not replaced.
    let too_long = || {
        blocked(format!(
            "The settings file is over 1 MB long, so it was not read. QuickEdit started with default settings, and changes will not be saved over the file. Move or rename {} to save settings again.",
            path.display()
        ))
    };
    let mut file = match open_for_reading(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return empty(None, None),
        Err(error) => return unreadable(error),
    };
    match file.metadata() {
        Ok(found) if found.len() > MAX_FILE_BYTES => return too_long(),
        Ok(_) => {}
        Err(error) => return unreadable(error),
    }
    // It may have grown since, so one byte over the limit still counts.
    let mut bytes = Vec::new();
    if let Err(error) = Read::take(&mut file, MAX_FILE_BYTES + 1).read_to_end(&mut bytes) {
        return unreadable(error);
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return too_long();
    }
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let Ok(text) = std::str::from_utf8(text) else {
        return broken(path, &bytes, "it is not UTF-8 text", None);
    };
    match parse(text) {
        Err(why) => broken(path, &bytes, &why, None),
        Ok(parsed) if parsed.bad_values.is_empty() => Loaded {
            parsed,
            problem: None,
            block: None,
            copy: None,
        },
        Ok(parsed) => {
            let why = parsed.bad_values.join("; ");
            broken(path, &bytes, &why, Some(parsed))
        }
    }
}

impl SettingsStore {
    /// Reads the file at `path`. A missing file gives the defaults. A file with a
    /// problem is kept beside itself, and `load_problem` says so for the caller to show.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let loaded = read(&path);
        let mut state = State {
            path,
            values: loaded.parsed.values,
            overrides: loaded.parsed.keys,
            kept: loaded.parsed.kept,
            resolved: Vec::new(),
            load_problem: loaded.problem,
            save_block: loaded.block,
            write_error: None,
        };
        state
            .values
            .retain(|(key, value)| *value != default_of(*key));
        state
            .overrides
            .retain(|(command, key)| *key != command.default_key());
        state.resolve();
        let writer = Writer {
            copy_to_flush: Mutex::new(loaded.copy),
            ..Writer::default()
        };
        Self(Rc::new(Inner {
            state: RefCell::new(state),
            listeners: RefCell::default(),
            next_listener: RefCell::default(),
            events: RefCell::default(),
            dispatching: RefCell::default(),
            writer: Arc::new(writer),
        }))
    }
    pub fn path(&self) -> PathBuf {
        self.0.state.borrow().path.clone()
    }
    /// What was wrong with the file at startup, in words for the user.
    pub fn load_problem(&self) -> Option<String> {
        self.0.state.borrow().load_problem.clone()
    }
    /// Why the last write failed. The next good write clears it.
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
    /// reason. No event and no write when the value is the same.
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
                return Ok(());
            }
            state.values.retain(|(seen, _)| *seen != key);
            if value != default_of(key) {
                state.values.push((key, value));
            }
        }
        self.save();
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
                return;
            }
            state.resolve();
        }
        self.save();
        self.emit(SettingChange::Keys);
    }
    /// Restore Keyboard Defaults: every change goes, also ones for commands this
    /// version does not know.
    pub fn restore_keyboard_defaults(&self) {
        if self.drop_key_changes() {
            self.save();
            self.emit(SettingChange::Keys);
        }
    }
    fn drop_key_changes(&self) -> bool {
        let mut state = self.0.state.borrow_mut();
        let before = (state.overrides.len(), state.kept.len());
        state.overrides.clear();
        state
            .kept
            .retain(|line| !line.trim_start().starts_with(SHORTCUT_PREFIX));
        state.resolve();
        before != (state.overrides.len(), state.kept.len())
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
            self.save();
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
    fn save(&self) {
        let (path, text, block) = {
            let state = self.0.state.borrow();
            (
                state.path.clone(),
                state.encoded(),
                state.save_block.clone(),
            )
        };
        let outcome = match block {
            Some(block) => Err(block),
            None => self
                .0
                .writer
                .queue(path, text)
                .map_err(|e| format!("Settings were not saved: {e}")),
        };
        if let Err(message) = outcome {
            self.note_write(Err(message));
        }
    }
    /// Turns a finished write into a `write_error` and an event. No wait.
    pub fn poll_writes(&self) {
        let finished = lock(&self.0.writer.slot).finished.take();
        if let Some(result) = finished {
            self.note_write(result);
        }
    }
    fn note_write(&self, result: Result<(), String>) {
        let event = {
            let mut state = self.0.state.borrow_mut();
            match result {
                Ok(()) => state
                    .write_error
                    .take()
                    .map(|_| SettingChange::WriteRecovered),
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
    /// Waits for the writer. For the app's exit and for tests: never while typing.
    /// Returns the last write's error, if there is one.
    pub fn flush(&self) -> Result<(), String> {
        self.0.writer.wait();
        self.poll_writes();
        self.write_error().map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
