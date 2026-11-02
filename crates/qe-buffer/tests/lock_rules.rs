#![cfg(windows)]
//! Another app's lock on the file. A lock is not a change and not text. It passes, or it
//! becomes a failure after its limit, whatever other reads do in the meantime.
use qe_api::{ByteOffset, LookupError, TextSnapshot, TextStore};
use qe_buffer::MappedStore;
use std::fs::{self, OpenOptions};
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{LockFile, UnlockFile};

const PAGE: usize = 64 * 1024;
const LINE: &str = "a short line of text\n";

fn temp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qe-lock-{}-{name}", std::process::id()))
}

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

fn write_lines(path: &PathBuf, size: usize) {
    fs::write(path, LINE.repeat(size / LINE.len())).unwrap();
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

/// Holds a lock on one byte range of a file.
struct RangeLock(fs::File, u32, u32);

impl RangeLock {
    fn new(path: &PathBuf, offset: usize, len: usize) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        // SAFETY: the handle is open for this call.
        unsafe {
            LockFile(
                HANDLE(file.as_raw_handle()),
                offset as u32,
                0,
                len as u32,
                0,
            )
            .unwrap();
        }
        Self(file, offset as u32, len as u32)
    }
}

impl Drop for RangeLock {
    fn drop(&mut self) {
        // SAFETY: the handle is open for this call, and this range is locked.
        unsafe {
            UnlockFile(HANDLE(self.0.as_raw_handle()), self.1, 0, self.2, 0).unwrap();
        }
    }
}

