//! Measures real heap use with a counting allocator. One test at a time, since
//! the counter is shared by the whole process.
use qe_api::{ByteOffset, TextSnapshot, TextStore};
use qe_buffer::{PieceStore, UndoHistory};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

// SAFETY: this only forwards to the system allocator and counts bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's layout goes straight through.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's pointer and layout go straight through.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn at(start: usize, end: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(start)..ByteOffset(end)
}

#[test]
fn a_million_typed_characters_stay_small() {
    let _one = ONE_AT_A_TIME.lock().unwrap();
    let before = LIVE.load(Ordering::Relaxed);
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    let count = 1_000_000;
    for i in 0..count {
        history.replace(&mut store, at(i, i), "x", true).unwrap();
    }
    let used = LIVE.load(Ordering::Relaxed) - before;
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
    let _one = ONE_AT_A_TIME.lock().unwrap();
    let before = LIVE.load(Ordering::Relaxed);
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
    let used = LIVE.load(Ordering::Relaxed) - before;
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
    let _one = ONE_AT_A_TIME.lock().unwrap();
    let text = "abcdefghij\n".repeat(100_000_000 / 11);
    let before = LIVE.load(Ordering::Relaxed);
    let mut store = PieceStore::new(&text);
    let end = store.len_bytes().0;
    drop(text);
    let full = LIVE.load(Ordering::Relaxed) as isize - before as isize;
    store.replace(at(1, end), "").unwrap();
    let after = LIVE.load(Ordering::Relaxed) as isize - before as isize;
    eprintln!("100 MB store: {full} bytes before the delete; with 1 byte left: {after}");
    assert_eq!(store.len_bytes(), ByteOffset(1));
    // `before` counted the source string, so this drops below zero when released.
    assert!(after < 100_000_000 / 100, "{after} bytes still held");
}

#[test]
fn undo_text_is_released_when_it_leaves_the_history() {
    let _one = ONE_AT_A_TIME.lock().unwrap();
    let mut store = PieceStore::new(&"abcdefghij\n".repeat(10_000_000 / 11));
    let end = store.len_bytes().0;
    let mut history = UndoHistory::with_limit(1_000_000);
    history.replace(&mut store, at(1, end), "", false).unwrap();
    let holding = LIVE.load(Ordering::Relaxed);
    history.replace(&mut store, at(0, 1), "x", false).unwrap();
    history.release_memory();
    let released = LIVE.load(Ordering::Relaxed);
    eprintln!("undo text past the limit: {holding} bytes down to {released}");
    assert!(holding - released > 9_000_000, "{holding} to {released}");
    assert_eq!(history.undo_groups(), 1);
}
