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

/// File times move in steps. A same-size rewrite in the step of the last write can
/// leave every time alone, which the README states as a limit. Tests about what follows
/// a rewrite that can be seen wait for a later step.
fn past_time_step() {
    std::thread::sleep(Duration::from_millis(40));
}

fn lines_of(line: &str, size: usize) -> String {
    line.repeat(size / line.len())
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
    past_time_step();
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

fn counted_open(path: &PathBuf) -> (MappedStore, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let store = MappedStore::open_with_notify(
        path,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    (store, calls)
}

#[test]
fn index_progress_reaches_the_notify_callback() {
    let path = temp("notify-grow.txt");
    fs::write(&path, lines_of("xxxxxxxxx\n", 1 << 20)).unwrap();
    let (store, calls) = counted_open(&path);
    assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
    assert!(wait_for(Duration::from_secs(5), || calls
        .load(Ordering::SeqCst)
        >= 1));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_failure_after_the_index_is_done_reaches_the_notify_callback() {
    let path = temp("notify-fail.txt");
    fs::write(&path, lines_of("xxxxxxxxx\n", 1 << 20)).unwrap();
    let (store, calls) = counted_open(&path);
    assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
    std::thread::sleep(Duration::from_millis(50));
    let first = calls.load(Ordering::SeqCst);
    fs::write(&path, "changed").unwrap();
    store.request_file_check();
    assert!(wait_for(Duration::from_secs(5), || store
        .error_state()
        .is_some()));
    assert!(
        wait_for(Duration::from_secs(5), || calls.load(Ordering::SeqCst)
            > first),
        "the failure gave no notice"
    );
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_page_load_reaches_the_notify_callback() {
    let path = temp("notify-page.txt");
    let line = "xxxxxxxxx\n";
    fs::write(&path, lines_of(line, 1 << 20)).unwrap();
    let (store, calls) = counted_open(&path);
    assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
    std::thread::sleep(Duration::from_millis(50));
    let first = calls.load(Ordering::SeqCst);
    let far = LineIndex(90_000);
    assert!(matches!(
        store.try_line_range(far),
        Err(LookupError::NotYetKnown { .. })
    ));
    assert!(wait_for(Duration::from_secs(5), || calls
        .load(Ordering::SeqCst)
        > first));
    assert_eq!(store.try_line_range(far), Ok(at(900_000, 900_010)));
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
    past_time_step();
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

/// Rewrites a part of the file and puts every time back, so the file check sees
/// nothing at all.
fn forge_rewrite(path: &PathBuf, at_byte: usize, new_bytes: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(7)
        .open(path)
        .unwrap();
    let handle = HANDLE(file.as_raw_handle());
    let mut basic = FILE_BASIC_INFO::default();
    let size = size_of::<FILE_BASIC_INFO>() as u32;
    // SAFETY: the handle is open and `basic` is a live value of the right size.
    unsafe {
        GetFileInformationByHandleEx(handle, FileBasicInfo, (&raw mut basic).cast(), size).unwrap();
    }
    (&file).seek(SeekFrom::Start(at_byte as u64)).unwrap();
    (&file).write_all(new_bytes).unwrap();
    file.sync_all().unwrap();
    // SAFETY: the same as above; the call only reads from `basic`.
    unsafe {
        SetFileInformationByHandle(handle, FileBasicInfo, (&raw const basic).cast(), size).unwrap();
    }
}

#[test]
fn a_forged_rewrite_of_a_page_not_in_memory_fails_on_its_hash() {
    let path = temp("forged.txt");
    let page = 64 * 1024;
    let mut text = "a".repeat(page * 3);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    forge_rewrite(&path, page, &b"b".repeat(page));
    assert_eq!(
        store.check_file(),
        Ok(()),
        "the forged times must pass the file check"
    );
    let mut read = String::new();
    assert!(!store.visit_chunks(at(page, page + 4), &mut |s| read.push_str(s)));
    assert_eq!(read, "", "changed bytes went out");
    let failure = store.status().failure.unwrap();
    assert!(failure.message.contains("differs"), "{}", failure.message);
    assert_eq!(failure.valid_up_to, ByteOffset(0));
    drop(store);
    fs::remove_file(path).unwrap();
}

/// This writes down the limit in the README: a forged rewrite of a page already in
/// memory goes unnoticed while that page stays there, and answers keep showing the
/// old bytes. They agree with the index, so they are old, not mixed.
#[test]
fn a_forged_rewrite_of_a_page_in_memory_is_the_stated_limit() {
    let path = temp("forged-cached.txt");
    let page = 64 * 1024;
    let mut text = "a".repeat(page * 3);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(page, page + 4), &mut |_| {}));
    forge_rewrite(&path, page, &b"b".repeat(page));
    assert_eq!(
        store.check_file(),
        Ok(()),
        "the forged times must pass the file check"
    );
    let mut read = String::new();
    assert_eq!(
        store.try_visit_chunks(at(page, page + 4), &mut |s| read.push_str(s)),
        Ok(())
    );
    assert_eq!(read, "aaaa");
    assert_eq!(store.error_state(), None);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn dropping_the_store_stops_the_worker_and_closes_the_file() {
    let path = temp("stop.txt");
    let size = 1 << 30;
    fs::write(&path, lines_of("語語語語語語語語語\n", size)).unwrap();
    let start = Instant::now();
    let store = MappedStore::open(&path).unwrap();
    // One whole 4 MiB scan gives this run's speed, whatever else the PC is doing.
    assert!(wait_for(Duration::from_secs(60), || {
        store.indexed_up_to().0 > 4 << 20
    }));
    let clone = store.clone();
    drop(store);
    let alone = || {
        OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .is_ok()
    };
    assert!(!clone.is_index_complete(), "a clone keeps the worker going");
    assert!(!alone(), "a clone keeps the file open");
    let done = clone.indexed_up_to().0;
    let per_byte = start.elapsed().as_secs_f64() / done as f64;
    let left = Duration::from_secs_f64(per_byte * (size - done) as f64);
    let drop_time = Instant::now();
    drop(clone);
    assert!(
        wait_for(Duration::from_secs(120), alone),
        "the file stayed open"
    );
    let took = drop_time.elapsed();
    eprintln!(
        "the file closed {took:?} after the last store went away; the rest of the index would take about {left:?}"
    );
    // The worker stops at the end of one scan, about 1/256 of the file. Had it gone on
    // to the end, this would take about as long as `left`. Both come from this run, so
    // a busy PC stretches them alike.
    assert!(
        took < left / 4,
        "closing took {took:?}, with about {left:?} of index left"
    );
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
    // This runs for the whole index, so it overlaps every time the index grows.
    let (mut rounds, mut slow, mut slowest) = (0u64, 0u64, Duration::ZERO);
    while !store.is_index_complete() {
        let start = Instant::now();
        let _ = store.line_count();
        let _ = store.try_line_range(LineIndex(1)).unwrap();
        let took = start.elapsed();
        rounds += 1;
        slow += u64::from(took > Duration::from_millis(5));
        slowest = slowest.max(took);
    }
    drop(store);
    fs::remove_file(path).unwrap();
    eprintln!("lookup while indexing: {slow} of {rounds} over 5 ms, slowest {slowest:?}");
    assert!(rounds > 1000, "only {rounds} rounds");
    assert!(slow * 100 <= rounds, "{slow} of {rounds} over 5 ms");
    // A busy PC can take a thread away for a while, so this limit is not tight.
    assert!(
        slowest < Duration::from_millis(100),
        "a lookup took {slowest:?}"
    );
}

#[test]
fn a_lookup_does_not_wait_for_a_slow_page_read() {
    let path = temp("slow-read.txt");
    let line = "xxxxxxxxx\n";
    fs::write(&path, lines_of(line, 8 << 20)).unwrap();
    let store = open_complete(&path);
    store.set_test_read_delay(Duration::from_millis(300));
    let far = LineIndex(500_000);
    let start = Instant::now();
    assert!(matches!(
        store.try_line_range(far),
        Err(LookupError::NotYetKnown { .. })
    ));
    let mut slowest = Duration::ZERO;
    while start.elapsed() < Duration::from_millis(200) {
        let lookup = Instant::now();
        // Page 0 is in memory, and this must get it while the other page loads.
        assert_eq!(store.try_line_range(LineIndex(1)), Ok(at(10, 20)));
        assert!(matches!(
            store.try_line_range(far),
            Err(LookupError::NotYetKnown { .. })
        ));
        slowest = slowest.max(lookup.elapsed());
    }
    assert!(
        slowest < Duration::from_millis(100),
        "a lookup took {slowest:?}"
    );
    store.set_test_read_delay(Duration::ZERO);
    assert!(store.line_range(far).is_some());
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn no_try_query_reads_the_disk_on_the_calling_thread() {
    let path = temp("no-disk.txt");
    let line = "語 line of text\n";
    fs::write(&path, lines_of(line, 8 << 20)).unwrap();
    let store = MappedStore::open(&path).unwrap();
    store.clear_disk_threads();
    let me = std::thread::current().id();
    let lines = (8 << 20) / line.len();
    // Past the file check interval, so a due check would run on this thread if wrong.
    std::thread::sleep(Duration::from_millis(40));
    for round in 0..200 {
        let n = round * 137 % lines;
        let byte = ByteOffset(n * line.len());
        let _ = store.try_line_range(LineIndex(n));
        let _ = store.try_known_line(LineIndex(n));
        let _ = store.try_byte_to_utf16(byte);
        let _ = store.try_utf16_to_byte(Utf16Offset(n * 15));
        let _ = store.try_byte_to_position(byte);
        let _ = store.try_position_to_byte(TextPosition {
            line: LineIndex(n),
            column: Utf16Offset(1),
        });
        let _ = store.try_visit_chunks(byte..ByteOffset(byte.0 + line.len()), &mut |_| {});
        let _ = store.try_line_ending_len(LineIndex(n));
        let _ = store.try_line_count();
        let _ = store.try_len_utf16();
        let _ = store.try_len_bytes();
        let _ = store.status();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
    let threads = store.disk_threads();
    assert!(
        !threads.contains(&me),
        "the calling thread touched the disk"
    );
    assert!(threads.len() >= 2, "reader and worker: {threads:?}");
    assert!(store.lookup_disk_reads() > 0, "a page should have loaded");
    assert!(store.file_checks() > 0, "a check should have run");
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_try_visit_gives_all_or_nothing_and_refuses_a_long_range() {
    let path = temp("try-visit.txt");
    let line = "語語語語語語語語語\n";
    fs::write(&path, lines_of(line, 8 << 20)).unwrap();
    let store = open_complete(&path);
    store.set_test_read_delay(Duration::from_millis(50));
    let far = (4 << 20) / line.len() * line.len();
    let mut calls = 0;
    assert!(matches!(
        store.try_visit_chunks(at(far, far + line.len()), &mut |_| calls += 1),
        Err(LookupError::NotYetKnown { .. })
    ));
    assert_eq!(calls, 0);
    let mut got = String::new();
    assert!(wait_for(Duration::from_secs(5), || {
        !matches!(
            store.try_visit_chunks(at(far, far + line.len()), &mut |_| {}),
            Err(LookupError::NotYetKnown { .. })
        )
    }));
    store
        .try_visit_chunks(at(far, far + line.len()), &mut |s| got.push_str(s))
        .unwrap();
    assert_eq!(got, line);
    // The same rule for a bad range as the plain route.
    assert_eq!(
        store.try_visit_chunks(at(far + 1, far + 3), &mut |_| panic!("no text")),
        Err(LookupError::OutOfRange)
    );
    assert_eq!(
        store.try_visit_chunks(at(far, far + 1), &mut |_| panic!("no text")),
        Err(LookupError::OutOfRange)
    );
    assert!(matches!(
        store.try_visit_chunks(at(0, 3 << 20), &mut |_| panic!("no text")),
        Err(LookupError::TooLong { .. })
    ));
    store.set_test_read_delay(Duration::ZERO);
    let mut stops = 0;
    assert!(store.visit_chunks_while(at(0, 3 << 20), &mut |_| {
        stops += 1;
        false
    }));
    assert_eq!(stops, 1, "the visit must stop when asked");
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_very_long_first_line_paints_before_the_index_ends() {
    let path = temp("long-first.txt");
    let size = 1 << 30;
    let mut text = "語".repeat(size / 3);
    text.push('\n');
    text.push_str("second");
    fs::write(&path, &text).unwrap();
    let total = text.len();
    drop(text);
    let start = Instant::now();
    let store = MappedStore::open(&path).unwrap();
    store.clear_disk_threads();
    let line = store.try_known_line(LineIndex(0)).unwrap();
    assert_eq!(line.range.start, ByteOffset(0));
    assert!(line.range.end.0 >= 60_000, "{:?}", line.range);
    let mut screen = String::new();
    store
        .try_visit_chunks(ByteOffset(0)..ByteOffset(3 * 4000), &mut |s| {
            screen.push_str(s)
        })
        .unwrap();
    let painted = start.elapsed();
    assert_eq!(screen, "語".repeat(4000));
    let known = store.try_known_line(LineIndex(0)).unwrap();
    if !store.is_index_complete() {
        assert!(!known.complete || known.range.end.0 == size / 3 * 3 + 1);
    }
    let position = TextPosition {
        line: LineIndex(0),
        column: Utf16Offset(100),
    };
    assert_eq!(store.try_position_to_byte(position), Ok(ByteOffset(300)));
    eprintln!("first screen on a very long first line: {painted:?}");
    // What this test proves is the work: the first screen came from memory, with no page
    // load and no disk touched on this thread. That holds on a busy PC. The time does
    // not: this takes about 2 ms, and a debug build next to a writer flushing 800 MB
    // measured 233 ms. So the 100 ms budget counts in an optimized build only.
    assert_eq!(
        store.lookup_disk_reads(),
        0,
        "a page load for the first screen"
    );
    assert!(
        !store.disk_threads().contains(&std::thread::current().id()),
        "the first screen touched the disk on this thread"
    );
    if cfg!(debug_assertions) {
        assert!(painted < Duration::from_secs(5), "a hang: {painted:?}");
    } else {
        assert!(painted < Duration::from_millis(100), "{painted:?}");
    }
    assert!(wait_for(Duration::from_secs(120), || store.is_index_complete()));
    let done = store.try_known_line(LineIndex(0));
    assert!(
        wait_for(Duration::from_secs(5), || {
            store.try_known_line(LineIndex(0)).is_ok()
        }),
        "{done:?}"
    );
    let done = store.try_known_line(LineIndex(0)).unwrap();
    assert!(done.complete);
    assert_eq!(done.range.end, ByteOffset(size / 3 * 3 + 1));
    assert_eq!(total, size / 3 * 3 + 7);
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_position_past_a_line_that_ends_is_out_of_range_while_the_index_runs() {
    let path = temp("short-then-long.txt");
    let mut text = String::from("ab\n");
    text.push_str(&"x".repeat(64 << 20));
    fs::write(&path, &text).unwrap();
    drop(text);
    let store = MappedStore::open(&path).unwrap();
    let position = |line, column| TextPosition {
        line: LineIndex(line),
        column: Utf16Offset(column),
    };
    assert_eq!(
        store.try_position_to_byte(position(0, 2)),
        Ok(ByteOffset(2))
    );
    assert_eq!(
        store.try_position_to_byte(position(0, 9)),
        Err(LookupError::OutOfRange)
    );
    assert_eq!(
        store.try_position_to_byte(position(1, 5)),
        Ok(ByteOffset(8))
    );
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_lock_that_lasts_past_the_limit_becomes_a_failure() {
    let path = temp("long-lock.txt");
    fs::write(&path, lines_of("some text here\n", 1 << 20)).unwrap();
    let store = open_complete(&path);
    store.set_test_lock_give_up(Duration::from_millis(50));
    let locker = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    locker.lock().unwrap();
    let far = LineIndex(60_000);
    let start = Instant::now();
    assert_eq!(store.line_range(far), None, "the plain query must end");
    assert!(start.elapsed() < Duration::from_secs(10));
    let failure = store.status().failure.unwrap();
    assert!(failure.message.contains("locked"), "{}", failure.message);
    locker.unlock().unwrap();
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn check_file_finds_a_change_and_try_queries_learn_of_it_without_waiting() {
    let path = temp("request-check.txt");
    fs::write(&path, "hello\nworld!").unwrap();
    let store = MappedStore::open(&path).unwrap();
    past_time_step();
    fs::write(&path, "saved\nover!!").unwrap();
    store.request_file_check();
    assert!(wait_for(Duration::from_secs(5), || matches!(
        store.try_line_range(LineIndex(1)),
        Err(LookupError::Failed(_))
    )));
    drop(store);
    fs::remove_file(path).unwrap();
}

fn random_mapped_text(next: &mut impl FnMut() -> usize, size: usize) -> String {
    let parts = [
        "x",
        "word ",
        "\n",
        "\r\n",
        "\r",
        "語語",
        "💡",
        "é",
        "\u{85}",
        "\u{2028}",
        "\u{2029}",
        "a long stretch of plain words on one line ",
    ];
    let mut text = String::with_capacity(size + 64);
    while text.len() < size {
        // Runs of one part make long lines and dense lines, and cross page edges.
        let part = parts[next() % parts.len()];
        for _ in 0..1 + next() % 300 {
            text.push_str(part);
        }
    }
    text
}

#[test]
fn random_files_match_the_reference_on_every_route() {
    for (seed, size) in [
        (0x1357_9bdf_0246_8ace_u64, 300_000),
        (0xfeed_face_cafe_beef, 5 << 20),
        (0x0bad_f00d_dead_10cc, 9 << 20),
    ] {
        let mut rng = seed;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng as usize
        };
        let text = random_mapped_text(&mut next, size);
        let path = temp(&format!("random-{seed:x}.txt"));
        fs::write(&path, &text).unwrap();
        let store = open_complete(&path);
        assert_eq!(store.error_state(), None);
        let reference = StringStore::new(text.as_str());
        assert_eq!(store.line_count(), reference.line_count());
        assert_eq!(store.len_utf16(), reference.len_utf16());
        let lines = reference.line_count();
        let units = reference.len_utf16().0;
        for _ in 0..60 {
            let byte = ByteOffset(next() % (text.len() + 2));
            assert_eq!(
                store.byte_to_utf16(byte),
                reference.byte_to_utf16(byte),
                "{byte:?}"
            );
            assert_eq!(
                store.byte_to_position(byte),
                reference.byte_to_position(byte),
                "{byte:?}"
            );
            let unit = Utf16Offset(next() % (units + 2));
            assert_eq!(
                store.utf16_to_byte(unit),
                reference.utf16_to_byte(unit),
                "{unit:?}"
            );
            let line = LineIndex(next() % (lines + 1));
            let range = store.line_range(line);
            assert_eq!(range, reference.line_range(line), "{line:?}");
            let position = TextPosition {
                line,
                column: Utf16Offset(next() % 50),
            };
            assert_eq!(
                store.position_to_byte(position),
                reference.position_to_byte(position),
                "{position:?}"
            );
            if let Some(range) = range {
                let len = store.try_line_ending_len(line);
                let wanted = {
                    let body = &text[range.start.0..range.end.0];
                    if body.ends_with("\r\n") {
                        2
                    } else if body.ends_with(['\r', '\n']) {
                        1
                    } else if body.ends_with('\u{85}') {
                        2
                    } else if body.ends_with(['\u{2028}', '\u{2029}']) {
                        3
                    } else {
                        0
                    }
                };
                assert!(
                    matches!(len, Err(LookupError::NotYetKnown { .. })) || len == Ok(wanted),
                    "{line:?}: {len:?}"
                );
            }
            let from = next() % text.len();
            let to = (from + next() % 200_000).min(text.len());
            let mut read = String::new();
            let ok = store.visit_chunks(at(from, to), &mut |s| read.push_str(s));
            match text.get(from..to) {
                Some(want) => {
                    assert!(ok, "{from}..{to}");
                    assert!(read == want, "{from}..{to}: the text differs");
                }
                None => assert!(!ok && read.is_empty(), "{from}..{to}"),
            }
        }
        let mut whole = String::with_capacity(text.len());
        assert!(store.visit_chunks(at(0, text.len()), &mut |s| whole.push_str(s)));
        assert!(whole == text, "the whole file differs");
        drop(store);
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn line_range_reads_at_most_four_pages() {
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
fn visit_chunks_checks_the_file_before_it_gives_any_text() {
    use std::io::{Seek, SeekFrom, Write};
    let path = temp("visit-check.txt");
    let page = 64 * 1024;
    let mut text = "a".repeat(page * 3);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(0, 5), &mut |_| {}));
    past_time_step();
    // The rewrite is in another page, so the hash of the page being read still fits.
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(2 * page as u64)).unwrap();
    file.write_all(b"b").unwrap();
    drop(file);
    let mut calls = 0;
    assert!(!store.visit_chunks(at(0, 5), &mut |_| calls += 1));
    assert_eq!(calls, 0, "text went out before the file check");
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_long_plain_chunk_read_meets_the_hash_of_a_middle_page_in_memory() {
    let path = temp("fresh-middle.txt");
    let page = 64 * 1024;
    let mut text = "a".repeat(page * 4);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    let range = at(page, 3 * page + 4);
    assert!(store.visit_chunks(range.clone(), &mut |_| {}));
    forge_rewrite(&path, 2 * page, &b"b".repeat(page));
    let mut read = String::new();
    assert!(!store.visit_chunks(range, &mut |s| read.push_str(s)));
    assert!(!read.contains('b'), "changed bytes went out");
    assert!(store.status().failure.is_some());
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_plain_chunk_read_of_a_page_in_memory_still_meets_its_hash() {
    let path = temp("fresh-read.txt");
    let page = 64 * 1024;
    let mut text = "a".repeat(page * 3);
    text.push('\n');
    fs::write(&path, &text).unwrap();
    let store = open_complete(&path);
    assert!(store.visit_chunks(at(page, page + 4), &mut |_| {}));
    forge_rewrite(&path, page, &b"b".repeat(page));
    let mut calls = 0;
    assert!(!store.visit_chunks(at(page, page + 4), &mut |_| calls += 1));
    assert_eq!(calls, 0, "old text went out");
    assert!(store.status().failure.is_some());
    drop(store);
    fs::remove_file(path).unwrap();
}
