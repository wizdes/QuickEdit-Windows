//! Undo, redo, saved state, groups and the tree's balance rule.
use qe_api::testing::StringStore;
use qe_api::{ByteOffset, EditError, LineIndex, TextSnapshot, TextStore};
use qe_buffer::{PieceStore, UndoHistory};
use std::time::{Duration, Instant};

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

fn text_of(store: &dyn TextSnapshot) -> String {
    let mut text = String::new();
    assert!(store.visit_chunks(ByteOffset(0)..store.len_bytes(), &mut |s| text.push_str(s)));
    text
}

#[test]
fn undo_and_redo_report_what_changed() {
    let mut store = PieceStore::new("one\ntwo\nthree");
    let mut history = UndoHistory::new();
    let first = text_of(&store);
    history
        .replace(&mut store, at(4, 7), "2\n2", false)
        .unwrap();
    let second = text_of(&store);
    let revision = store.revision();

    let changes = history.undo(&mut store).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].replaced, at(4, 7));
    assert_eq!(changes[0].new_len, 3);
    assert_eq!(changes[0].line_count_delta, -1);
    assert_eq!(changes[0].revision, revision + 1);
    assert_eq!(store.revision(), revision + 1);
    assert_eq!(text_of(&store), first);

    let changes = history.redo(&mut store).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].replaced, at(4, 7));
    assert_eq!(changes[0].line_count_delta, 1);
    assert_eq!(text_of(&store), second);

    history.undo(&mut store).unwrap();
    history.undo(&mut store).unwrap();
    assert!(
        history.undo(&mut store).unwrap().is_empty(),
        "nothing to undo"
    );
    history.redo(&mut store).unwrap();
    assert!(
        history.redo(&mut store).unwrap().is_empty(),
        "nothing to redo"
    );
}

#[test]
fn the_saved_state_follows_undo_and_redo() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::new();
    assert!(history.is_at_save_point(&store));
    history.replace(&mut store, at(4, 4), "1", true).unwrap();
    assert!(!history.is_at_save_point(&store));
    history.save_point();
    assert!(history.is_at_save_point(&store));
    history.replace(&mut store, at(5, 5), "2", true).unwrap();
    assert!(!history.is_at_save_point(&store));
    history.undo(&mut store).unwrap();
    assert!(history.is_at_save_point(&store), "back at the save point");
    history.undo(&mut store).unwrap();
    assert!(!history.is_at_save_point(&store), "before the save point");
    history.redo(&mut store).unwrap();
    assert!(history.is_at_save_point(&store));
    history.redo(&mut store).unwrap();
    assert!(!history.is_at_save_point(&store));
}

#[test]
fn a_save_point_on_lost_redo_can_never_come_back() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "a", false).unwrap();
    history.save_point();
    history.undo(&mut store).unwrap();
    history.replace(&mut store, at(0, 0), "a", false).unwrap();
    assert_eq!(text_of(&store), "a");
    assert!(
        !history.is_at_save_point(&store),
        "the check follows history and does not compare text"
    );
}

#[test]
fn a_direct_edit_stops_the_saved_state_and_reset_starts_again() {
    let mut store = PieceStore::new("text");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "a", false).unwrap();
    history.save_point();
    store.replace(at(0, 0), "b").unwrap();
    assert!(!history.is_at_save_point(&store));
    assert_eq!(
        history.replace(&mut store, at(0, 0), "c", false),
        Err(EditError::HistoryOutOfStep)
    );
    assert_eq!(history.undo(&mut store), Err(EditError::HistoryOutOfStep));
    assert_eq!(history.redo(&mut store), Err(EditError::HistoryOutOfStep));
    assert_eq!(text_of(&store), "batext");
    history.reset(&store, true);
    assert!(history.is_at_save_point(&store));
    assert_eq!(history.undo_groups(), 0);
    history.replace(&mut store, at(0, 0), "c", false).unwrap();
    assert_eq!(text_of(&store), "cbatext");
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), "batext");
}

