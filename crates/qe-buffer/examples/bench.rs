use qe_api::{ByteOffset, LineIndex, TextSnapshot, TextStore};
use qe_buffer::{PieceStore, UndoHistory};
use std::time::{Duration, Instant};

fn sample(mut times: Vec<Duration>) -> (Duration, Duration, Duration) {
    times.sort();
    (
        times[times.len() / 2],
        times[times.len() * 99 / 100],
        *times.last().unwrap(),
    )
}
fn edits(name: &str, mut store: PieceStore, position: usize) {
    let mut times = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        let at = position + i;
        let start = Instant::now();
        store.replace(ByteOffset(at)..ByteOffset(at), "x").unwrap();
        let _ = store.byte_to_utf16(ByteOffset(at + 1));
        let _ = store.byte_to_position(ByteOffset(at + 1));
        times.push(start.elapsed());
    }
    let (p50, p99, max) = sample(times);
    println!("100 MB {name} edit + lookups: p50 {p50:?}, p99 {p99:?}, max {max:?}");
}
fn main() {
    let mut hundred = None;
    for size in [1usize << 20, 100_000_000, 512usize << 20] {
        let mut text = String::with_capacity(size);
        while text.len() + 2 <= size {
            text.push_str("x\n");
        }
        if text.len() < size {
            text.push('x');
        }
        let start = Instant::now();
        let worker = PieceStore::build_async(text.into_bytes()).unwrap();
        let open = start.elapsed();
        let store = worker.join().unwrap().unwrap();
        let built = start.elapsed();
        let mut first = String::new();
        store.visit_chunks(ByteOffset(0)..ByteOffset(80), &mut |s| first.push_str(s));
        println!(
            "{size} bytes: open {open:?}, first read {:?}, tree ready {built:?}",
            start.elapsed()
        );
        if size == 100_000_000 {
            hundred = Some(store);
        }
    }
    let store = hundred.unwrap();
    let start = Instant::now();
    for i in 0..10_000 {
        let _ = store.line_range(LineIndex(i * 3));
    }
    println!(
        "100 MB line lookup: {:?} per query",
        start.elapsed() / 10_000
    );
    edits("end", store.clone(), store.len_bytes().0);
    edits("middle", store.clone(), 50_000_000);
    history_numbers(store);
}
fn history_numbers(mut big: PieceStore) {
    let at = |a: usize, b: usize| ByteOffset(a)..ByteOffset(b);
    let mut history = UndoHistory::new();
    let end = big.len_bytes().0;
    let start = Instant::now();
    history.replace(&mut big, at(0, end), "x", true).unwrap();
    println!("100 MB select all then type: {:?}", start.elapsed());
    let start = Instant::now();
    history.undo(&mut big).unwrap();
    println!("100 MB undo of that: {:?}", start.elapsed());

    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    let count = 1_000_000;
    let start = Instant::now();
    for i in 0..count {
        history.replace(&mut store, at(i, i), "x", true).unwrap();
    }
    println!(
        "1,000,000 typed characters: {:?} total, {:?} tree shape",
        start.elapsed(),
        store.check_tree().unwrap()
    );
    let start = Instant::now();
    history.undo(&mut store).unwrap();
    println!("undo of the run: {:?}", start.elapsed());
    let start = Instant::now();
    history.redo(&mut store).unwrap();
    println!("redo of the run: {:?}", start.elapsed());
    history.undo(&mut store).unwrap();
    let start = Instant::now();
    history.replace(&mut store, at(0, 0), "y", true).unwrap();
    println!("key press that clears redo: {:?}", start.elapsed());

    let mut text = PieceStore::new(&"吾輩は猫である。\n".repeat(50_000_000 / 25));
    let mut history = UndoHistory::new();
    let mut times = Vec::with_capacity(20_000);
    let mut rng = 0x1357_9bdf_0246_8ace_u64;
    for _ in 0..20_000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let pos = (rng as usize % (text.len_bytes().0 / 3)) * 3;
        let pos = (0..3)
            .map(|d| pos + d)
            .find(|p| text.byte_to_utf16(ByteOffset(*p)).is_some());
        let pos = pos.unwrap();
        let start = Instant::now();
        history
            .replace(&mut text, at(pos, pos), "語", true)
            .unwrap();
        let _ = text.byte_to_position(ByteOffset(pos + 3));
        times.push(start.elapsed());
    }
    let (p50, p99, max) = sample(times);
    println!("50 MB Japanese, random key press + lookup: p50 {p50:?}, p99 {p99:?}, max {max:?}");
}
