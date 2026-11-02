//! Faults the second M2 reviews proved by running code. These tests use only the
//! API that existed when the reviews ran, so they could fail on that code first.
use qe_api::{ByteOffset, EditError, TextSnapshot, TextStore};
use qe_buffer::{PieceStore, UndoHistory};

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

#[test]
fn item_1_a_new_history_does_not_call_unknown_text_saved() {
    let mut store = PieceStore::new("abc");
    let mut history = UndoHistory::new();
    history.save_point();
    store.replace(at(0, 0), "X").unwrap();
    assert!(
        !history.is_at_save_point(&store),
        "a direct edit before any record"
    );

    history.replace(&mut store, at(0, 0), "Y", false).unwrap();
    history.undo(&mut store).unwrap();
    assert!(
        !history.is_at_save_point(&store),
        "the text is Xabc, and the saved text was abc"
    );

    let other = PieceStore::new("something else");
    let fresh = UndoHistory::new();
    assert!(!fresh.is_at_save_point(&other), "an unrelated store");
}

#[test]
fn item_13_an_out_of_step_history_says_so_for_every_edit() {
    let mut store = PieceStore::new("a💡b");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "x", false).unwrap();
    store.replace(at(0, 0), "z").unwrap();
    assert_eq!(
        history.replace(&mut store, at(0, 0), "", false),
        Err(EditError::HistoryOutOfStep),
        "an empty edit"
    );
    assert_eq!(
        history.replace(&mut store, at(3, 4), "y", false),
        Err(EditError::HistoryOutOfStep),
        "a range inside a character"
    );
}

#[test]
fn item_4_one_edit_does_a_bounded_amount_of_eviction() {
    // Text of 200 MB makes the next edit's undo step far bigger than the limit.
    let mut big = PieceStore::new(&"y".repeat(200 << 20));
    let mut big_history = UndoHistory::with_limit(32 << 20);
    for _ in 0..200_000 {
        big_history.break_group();
        big_history.replace(&mut big, at(0, 0), "x", false).unwrap();
    }
    let before = big_history.undo_groups();
    let end = big.len_bytes().0;
    big_history
        .replace(&mut big, at(0, end), "z", false)
        .unwrap();
    let after = big_history.undo_groups();
    assert!(
        before + 1 - after <= 64,
        "one edit dropped {} groups",
        before + 1 - after
    );
}

#[test]
fn typing_over_a_selection_is_one_undo_step() {
    let mut store = PieceStore::new("hello world");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 5), "H", true).unwrap();
    history.replace(&mut store, at(1, 1), "i", true).unwrap();
    assert_eq!(history.undo_groups(), 1);
    history.undo(&mut store).unwrap();
    let mut text = String::new();
    assert!(store.visit_chunks(at(0, 11), &mut |s| text.push_str(s)));
    assert_eq!(text, "hello world");
}

#[test]
fn a_run_of_backspaces_is_one_undo_step() {
    let mut store = PieceStore::new("hello");
    let mut history = UndoHistory::new();
    for end in (1..=5).rev() {
        history
            .replace(&mut store, at(end - 1, end), "", true)
            .unwrap();
    }
    assert_eq!(store.len_bytes(), ByteOffset(0));
    assert_eq!(history.undo_groups(), 1);
    history.undo(&mut store).unwrap();
    let mut text = String::new();
    assert!(store.visit_chunks(at(0, 5), &mut |s| text.push_str(s)));
    assert_eq!(text, "hello");
    history.redo(&mut store).unwrap();
    assert_eq!(store.len_bytes(), ByteOffset(0));
}

#[cfg(windows)]
mod mapped {
    use super::*;
    use qe_api::{LineIndex, LookupError, TextPosition, Utf16Offset};
    use qe_buffer::MappedStore;
    use std::fs::{self, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn temp(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("qe-round3-{}-{name}", std::process::id()))
    }

