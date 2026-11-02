//! Times the plain `visit_chunks` the way PDF export uses it: three short reads of
//! each line, in order. Run it with
//! `cargo run --release --locked -p qe-buffer --example visit_bench -- <folder>`.
//! It writes a 5.5 MB file and a 1 GiB file of 37-byte lines to `<folder>`, and
//! deletes them.
use qe_api::{ByteOffset, LineIndex, TextSnapshot};
use qe_buffer::MappedStore;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const LINES_TIMED: usize = 150_000;
const LINE: &str = "a line of text in the mapped file 01\n";

fn run(folder: &Path, name: &str, size: usize) {
    let path = folder.join(format!("qe-visit-bench-{name}.txt"));
    let mut file = BufWriter::new(fs::File::create(&path).unwrap());
    let lines = size / LINE.len();
    for _ in 0..lines {
        file.write_all(LINE.as_bytes()).unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    let store = MappedStore::open(&path).unwrap();
    while !store.is_index_complete() {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(store.error_state(), None);
    assert_eq!(store.line_count(), lines + 1);
    // Three short reads inside one line, as `qe-pdf` makes for the runs of a line.
    let one = |line: usize, out: &mut usize| {
        let start = store.try_line_range(LineIndex(line)).ok();
        let start = start.map_or_else(
            || store.line_range(LineIndex(line)).unwrap().start.0,
            |r| r.start.0,
        );
        for (from, to) in [(0, 12), (12, 24), (24, 36)] {
            let range = ByteOffset(start + from)..ByteOffset(start + to);
            assert!(store.visit_chunks(range, &mut |s| *out += s.len()));
        }
    };
    let timed = LINES_TIMED.min(lines);
    let first = (lines - timed) / 2;
    let checks = store.file_checks();
    let reads = store.lookup_disk_reads();
    let mut got = 0;
    let start = Instant::now();
    for line in first..first + timed {
        one(line, &mut got);
    }
    let elapsed = start.elapsed();
    assert_eq!(got, timed * 36);
    let per_call = elapsed.as_secs_f64() * 1e6 / (timed * 3) as f64;
    println!(
        "{name}: {size} bytes, {lines} lines. {} reads in {elapsed:.2?}: {per_call:.2} us a call. \
         All lines at 3 reads each: {:.1} s. {} file checks, {} page loads, index memory {} bytes",
        timed * 3,
        per_call * (lines * 3) as f64 / 1e6,
        store.file_checks() - checks,
        store.lookup_disk_reads() - reads,
        store.index_memory_bytes(),
    );
    // One long read, to see that the route for a whole-file scan also holds up.
    let start = Instant::now();
    let mut bytes = 0;
    assert!(store.visit_chunks(ByteOffset(0)..store.len_bytes(), &mut |s| bytes += s.len()));
    println!("{name}: read all {bytes} bytes in {:.2?}", start.elapsed());
    drop(store);
    fs::remove_file(path).unwrap();
}

fn main() {
    let folder = std::env::args().nth(1).expect("give a folder");
    let folder = Path::new(&folder);
    run(folder, "5.5mb", 5_500_000);
    run(folder, "1gib", 1 << 30);
}
