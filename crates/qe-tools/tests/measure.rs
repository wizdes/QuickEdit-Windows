//! Measurements, not checks. Run them in release:
//! `cargo test -p qe-tools --release --locked --test measure -- --ignored --nocapture --test-threads=1`.
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorCommand, EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
use qe_tools::{FindJob, FindOptions, JobState, Matcher, prepare_replace_all};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

/// (private bytes, working set) of this process.
fn memory() -> (usize, usize) {
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    // SAFETY: the struct is valid for this call and its size is given.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut c).cast(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
        .unwrap();
    }
    (c.PrivateUsage, c.WorkingSetSize)
}

/// Samples memory on a thread every 2 ms until `stop`, and gives the highest seen.
struct Sampler {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<(usize, usize)>,
    start: (usize, usize),
}
impl Sampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let start = memory();
        let thread = std::thread::spawn(move || {
            let mut peak = memory();
            while !flag.load(Ordering::Relaxed) {
                let now = memory();
                peak = (peak.0.max(now.0), peak.1.max(now.1));
                std::thread::sleep(Duration::from_millis(2));
            }
            peak
        });
        Self {
            stop,
            thread,
            start,
        }
    }
    /// Added private bytes and added working set, at their peaks, in KiB.
    fn finish(self) -> (i64, i64) {
        self.stop.store(true, Ordering::Relaxed);
        let peak = self.thread.join().unwrap();
        (
            (peak.0 as i64 - self.start.0 as i64) / 1024,
            (peak.1 as i64 - self.start.1 as i64) / 1024,
        )
    }
}

const GIB: u64 = 1024 * 1024 * 1024;

#[test]
#[ignore = "a measurement; it writes a 1 GiB file to the temp folder and deletes it"]
fn measure_find_on_1_gib_mapped_while_indexing() {
    let dir = std::env::temp_dir().join(format!("qe-tools-measure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("one-gib.txt");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 and more words\r\n";
    let rare = "a needle sits on this line, then some more words go on to fill it all\r\n";
    assert_eq!(line.len(), rare.len());
    let lines = GIB as usize / line.len();
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
        for i in 0..lines {
            let text = if i % 1000 == 999 { rare } else { line };
            out.write_all(text.as_bytes()).unwrap();
        }
        out.flush().unwrap();
    }
    let size = std::fs::metadata(&path).unwrap().len();
    println!("file: {size} bytes, {lines} lines, just written so Windows has it in its file cache");
    for (query, want) in [
        ("NEEDLE", (lines / 1000) as u64),
        ("fox", (lines - lines / 1000) as u64),
        ("e", 0),
    ] {
        let quiet: qe_tools::Wake = Arc::new(|| {});
        let options = LoadOptions {
            force_read_only: true,
            ..Default::default()
        };
        let mut load = FileLoad::start(&path, options, quiet).unwrap();
        let loaded = loop {
            if let Some(result) = load.poll() {
                break result;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let LoadResult::Loaded(LoadedFile::ReadOnly(store)) = loaded else {
            panic!("the file must open read-only");
        };
        let mut view = EditorView::from_text("").unwrap();
        view.adopt(LoadedFile::ReadOnly(store)).unwrap();
        let at_start = view.store_status();
        let snapshot = view.worker_snapshot();
        let len = snapshot.len_bytes();
        let sampler = Sampler::start();
        let started = Instant::now();
        let job = FindJob::start(
            snapshot,
            Matcher::new(query, FindOptions::default()).unwrap(),
            ByteOffset(0)..len,
            qe_tools::QUEUE_LIMIT,
            None,
        );
        let mut first = None;
        while job.state() == JobState::Running {
            if first.is_none() && job.count() > 0 {
                first = Some(started.elapsed());
            }
            job.drain();
            job.feed(&view);
            std::thread::sleep(Duration::from_millis(1));
        }
        let elapsed = started.elapsed();
        let (private_kib, working_kib) = sampler.finish();
        assert_eq!(job.state(), JobState::Finished);
        if want > 0 {
            assert_eq!(job.count(), want);
        }
        println!(
            "find {query:?}: {} matches in {elapsed:?}, first seen after {first:?}, {:.0} MiB/s; peak added private {private_kib} KiB, peak added working set {working_kib} KiB; index complete at start: {}, known at start {} MiB, known at end {} MiB",
            job.count(),
            size as f64 / 1_048_576.0 / elapsed.as_secs_f64(),
            at_start.complete,
            at_start.known_bytes.0 / 1_048_576,
            view.store_status().known_bytes.0 / 1_048_576,
        );
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}

#[test]
#[ignore = "a measurement"]
fn measure_replace_all_100_000_matches_in_100_mb() {
    let line =
        "the quick brown fox jumps over the lazy dog 0123456789 and then some more words go\n";
    let plain =
        "the quick brown cat jumps over the lazy dog 0123456789 and then some more words go\n";
    assert_eq!(line.len(), plain.len());
    let lines = 100_000_000 / line.len();
    let step = lines / 100_000;
    let mut text = String::with_capacity(lines * line.len());
    let mut placed = 0;
    for i in 0..lines {
        if i % step == 0 && placed < 100_000 {
            text.push_str(line);
            placed += 1;
        } else {
            text.push_str(plain);
        }
    }
    let mut view = EditorView::from_text(&text).unwrap();
    let matcher = Matcher::new("fox", FindOptions::default()).unwrap();
    let snapshot = view.worker_snapshot();
    let cancel = AtomicBool::new(false);
    let sampler = Sampler::start();
    let started = Instant::now();
    let ready = prepare_replace_all(snapshot.as_ref(), &matcher, "slow", &cancel)
        .unwrap()
        .unwrap();
    let prepare = started.elapsed();
    let (private_kib, _) = sampler.finish();
    assert_eq!(ready.count, 100_000);
    drop(snapshot);
    let mut ctx = ViewContext::default();
    let started = Instant::now();
    view.apply_edits(
        &ready.prepared,
        ready.caret_after..ready.caret_after,
        &mut ctx,
    )
    .unwrap();
    let apply = started.elapsed();
    let started = Instant::now();
    view.run(EditorCommand::Undo, &mut ctx);
    let undo = started.elapsed();
    println!(
        "Replace All: {} matches in {} MB: prepare on a worker {prepare:?} (peak added private {private_kib} KiB), apply on the UI thread {apply:?}, undo {undo:?}",
        ready.count,
        text.len() / 1_000_000,
    );
}
