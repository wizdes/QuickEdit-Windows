//! No thread stays after a write. Its own test binary, so no other test's writer counts.
mod support;

use qe_api::SettingKey;
use qe_settings::SettingsStore;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows::Win32::System::Threading::GetCurrentProcessId;

fn threads_here() -> usize {
    // SAFETY: the snapshot is closed below, and the entry has its size set.
    unsafe {
        let me = GetCurrentProcessId();
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).unwrap();
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut count = 0;
        let mut more = Thread32First(snapshot, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == me {
                count += 1;
            }
            more = Thread32Next(snapshot, &mut entry).is_ok();
        }
        CloseHandle(snapshot).unwrap();
        count
    }
}
#[test]
fn no_thread_stays_after_a_write() {
    let store = SettingsStore::open(support::settings_path("idle-threads"));
    let before = threads_here();
    for value in 0..5 {
        store.set_integer(SettingKey::TabWidth, 2 + value).unwrap();
        store.flush().unwrap();
    }
    // The writer ends just after it says so. Windows counts it a moment longer.
    let started = Instant::now();
    while threads_here() > before {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{} threads, {before} before the writes",
            threads_here()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
