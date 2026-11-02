//! Failures the M2 reviews proved by running code, plus exact-text checks that
//! guard the same code. All of these compile against the old API too.
use qe_api::testing::StringStore;
use qe_api::{ByteOffset, LineIndex, TextPosition, TextSnapshot, TextStore, Utf16Offset};
use qe_buffer::{PieceStore, UndoHistory};
use std::time::{Duration, Instant};

fn text_of(store: &dyn TextSnapshot) -> String {
    let mut text = String::new();
    assert!(store.visit_chunks(ByteOffset(0)..store.len_bytes(), &mut |s| text.push_str(s)));
    text
}

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

#[test]
fn item_12_empty_edit_keeps_redo_and_adds_no_group() {
    let mut store = PieceStore::new("abc");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(1, 1), "x", false).unwrap();
    history.undo(&mut store).unwrap();
    let groups = history.undo_groups();
    let revision = store.revision();
    history.replace(&mut store, at(1, 1), "", false).unwrap();
    assert_eq!(history.undo_groups(), groups, "an empty edit added a group");
    assert_eq!(
        store.revision(),
        revision,
        "an empty edit moved the revision"
    );
    history.redo(&mut store).unwrap();
    assert_eq!(text_of(&store), "axbc", "an empty edit destroyed redo");
}

#[test]
fn item_11_undo_after_a_direct_edit_refuses_and_changes_nothing() {
    let mut store = PieceStore::new("hello world");
    let mut history = UndoHistory::new();
    history
        .replace(&mut store, at(0, 5), "HOWDY", false)
        .unwrap();
    store.replace(at(6, 11), "there").unwrap();
    let before = text_of(&store);
    let groups = history.undo_groups();
    assert!(
        history.undo(&mut store).is_err(),
        "undo ran over an edit it did not record"
    );
    assert_eq!(text_of(&store), before);
    assert_eq!(
        history.undo_groups(),
        groups,
        "the failed undo lost its step"
    );
}

#[test]
fn item_11_undo_after_a_direct_edit_that_breaks_the_range_keeps_the_step() {
    let mut store = PieceStore::new("hello world");
    let mut history = UndoHistory::new();
    history
        .replace(&mut store, at(6, 11), "there", false)
        .unwrap();
    store.replace(at(0, 11), "x").unwrap();
    let groups = history.undo_groups();
    assert!(history.undo(&mut store).is_err());
    assert_eq!(text_of(&store), "x");
    assert_eq!(
        history.undo_groups(),
        groups,
        "the failed undo lost its step"
    );
}

#[test]
fn item_14_a_typed_run_reads_back_in_few_chunks() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    let count = 200_000;
    for i in 0..count {
        history.replace(&mut store, at(i, i), "x", true).unwrap();
    }
    let mut calls = 0;
    assert!(store.visit_chunks(at(0, count), &mut |_| calls += 1));
    assert!(calls < 200, "{calls} callbacks for {count} typed bytes");
}

#[test]
fn item_15_undo_and_the_next_key_press_do_not_replay_the_run() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    let count = 1_000_000;
    for i in 0..count {
        history.replace(&mut store, at(i, i), "x", true).unwrap();
    }
    let start = Instant::now();
    history.undo(&mut store).unwrap();
    let undo = start.elapsed();
    assert_eq!(store.len_bytes(), ByteOffset(0));
    let start = Instant::now();
    history.redo(&mut store).unwrap();
    let redo = start.elapsed();
    assert_eq!(store.len_bytes(), ByteOffset(count));
    history.undo(&mut store).unwrap();
    let start = Instant::now();
    history.replace(&mut store, at(0, 0), "y", true).unwrap();
    let key = start.elapsed();
    assert!(undo < Duration::from_millis(200), "undo {undo:?}");
    assert!(redo < Duration::from_millis(200), "redo {redo:?}");
    assert!(
        key < Duration::from_millis(30),
        "key press after undo {key:?}"
    );
}