#[test]
fn a_copy_of_the_store_cannot_be_undone_by_mistake() {
    let mut store = PieceStore::new("text");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "a", false).unwrap();
    let mut copy = store.clone();
    copy.replace(at(0, 0), "z").unwrap();
    assert_eq!(history.undo(&mut copy), Err(EditError::HistoryOutOfStep));
    let mut other = PieceStore::new("atext");
    assert_eq!(history.undo(&mut other), Err(EditError::HistoryOutOfStep));
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), "text");
}

#[test]
fn a_bad_range_changes_nothing_in_the_history() {
    let mut store = PieceStore::new("a💡b");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "x", false).unwrap();
    history.undo(&mut store).unwrap();
    let revision = store.revision();
    for bad in [at(2, 2), at(2, 3), at(9, 9), at(3, 1)] {
        assert_eq!(
            history.replace(&mut store, bad.clone(), "y", false),
            Err(EditError::InvalidRange)
        );
        assert_eq!(
            history.replace(&mut store, bad, "", false),
            Err(EditError::InvalidRange)
        );
    }
    assert_eq!(store.revision(), revision);
    assert_eq!(history.redo_groups(), 1, "redo must survive a bad range");
}

#[test]
fn an_empty_edit_keeps_a_typed_run_going() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "a", true).unwrap();
    let change = history.replace(&mut store, at(1, 1), "", true).unwrap();
    assert_eq!(change.revision, store.revision());
    assert_eq!(change.new_len, 0);
    history.replace(&mut store, at(1, 1), "b", true).unwrap();
    assert_eq!(history.undo_groups(), 1);
}

#[test]
fn a_direct_empty_edit_changes_nothing() {
    let mut store = PieceStore::new("abc");
    let revision = store.revision();
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "x", false).unwrap();
    let revision_after = store.revision();
    assert_eq!(revision_after, revision + 1);
    store.replace(at(1, 1), "").unwrap();
    assert_eq!(store.revision(), revision_after);
    history.undo(&mut store).unwrap();
    assert_eq!(
        text_of(&store),
        "abc",
        "an empty edit is not an outside edit"
    );
}

#[test]
fn a_group_is_one_undo_step_with_every_change_in_order() {
    let mut store = PieceStore::new("cat cat cat");
    let mut history = UndoHistory::new();
    let original = text_of(&store);
    history.begin_group();
    history.begin_group();
    // Replace All from the end, so that earlier offsets stay right.
    for start in [8, 4, 0] {
        history
            .replace(&mut store, at(start, start + 3), "horse", false)
            .unwrap();
    }
    history.end_group();
    history.replace(&mut store, at(0, 0), "", false).unwrap();
    history.end_group();
    let changed = text_of(&store);
    assert_eq!(changed, "horse horse horse");
    assert_eq!(history.undo_groups(), 1);

    let changes = history.undo(&mut store).unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(text_of(&store), original);
    assert_eq!(changes[0].replaced, at(0, 5));
    assert_eq!(changes[2].replaced, at(8, 13));
    assert_eq!(changes[0].new_len, 3);

    let changes = history.redo(&mut store).unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(text_of(&store), changed);

    history.replace(&mut store, at(0, 0), "z", false).unwrap();
    assert_eq!(history.undo_groups(), 2, "the group is closed");
}

#[test]
fn an_empty_group_adds_nothing() {
    let mut store = PieceStore::new("x");
    let mut history = UndoHistory::new();
    history.begin_group();
    history.end_group();
    history.end_group();
    assert_eq!(history.undo_groups(), 0);
    history.replace(&mut store, at(0, 0), "a", false).unwrap();
    history.replace(&mut store, at(0, 0), "b", false).unwrap();
    assert_eq!(history.undo_groups(), 2);
}

