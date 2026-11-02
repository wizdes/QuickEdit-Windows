#![cfg(windows)]
//! Work and time on 1 GiB of real text, one ASCII file and one Japanese file.
//!
//! What this proves is the amount of work, by counting page reads, because a count
//! is the same on a busy PC and an idle one. A line lookup that goes back to reading
//! a page per search step would read about 14 pages on this file, against a limit of
//! 4. The time limits are only there to catch a hang: `cargo test` builds without
//! optimizing, and with every CPU busy a p99 near 16 ms was seen from scheduling
//! alone. The real budgets (100 ms first screen, 2 s index, 2 ms p99 key press) belong
//! to the optimized `examples/mapped_bench.rs`.
use qe_api::{ByteOffset, LineIndex, TextSnapshot, Utf16Offset};
use qe_buffer::MappedStore;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SIZE: usize = 1 << 30;
/// A block spans at most 2 pages, and `line_range` looks in at most 2 blocks.
const LINE_RANGE_PAGES: u64 = 4;
/// `byte_to_position` looks in the byte's block, one byte after it, the line start's
/// block, and that block again for its totals, which can be the one before.
const POSITION_PAGES: u64 = 7;
/// Far above anything seen at 100% CPU, so that only a hang or a huge slip trips it.
const HANG_LIMIT: Duration = Duration::from_millis(500);
const FIRST_SCREEN_HANG_LIMIT: Duration = Duration::from_secs(2);
const INDEX_HANG_LIMIT: Duration = Duration::from_secs(300);

fn write_text(name: &str, line: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qe-speed-{}-{name}.txt", std::process::id()));
    let block = line.repeat((1 << 20) / line.len());
    let mut file = fs::File::create(&path).unwrap();
    let mut written = 0;
    while written + block.len() <= SIZE {
        file.write_all(block.as_bytes()).unwrap();
        written += block.len();
    }
    path
}

fn p99(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() * 99 / 100]
}

fn check_file(name: &str, line: &str) {
    let path = write_text(name, line);
    let total = SIZE / ((1 << 20) / line.len() * line.len()) * ((1 << 20) / line.len());
    let bytes = total * line.len();
    let units_per_line = line.encode_utf16().count();

    let start = Instant::now();
    let store = MappedStore::open(&path).unwrap();
    for n in 0..50 {
        let range = store.try_line_range(LineIndex(n)).unwrap();
        assert_eq!(range.start, ByteOffset(n * line.len()));
    }
    let mut screen = String::new();
    store
        .try_visit_chunks(ByteOffset(0)..ByteOffset(line.len() * 80), &mut |s| {
            screen.push_str(s)
        })
        .unwrap();
    let first_screen = start.elapsed();
    assert_eq!(screen, line.repeat(80));
    assert_eq!(
        store.lookup_disk_reads(),
        0,
        "{name}: the first screen must come from the page that open read"
    );

    let mut checks = Vec::new();
    while !store.is_index_complete() {
        assert_eq!(store.error_state(), None);
        let lookup = Instant::now();
        let _ = store.line_count();
        checks.push(lookup.elapsed());
        assert!(
            start.elapsed() < INDEX_HANG_LIMIT,
            "{name}: index not done in {INDEX_HANG_LIMIT:?}"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    let index_time = start.elapsed();
    assert_eq!(store.try_line_count(), Ok(total + 1));
    assert_eq!(
        store.try_len_utf16(),
        Ok(Utf16Offset(total * units_per_line))
    );

    let mut rng = 0x1234_5678_9abc_def1_u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng as usize
    };
    let mut line_times = Vec::with_capacity(1000);
    let mut position_times = Vec::with_capacity(1000);
    let (mut most_line_pages, mut most_position_pages) = (0, 0);
    for _ in 0..1000 {
        let n = next() % total;
        let reads = store.lookup_disk_reads();
        let lookup = Instant::now();
        let range = store.line_range(LineIndex(n));
        line_times.push(lookup.elapsed());
        most_line_pages = most_line_pages.max(store.lookup_disk_reads() - reads);
        assert_eq!(
            range,
            Some(ByteOffset(n * line.len())..ByteOffset((n + 1) * line.len()))
        );
        let byte = ByteOffset((next() % total) * line.len());
        let reads = store.lookup_disk_reads();
        let lookup = Instant::now();
        let position = store.byte_to_position(byte).unwrap();
        position_times.push(lookup.elapsed());
        most_position_pages = most_position_pages.max(store.lookup_disk_reads() - reads);
        assert_eq!(position.line, LineIndex(byte.0 / line.len()));
    }
    let (line_p99, position_p99) = (p99(line_times), p99(position_times));
    let timing = store.index_timing();
    eprintln!(
        "{name} (unoptimized test build): first screen {first_screen:?}, index {index_time:?} \
         {timing:?}, line_range p99 {line_p99:?}, byte_to_position p99 {position_p99:?}, \
         most pages per line_range {most_line_pages}, per byte_to_position \
         {most_position_pages}, index memory {} bytes, {bytes} bytes of text",
        store.index_memory_bytes(),
    );
    drop(store);
    fs::remove_file(&path).unwrap();
    assert!(
        most_line_pages <= LINE_RANGE_PAGES,
        "{name}: a line_range read {most_line_pages} pages"
    );
    assert!(
        most_position_pages <= POSITION_PAGES,
        "{name}: a byte_to_position read {most_position_pages} pages"
    );
    assert!(
        first_screen < FIRST_SCREEN_HANG_LIMIT,
        "{name}: {first_screen:?}"
    );
    assert!(line_p99 < HANG_LIMIT, "{name}: {line_p99:?}");
    assert!(position_p99 < HANG_LIMIT, "{name}: {position_p99:?}");
}

#[test]
fn gib_of_real_ascii() {
    check_file(
        "ascii",
        "The quick brown fox jumps over the lazy dog, and then it naps in the sun.\r\n",
    );
}

#[test]
fn gib_of_real_japanese() {
    check_file(
        "japanese",
        "吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。\n",
    );
}
