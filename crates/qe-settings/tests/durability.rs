//! A save against other readers, other writers, a held file and a killed process. Run it
//! in release too, because the review's missing-file window showed there:
//! `cargo test -p qe-settings --release --locked --test durability -- --nocapture`.
mod support;

use qe_api::SettingKey;
use qe_settings::SettingsStore;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
use support::{settings_path, test_folder};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CHILD_PATH: &str = "QE_SETTINGS_CHILD_PATH";

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
/// Holds the file so that no one else can open, rename or delete it.
fn hold(path: &Path) -> std::fs::File {
    OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .unwrap()
}
fn saved_store(name: &str) -> (SettingsStore, PathBuf) {
    let path = settings_path(name);
    let store = SettingsStore::open(&path);
    store.set_integer(SettingKey::TabWidth, 2).unwrap();
    store.flush().unwrap();
    (store, path)
}

/// The review's probe: 1,963 of 28,199 opens found no file with `ReplaceFileW`.
#[test]
fn a_reader_never_finds_the_file_missing_during_saves() {
    let (store, path) = saved_store("reader");
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (path, stop) = (path.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut kinds = BTreeMap::<String, u32>::new();
            let mut opens = 0u32;
            while !stop.load(Ordering::Relaxed) {
                opens += 1;
                if let Err(error) = std::fs::File::open(&path) {
                    let code = error.raw_os_error().unwrap_or(-1);
                    *kinds
                        .entry(format!("{:?} {code}", error.kind()))
                        .or_default() += 1;
                }
            }
            (opens, kinds)
        })
    };
    let started = Instant::now();
    let mut failed = Vec::new();
    for i in 0..300 {
        store.set_integer(SettingKey::TabWidth, 2 + i % 50).unwrap();
        if let Err(error) = store.flush() {
            failed.push(error);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let (opens, kinds) = reader.join().unwrap();
    println!(
        "300 saves in {:?}; {opens} opens; open errors {kinds:?}; failed saves {}",
        started.elapsed(),
        failed.len()
    );
    assert!(opens > 100, "the reader really ran: {opens}");
    assert!(
        !kinds.keys().any(|kind| kind.starts_with("NotFound")),
        "{kinds:?}"
    );
    assert_eq!(failed, Vec::<String>::new());
}
/// What a second QuickEdit sees when it starts during a save: never the defaults, and
/// never a problem that would block its own saves.
#[test]
fn a_store_opened_during_saves_always_reads_the_settings() {
    let (store, path) = saved_store("second-open");
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (path, stop) = (path.clone(), stop.clone());
        std::thread::spawn(move || {
            let (mut opens, mut wrong) = (0u32, Vec::new());
            while !stop.load(Ordering::Relaxed) {
                opens += 1;
                let other = SettingsStore::open(&path);
                let width = other.get_integer(SettingKey::TabWidth);
                if let Some(problem) = other.load_problem() {
                    wrong.push(problem);
                } else if width == Some(8) {
                    wrong.push("the defaults, with no word about it".to_owned());
                }
            }
            (opens, wrong)
        })
    };
    for i in 0..200 {
        store.set_integer(SettingKey::TabWidth, 2 + i % 5).unwrap();
        store.flush().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let (opens, wrong) = reader.join().unwrap();
    println!("{opens} opens during 200 saves, {} wrong", wrong.len());
    assert!(opens > 20, "{opens}");
    assert_eq!(wrong.first(), None);
}
#[test]
fn four_writers_leave_a_whole_file_and_no_temp_files() {
    let path = settings_path("four-writers");
    let mut writers = Vec::new();
    for thread in 0..4i64 {
        let path = path.clone();
        writers.push(std::thread::spawn(move || {
            let store = SettingsStore::open(&path);
            let mut errors = Vec::new();
            for i in 0..100 {
                store
                    .set_integer(SettingKey::WindowWidthCharacters, 100 + thread * 1000 + i)
                    .unwrap();
                if let Err(error) = store.flush() {
                    errors.push(error);
                }
            }
            errors
        }));
    }
    let errors: Vec<String> = writers
        .into_iter()
        .flat_map(|writer| writer.join().unwrap())
        .collect();
    println!("{} failed saves of 400", errors.len());
    assert_eq!(errors.first(), None);
    assert_eq!(
        names_in(path.parent().unwrap()),
        ["settings.txt"],
        "no temp file and no Windows backup name"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("WindowWidthCharacters="), "{text}");
    assert!(text.ends_with("\r\n"), "{text}");
}
/// A real failed swap: the target stays held past every retry.
#[test]
fn a_failed_swap_keeps_the_old_file_and_leaves_no_temp_file() {
    let (store, path) = saved_store("failed-swap");
    let held = hold(&path);
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    let error = store.flush().unwrap_err();
    assert!(error.starts_with("Settings were not saved:"), "{error}");
    drop(held);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=2\r\n");
    assert_eq!(names_in(path.parent().unwrap()), ["settings.txt"]);
    // The change is still in memory, and the next save writes it.
    store.set_integer(SettingKey::TabWidth, 4).unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=4\r\n");
}
#[test]
fn a_file_held_for_a_moment_only_delays_the_save() {
    let (store, path) = saved_store("short-hold");
    let held = hold(&path);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        drop(held);
    });
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    store.flush().unwrap();
    release.join().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=3\r\n");
}
#[test]
fn a_file_held_for_a_moment_does_not_look_broken_on_open() {
    let (_store, path) = saved_store("short-hold-open");
    let held = hold(&path);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        drop(held);
    });
    let other = SettingsStore::open(&path);
    release.join().unwrap();
    assert_eq!(other.load_problem(), None);
    assert_eq!(other.get_integer(SettingKey::TabWidth), Some(2));
}
#[test]
fn a_file_held_for_long_is_reported_and_not_saved_over() {
    let (_store, path) = saved_store("long-hold-open");
    let held = hold(&path);
    let started = Instant::now();
    let other = SettingsStore::open(&path);
    let waited = started.elapsed();
    let problem = other.load_problem().expect("the caller hears of it");
    assert!(problem.contains("could not be read"), "{problem}");
    assert!(waited < Duration::from_millis(500), "{waited:?}");
    drop(held);
    other.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert!(other.write_error().is_some(), "the block holds");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=2\r\n");
}
fn age(path: &Path, seconds: u64) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
        .unwrap();
}
/// A crash can leave a temp file. The next save removes it, and nothing else.
#[test]
fn the_next_save_removes_old_temp_files_by_exact_name_only() {
    let folder = test_folder("old-temps");
    let path = folder.join("settings.txt");
    std::fs::write(&path, "TabWidth=4\r\n").unwrap();
    let left_by_crashes = ["settings.txt.tmp-4242-0", "settings.txt.tmp-1-77"];
    let not_ours = [
        "settings.txt.tmp-4242",
        "settings.txt.tmp-abc-1",
        "settings.txt.tmp-1-2.bak",
        "settings.txt.tmp--1",
        "settings.txt.tmp-1-",
        "other.txt.tmp-1-1",
        "xsettings.txt.tmp-1-1",
        "settings.txt~RF1234.TMP",
    ];
    for name in left_by_crashes.iter().chain(&not_ours) {
        std::fs::write(folder.join(name), "old").unwrap();
        age(&folder.join(name), 3600);
    }
    // Another running copy may be about to swap this one in, so a new one stays.
    let in_use = "settings.txt.tmp-4243-0";
    std::fs::write(folder.join(in_use), "new").unwrap();

    let store = SettingsStore::open(&path);
    assert_eq!(store.load_problem(), None, "reading removes nothing");
    assert_eq!(names_in(&folder).len(), 12);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    let mut want: Vec<String> = not_ours.iter().map(|n| (*n).to_owned()).collect();
    want.push(in_use.to_owned());
    want.push("settings.txt".to_owned());
    want.sort();
    assert_eq!(names_in(&folder), want);
}
/// Not a test by itself: the process that `a_killed_save_leaves_a_whole_file` kills.
#[test]
fn child_saves_until_killed() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let store = SettingsStore::open(PathBuf::from(path));
    let mut i = 0;
    loop {
        // Never 8, the default, which would leave the file empty.
        store.set_integer(SettingKey::TabWidth, 9 + i % 50).unwrap();
        store.flush().unwrap();
        i += 1;
    }
}
#[test]
fn a_killed_save_leaves_a_whole_file() {
    let path = settings_path("killed");
    let folder = path.parent().unwrap().to_owned();
    std::fs::write(&path, "TabWidth=9\r\n").unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut temps_seen = 0;
    for round in 0..12u64 {
        let mut child = std::process::Command::new(&exe)
            .args(["child_saves_until_killed", "--exact"])
            .env(CHILD_PATH, &path)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Long enough to start, and a different moment in a save each time.
        std::thread::sleep(Duration::from_millis(60 + round * 7));
        child.kill().unwrap();
        child.wait().unwrap();
        let after = SettingsStore::open(&path);
        assert_eq!(after.load_problem(), None, "round {round}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("TabWidth=") && text.ends_with("\r\n"),
            "round {round}: {text:?}"
        );
        temps_seen += names_in(&folder).len() - 1;
    }
    println!("12 kills; {temps_seen} temp files seen after them");
    for name in names_in(&folder) {
        if name != "settings.txt" {
            age(&folder.join(name), 3600);
        }
    }
    let last = SettingsStore::open(&path);
    last.set_bool(SettingKey::SmartQuotes, true).unwrap();
    last.flush().unwrap();
    assert_eq!(names_in(&folder), ["settings.txt"]);
}
