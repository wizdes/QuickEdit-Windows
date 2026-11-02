#![cfg(windows)]
//! The rules for errors, snapshots, partial answers and locks in `MappedStore`.
use qe_api::testing::StringStore;
use qe_api::{
    ByteOffset, LineIndex, LookupError, TextPosition, TextSnapshot, TextStore, Utf16Offset,
};
use qe_buffer::MappedStore;
use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn temp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qe-rules-{}-{name}", std::process::id()))
}

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

fn wait_for(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn lines_of(line: &str, size: usize) -> String {
    line.repeat(size / line.len())
}

fn past_check_interval() {
    std::thread::sleep(Duration::from_millis(60));
}

/// Every lookup route on a store whose whole index is in place.
fn compare_everything(store: &dyn TextSnapshot, text: &str, bytes: &[usize], units: &[usize]) {
    let reference = StringStore::new(text);
    assert_eq!(store.len_bytes(), reference.len_bytes());
    assert_eq!(store.len_utf16(), reference.len_utf16());
    assert_eq!(store.line_count(), reference.line_count());
    assert_eq!(store.try_line_count(), Ok(reference.line_count()));
    assert_eq!(store.try_len_utf16(), Ok(reference.len_utf16()));
    for &byte in bytes {
        assert_eq!(
            store.byte_to_utf16(ByteOffset(byte)),
            reference.byte_to_utf16(ByteOffset(byte)),
            "byte_to_utf16({byte})"
        );
        assert_eq!(
            store.byte_to_position(ByteOffset(byte)),
            reference.byte_to_position(ByteOffset(byte)),
            "byte_to_position({byte})"
        );
    }
    for &unit in units {
        assert_eq!(
            store.utf16_to_byte(Utf16Offset(unit)),
            reference.utf16_to_byte(Utf16Offset(unit)),
            "utf16_to_byte({unit})"
        );
    }
    for line in 0..=reference.line_count() {
        let range = store.line_range(LineIndex(line));
        assert_eq!(range, reference.line_range(LineIndex(line)), "line {line}");
        for column in [0, 1, 2, 3, 40, 1_000_000] {
            let position = TextPosition {
                line: LineIndex(line),
                column: Utf16Offset(column),
            };
            assert_eq!(
                store.position_to_byte(position),
                reference.position_to_byte(position),
                "line {line}, column {column}"
            );
        }
    }
}

fn open_complete(path: &PathBuf) -> MappedStore {
    let store = MappedStore::open(path).unwrap();
    assert!(wait_for(Duration::from_secs(60), || store.is_index_complete()));
    store
}

#[test]
fn every_route_matches_the_reference_on_a_short_mixed_file() {
    let path = temp("short.txt");
    let text = "one\r\ntwo\u{2028}💡x\u{85}é\rlast\n\n\r\nend";
    fs::write(&path, text).unwrap();
    let store = open_complete(&path);
    let bytes: Vec<usize> = (0..=text.len() + 1).collect();
    let units: Vec<usize> = (0..=text.encode_utf16().count() + 1).collect();
    compare_everything(&store, text, &bytes, &units);
    drop(store);
    fs::remove_file(path).unwrap();
}

/// Text in which characters, CRLF pairs and special breaks fall on page and scan
/// edges. Page edges are 64 KiB apart, and a scan is 4 MiB.
#[test]
fn every_route_matches_the_reference_across_page_and_scan_edges() {
    const PAGE: usize = 64 * 1024;
    const SCAN: usize = 4 * 1024 * 1024;
    for pad in 0..3 {
        for (name, tail) in [("crlf", "\r\n"), ("sep", "\u{2028}"), ("cr", "\rz")] {
            let mut text = String::with_capacity(SCAN + PAGE * 3);
            text.push_str(&"a".repeat(pad));
            while text.len() < SCAN + PAGE * 2 {
                let page_left = PAGE - text.len() % PAGE;
                if page_left <= 4 {
                    text.push_str(tail);
                } else if text.len().is_multiple_of(97) {
                    text.push('\n');
                } else {
                    text.push('語');
                }
            }
            let path = temp(&format!("edges-{pad}-{name}.txt"));
            fs::write(&path, &text).unwrap();
            let store = open_complete(&path);
            assert!(store.error_state().is_none(), "{:?}", store.error_state());
            let mut bytes = Vec::new();
            for edge in [PAGE, 2 * PAGE, SCAN, SCAN + PAGE, text.len()] {
                bytes.extend(edge.saturating_sub(4)..=(edge + 4).min(text.len()));
            }
            let reference = StringStore::new(text.as_str());
            let total = reference.len_utf16().0;
            let mut units = vec![0, 1, 2, total - 1, total, total + 1];
            for &byte in &bytes {
                if let Some(u) = reference.byte_to_utf16(ByteOffset(byte)) {
                    units.extend([u.0, u.0 + 1]);
                }
            }
            for &byte in &bytes {
                assert_eq!(
                    store.byte_to_utf16(ByteOffset(byte)),
                    reference.byte_to_utf16(ByteOffset(byte)),
                    "{name} pad {pad} byte {byte}"
                );
            }
            // The reference is slow on long text, so this samples around each edge.
            for &unit in units.iter().step_by(4) {
                assert_eq!(
                    store.utf16_to_byte(Utf16Offset(unit)),
                    reference.utf16_to_byte(Utf16Offset(unit)),
                    "{name} pad {pad} unit {unit}"
                );
            }
            let lines = reference.line_count();
            for &line in &[0, 1, lines / 2, lines - 2, lines - 1, lines] {
                assert_eq!(
                    store.line_range(LineIndex(line)),
                    reference.line_range(LineIndex(line)),
                    "{name} pad {pad} line {line}"
                );
            }
            assert_eq!(store.line_count(), lines, "{name} pad {pad}");
            assert_eq!(store.len_utf16(), reference.len_utf16(), "{name} pad {pad}");
            let mut read = String::with_capacity(text.len());
            assert!(store.visit_chunks(at(0, text.len()), &mut |s| read.push_str(s)));
            assert!(read == text, "{name} pad {pad}: text differs");
            drop(store);
            fs::remove_file(path).unwrap();
        }
    }
}

#[test]
fn position_past_the_end_of_a_line_has_no_byte() {
    let path = temp("column.txt");
    fs::write(&path, "ab\ncd").unwrap();
    let store = open_complete(&path);
    let position = |line, column| TextPosition {
        line: LineIndex(line),
        column: Utf16Offset(column),
    };
    assert_eq!(store.position_to_byte(position(0, 3)), Some(ByteOffset(3)));
    assert_eq!(store.position_to_byte(position(0, 4)), None);
    assert_eq!(
        store.try_position_to_byte(position(0, 4)),
        Err(LookupError::OutOfRange)
    );
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn every_query_reports_a_changed_file() {
    let path = temp("failed.txt");
    fs::write(&path, "hello\nworld!").unwrap();
    let store = MappedStore::open(&path).unwrap();
    fs::write(&path, "saved\nover!!").unwrap();
    assert!(store.check_file().is_err());
    let failed =
        |e: LookupError| matches!(e, LookupError::Failed(f) if f.valid_up_to == ByteOffset(0));
    assert!(failed(store.try_len_bytes().unwrap_err()));
    assert!(failed(store.try_len_utf16().unwrap_err()));
    assert!(failed(store.try_line_count().unwrap_err()));
    assert!(failed(store.try_byte_to_utf16(ByteOffset(0)).unwrap_err()));
    assert!(failed(store.try_byte_to_utf16(ByteOffset(1)).unwrap_err()));
    assert!(failed(store.try_utf16_to_byte(Utf16Offset(0)).unwrap_err()));
    assert!(failed(store.try_utf16_to_byte(Utf16Offset(1)).unwrap_err()));
    assert!(failed(store.try_line_range(LineIndex(0)).unwrap_err()));
    assert!(failed(
        store.try_byte_to_position(ByteOffset(0)).unwrap_err()
    ));
    assert!(failed(
        store
            .try_position_to_byte(TextPosition {
                line: LineIndex(0),
                column: Utf16Offset(0),
            })
            .unwrap_err()
    ));
    assert!(failed(
        store.try_visit_chunks(at(0, 5), &mut |_| {}).unwrap_err()
    ));
    assert_eq!(store.byte_to_utf16(ByteOffset(1)), None);
    assert_eq!(store.utf16_to_byte(Utf16Offset(1)), None);
    assert_eq!(store.byte_to_position(ByteOffset(1)), None);
    assert_eq!(store.line_range(LineIndex(0)), None);
    assert!(!store.visit_chunks(at(0, 5), &mut |_| panic!("no text after a failure")));
    let status = store.status();
    assert!(status.failure.is_some());
    assert!(store.error_state().is_some());
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_bad_byte_keeps_lookups_for_the_valid_part() {
    let path = temp("bad-byte.txt");
    let line = "ascii and 語 text\n";
    let mut bytes = lines_of(line, 300_000).into_bytes();
    let bad = 200_000 / line.len() * line.len();
    bytes[bad] = 0xFF;
    fs::write(&path, &bytes).unwrap();
    let store = MappedStore::open(&path).unwrap();
    assert!(wait_for(Duration::from_secs(30), || store
        .error_state()
        .is_some()));
    let failure = store.status().failure.unwrap();
    assert_eq!(failure.valid_up_to, ByteOffset(bad));
    assert!(
        failure.message.contains(&bad.to_string()),
        "{}",
        failure.message
    );
    // Inside the valid part, every lookup keeps answering.
    assert_eq!(
        store.line_range(LineIndex(1)),
        Some(at(line.len(), 2 * line.len()))
    );
    assert!(store.byte_to_utf16(ByteOffset(bad)).is_some());
    assert_eq!(store.utf16_to_byte(Utf16Offset(1)), Some(ByteOffset(1)));
    let mut read = String::new();
    assert!(store.visit_chunks(at(0, line.len()), &mut |s| read.push_str(s)));
    assert_eq!(read, line);
    // Past it, each one says why not.
    assert!(matches!(
        store.try_byte_to_utf16(ByteOffset(bad + 1)),
        Err(LookupError::Failed(_))
    ));
    assert!(matches!(
        store.try_line_count(),
        Err(LookupError::Failed(_))
    ));
    assert!(matches!(store.try_len_utf16(), Err(LookupError::Failed(_))));
    assert!(matches!(
        store.try_line_range(LineIndex(1_000_000)),
        Err(LookupError::Failed(_))
    ));
    assert!(!store.visit_chunks(at(0, bad + 1), &mut |_| panic!(
        "range is past the bad byte"
    )));
    // A later file change takes the valid part away too.
    fs::write(&path, "x").unwrap();
    assert!(store.check_file().is_err());
    assert_eq!(store.status().failure.unwrap().valid_up_to, ByteOffset(0));
    assert_eq!(store.line_range(LineIndex(1)), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_bad_byte_in_the_first_page_refuses_to_open() {
    let path = temp("bad-start.txt");
    fs::write(&path, b"ok\xFFtext").unwrap();
    assert!(MappedStore::open(&path).is_err());
    fs::remove_file(path).unwrap();
}

#[test]
fn a_cut_character_at_the_end_of_the_file_is_a_failure() {
    let path = temp("cut-end.txt");
    let mut bytes = lines_of("xxxxxxxxx\n", 100_000).into_bytes();
    bytes.extend_from_slice(&"語".as_bytes()[..2]);
    fs::write(&path, &bytes).unwrap();
    let store = MappedStore::open(&path).unwrap();
    assert!(wait_for(Duration::from_secs(30), || store
        .error_state()
        .is_some()));
    assert_eq!(
        store.status().failure.unwrap().valid_up_to,
        ByteOffset(bytes.len() - 2)
    );
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn worker_failure_and_progress_reach_the_notify_callback() {
    let path = temp("notify.txt");
    let mut bytes = lines_of("xxxxxxxxx\n", 1 << 20).into_bytes();
    let bad = bytes.len() / 2;
    bytes[bad] = 0xFF;
    fs::write(&path, &bytes).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let store = MappedStore::open_with_notify(
        &path,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    assert!(wait_for(Duration::from_secs(30), || store
        .status()
        .failure
        .is_some()));
    assert!(wait_for(Duration::from_secs(5), || calls
        .load(Ordering::SeqCst)
        >= 1));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn offsets_past_the_index_say_not_yet_and_never_block() {
    let path = temp("not-yet.txt");
    let text = lines_of("語語語語語語語語語\n", 800 << 20);
    fs::write(&path, &text).unwrap();
    let size = text.len();
    drop(text);
    let store = MappedStore::open(&path).unwrap();
    let start = Instant::now();
    // In the first page: real answers at once.
    assert_eq!(store.try_utf16_to_byte(Utf16Offset(5)), Ok(ByteOffset(15)));
    assert_eq!(store.try_byte_to_utf16(ByteOffset(15)), Ok(Utf16Offset(5)));
    assert_eq!(store.try_line_range(LineIndex(1)), Ok(at(28, 56)));
    // Far away: an answer or `NotYetKnown`, and never a wait.
    let not_yet = |e: &LookupError| matches!(e, LookupError::NotYetKnown { .. });
    let mut saw_not_yet = false;
    if let Err(e) = store.try_byte_to_utf16(ByteOffset(size - 28)) {
        saw_not_yet |= not_yet(&e);
    }
    if let Err(e) = store.try_utf16_to_byte(Utf16Offset(size / 3 - 1)) {
        saw_not_yet |= not_yet(&e);
    }
    if let Err(e) = store.try_line_range(LineIndex(size / 28 - 1)) {
        saw_not_yet |= not_yet(&e);
    }
    if let Err(e) = store.try_line_count() {
        saw_not_yet |= not_yet(&e);
    }
    let took = start.elapsed();
    assert!(took < Duration::from_millis(200), "{took:?}");
    assert!(saw_not_yet || store.is_index_complete());
    // Past the end stays a wrong request, and the plain route waits for the worker.
    assert_eq!(
        store.try_byte_to_utf16(ByteOffset(size + 1)),
        Err(LookupError::OutOfRange)
    );
    assert_eq!(
        store.line_range(LineIndex(size / 28 - 1)),
        Some(at(size - 28, size))
    );
    assert_eq!(store.try_line_count(), Ok(size / 28 + 1));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_snapshot_is_stable_and_a_live_store_shows_its_revision_move() {
    let path = temp("stable.txt");
    let text = lines_of("x\n", 512 << 20);
    fs::write(&path, &text).unwrap();
    let total_lines = text.len() / 2 + 1;
    drop(text);
    let store = MappedStore::open(&path).unwrap();
    let old = store.snapshot();
    let (lines, units, revision) = (old.line_count(), old.len_utf16(), old.revision());
    let status = old.status();
    let live_start = store.revision();
    assert!(wait_for(Duration::from_secs(120), || store.is_index_complete()));
    assert_eq!(old.line_count(), lines, "the snapshot moved");
    assert_eq!(old.len_utf16(), units);
    assert_eq!(old.revision(), revision);
    assert_eq!(old.status(), status);
    assert!(!status.complete);
    assert!(
        store.revision() > live_start,
        "the live store's revision moves"
    );
    assert_eq!(store.line_count(), total_lines);
    // Past its pinned part a snapshot answers "not yet", even with the index done.
    let far = LineIndex(total_lines - 1);
    assert!(matches!(
        old.try_line_range(far),
        Err(LookupError::NotYetKnown { .. })
    ));
    assert_eq!(old.line_range(far), None, "a snapshot never waits");
    assert_eq!(old.try_line_range(LineIndex(1)), Ok(at(2, 4)));
    assert!(matches!(
        old.try_line_count(),
        Err(LookupError::NotYetKnown { .. })
    ));
    // A new snapshot sees it all.
    let fresh = store.snapshot();
    assert_eq!(fresh.try_line_count(), Ok(total_lines));
    assert!(fresh.line_range(far).is_some());
    drop(store);
    drop(old);
    drop(fresh);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_same_size_rewrite_that_restores_the_times_is_detected() {
    let path = temp("same-size.txt");
    fs::write(&path, "hello world\n").unwrap();
    let store = MappedStore::open(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let created = fs::metadata(&path).unwrap().created().unwrap();
    // Time stamps need a moment to differ at all.
    std::thread::sleep(Duration::from_millis(20));
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    use std::os::windows::fs::FileTimesExt;
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(modified)
            .set_created(created),
    )
    .unwrap();
    use std::io::Write;
    (&file).write_all(b"HELLO WORLD\n").unwrap();
    file.sync_all().unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(modified)
            .set_created(created),
    )
    .unwrap();
    drop(file);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(fs::metadata(&path).unwrap().len(), 12);
    assert!(
        !store.visit_chunks(at(0, 5), &mut |_| panic!("the old page came back")),
        "the change time must catch this"
    );
    assert!(store.error_state().is_some());
    drop(store);
    fs::remove_file(path).unwrap();
}

/// This writes down the limit in the README: a program that also forges the change
/// time goes unnoticed, and a read can then give new bytes under the old index.
#[test]
fn a_rewrite_that_forges_the_change_time_is_not_detected() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
    };
    let path = temp("forged.txt");
    let page = 64 * 1024;
    let mut text = "a".repeat(page * 2);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(7)
        .open(&path)
        .unwrap();
    let handle = HANDLE(file.as_raw_handle());
    let mut basic = FILE_BASIC_INFO::default();
    let size = size_of::<FILE_BASIC_INFO>() as u32;
    // SAFETY: the handle is open and `basic` is a live value of the right size.
    unsafe {
        GetFileInformationByHandleEx(handle, FileBasicInfo, (&raw mut basic).cast(), size).unwrap();
    }
    use std::io::{Seek, SeekFrom, Write};
    (&file).seek(SeekFrom::Start(page as u64)).unwrap();
    (&file).write_all(&b"b".repeat(page)).unwrap();
    file.sync_all().unwrap();
    // SAFETY: the same as above; the call only reads from `basic`.
    unsafe {
        SetFileInformationByHandle(handle, FileBasicInfo, (&raw const basic).cast(), size).unwrap();
    }
    drop(file);
    past_check_interval();
    let mut read = String::new();
    let ok = store.visit_chunks(at(page, page + 4), &mut |s| read.push_str(s));
    if ok {
        assert_eq!(read, "bbbb", "the limit: new bytes under the old index");
        assert!(store.error_state().is_none());
    } else {
        // If NTFS moves a time we did not restore, the store is safer than documented.
        assert!(store.error_state().is_some());
    }
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn dropping_the_store_stops_the_worker_and_closes_the_file() {
    let path = temp("stop.txt");
    fs::write(&path, lines_of("語語語語語語語語語\n", 1 << 30)).unwrap();
    let store = MappedStore::open(&path).unwrap();
    assert!(!store.is_index_complete(), "this test needs a slow index");
    let clone = store.clone();
    drop(store);
    std::thread::sleep(Duration::from_millis(20));
    let alone = || {
        OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .is_ok()
    };
    assert!(!clone.is_index_complete(), "a clone keeps the worker going");
    assert!(!alone(), "a clone keeps the file open");
    let start = Instant::now();
    drop(clone);
    let closed = wait_for(Duration::from_secs(1), alone);
    let took = start.elapsed();
    assert!(closed, "the file stayed open for {took:?}");
    eprintln!("the file closed {took:?} after the last store went away");
    fs::remove_file(path).unwrap();
}

#[test]
fn a_snapshot_still_answers_after_the_store_and_its_worker_are_gone() {
    let path = temp("stop-snapshot.txt");
    fs::write(
        &path,
        lines_of(
            "x
",
            1 << 20,
        ),
    )
    .unwrap();
    let store = MappedStore::open(&path).unwrap();
    let snapshot = store.snapshot();
    drop(store);
    assert_eq!(snapshot.try_line_range(LineIndex(1)), Ok(at(2, 4)));
    drop(snapshot);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_lookup_does_not_wait_while_the_worker_reads_and_scans() {
    let path = temp("no-wait.txt");
    let line = "語語語語語語語語語語語語語語語語語語語\n";
    fs::write(&path, lines_of(line, 1 << 30)).unwrap();
    let store = MappedStore::open(&path).unwrap();
    let mut slowest = Duration::ZERO;
    let mut rounds = 0;
    while !store.is_index_complete() && rounds < 2000 {
        let start = Instant::now();
        let _ = store.line_count();
        let _ = store.try_line_range(LineIndex(1)).unwrap();
        slowest = slowest.max(start.elapsed());
        rounds += 1;
    }
    drop(store);
    fs::remove_file(path).unwrap();
    eprintln!("slowest lookup while indexing: {slowest:?} over {rounds} rounds");
    assert!(
        slowest < Duration::from_millis(250),
        "a lookup took {slowest:?} while the worker ran"
    );
}

#[test]
fn line_lookups_read_at_most_two_pages() {
    let path = temp("reads.txt");
    let line = "the quick brown fox\n";
    let text = lines_of(line, 16 << 20);
    fs::write(&path, &text).unwrap();
    let lines = text.len() / line.len();
    drop(text);
    let store = open_complete(&path);
    let mut rng = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..200 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let n = rng as usize % lines;
        let before = store.lookup_disk_reads();
        assert_eq!(
            store.line_range(LineIndex(n)),
            Some(at(n * line.len(), (n + 1) * line.len()))
        );
        let reads = store.lookup_disk_reads() - before;
        assert!(reads <= 4, "line {n} made {reads} page reads");
    }
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn the_file_check_runs_at_most_once_per_interval_for_lookups() {
    let path = temp("checks.txt");
    fs::write(&path, lines_of("line\n", 1 << 20)).unwrap();
    let store = open_complete(&path);
    let before = store.file_checks();
    let start = Instant::now();
    let mut rounds = 0;
    while start.elapsed() < Duration::from_millis(100) {
        store.try_line_range(LineIndex(rounds % 1000)).unwrap();
        rounds += 1;
    }
    let checks = store.file_checks() - before;
    assert!(rounds > 100, "only {rounds} rounds");
    // 100 ms at 25 ms apart is 5, with room for a slow test machine.
    assert!(checks <= 8, "{checks} checks in {rounds} lookups");
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_replace_with_the_same_times_is_caught_by_the_file_id() {
    let path = temp("swap.txt");
    let other = temp("swap-other.txt");
    fs::write(&path, "old text\n").unwrap();
    fs::write(&other, "new text\n").unwrap();
    let store = MappedStore::open(&path).unwrap();
    fs::rename(&other, &path).unwrap();
    assert!(store.check_file().is_err());
    assert_eq!(store.line_range(LineIndex(0)), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_read_only_store_refuses_edits() {
    let path = temp("read-only.txt");
    fs::write(&path, "text").unwrap();
    let mut store = MappedStore::open(&path).unwrap();
    assert!(store.is_read_only());
    assert!(store.replace(at(0, 0), "x").is_err());
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn visit_chunks_checks_the_file_before_it_gives_text_from_the_cache() {
    let path = temp("visit-check.txt");
    fs::write(&path, "hello world\n").unwrap();
    let store = MappedStore::open(&path).unwrap();
    // This puts the page in the cache and makes a check just now, so only the
    // check at the start of the next visit can catch the rewrite.
    assert!(store.visit_chunks(at(0, 5), &mut |_| {}));
    fs::write(&path, "HELLO WORLD\n").unwrap();
    let mut calls = 0;
    assert!(!store.visit_chunks(at(0, 5), &mut |_| calls += 1));
    assert_eq!(calls, 0, "text from the old file went out");
    drop(store);
    fs::remove_file(path).unwrap();
}
