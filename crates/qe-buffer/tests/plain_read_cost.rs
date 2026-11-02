#![cfg(windows)]
//! A plain chunk read has to stay cheap enough to run a few times per line, as PDF
//! export does, and it has to stay as safe as it was. The cost tests count work, so
//! they mean the same on a busy PC and an idle one. Real times are in
//! `examples/visit_bench.rs`.
use qe_api::{ByteOffset, TextSnapshot};
use qe_buffer::MappedStore;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const PAGE: usize = 64 * 1024;

fn temp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qe-plain-{}-{name}", std::process::id()))
}

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

fn open_complete(path: &PathBuf) -> MappedStore {
    let store = MappedStore::open(path).unwrap();
    let start = Instant::now();
    while !store.is_index_complete() {
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(1));
    }
    store
}

fn lines(size: usize) -> String {
    "a short line of text\n".repeat(size / 21)
}

#[test]
fn short_reads_inside_one_page_load_it_once_and_do_not_check_the_path_each_time() {
    let path = temp("cost.txt");
    let text = lines(PAGE * 3);
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(PAGE, PAGE + 4), &mut |_| {}));
    let reads = store.lookup_disk_reads();
    let checks = store.file_checks();
    let start = Instant::now();
    let rounds = 6000;
    for round in 0..rounds {
        let from = PAGE + 21 * (round % 1000);
        assert!(store.visit_chunks(at(from, from + 12), &mut |s| {
            assert_eq!(s.len(), 12);
        }));
    }
    let elapsed = start.elapsed();
    assert_eq!(
        store.lookup_disk_reads() - reads,
        0,
        "pages were read again"
    );
    let checks = store.file_checks() - checks;
    // One path check per millisecond at most, and a few from the index thread.
    let limit = elapsed.as_millis() as u64 + 8;
    assert!(checks <= limit, "{checks} path checks in {elapsed:?}");
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_read_right_after_a_shrink_gives_no_text() {
    let path = temp("shrink.txt");
    for keep in [0, 5, PAGE as u64 + 3] {
        fs::write(&path, lines(PAGE * 3)).unwrap();
        let store = open_complete(&path);
        for _ in 0..20 {
            assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
        }
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(keep)
            .unwrap();
        // No wait, so this falls inside the time between two path checks.
        let mut calls = 0;
        assert!(!store.visit_chunks(at(0, 12), &mut |_| calls += 1));
        assert_eq!(calls, 0, "text went out after the file shrank to {keep}");
        assert!(store.status().failure.is_some());
        drop(store);
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn a_read_right_after_a_replace_gives_no_text() {
    let path = temp("swap.txt");
    let other = temp("swap-other.txt");
    let text = lines(PAGE * 3);
    for round in 0..40 {
        fs::write(&path, &text).unwrap();
        fs::write(&other, text.replace('a', "b")).unwrap();
        let store = open_complete(&path);
        for _ in 0..20 {
            assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
        }
        fs::rename(&other, &path).unwrap();
        // No wait: the store's handle still reads the old file, so only a file check
        // on that handle can catch this one.
        let mut calls = 0;
        assert!(
            !store.visit_chunks(at(0, 12), &mut |_| calls += 1),
            "round {round}"
        );
        assert_eq!(calls, 0);
        drop(store);
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn a_read_right_after_a_delete_and_a_new_file_gives_no_text() {
    let path = temp("delete.txt");
    let text = lines(PAGE * 3);
    for round in 0..40 {
        fs::write(&path, &text).unwrap();
        let store = open_complete(&path);
        for _ in 0..20 {
            assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
        }
        fs::remove_file(&path).unwrap();
        fs::write(&path, &text).unwrap();
        let mut calls = 0;
        assert!(
            !store.visit_chunks(at(0, 12), &mut |_| calls += 1),
            "round {round}"
        );
        assert_eq!(calls, 0);
        drop(store);
    }
    fs::remove_file(path).unwrap();
}

/// Moves the file and puts every time back, so the store's handle sees no change at all.
fn forge_rename(from: &PathBuf, to: &PathBuf) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
    };
    let size = size_of::<FILE_BASIC_INFO>() as u32;
    let mut basic = FILE_BASIC_INFO::default();
    let file = OpenOptions::new().read(true).open(from).unwrap();
    // SAFETY: the handle is open and `basic` is a live value of the right size.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            (&raw mut basic).cast(),
            size,
        )
        .unwrap();
    }
    drop(file);
    fs::rename(from, to).unwrap();
    let file = OpenOptions::new().write(true).open(to).unwrap();
    // SAFETY: the same as above; the call only reads from `basic`.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            (&raw const basic).cast(),
            size,
        )
        .unwrap();
    }
}

/// Only the path check can catch this one, and it is due after 1 ms of reads.
#[test]
fn a_read_a_little_after_a_rename_the_handle_cannot_see_gives_no_text() {
    let path = temp("away.txt");
    let other = temp("away-other.txt");
    fs::write(&path, lines(PAGE * 3)).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
    forge_rename(&path, &other);
    std::thread::sleep(Duration::from_millis(2));
    let mut calls = 0;
    assert!(!store.visit_chunks(at(0, 12), &mut |_| calls += 1));
    assert_eq!(calls, 0);
    drop(store);
    fs::remove_file(other).unwrap();
}