#[test]
fn item_16_select_all_then_type_does_not_copy_the_selection() {
    let line = "the quick brown fox\n";
    let text = line.repeat(100_000_000 / line.len());
    let mut store = PieceStore::new(&text);
    drop(text);
    let mut history = UndoHistory::new();
    let end = store.len_bytes().0;
    let start = Instant::now();
    history.replace(&mut store, at(0, end), "x", true).unwrap();
    let key = start.elapsed();
    assert!(
        key < Duration::from_millis(25),
        "select all then type took {key:?}"
    );
    history.undo(&mut store).unwrap();
    assert_eq!(store.len_bytes(), ByteOffset(end));
    assert_eq!(store.line_count(), 100_000_000 / line.len() + 1);
}

#[test]
fn position_to_byte_matches_the_reference_on_every_line_and_column() {
    let text = "one\r\ntwo\u{2028}💡x\u{85}é\rlast\n";
    let store = PieceStore::new(text);
    let reference = StringStore::new(text);
    for line in 0..reference.line_count() + 1 {
        for column in 0..12 {
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

#[test]
fn chunk_text_is_exact_when_characters_cross_a_chunk_edge() {
    // Three-byte characters that start at every offset around the 4 KiB edge.
    for pad in 0..4 {
        let text = format!("{}{}", "a".repeat(pad), "語".repeat(3000));
        let store = PieceStore::new(&text);
        assert_eq!(text_of(&store), text, "pad {pad}");
        let mut pieces = 0;
        assert!(store.visit_chunks(at(pad + 3, text.len() - 3), &mut |_| pieces += 1));
        assert!(pieces > 1, "the range must cross a chunk edge");
    }
}

#[test]
fn undo_and_redo_restore_the_exact_text() {
    let mut store = PieceStore::new("alpha 語 beta\r\ngamma");
    let mut history = UndoHistory::new();
    let first = text_of(&store);
    history
        .replace(&mut store, at(6, 9), "💡💡", false)
        .unwrap();
    let second = text_of(&store);
    history.replace(&mut store, at(0, 5), "", false).unwrap();
    let third = text_of(&store);
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), second);
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), first);
    history.redo(&mut store).unwrap();
    assert_eq!(text_of(&store), second);
    history.redo(&mut store).unwrap();
    assert_eq!(text_of(&store), third);
}

