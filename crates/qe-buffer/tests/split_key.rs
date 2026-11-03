//! `split_last_key`: an undo takes back the key that raised a question, and only that key.
use qe_api::{ByteOffset, TextSnapshot};
use qe_buffer::{PieceStore, UndoHistory};
use std::ops::Range;

fn at(offset: usize) -> Range<ByteOffset> {
    ByteOffset(offset)..ByteOffset(offset)
}

fn text(store: &PieceStore) -> String {
    let mut out = String::new();
    assert!(
        store.visit_chunks(ByteOffset(0)..store.len_bytes(), &mut |chunk| {
            out.push_str(chunk);
        })
    );
    out
}

fn typed(history: &mut UndoHistory, store: &mut PieceStore, from: usize, keys: &str) -> usize {
    let mut place = from;
    for key in keys.chars() {
        let key = key.to_string();
        history.replace(store, at(place), &key, true).unwrap();
        place += key.len();
    }
    place
}

#[test]
fn a_split_key_undoes_alone_and_the_run_before_it_undoes_as_one() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::for_store(&store);
    let end = typed(&mut history, &mut store, 4, "abc");
    history.mark_seen();
    typed(&mut history, &mut store, end, "x");
    assert_eq!(history.undo_groups(), 1, "the key joined the run");
    history.split_last_key();
    assert_eq!(history.undo_groups(), 2);
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "baseabc");
    assert!(!history.is_at_save_point(&store));
    history.redo(&mut store).unwrap();
    assert_eq!(text(&store), "baseabcx");
    history.undo(&mut store).unwrap();
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "base");
    assert!(history.is_at_save_point(&store));
}

#[test]
fn a_split_backspace_gives_back_its_character() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::for_store(&store);
    typed(&mut history, &mut store, 4, "abc");
    history.mark_seen();
    let range = ByteOffset(6)..ByteOffset(7);
    history.replace(&mut store, range, "", true).unwrap();
    assert_eq!(text(&store), "baseab");
    history.split_last_key();
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "baseabc");
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "base");
}

#[test]
fn a_split_backspace_past_the_run_gives_back_all_it_took() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::for_store(&store);
    typed(&mut history, &mut store, 4, "ab");
    history.mark_seen();
    // One delete takes the two typed bytes and two bytes from before the run.
    let range = ByteOffset(2)..ByteOffset(6);
    history.replace(&mut store, range, "", true).unwrap();
    assert_eq!(text(&store), "ba");
    history.split_last_key();
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "baseab");
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "base");
}

#[test]
fn keys_that_came_in_one_batch_stay_together_when_their_group_is_all_new() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::for_store(&store);
    history.mark_seen();
    typed(&mut history, &mut store, 4, "abc");
    history.split_last_key();
    assert_eq!(history.undo_groups(), 1);
    history.undo(&mut store).unwrap();
    assert_eq!(text(&store), "base");
    assert!(history.is_at_save_point(&store));
}

#[test]
fn a_new_group_or_an_undo_leaves_nothing_to_split() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::for_store(&store);
    let end = typed(&mut history, &mut store, 4, "ab");
    history.mark_seen();
    history
        .replace(&mut store, at(end), "paste", false)
        .unwrap();
    history.split_last_key();
    assert_eq!(history.undo_groups(), 2, "a paste is its own step already");
    history.undo(&mut store).unwrap();
    history.split_last_key();
    assert_eq!(history.undo_groups(), 1);
    assert_eq!(text(&store), "baseab");
}

#[test]
fn the_charge_comes_out_even_after_a_split_and_a_clear() {
    let mut store = PieceStore::new("base");
    let mut history = UndoHistory::for_store(&store);
    let end = typed(&mut history, &mut store, 4, "abc");
    history.mark_seen();
    typed(&mut history, &mut store, end, "x");
    history.split_last_key();
    let charged = history.memory_bytes();
    assert!(charged > 0);
    history.undo(&mut store).unwrap();
    history.undo(&mut store).unwrap();
    history.redo(&mut store).unwrap();
    history.redo(&mut store).unwrap();
    assert_eq!(text(&store), "baseabcx");
    assert_eq!(history.memory_bytes(), charged);
    history.reset(&store, true);
    assert_eq!(history.memory_bytes(), 0);
}
