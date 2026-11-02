//! Timings. Run them by hand, in a release build, as the README says. They are ignored in a
//! normal run, because they take minutes and need 100 MB of text in memory.
use qe_api::{InputKind, ThemeKind, ViewContext};
use qe_pdf::{PdfOptions, check::check_pdf, export_to_file};
use qe_views_check::*;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;

fn peak_memory_mib() -> f64 {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .unwrap();
    }
    counters.PeakWorkingSetSize as f64 / 1_048_576.0
}

fn document(bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 100);
    let mut n = 0;
    while text.len() < bytes {
        text.push_str(&format!(
            "line {n} of a plain document with some ordinary words in it\n"
        ));
        n += 1;
    }
    text.truncate(bytes);
    text
}

/// One key from input to pixels: the frame's handling, the display list and the paint.
fn key_times(text: &str, map_on: bool) -> Vec<Duration> {
    let mut scene = Scene::from_text(text, 1200.0, 900.0);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_document_map(map_on, &mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());
    let mut paper = renderer(1200.0, 900.0, 96);
    let mut seen = HashSet::new();
    paint(&mut paper, &scene.list(ThemeKind::Light), &mut seen);
    let mut times = Vec::with_capacity(400);
    for n in 0..400 {
        let mut repaint = Vec::new();
        let start = Instant::now();
        scene.send(
            InputKind::Char(char::from(b'a' + (n % 26) as u8)),
            &mut repaint,
        );
        let list = scene.list(ThemeKind::Light);
        for rect in &repaint {
            paper.invalidate(*rect);
        }
        paint(&mut paper, &list, &mut seen);
        times.push(start.elapsed());
        // The time between keys of a fast typist, and the work that falls in it.
        scene.clock.set(scene.clock.get() + 0.03);
        let mut ctx = ViewContext::default();
        scene.editor.poll_layout(&mut ctx);
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        let mut repaint = Vec::new();
        take_repaints(&ctx, &mut repaint);
        for rect in &repaint {
            paper.invalidate(*rect);
        }
        paint(&mut paper, &scene.list(ThemeKind::Light), &mut seen);
    }
    times
}

fn report(name: &str, mut times: Vec<Duration>) -> Duration {
    times.sort();
    let at = |share: f64| times[((times.len() - 1) as f64 * share) as usize];
    println!(
        "{name}: median {:?}, 99th {:?}, slowest {:?}",
        at(0.5),
        at(0.99),
        at(1.0)
    );
    at(0.5)
}

#[test]
#[ignore = "by hand, in release: see the README"]
fn a_key_with_the_map_on_and_off() {
    for (name, bytes) in [("1 MB", 1_000_000usize), ("50 MiB", 50 * 1024 * 1024)] {
        let text = document(bytes);
        // Off first, then on, then off again, so that warming up favors neither.
        let off = report(&format!("{name}, map off"), key_times(&text, false));
        let on = report(&format!("{name}, map on"), key_times(&text, true));
        let again = report(&format!("{name}, map off again"), key_times(&text, false));
        assert!(
            on <= off.max(again) + Duration::from_micros(300),
            "{name}: the map added {:?}",
            on.saturating_sub(off)
        );
    }
}

#[test]
#[ignore = "by hand, in release: see the README"]
fn a_map_job_on_50_mib() {
    let text = document(50 * 1024 * 1024);
    let mut scene = Scene::from_text(&text, 1200.0, 2000.0);
    drop(text);
    let geometry = scene.frame.geometry().unwrap();
    let mut times = Vec::new();
    for n in 0..30 {
        let target = scene.editor.max_scroll_position() * f64::from(n) / 30.0;
        let mut ctx = ViewContext::default();
        scene.editor.set_scroll_position(target, &mut ctx);
        let start = Instant::now();
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        while scene.frame.map_job_running() {
            scene
                .frame
                .sync(&mut scene.editor, &scene.measure, &mut ctx);
            std::thread::yield_now();
        }
        times.push(start.elapsed());
        scene.settle(&mut Vec::new());
    }
    println!("rows in the map: {}", geometry.rows);
    report("a far scroll's map job, 1000 rows", times);
}

fn export_large(name: &str, megabytes: usize, header: bool) {
    let editor = qe_editor::EditorView::from_text(&document(megabytes * 1_000_000)).unwrap();
    let snapshot = editor.worker_snapshot();
    let before = peak_memory_mib();
    let path = output_folder().join(format!("measure-{name}.pdf"));
    let options = PdfOptions {
        header_and_footer: header,
        title: "large.txt".into(),
        date: "1/2/26, 3:04 PM".into(),
        ..PdfOptions::default()
    };
    let start = Instant::now();
    let mut gaps = Duration::ZERO;
    let mut last = Instant::now();
    let summary = export_to_file(
        snapshot.as_ref(),
        &options,
        &path,
        &AtomicBool::new(false),
        &mut |_| {
            gaps = gaps.max(last.elapsed());
            last = Instant::now();
        },
    )
    .unwrap();
    let took = start.elapsed();
    println!(
        "{name}: {took:?}; {} pages; file {:.1} MB; peak memory {before:.0} MiB with the text, {:.0} MiB at the end; longest gap between progress calls {gaps:?}",
        summary.pages,
        summary.bytes as f64 / 1e6,
        peak_memory_mib()
    );
    if megabytes <= 10 {
        let bytes = std::fs::read(&path).unwrap();
        let start = Instant::now();
        let report = check_pdf(&bytes).unwrap();
        println!(
            "the checker read {} objects in {:?}",
            report.objects,
            start.elapsed()
        );
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
#[ignore = "by hand, in release: see the README"]
fn export_100_mb_with_no_header() {
    export_large("100 MB", 100, false);
}

#[test]
#[ignore = "by hand, in release: see the README"]
fn export_10_mb_with_a_header() {
    export_large("10 MB with a header", 10, true);
}

#[test]
#[ignore = "by hand, in release: see the README"]
fn cancel_on_100_mb_stops_quickly() {
    let editor = qe_editor::EditorView::from_text(&document(100_000_000)).unwrap();
    let path = output_folder().join("measure-cancel.pdf");
    let job = qe_pdf::export_worker(
        editor.worker_snapshot(),
        PdfOptions::default(),
        path.clone(),
        None,
    )
    .unwrap();
    while job.fraction() < 0.3 {
        std::thread::yield_now();
    }
    let start = Instant::now();
    job.cancel();
    while !job.is_finished() {
        std::thread::yield_now();
    }
    println!(
        "the cancel took {:?}, at {:.2} of the way",
        start.elapsed(),
        job.fraction()
    );
    assert_eq!(job.join(), Err(qe_pdf::PdfError::Cancelled));
    assert!(!path.exists());
}
