//! Measures real heap use with a counting allocator.
//!
//! The count is per thread. A shared count also moves when the test harness's main
//! thread frees its own data and when a finished test's thread ends, and both happen
//! during a later test's measure. Nothing in this file starts a thread of its own and
//! no store here has one, so each test's thread makes and frees all it measures.
use qe_api::{ByteOffset, TextSnapshot, TextStore};
use qe_buffer::{PieceStore, UndoHistory};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    // A const start and no destructor, so reading this never allocates.
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn count(delta: isize) {
    // This fails only while the thread is ending, after any measure has finished.
    let _ = LIVE.try_with(|live| live.set(live.get() + delta));
}

// SAFETY: this only forwards to the system allocator and counts bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size() as isize);
        // SAFETY: the caller's layout goes straight through.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(-(layout.size() as isize));
        // SAFETY: the caller's pointer and layout go straight through.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn live() -> isize {
    LIVE.with(Cell::get)
}

/// Bytes this thread holds beyond `before`. Holding less than at the start would
/// mean this thread freed memory it did not make, so that stops the test.
fn grown_since(before: isize) -> usize {
    let now = live();
    assert!(now >= before, "live count fell from {before} to {now}");
    (now - before) as usize
}

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

#[test]
fn a_million_typed_characters_stay_small() {
    let before = live();
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    let count = 1_000_000;
    for i in 0..count {
        history.replace(&mut store, at(i, i), "x", true).unwrap();
    }
    let used = grown_since(before);
    let shape = store.check_tree().unwrap();
    let mut calls = 0;
    assert!(store.visit_chunks(at(0, count), &mut |_| calls += 1));
    eprintln!(
        "1,000,000 typed characters: {used} bytes on the heap with undo, {} leaves, {calls} callbacks",
        shape.leaves
    );
    assert!(used < 3_000_000, "{used} bytes");
    assert!(calls < 500, "{calls} callbacks");
    assert!(shape.longest_leaf <= 4096);
}

#[test]
fn random_typing_also_merges_pieces() {
    let before = live();
    let mut store = PieceStore::new("");
    let mut rng = 0x1357_9bdf_0246_8ace_u64;
    let count = 200_000;
    for i in 0..count {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let pos = rng as usize % (i + 1);
        store.replace(at(pos, pos), "x").unwrap();
    }
    let used = grown_since(before);
    let shape = store.check_tree().unwrap();
    eprintln!(
        "200,000 random inserts: {used} bytes, {} leaves",
        shape.leaves
    );
    assert!(used < 1_000_000, "{used} bytes");
    assert!(shape.leaves < 200, "{} leaves", shape.leaves);
}

#[test]
fn deleting_almost_everything_releases_the_text() {
    let before = live();
    let text = "abcdefghij\n".repeat(100_000_000 / 11);
    let mut store = PieceStore::new(&text);
    let end = store.len_bytes().0;
    drop(text);
    let full = grown_since(before);
    store.replace(at(1, end), "").unwrap();
    let after = grown_since(before);
    eprintln!("100 MB store: {full} bytes before the delete; with 1 byte left: {after}");
    assert_eq!(store.len_bytes(), ByteOffset(1));
    assert!(full > 100_000_000, "{full} bytes");
    assert!(after < 1_000_000, "{after} bytes still held");
}

#[test]
fn repeated_huge_replacements_do_not_grow_without_release_memory() {
    let before = live();
    let paste = "abcdefghij\n".repeat((4 << 20) / 11);
    let paste_bytes = grown_since(before);
    let mut store = PieceStore::new(&paste);
    let limit = 8 << 20;
    let mut history = UndoHistory::with_limit(limit);
    let mut peak = 0;
    for _ in 0..100 {
        let end = store.len_bytes().0;
        history
            .replace(&mut store, at(0, end), &paste, false)
            .unwrap();
        peak = peak.max(grown_since(before) - paste_bytes);
    }
    eprintln!(
        "100 replacements of 4 MiB with an 8 MiB limit: peak {peak} bytes, {} waiting",
        history.memory_to_release_bytes()
    );
    // The document, the kept undo text and at most about one more paste of each.
    assert!(peak < 40 << 20, "{peak} bytes at the peak");
    assert!(history.memory_to_release_bytes() < 24 << 20);
}

#[test]
fn undo_text_is_released_when_it_leaves_the_history() {
    let mut store = PieceStore::new(&"abcdefghij\n".repeat(10_000_000 / 11));
    let end = store.len_bytes().0;
    let mut history = UndoHistory::with_limit(1_000_000);
    history.replace(&mut store, at(1, end), "", false).unwrap();
    let holding = live();
    history.replace(&mut store, at(0, 1), "x", false).unwrap();
    history.release_memory();
    let released = live();
    eprintln!("undo text past the limit: {holding} bytes down to {released}");
    assert!(holding - released > 9_000_000, "{holding} to {released}");
    assert_eq!(history.undo_groups(), 1);
}