#[cfg(windows)]
mod mapped {
    use super::*;
    use qe_buffer::MappedStore;
    use std::fs;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("qe-regress-{}-{name}", std::process::id()))
    }

    /// Real text with a line break every `line` characters.
    fn lines_of(c: char, line: usize, size: usize) -> String {
        let mut one: String = std::iter::repeat_n(c, line).collect();
        one.push('\n');
        one.repeat(size / one.len())
    }

    /// Stays above the store's file check interval, so the check has to run.
    fn after_check_interval() {
        std::thread::sleep(Duration::from_millis(60));
    }

    #[test]
    fn item_1_lookups_after_a_store_error_answer_at_once() {
        let limit = Duration::from_secs(2);
        for action in ["delete", "save-over", "truncate", "rename"] {
            let path = temp(&format!("item1-{action}.txt"));
            let renamed = temp(&format!("item1-{action}.moved"));
            let text = lines_of('x', 79, 2 << 20);
            fs::write(&path, &text).unwrap();
            let store = MappedStore::open(&path).unwrap();
            while !store.is_index_complete() {
                std::thread::sleep(Duration::from_millis(1));
            }
            match action {
                "delete" => fs::remove_file(&path).unwrap(),
                "save-over" => fs::write(&path, "saved\nover!!").unwrap(),
                "truncate" => fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(12)
                    .unwrap(),
                _ => fs::rename(&path, &renamed).unwrap(),
            }
            after_check_interval();
            for line in [1, 1000] {
                let start = Instant::now();
                let answer = store.line_range(LineIndex(line));
                let took = start.elapsed();
                assert!(took < limit, "{action}: line_range({line}) took {took:?}");
                assert_eq!(answer, None, "{action}: line {line}");
            }
            assert!(store.error_state().is_some(), "{action}");
            drop(store);
            fs::remove_file(&path).ok();
            fs::remove_file(&renamed).ok();
        }
    }

    #[test]
    fn item_1_save_over_a_small_file_gives_no_line() {
        let path = temp("item1-small.txt");
        fs::write(&path, "hello\nworld!").unwrap();
        let store = MappedStore::open(&path).unwrap();
        fs::write(&path, "saved\nover!!").unwrap();
        after_check_interval();
        assert_eq!(store.line_range(LineIndex(1)), None);
        drop(store);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn item_1_invalid_utf8_after_the_first_page_stops_the_search_at_once() {
        let path = temp("item1-bad.txt");
        let mut bytes = lines_of('x', 79, 1 << 20).into_bytes();
        bytes[300_000] = 0xFF;
        fs::write(&path, &bytes).unwrap();
        let store = MappedStore::open(&path).unwrap();
        let start = Instant::now();
        assert_eq!(store.line_range(LineIndex(1_000_000)), None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        assert!(store.error_state().is_some());
        drop(store);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn item_3_a_small_offset_does_not_wait_for_the_whole_index() {
        let path = temp("item3.txt");
        fs::write(&path, lines_of('語', 40, 512 << 20)).unwrap();
        let start = Instant::now();
        let store = MappedStore::open(&path).unwrap();
        let answer = store.utf16_to_byte(Utf16Offset(5));
        let took = start.elapsed();
        assert_eq!(answer, Some(ByteOffset(15)));
        assert!(
            took < Duration::from_millis(100),
            "open plus lookup took {took:?}"
        );
        drop(store);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn item_5_utf16_length_is_not_zero_after_an_error() {
        let path = temp("item5.txt");
        fs::write(&path, "hello\nworld!").unwrap();
        let store = MappedStore::open(&path).unwrap();
        fs::write(&path, "saved\nover!!").unwrap();
        after_check_interval();
        assert_eq!(store.line_range(LineIndex(1)), None);
        assert_ne!(
            store.len_utf16(),
            Utf16Offset(0),
            "an error looked like an empty file"
        );
        drop(store);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn item_6_a_snapshot_does_not_move() {
        let path = temp("item6.txt");
        fs::write(&path, lines_of('x', 1, 256 << 20)).unwrap();
        let store = MappedStore::open(&path).unwrap();
        let snapshot = TextStore::snapshot(&store);
        let first = snapshot.line_count();
        while !store.is_index_complete() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(snapshot.line_count(), first, "the snapshot moved");
        assert_eq!(store.line_count(), (256 << 20) / 2 + 1);
        drop(store);
        drop(snapshot);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn item_8_both_stores_treat_a_bad_range_the_same() {
        let path = temp("item8.txt");
        let text = "a💡b語c";
        fs::write(&path, text).unwrap();
        let mapped = MappedStore::open(&path).unwrap();
        let piece = PieceStore::new(text);
        let stores: [&dyn TextSnapshot; 2] = [&mapped, &piece];
        for store in stores {
            for start in 0..=text.len() {
                for end in 0..=text.len() + 1 {
                    let mut got = String::new();
                    let mut calls = 0;
                    let ok = store.visit_chunks(at(start, end), &mut |s| {
                        got.push_str(s);
                        calls += 1;
                    });
                    match text.get(start..end) {
                        Some(want) => {
                            assert!(ok, "{start}..{end}");
                            assert_eq!(got, want, "{start}..{end}");
                        }
                        None => {
                            assert!(!ok, "{start}..{end} was accepted");
                            assert_eq!(calls, 0, "{start}..{end} delivered text");
                        }
                    }
                }
            }
        }
        drop(mapped);
        fs::remove_file(&path).unwrap();
    }
}