#[test]
fn the_size_limit_drops_the_oldest_but_keeps_the_newest() {
    let line = "0123456789".repeat(100);
    let mut store = PieceStore::new(&line.repeat(50));
    let mut history = UndoHistory::with_limit(10_000);
    for i in 0..20 {
        history
            .replace(&mut store, at(i, i + 500), "", false)
            .unwrap();
    }
    assert!(history.undo_groups() < 20, "old groups must go");
    assert!(
        history.memory_bytes() <= 10_000,
        "{}",
        history.memory_bytes()
    );

    // One edit bigger than the limit still stays undoable.
    let before = text_of(&store);
    let end = store.len_bytes().0;
    history.replace(&mut store, at(0, end), "x", false).unwrap();
    assert_eq!(history.undo_groups(), 1);
    assert!(history.memory_bytes() > 10_000);
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), before);
    assert_eq!(history.redo_groups(), 1);
}

#[test]
fn an_undo_run_is_one_record_and_costs_no_memory_per_key() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    history.replace(&mut store, at(0, 0), "x", true).unwrap();
    let one = history.memory_bytes();
    for i in 1..50_000 {
        history.replace(&mut store, at(i, i), "x", true).unwrap();
    }
    assert_eq!(
        history.memory_bytes(),
        one,
        "typing adds nothing to history"
    );
}

#[test]
fn a_key_press_after_undoing_many_steps_is_fast() {
    let mut store = PieceStore::new("start ");
    let mut history = UndoHistory::new();
    let count = 200_000;
    for i in 0..count {
        history.break_group();
        let end = store.len_bytes().0;
        history
            .replace(&mut store, at(end, end), "word ", i % 2 == 0)
            .unwrap();
    }
    for _ in 0..count {
        history.undo(&mut store).unwrap();
    }
    assert_eq!(history.redo_groups(), count);
    assert_eq!(text_of(&store), "start ");
    let start = Instant::now();
    history.replace(&mut store, at(0, 0), "y", true).unwrap();
    let key = start.elapsed();
    assert_eq!(history.redo_groups(), 0);
    assert!(key < Duration::from_millis(30), "key press took {key:?}");
    assert!(history.has_memory_to_release());
    history.release_memory();
    assert!(!history.has_memory_to_release());
}

fn random_run(seed: u64, steps: usize, keep: usize) {
    let mut rng = seed;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng as usize
    };
    let choices = [
        "x",
        "yz",
        "💡",
        "é",
        "\r",
        "\n",
        "\r\n",
        "\u{2028}",
        "\u{85}",
        "語語語",
    ];
    let mut store = PieceStore::new("");
    let mut reference = StringStore::new("");
    let mut history = UndoHistory::new();
    let mut undone: Vec<String> = Vec::new();
    let mut done: Vec<String> = vec![String::new()];
    for step in 0..steps {
        let text = reference.as_str().to_owned();
        match next() % 12 {
            0 if done.len() > 1 => {
                history.undo(&mut store).unwrap();
                undone.push(done.pop().unwrap());
                reference = StringStore::new(done.last().unwrap().clone());
            }
            1 if !undone.is_empty() => {
                history.redo(&mut store).unwrap();
                let back = undone.pop().unwrap();
                reference = StringStore::new(back.clone());
                done.push(back);
            }
            _ => {
                let edges: Vec<usize> = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain(std::iter::once(text.len()))
                    .collect();
                let a = next() % edges.len();
                let b = if next() % 3 == 0 && text.len() > keep {
                    (a + next() % 20).min(edges.len() - 1)
                } else {
                    a
                };
                let range = at(edges[a], edges[b]);
                let insert = if text.len() > keep {
                    ""
                } else {
                    choices[next() % choices.len()]
                };
                if range.start == range.end && insert.is_empty() {
                    continue;
                }
                history.break_group();
                let left = history.replace(&mut store, range.clone(), insert, false);
                let right = reference.replace(range, insert);
                assert_eq!(
                    left.unwrap().line_count_delta,
                    right.unwrap().line_count_delta
                );
                done.push(reference.as_str().to_owned());
                undone.clear();
            }
        }
        store
            .check_tree()
            .unwrap_or_else(|e| panic!("seed {seed:x}, step {step}: {e}"));
        assert_eq!(
            text_of(&store),
            reference.as_str(),
            "seed {seed:x}, step {step}"
        );
        assert_eq!(store.line_count(), reference.line_count());
        assert_eq!(store.len_utf16(), reference.len_utf16());
    }
}