    fn lines_of(line: &str, size: usize) -> String {
        line.repeat(size / line.len())
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

    #[test]
    fn item_2_an_outside_write_never_gives_a_wrong_answer() {
        let path = temp("outside.txt");
        let text = "a".repeat(1 << 20);
        fs::write(&path, &text).unwrap();
        let store = MappedStore::open(&path).unwrap();
        assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
        let target = 262_144;
        // Two ASCII bytes become one two-byte character, so UTF-16 offsets move.
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(target as u64 - 2)).unwrap();
        file.write_all("é".as_bytes()).unwrap();
        drop(file);
        let end = Instant::now() + Duration::from_secs(5);
        let mut failed = false;
        while Instant::now() < end {
            match store.try_byte_to_utf16(ByteOffset(target)) {
                Ok(answer) => assert_eq!(answer, Utf16Offset(target), "a wrong answer"),
                Err(LookupError::Failed(_)) => {
                    failed = true;
                    break;
                }
                Err(LookupError::NotYetKnown { .. }) => {}
                Err(e) => panic!("{e:?}"),
            }
            std::thread::yield_now();
        }
        assert!(failed, "the change was never reported");
        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn item_5_each_revision_stands_for_one_index_state() {
        let path = temp("revision.txt");
        fs::write(&path, lines_of("語語語語語語語\n", 512 << 20)).unwrap();
        let store = MappedStore::open(&path).unwrap();
        let mut seen = std::collections::HashMap::new();
        while !store.is_index_complete() {
            let snapshot = store.snapshot();
            let known = snapshot.status().known_bytes;
            let old = seen.insert(snapshot.revision(), known);
            if let Some(old) = old {
                assert_eq!(
                    old,
                    known,
                    "revision {} had two states",
                    snapshot.revision()
                );
            }
        }
        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn item_6_a_chunk_read_stays_inside_a_snapshot() {
        let path = temp("pinned.txt");
        fs::write(&path, lines_of("line of text\n", 64 << 20)).unwrap();
        let store = MappedStore::open(&path).unwrap();
        let snapshot = store.snapshot();
        let pinned = snapshot.status().known_bytes.0;
        let size = snapshot.len_bytes().0;
        assert!(pinned < size);
        let mut calls = 0;
        assert!(matches!(
            snapshot.try_visit_chunks(at(size - 4, size), &mut |_| calls += 1),
            Err(LookupError::NotYetKnown { .. })
        ));
        assert!(!snapshot.visit_chunks(at(size - 4, size), &mut |_| calls += 1));
        assert_eq!(calls, 0);
        drop(store);
        drop(snapshot);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn item_7_a_known_position_on_an_unfinished_line_resolves() {
        let path = temp("long-line.txt");
        fs::write(&path, "x".repeat(256 << 20)).unwrap();
        let store = MappedStore::open(&path).unwrap();
        let snapshot = store.snapshot();
        let position = TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(5),
        };
        assert_eq!(snapshot.try_byte_to_position(ByteOffset(5)), Ok(position));
        assert_eq!(snapshot.try_position_to_byte(position), Ok(ByteOffset(5)));
        let start = Instant::now();
        assert_eq!(store.position_to_byte(position), Some(ByteOffset(5)));
        assert!(start.elapsed() < Duration::from_millis(100));
        drop(store);
        drop(snapshot);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn item_8_the_index_never_grows_under_the_lock() {
        for size in [64usize << 20, 128 << 20] {
            let path = temp(&format!("reserve-{size}.txt"));
            fs::write(&path, lines_of("吾輩は猫である。名前はまだ無い。\n", size)).unwrap();
            let store = MappedStore::open(&path).unwrap();
            let first = store.index_memory_bytes();
            assert!(wait_for(Duration::from_secs(60), || store.is_index_complete()));
            assert_eq!(store.index_memory_bytes(), first, "{size} bytes of text");
            drop(store);
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn item_9_a_short_lock_is_not_a_failure_and_check_file_keeps_failures() {
        let path = temp("locked.txt");
        fs::write(&path, lines_of("some text here\n", 1 << 20)).unwrap();
        let store = MappedStore::open(&path).unwrap();
        assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
        let far = LineIndex(((1 << 20) - 1000) / 15);
        let locker = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        locker.lock().unwrap();
        let during = store.try_line_range(far);
        std::thread::sleep(Duration::from_millis(100));
        let during_later = store.try_line_range(far);
        assert!(store.error_state().is_none(), "{during:?} {during_later:?}");
        locker.unlock().unwrap();
        drop(locker);
        assert!(store.line_range(far).is_some());
        assert_eq!(store.error_state(), None);

        fs::write(&path, "changed").unwrap();
        assert!(store.check_file().is_err());
        // The file looks fine to a new check only if it comes back; the store must
        // still report what it stored.
        assert!(store.check_file().is_err());
        drop(store);
        fs::remove_file(&path).unwrap();

        let bad = temp("locked-bad.txt");
        let mut bytes = lines_of("some text here\n", 1 << 20).into_bytes();
        bytes[500_000] = 0xFF;
        fs::write(&bad, &bytes).unwrap();
        let store = MappedStore::open(&bad).unwrap();
        assert!(wait_for(Duration::from_secs(30), || store
            .error_state()
            .is_some()));
        assert!(
            store.check_file().is_err(),
            "check_file hid the stored failure"
        );
        drop(store);
        fs::remove_file(bad).unwrap();
    }

    #[test]
    fn item_10_a_relative_path_survives_a_folder_change() {
        let name = format!("qe-round3-{}-relative.txt", std::process::id());
        let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        fs::write(folder.join(&name), "text\n").unwrap();
        std::env::set_current_dir(&folder).unwrap();
        let store = MappedStore::open(&name).unwrap();
        std::env::set_current_dir(folder.parent().unwrap()).unwrap();
        assert_eq!(store.check_file(), Ok(()));
        assert!(store.visit_chunks(at(0, 4), &mut |_| {}));
        drop(store);
        fs::remove_file(folder.join(name)).unwrap();
    }

    #[test]
    fn item_11_a_visit_past_the_worker_does_not_stop_the_index() {
        let path = temp("visit-bad.txt");
        let line = "x".repeat(99) + "\n";
        let mut bytes = lines_of(&line, 256 << 20).into_bytes();
        let bad = 200 << 20;
        bytes[bad] = 0xFF;
        fs::write(&path, &bytes).unwrap();
        drop(bytes);
        let store = MappedStore::open(&path).unwrap();
        let mut failed_visit = false;
        for _ in 0..1000 {
            if !store.visit_chunks(at(bad - 10, bad + 1), &mut |_| {}) {
                failed_visit = true;
                break;
            }
        }
        assert!(failed_visit);
        assert!(wait_for(Duration::from_secs(60), || store
            .status()
            .known_bytes
            .0
            >= bad));
        assert_eq!(store.status().known_bytes, ByteOffset(bad));
        assert_eq!(
            store.byte_to_utf16(ByteOffset(bad - 40)),
            Some(Utf16Offset(bad - 40))
        );
        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn item_12_notify_runs_only_between_open_and_the_last_drop() {
        let path = temp("notify-small.txt");
        fs::write(&path, "small file\n").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let store = MappedStore::open_with_notify(
            &path,
            Arc::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a call for work open did itself"
        );
        drop(store);
        fs::remove_file(&path).unwrap();

        // Page loads for a snapshot after the last live store is gone give no calls.
        let path = temp("notify-big.txt");
        let line = "line\n";
        fs::write(&path, lines_of(line, 8 << 20)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let store = MappedStore::open_with_notify(
            &path,
            Arc::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        assert!(wait_for(Duration::from_secs(30), || store.is_index_complete()));
        let snapshot = store.snapshot();
        drop(store);
        // One call that had already started may still end.
        std::thread::sleep(Duration::from_millis(50));
        let settled = calls.load(Ordering::SeqCst);
        let far = LineIndex(1_000_000);
        assert!(snapshot.line_range(far).is_some());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            settled,
            "a call after the last drop"
        );
        drop(snapshot);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn utf8_bom_stays_in_the_text_as_one_character() {
        let path = temp("bom.txt");
        fs::write(&path, "\u{feff}first\nsecond").unwrap();
        let store = MappedStore::open(&path).unwrap();
        assert_eq!(store.line_range(LineIndex(0)), Some(at(0, 9)));
        assert_eq!(store.byte_to_utf16(ByteOffset(3)), Some(Utf16Offset(1)));
        drop(store);
        fs::remove_file(path).unwrap();
    }
}