#[test]
fn a_long_read_always_checks_the_path_first() {
    let path = temp("long.txt");
    let other = temp("long-other.txt");
    let text = lines(PAGE * 3);
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    let checks = store.file_checks();
    for _ in 0..5 {
        assert!(store.visit_chunks(at(0, PAGE + 21), &mut |_| {}));
    }
    assert!(
        store.file_checks() - checks >= 10,
        "a long read checks at its start and its end"
    );
    drop(store);
    fs::write(&other, &text).unwrap();
    let store = open_complete(&path);
    fs::rename(&other, &path).unwrap();
    let mut calls = 0;
    assert!(!store.visit_chunks(at(0, PAGE + 21), &mut |_| calls += 1));
    assert_eq!(calls, 0);
    drop(store);
    fs::remove_file(path).unwrap();
}

/// Text that leaves is compared with the file, byte for byte. A change somewhere else
/// in the same page is not that text, and it is caught when that part is read.
#[test]
fn a_read_compares_its_own_bytes_with_the_file() {
    use std::io::{Seek, SeekFrom, Write};
    let path = temp("compare.txt");
    let text = lines(PAGE * 3);
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(PAGE, PAGE + 21), &mut |_| {}));
    let times = fs::metadata(&path).unwrap();
    let modified = times.modified().unwrap();
    {
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(PAGE as u64 + 2100)).unwrap();
        file.write_all(b"Z").unwrap();
        file.sync_all().unwrap();
        // Hide it from the file check as far as a plain writer can.
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }
    let mut seen = String::new();
    let ok = store.visit_chunks(at(PAGE + 2100 - 21, PAGE + 2100 + 21), &mut |s| {
        seen.push_str(s);
    });
    assert!(!ok, "the changed byte was in this read");
    assert!(!seen.contains('Z'), "changed text went out: {seen}");
    assert!(store.status().failure.is_some());
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_lock_on_a_page_in_memory_waits_and_then_passes() {
    let path = temp("lock-passes.txt");
    fs::write(&path, lines(PAGE * 3)).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
    let locker = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    locker.lock().unwrap();
    let unlock = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        locker.unlock().unwrap();
    });
    let mut text = String::new();
    assert!(store.visit_chunks(at(0, 12), &mut |s| text.push_str(s)));
    assert_eq!(text, "a short line");
    unlock.join().unwrap();
    assert_eq!(store.error_state(), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_lock_on_a_page_in_memory_past_the_limit_is_a_failure() {
    let path = temp("lock-stays.txt");
    fs::write(&path, lines(PAGE * 3)).unwrap();
    let store = open_complete(&path);
    store.set_test_lock_give_up(Duration::from_millis(50));
    assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
    let locker = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    locker.lock().unwrap();
    let start = Instant::now();
    let mut calls = 0;
    assert!(!store.visit_chunks(at(0, 12), &mut |_| calls += 1));
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(calls, 0);
    let failure = store.status().failure.unwrap();
    assert!(failure.message.contains("locked"), "{}", failure.message);
    locker.unlock().unwrap();
    drop(store);
    fs::remove_file(path).unwrap();
}

/// Reads of the wrong kind keep their old answers on the new route.
#[test]
fn a_range_that_cuts_a_character_is_refused_on_both_edges() {
    let path = temp("edges.txt");
    fs::write(&path, "語語語語\n".repeat(3)).unwrap();
    let store = open_complete(&path);
    let mut calls = 0;
    assert!(!store.visit_chunks(at(1, 3), &mut |_| calls += 1));
    assert!(!store.visit_chunks(at(0, 4), &mut |_| calls += 1));
    assert_eq!(calls, 0);
    let mut text = String::new();
    assert!(store.visit_chunks(at(3, 6), &mut |s| text.push_str(s)));
    assert_eq!(text, "語");
    assert!(store.visit_chunks(at(3, 3), &mut |_| panic!("no text")));
    assert_eq!(store.error_state(), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

/// An empty read at the end of a file whose size is a whole number of pages names a
/// page past the last one. Layout makes this read for an empty last line.
#[test]
fn an_empty_read_at_the_end_of_a_whole_number_of_pages_is_fine() {
    let path = temp("whole-pages.txt");
    let mut text = "x".repeat(2 * PAGE - 1);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    let size = text.len();
    let mut calls = 0;
    assert_eq!(
        store.try_visit_chunks(at(size, size), &mut |_| calls += 1),
        Ok(())
    );
    assert_eq!(calls, 0);
    // The reader thread would have failed the store a moment later.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(store.error_state(), None);
    assert!(store.visit_chunks(at(size, size), &mut |_| calls += 1));
    assert_eq!(calls, 0);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn memory_does_not_grow_with_short_reads() {
    let path = temp("memory.txt");
    fs::write(&path, lines(PAGE * 20)).unwrap();
    let store = open_complete(&path);
    let before = store.index_memory_bytes();
    for round in 0..3000 {
        let from = 21 * (round * 19 % (PAGE * 20 / 21 - 1));
        assert!(store.visit_chunks(at(from, from + 12), &mut |_| {}));
    }
    assert_eq!(store.index_memory_bytes(), before);
    drop(store);
    fs::remove_file(path).unwrap();
}
