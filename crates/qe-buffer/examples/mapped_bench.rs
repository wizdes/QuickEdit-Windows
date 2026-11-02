//! Times `MappedStore` on 1 GiB of real ASCII and Japanese text. Run it with
//! `cargo run --release --locked -p qe-buffer --example mapped_bench -- <folder>`.
//! The files are written to `<folder>`, read once to warm the cache, and deleted.
use qe_api::{ByteOffset, LineIndex, TextSnapshot, Utf16Offset};
use qe_buffer::MappedStore;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const SIZE: usize = 1 << 30;

fn sample(mut times: Vec<Duration>) -> String {
    times.sort();
    format!(
        "p50 {:?}, p99 {:?}, max {:?}",
        times[times.len() / 2],
        times[times.len() * 99 / 100],
        times.last().unwrap()
    )
}

fn run(folder: &Path, name: &str, line: &str) {
    let path = folder.join(format!("qe-mapped-bench-{name}.txt"));
    let block = line.repeat((1 << 20) / line.len());
    let mut file = fs::File::create(&path).unwrap();
    let mut written = 0;
    while written + block.len() <= SIZE {
        file.write_all(block.as_bytes()).unwrap();
        written += block.len();
    }
    drop(file);
    let lines = written / line.len();
    // Warm the file cache, as the budgets ask for.
    let mut buffer = vec![0; 1 << 20];
    let mut warm = fs::File::open(&path).unwrap();
    while warm.read(&mut buffer).unwrap() > 0 {}
    println!("--- {name}: {written} bytes, {lines} lines (warm cache)");

    for round in 0..3 {
        let start = Instant::now();
        let store = MappedStore::open(&path).unwrap();
        let open = start.elapsed();
        for n in 0..50 {
            store.try_line_range(LineIndex(n)).unwrap();
        }
        let mut chars = 0;
        assert!(
            store.visit_chunks(ByteOffset(0)..ByteOffset(line.len() * 26), &mut |s| {
                chars += s.len()
            })
        );
        let first_screen = start.elapsed();
        let small = Instant::now();
        store.try_utf16_to_byte(Utf16Offset(5)).unwrap();
        let small = small.elapsed();
        let mut during = Vec::new();
        while !store.is_index_complete() {
            let lookup = Instant::now();
            let _ = store.line_count();
            let _ = store.try_line_range(LineIndex(10));
            during.push(lookup.elapsed());
            std::thread::sleep(Duration::from_micros(100));
        }
        let index = start.elapsed();
        let t = store.index_timing();
        println!(
            "round {round}: open {open:?}, open + 50 lines + {chars} bytes read {first_screen:?}, \
             utf16_to_byte(5) {small:?}, index {index:?} (read {:.3} s, validate {:.3} s, \
             measure {:.3} s, check {:.3} s), index memory {} bytes",
            t.read as f64 / 1e9,
            t.validate as f64 / 1e9,
            t.measure as f64 / 1e9,
            t.check as f64 / 1e9,
            store.index_memory_bytes(),
        );
        if !during.is_empty() {
            let during_count = during.len();
            println!(
                "  line_count + line_range(10) while indexing: {} over {} samples",
                sample(during),
                during_count
            );
        }
        let mut rng = 0x1234_5678_9abc_def1_u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng as usize
        };
        let count = 10_000;
        let mut line_times = Vec::with_capacity(count);
        let mut position_times = Vec::with_capacity(count);
        let mut unit_times = Vec::with_capacity(count);
        let units = store.len_utf16().0;
        let checks_before = store.file_checks();
        let all = Instant::now();
        for _ in 0..count {
            let n = next() % lines;
            let t = Instant::now();
            std::hint::black_box(store.line_range(LineIndex(n)));
            line_times.push(t.elapsed());
            let byte = ByteOffset((next() % lines) * line.len());
            let t = Instant::now();
            std::hint::black_box(store.byte_to_position(byte));
            position_times.push(t.elapsed());
            let unit = Utf16Offset(next() % units);
            let t = Instant::now();
            std::hint::black_box(store.utf16_to_byte(unit));
            unit_times.push(t.elapsed());
        }
        println!(
            "  random line_range {}\n  random byte_to_position {}\n  random utf16_to_byte {}",
            sample(line_times),
            sample(position_times),
            sample(unit_times)
        );
        println!(
            "  {} file checks in {:?} for {} lookups",
            store.file_checks() - checks_before,
            all.elapsed(),
            count * 3
        );
        let drop_start = Instant::now();
        drop(store);
        println!("  drop {:?}", drop_start.elapsed());
    }
    let check_only = MappedStore::open(&path).unwrap();
    let mut checks = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let t = Instant::now();
        check_only.check_file().unwrap();
        checks.push(t.elapsed());
    }
    println!("  one file check: {}", sample(checks));
    drop(check_only);
    fs::remove_file(&path).unwrap();
}

fn main() {
    let folder = std::env::args()
        .nth(1)
        .expect("give a folder for the files");
    run(
        Path::new(&folder),
        "ascii",
        "The quick brown fox jumps over the lazy dog, and then it naps in the sun.\r\n",
    );
    run(
        Path::new(&folder),
        "japanese",
        "吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。\n",
    );
}
