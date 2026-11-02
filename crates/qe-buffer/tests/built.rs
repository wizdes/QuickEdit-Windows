//! Text that a worker cuts into a tree, and the history taking it without reading it again.
use qe_api::{ByteOffset, TextSnapshot, TextStore};
use qe_buffer::{BuiltText, PieceStore, TextBuilder, UndoHistory};

fn built(parts: &[&str]) -> BuiltText {
    let mut builder = TextBuilder::new();
    for part in parts {
        builder.push_str(part);
    }
    builder.finish()
}
fn text_of(store: &PieceStore) -> String {
    let mut text = String::new();
    assert!(store.visit_chunks(ByteOffset(0)..store.len_bytes(), &mut |s| text.push_str(s)));
    text
}

#[test]
fn parts_of_every_size_join_with_no_character_cut() {
    // A 3-byte character in parts of 1, 7, 4,095, 4,096 and 9,000 bytes' worth, so that each
    // kind of step meets a leaf's end at a different place in a character.
    let long = "\u{4e2d}".repeat(3_000);
    let mut expected = String::new();
    let mut builder = TextBuilder::new();
    for part in [
        "a",
        "\u{1f600}bc",
        &long,
        "x",
        &"y".repeat(4_095),
        &"z".repeat(4_096),
        &long,
    ] {
        builder.push_str(part);
        expected.push_str(part);
    }
    let text = builder.finish();
    assert_eq!(text.len(), expected.len());
    assert_eq!(text.to_text(), expected);
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::for_store(&store);
    history
        .replace_built(&mut store, ByteOffset(0)..ByteOffset(0), &text)
        .unwrap();
    assert_eq!(text_of(&store), expected);
    store.check_tree().unwrap();
}

#[test]
fn a_built_edit_reports_what_a_plain_one_does() {
    let start = "one\ntwo\r\nthree\n\u{1f600}four";
    let range = ByteOffset(2)..ByteOffset(14);
    let new_text = "\u{4e2d}\r\nmid\n";

    let mut plain = PieceStore::new(start);
    let mut plain_history = UndoHistory::for_store(&plain);
    let by_text = plain_history
        .replace_tracked(&mut plain, range.clone(), new_text, false)
        .unwrap();

    let mut store = PieceStore::new(start);
    let mut history = UndoHistory::for_store(&store);
    let by_tree = history
        .replace_built(&mut store, range, &built(&[new_text]))
        .unwrap();

    assert_eq!(by_tree, by_text);
    assert_eq!(by_tree.change.new_len, new_text.len());
    assert_eq!(text_of(&store), text_of(&plain));
    assert_eq!(store.line_count(), plain.line_count());
    assert_eq!(store.revision(), plain.revision());
}

#[test]
fn a_built_edit_is_one_undo_step_with_a_redo_and_a_clear_save_point() {
    let start = "abc def ghi";
    let mut store = PieceStore::new(start);
    let mut history = UndoHistory::for_store(&store);
    assert!(history.is_at_save_point(&store));
    history
        .replace_built(&mut store, ByteOffset(4)..ByteOffset(7), &built(&["XY"]))
        .unwrap();
    assert_eq!(history.undo_groups(), 1);
    assert!(!history.is_at_save_point(&store));
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), start);
    assert!(history.is_at_save_point(&store));
    history.redo(&mut store).unwrap();
    assert_eq!(text_of(&store), "abc XY ghi");
    store.check_tree().unwrap();
}

#[test]
fn a_built_edit_never_joins_the_typing_before_it() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::for_store(&store);
    history
        .replace(&mut store, ByteOffset(0)..ByteOffset(0), "a", true)
        .unwrap();
    history
        .replace_built(&mut store, ByteOffset(1)..ByteOffset(1), &built(&["b"]))
        .unwrap();
    history
        .replace(&mut store, ByteOffset(2)..ByteOffset(2), "c", true)
        .unwrap();
    assert_eq!(history.undo_groups(), 3);
}

#[test]
fn a_built_edit_refuses_a_bad_range_and_a_store_edited_behind_the_history() {
    let mut store = PieceStore::new("\u{4e2d}abc");
    let mut history = UndoHistory::for_store(&store);
    let text = built(&["x"]);
    assert!(
        history
            .replace_built(&mut store, ByteOffset(1)..ByteOffset(3), &text)
            .is_err()
    );
    assert_eq!(text_of(&store), "\u{4e2d}abc");
    store
        .replace(ByteOffset(3)..ByteOffset(4), "")
        .expect("a direct edit");
    assert!(
        history
            .replace_built(&mut store, ByteOffset(0)..ByteOffset(0), &text)
            .is_err()
    );
}

#[test]
fn an_empty_built_edit_changes_nothing() {
    let mut store = PieceStore::new("abc");
    let mut history = UndoHistory::for_store(&store);
    let before = store.revision();
    let empty = built(&[]);
    assert!(empty.is_empty());
    history
        .replace_built(&mut store, ByteOffset(1)..ByteOffset(1), &empty)
        .unwrap();
    assert_eq!(store.revision(), before);
    assert_eq!(history.undo_groups(), 0);
}

#[test]
fn released_history_comes_out_whole_and_frees_on_another_thread() {
    let mut store = PieceStore::new("abc");
    let mut history = UndoHistory::for_store(&store);
    let big = "text ".repeat(400_000);
    let all = ByteOffset(0)..store.len_bytes();
    history
        .replace_built(&mut store, all, &built(&[&big]))
        .unwrap();
    // The undo puts the big text in the redo, and a new edit sends the redo to the list.
    history.undo(&mut store).unwrap();
    history
        .replace(&mut store, ByteOffset(0)..ByteOffset(0), "x", false)
        .unwrap();
    assert!(history.has_memory_to_release());
    assert!(history.memory_to_release_bytes() > 0);
    let released = history.take_released();
    assert!(!history.has_memory_to_release());
    assert_eq!(history.memory_to_release_bytes(), 0);
    std::thread::spawn(move || drop(released)).join().unwrap();
    assert_eq!(text_of(&store), "xabc");
    history.undo(&mut store).unwrap();
    assert_eq!(text_of(&store), "abc");
    store.check_tree().unwrap();
}

#[test]
fn a_clone_of_built_text_compares_equal_and_debug_never_prints_the_text() {
    let text = built(&["secret words"]);
    assert_eq!(text.clone(), text);
    assert_ne!(built(&["secret wordz"]), text);
    assert_eq!(format!("{text:?}"), "BuiltText(12 bytes)");
}
