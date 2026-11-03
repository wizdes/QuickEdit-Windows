//! Startup's share of the settings: read the file and resolve the keys. Run it in release
//! with `cargo test -p qe-settings --release --locked --test speed -- --nocapture`. The
//! 1 ms limit holds in release. A debug build only reports, because it is not what ships.
mod support;

use qe_api::CommandId;
use qe_settings::{SettingsStore, default_settings_path};
use std::time::{Duration, Instant};

fn loaded(dll: windows::core::PCWSTR) -> bool {
    // SAFETY: a plain query with a static name. It loads nothing.
    unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(dll) }.is_ok()
}

#[test]
fn reading_the_file_and_resolving_keys_stays_under_a_millisecond() {
    use windows::core::w;
    // First in this process, so this is the cold cost.
    let start = Instant::now();
    let folder = default_settings_path().unwrap();
    let folder_lookup = start.elapsed();
    assert!(folder.ends_with("QuickEdit\\settings.txt"));

    let path = support::settings_path("speed");
    std::fs::write(
        &path,
        "WindowWidthCharacters=120\r\nWindowHeightLines=40\r\nPlainTextFont=Consolas 12\r\nSmartQuotes=true\r\nShowLineNumbers=true\r\nOpenEncoding=UTF-8\r\nKeyboardShortcut.Find=Ctrl+G\r\nKeyboardShortcut.Save=none\r\nKeyboardShortcut.Rename=F5\r\nFuture=1\r\n",
    )
    .unwrap();
    // The first read of a file just written is slower than later ones, so it is measured
    // on its own and reported.
    let start = Instant::now();
    let _ = std::fs::read(&path).unwrap();
    let first_read = start.elapsed();
    let mut times = Vec::with_capacity(200);
    let mut first = Duration::ZERO;
    for run in 0..200 {
        let start = Instant::now();
        let store = SettingsStore::open(&path);
        let bound = CommandId::ALL
            .iter()
            .filter(|command| store.key_of(**command).is_some())
            .count();
        let elapsed = start.elapsed();
        assert!(bound > 10);
        assert_eq!(store.load_problem(), None);
        if run == 0 {
            first = elapsed;
        }
        times.push(elapsed);
    }
    times.sort();
    let (median, worst) = (times[100], times[199]);
    println!(
        "folder lookup, cold: {folder_lookup:?}; first read of the new file: {first_read:?}; open and resolve: first {first:?}, median {median:?}, worst {worst:?}"
    );
    if !cfg!(debug_assertions) {
        assert!(first < Duration::from_millis(1), "first run: {first:?}");
        assert!(median < Duration::from_millis(1), "median: {median:?}");
    }
    // Startup loads no heavy DLL: rule 6 of the design spec's speed rules.
    assert!(!loaded(w!("shell32.dll")), "shell32 is loaded");
    assert!(!loaded(w!("comdlg32.dll")), "comdlg32 is loaded");
    // The window's libraries wait for the window. A debug build keeps code that a release
    // build drops, so these hold only where they count.
    if !cfg!(debug_assertions) {
        assert!(!loaded(w!("user32.dll")), "user32 is loaded");
        assert!(!loaded(w!("gdi32.dll")), "gdi32 is loaded");
        assert!(!loaded(w!("dwmapi.dll")), "dwmapi is loaded");
        assert!(!loaded(w!("uxtheme.dll")), "uxtheme is loaded");
        assert!(!loaded(w!("dwrite.dll")), "dwrite is loaded");
        assert!(!loaded(w!("d2d1.dll")), "d2d1 is loaded");
        assert!(!loaded(w!("imm32.dll")), "imm32 is loaded");
    }
}
#[test]
fn a_broken_file_costs_no_flush_on_the_launch_thread() {
    // Report the cost, and hold the limit in release once its copy is there.
    let path = support::settings_path("speed-broken");
    std::fs::write(&path, "TabWidth=abc\r\nWindowHeightLines=40\r\n").unwrap();
    let start = Instant::now();
    let store = SettingsStore::open(&path);
    let with_new_copy = start.elapsed();
    assert!(store.load_problem().is_some());
    let mut times = Vec::with_capacity(200);
    for _ in 0..200 {
        let start = Instant::now();
        let store = SettingsStore::open(&path);
        times.push(start.elapsed());
        assert!(store.load_problem().is_some());
    }
    times.sort();
    let (median, worst) = (times[100], times[199]);
    println!(
        "broken file: first open with a new copy {with_new_copy:?}; copy already there: median {median:?}, worst {worst:?}"
    );
    if !cfg!(debug_assertions) {
        assert!(median < Duration::from_millis(1), "median: {median:?}");
    }
}
