use qe_api::{ByteOffset, LineIndex, TextSnapshot, TextStore};
use qe_buffer::PieceStore;
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
        let worker = PieceStore::build_async(text.into_bytes());
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
    edits("middle", store, 50_000_000);
}