/// The text of the first page is not known during a lock. Nothing may present zeros.
#[test]
fn a_file_opened_during_a_lock_gives_no_text_on_a_try_read_until_it_ends() {
    let path = temp("open-try.txt");
    write_lines(&path, PAGE * 2);
    let lock = RangeLock::new(&path, 0, PAGE * 4);
    let store = MappedStore::open(&path).unwrap();
    for _ in 0..5 {
        let mut seen = String::new();
        let result = store.try_visit_chunks(at(0, 12), &mut |s| seen.push_str(s));
        assert!(
            matches!(result, Err(LookupError::NotYetKnown { .. })),
            "{result:?}, {seen:?}"
        );
        assert_eq!(seen, "");
        assert_eq!(store.error_state(), None, "a lock is not a failure");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(lock);
    let start = Instant::now();
    let mut seen = String::new();
    while store
        .try_visit_chunks(at(0, 12), &mut |s| seen.push_str(s))
        .is_err()
    {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the text never came"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(seen, "a short line");
    assert_eq!(store.error_state(), None);
    let start = Instant::now();
    while !store.is_index_complete() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the index never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(store.error_state(), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_file_opened_during_a_lock_has_a_plain_read_wait_and_then_give_the_text() {
    let path = temp("open-plain.txt");
    write_lines(&path, PAGE * 2);
    let lock = RangeLock::new(&path, 0, PAGE * 4);
    let store = MappedStore::open(&path).unwrap();
    let reader = store.clone();
    let read = std::thread::spawn(move || {
        let mut seen = String::new();
        let ok = reader.visit_chunks(at(0, 12), &mut |s| seen.push_str(s));
        (ok, seen)
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(!read.is_finished(), "the read did not wait for the lock");
    assert_eq!(store.error_state(), None, "a lock is not a failure");
    drop(lock);
    let (ok, seen) = read.join().unwrap();
    assert!(ok, "{:?}", store.error_state());
    assert_eq!(seen, "a short line");
    // A plain read after the lock too, which goes through the compare again.
    let mut seen = String::new();
    assert!(store.visit_chunks(at(0, 12), &mut |s| seen.push_str(s)));
    assert_eq!(seen, "a short line");
    assert_eq!(store.error_state(), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_lock_on_the_start_of_a_file_is_reported_as_a_lock_past_its_limit() {
    let path = temp("open-limit.txt");
    write_lines(&path, PAGE * 2);
    let lock = RangeLock::new(&path, 0, PAGE * 4);
    let store = MappedStore::open(&path).unwrap();
    store.set_test_lock_give_up(Duration::from_millis(100));
    let start = Instant::now();
    assert!(!store.visit_chunks(at(0, 12), &mut |_| panic!("no text")));
    assert!(start.elapsed() < Duration::from_secs(10));
    let failure = store.status().failure.unwrap();
    assert!(failure.message.contains("locked"), "{}", failure.message);
    drop(lock);
    drop(store);
    fs::remove_file(path).unwrap();
}

/// Reads of another page go on working all the time, and that must not hold the clock of
/// the locked one back. It did: any good read cleared one shared clock.
fn locked_read_ends_while_others_succeed(short: bool) {
    let path = temp(if short {
        "others-short.txt"
    } else {
        "others-long.txt"
    });
    write_lines(&path, PAGE * 8);
    let store = open_complete(&path);
    store.set_test_lock_give_up(Duration::from_millis(200));
    let start_byte = 6 * PAGE / LINE.len() * LINE.len();
    let lock = RangeLock::new(&path, start_byte, 21);

    let stop = Arc::new(AtomicBool::new(false));
    let other = store.clone();
    let stop_other = stop.clone();
    let reads = std::thread::spawn(move || {
        let mut good = 0;
        while !stop_other.load(Ordering::Relaxed) {
            if other.visit_chunks(at(0, 12), &mut |_| {}) {
                good += 1;
            }
            // A long read too, so that this also clears the clock of a page load.
            let _ = other.visit_chunks(at(0, PAGE + 21), &mut |_| {});
            std::thread::sleep(Duration::from_millis(5));
        }
        good
    });
    std::thread::sleep(Duration::from_millis(50));
    let end = if short {
        start_byte + 12
    } else {
        start_byte + PAGE + 21
    };
    // On its own thread, so that a read that waits for ever fails this test and not the
    // whole run.
    let waiter = store.clone();
    let read = std::thread::spawn(move || waiter.visit_chunks(at(start_byte, end), &mut |_| {}));
    let begun = Instant::now();
    while !read.is_finished() {
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "the read still waits, far past its limit, while other reads work"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!read.join().unwrap());
    stop.store(true, Ordering::Relaxed);
    let good = reads.join().unwrap();
    assert!(good > 0, "no read worked, so this proves nothing");
    let failure = store.status().failure.unwrap();
    assert!(failure.message.contains("locked"), "{}", failure.message);
    drop(lock);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_short_read_of_a_locked_range_ends_at_the_limit_while_other_reads_work() {
    locked_read_ends_while_others_succeed(true);
}

#[test]
fn a_long_read_of_a_locked_range_ends_at_the_limit_while_other_reads_work() {
    locked_read_ends_while_others_succeed(false);
}

/// One lock passes and a second starts on the same bytes. The two together last longer
/// than the limit, but neither does.
#[test]
fn a_second_lock_starts_its_own_clock_on_the_short_read() {
    let path = temp("two.txt");
    write_lines(&path, PAGE * 3);
    let store = open_complete(&path);
    store.set_test_lock_give_up(Duration::from_millis(1000));
    assert!(store.visit_chunks(at(0, 12), &mut |_| {}));
    for round in 0..2 {
        let lock = RangeLock::new(&path, 0, 21);
        let unlock = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            drop(lock);
        });
        let mut seen = String::new();
        assert!(
            store.visit_chunks(at(0, 12), &mut |s| seen.push_str(s)),
            "round {round}: {:?}",
            store.error_state()
        );
        assert_eq!(seen, "a short line");
        unlock.join().unwrap();
    }
    drop(store);
    fs::remove_file(path).unwrap();
}

/// A snapshot's read outlives the tab during a lock. It stops at the next retry, without
/// failing the file for anyone else.
#[test]
fn closing_the_file_ends_a_short_read_that_waits_for_a_lock() {
    let path = temp("cancel.txt");
    write_lines(&path, PAGE * 3);
    let store = open_complete(&path);
    let snapshot = store.snapshot();
    assert!(snapshot.visit_chunks(at(0, 12), &mut |_| {}));
    let lock = RangeLock::new(&path, 0, 21);
    let reader = snapshot.clone();
    let read = std::thread::spawn(move || {
        let start = Instant::now();
        (reader.visit_chunks(at(0, 12), &mut |_| {}), start.elapsed())
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(!read.is_finished(), "the read did not wait for the lock");
    drop(store);
    let begun = Instant::now();
    while !read.is_finished() {
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "the read did not end when the file closed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let (ok, _) = read.join().unwrap();
    assert!(!ok);
    assert_eq!(snapshot.status().failure, None, "only this read ended");
    drop(lock);
    let mut seen = String::new();
    assert!(snapshot.visit_chunks(at(0, 12), &mut |s| seen.push_str(s)));
    assert_eq!(seen, "a short line");
    drop(snapshot);
    fs::remove_file(path).unwrap();
}

/// A store in a lock wakes its view only while a read is held back, and it stops when
/// nobody asks. An idle store has no timer.
#[test]
fn a_held_back_page_wakes_the_asker_and_stops_when_nobody_asks() {
    let path = temp("wakes.txt");
    write_lines(&path, PAGE * 40);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let store = MappedStore::open_with_notify(
        &path,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .unwrap();
    let start = Instant::now();
    while !store.is_index_complete() {
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(50));
    let far = 30 * PAGE / LINE.len() * LINE.len();
    let quiet = wakes.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(wakes.load(Ordering::Relaxed), quiet, "an idle store woke");

    let lock = RangeLock::new(&path, 0, PAGE * 40);
    // One ask, and then nobody asks: exactly one wake, and then quiet.
    let result = store.try_visit_chunks(at(far, far + 12), &mut |_| {});
    assert!(matches!(result, Err(LookupError::NotYetKnown { .. })));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(wakes.load(Ordering::Relaxed), quiet + 1);
    // Someone who asks at each wake goes on being woken.
    let mut seen = wakes.load(Ordering::Relaxed);
    let _ = store.try_visit_chunks(at(far, far + 12), &mut |_| {});
    let begun = Instant::now();
    while begun.elapsed() < Duration::from_millis(600) {
        let now = wakes.load(Ordering::Relaxed);
        if now != seen {
            seen = now;
            let _ = store.try_visit_chunks(at(far, far + 12), &mut |_| {});
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let during = wakes.load(Ordering::Relaxed) - quiet - 1;
    assert!(during >= 2, "{during} wakes in 600 ms");
    drop(lock);
    // A wake after the lock lets the asker in. Then the store is quiet again.
    let begun = Instant::now();
    let mut text = false;
    while !text {
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "the text never came"
        );
        let _ = store.try_visit_chunks(at(far, far + 12), &mut |_| text = true);
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(250));
    let settled = wakes.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(wakes.load(Ordering::Relaxed), settled, "the wakes went on");
    drop(store);
    fs::remove_file(path).unwrap();
}