#[test]
fn undo_and_redo_match_a_plain_string_and_the_tree_stays_balanced() {
    for seed in [
        0x1234_5678_9abc_def1,
        0xdead_beef_cafe_f00d,
        0x0f0f_0f0f_1357_9bdf,
    ] {
        random_run(seed, 4_000, 1_500);
    }
}

#[test]
fn the_balance_rule_holds_after_every_edit_at_scale() {
    let mut rng = 0xa5a5_5a5a_1234_4321_u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng as usize
    };
    // Big inserts make many leaves, and random positions used to unbalance them.
    let block = "abcdefghij".repeat(600);
    let mut store = PieceStore::new("");
    for step in 0..1_500 {
        let len = store.len_bytes().0;
        let pos = next() % (len + 1);
        match next() % 10 {
            0 if len > 0 => {
                let end = (pos + next() % 40_000).min(len);
                store.replace(at(pos, end), "").unwrap();
            }
            _ => {
                store.replace(at(pos, pos), &block).unwrap();
            }
        }
        store
            .check_tree()
            .unwrap_or_else(|e| panic!("step {step}: {e}"));
    }
    let shape = store.check_tree().unwrap();
    // The height of a tree that obeys the rule is below 1.45 log2(leaves + 2).
    let limit = (1.45 * ((shape.leaves + 2) as f64).log2()).ceil() as u32 + 1;
    assert!(
        shape.height <= limit,
        "height {} over limit {limit} for {} leaves",
        shape.height,
        shape.leaves
    );
    assert!(shape.leaves > 1000, "{} leaves", shape.leaves);
}

#[test]
fn a_join_of_very_different_heights_is_balanced() {
    let big = "b".repeat(8_000_000);
    let small = "s".repeat(5000);
    let mut store = PieceStore::new(&big);
    store.check_tree().unwrap();
    for pos in [0, 4096, 1_000_000, 8_000_000] {
        store.replace(at(pos, pos), &small).unwrap();
        store.check_tree().unwrap();
    }
    let len = store.len_bytes().0;
    store.replace(at(100, len - 100), "").unwrap();
    let shape = store.check_tree().unwrap();
    assert_eq!(store.len_bytes(), ByteOffset(200));
    assert_eq!(shape.leaves, 1, "small neighbours merge");
}

#[test]
fn lines_stay_right_when_a_crlf_crosses_merged_leaves() {
    let mut store = PieceStore::new("a\r");
    store.replace(at(2, 2), "\nb").unwrap();
    assert_eq!(store.line_count(), 2);
    assert_eq!(store.line_range(LineIndex(1)), Some(at(3, 4)));
    store.check_tree().unwrap();
    let mut pad = "x".repeat(4095);
    pad.push('\r');
    let mut store = PieceStore::new(&pad);
    store.replace(at(4096, 4096), "\n").unwrap();
    assert_eq!(store.check_tree().unwrap().leaves, 2);
    assert_eq!(store.line_count(), 2);
    assert_eq!(store.line_range(LineIndex(1)), Some(at(4097, 4097)));
}

#[test]
fn undo_text_stays_exact_across_a_leaf_edge() {
    let text = "語".repeat(5000);
    let mut store = PieceStore::new(&text);
    let mut history = UndoHistory::new();
    let start = 4095;
    history
        .replace(&mut store, at(start, start + 300), "💡", false)
        .unwrap();
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), text);
    store.check_tree().unwrap();
}
