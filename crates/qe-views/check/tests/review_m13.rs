//! Regressions from the first review of M13. Each test failed on the code the review saw.
use qe_api::{
    ByteOffset, Dip, DrawCommand, InputEvent, InputKind, LineIndex, Modifiers, MouseButton, Point,
    ThemeKind, ViewContext, ViewRequest,
};
use qe_buffer::MappedStore;
use qe_editor::EditorView;
use qe_pdf::{PdfError, PdfOptions, check::check_pdf, export_to_file, export_worker};
use qe_views_check::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn pt(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn lines(n: usize) -> String {
    (0..n).map(|i| format!("line {i}\n")).collect()
}
fn down(at: Point, button: MouseButton) -> InputKind {
    InputKind::MouseDown {
        at,
        button,
        modifiers: Modifiers::default(),
    }
}
fn up(at: Point, button: MouseButton) -> InputKind {
    InputKind::MouseUp {
        at,
        button,
        modifiers: Modifiers::default(),
    }
}
fn wide_blocks(scene: &Scene) -> usize {
    let left = scene.frame.map_area().x.0;
    scene
        .list(ThemeKind::Light)
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRects { rects, .. } => Some(rects.clone()),
            _ => None,
        })
        .flatten()
        .filter(|rect| rect.x.0 >= left && rect.width.0 > 50.0)
        .count()
}
fn insert_keeping_view(scene: &mut Scene, at: ByteOffset, text: &str) {
    let snapshot = scene.editor.worker_snapshot();
    let edits = [qe_editor::TextEdit {
        range: at..at,
        text: text.into(),
    }];
    let prepared =
        qe_editor::prepare_edits(snapshot.as_ref(), &edits, &AtomicBool::new(false)).unwrap();
    let end = ByteOffset(at.0 + text.len());
    scene
        .editor
        .apply_edits(&prepared, end..end, &mut ViewContext::default())
        .unwrap();
}
/// Scrolls to a place the map holds no rows for, and stops with its job not yet taken.
fn start_a_map_job(scene: &mut Scene, fraction: f64) {
    let max = scene.editor.max_scroll_position();
    scene
        .editor
        .set_scroll_position(max * fraction, &mut ViewContext::default());
    let jobs = scene.frame.counts().map_jobs;
    let deadline = Instant::now() + Duration::from_secs(20);
    while scene.frame.counts().map_jobs == jobs {
        let mut ctx = ViewContext::default();
        scene.editor.poll_layout(&mut ctx);
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        assert!(Instant::now() < deadline, "no job started");
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct Remove(PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        // A failed removal leaves a file in temp, and must not hide the test's result.
        let _ = std::fs::remove_file(&self.0);
    }
}
static NEXT: AtomicU64 = AtomicU64::new(0);
fn temp(name: &str) -> Remove {
    Remove(std::env::temp_dir().join(format!(
        "qe-review-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )))
}
fn numbered_file(count: usize) -> Remove {
    let file = temp("lines.txt");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&file.0).unwrap());
    for n in 0..count {
        writeln!(out, "line number {n} of the mapped file").unwrap();
    }
    out.flush().unwrap();
    file
}

// ---- Finding 1: a mapped file ----

/// Offers snapshots the way a host does on each wake, until the job ends.
fn run_job_like_a_host(editor: &EditorView, job: &qe_pdf::PdfJob) -> (bool, Vec<f64>) {
    // A debug build under load takes a long time on this file.
    let deadline = Instant::now() + Duration::from_secs(280);
    let mut waited = false;
    let mut seen = Vec::new();
    while !job.is_finished() {
        if job.wants_snapshot() {
            waited = true;
            job.offer_snapshot(editor.worker_snapshot());
        }
        seen.push(job.fraction());
        assert!(Instant::now() < deadline, "the export never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    (waited, seen)
}

/// Holds the index back with a slow read, starts the export, and acts as the host would.
/// Returns the last row of the PDF.
fn export_during_the_index(count: usize) -> String {
    let file = numbered_file(count);
    // The index may pass its first read before the delay is set, so this tries again.
    let mut opened = None;
    for _ in 0..50 {
        let store = MappedStore::open(&file.0).unwrap();
        store.set_test_read_delay(Duration::from_millis(500));
        let speed = store.clone();
        let editor = EditorView::from_mapped(store).unwrap();
        let first = editor.worker_snapshot();
        if !first.status().complete {
            opened = Some((speed, editor, first));
            break;
        }
    }
    let (speed, editor, first) = opened.expect("the index always ended before the snapshot");
    let out = temp("mapped.pdf");
    let job = export_worker(first, PdfOptions::default(), out.0.clone(), None).unwrap();
    let start = Instant::now();
    while !job.wants_snapshot() {
        assert!(!job.is_finished());
        assert!(start.elapsed() < Duration::from_secs(30), "it never asked");
        std::thread::yield_now();
    }
    let mut seen = Vec::new();
    while job.wants_snapshot() {
        job.offer_snapshot(editor.worker_snapshot());
        seen.push(job.fraction());
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the index never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // The layout's own reads need no delay.
    speed.set_test_read_delay(Duration::ZERO);
    assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]));
    let writing = Instant::now();
    run_job_like_a_host(&editor, &job);
    println!("{count} lines: the write took {:?}", writing.elapsed());
    assert_eq!(job.fraction(), 1.0);
    job.join().unwrap();
    let checking = Instant::now();
    let report = check_pdf(&std::fs::read(&out.0).unwrap()).unwrap();
    println!("{count} lines: the check took {:?}", checking.elapsed());
    let rows = report.page_rows(report.pages.len() - 1);
    rows.last().cloned().unwrap_or_default()
}

#[test]
fn a_mapped_file_exports_whole_when_the_export_starts_during_the_index() {
    assert_eq!(
        export_during_the_index(2000),
        "line number 1999 of the mapped file"
    );
}

/// The same with a file that takes the index two blocks. It takes minutes in a debug build.
#[test]
#[ignore = "run by hand with --release"]
fn a_larger_mapped_file_exports_whole_when_the_export_starts_during_the_index() {
    assert_eq!(
        export_during_the_index(150_000),
        "line number 149999 of the mapped file"
    );
}

#[test]
fn a_snapshot_pinned_part_way_is_refused_and_writes_nothing() {
    let file = numbered_file(150_000);
    let store = MappedStore::open(&file.0).unwrap();
    let editor = EditorView::from_mapped(store).unwrap();
    let snapshot = editor.worker_snapshot();
    if snapshot.status().complete {
        eprintln!("the index finished before the snapshot, so this run proves nothing");
        return;
    }
    let out = temp("partial.pdf");
    let result = export_to_file(
        snapshot.as_ref(),
        &PdfOptions::default(),
        &out.0,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(
        matches!(result, Err(PdfError::NotComplete(_))),
        "{result:?}"
    );
    assert!(!out.0.exists());
}

#[test]
fn a_file_that_changes_during_the_index_gives_a_plain_message_and_no_pdf() {
    let file = numbered_file(150_000);
    let store = MappedStore::open(&file.0).unwrap();
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .open(&file.0)
        .unwrap();
    opened.set_len(100).unwrap();
    assert!(store.check_file().is_err());
    let editor = EditorView::from_mapped(store).unwrap();
    let out = temp("failed.pdf");
    let job = export_worker(
        editor.worker_snapshot(),
        PdfOptions::default(),
        out.0.clone(),
        None,
    )
    .unwrap();
    run_job_like_a_host(&editor, &job);
    let Err(PdfError::Read(message)) = job.join() else {
        panic!("the export did not stop on the failure");
    };
    assert!(message.contains("no PDF was written"), "{message}");
    assert!(!out.0.exists());
}

#[test]
fn closing_the_document_during_the_wait_ends_the_export() {
    let file = numbered_file(150_000);
    let store = MappedStore::open(&file.0).unwrap();
    let editor = EditorView::from_mapped(store).unwrap();
    let snapshot = editor.worker_snapshot();
    if snapshot.status().complete {
        eprintln!("the index finished before the snapshot, so this run proves nothing");
        return;
    }
    let out = temp("closed.pdf");
    let job = export_worker(snapshot, PdfOptions::default(), out.0.clone(), None).unwrap();
    // No snapshot is ever offered. A join gives up the host's end of the wait.
    let result = job.join();
    assert!(
        matches!(result, Err(PdfError::NotComplete(_))),
        "{result:?}"
    );
    assert!(!out.0.exists());
}

// ---- Finding 2: an edit while a job runs ----

#[test]
fn an_edit_while_a_map_job_runs_still_reaches_the_map() {
    let mut scene = Scene::from_text(&lines(20_000), 900.0, 400.0);
    start_a_map_job(&mut scene, 0.5);
    let line = LineIndex(scene.frame.geometry().unwrap().first_line.ceil() as usize + 3);
    let start = scene.editor.line_range(line).unwrap().start;
    insert_keeping_view(&mut scene, start, &"X".repeat(70));
    let mut ctx = ViewContext::default();
    scene
        .frame
        .sync(&mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());
    assert_eq!(wide_blocks(&scene), 1, "the map must show the new long row");
}

// ---- Finding 3: a lost mouse ----

#[test]
fn a_text_drag_ends_when_the_mouse_is_lost() {
    let mut scene = Scene::from_text(&lines(200), 900.0, 400.0);
    let mut repaint = Vec::new();
    scene.send(down(pt(100.0, 10.0), MouseButton::Left), &mut repaint);
    let before = scene.editor.selection();
    let mut ctx = ViewContext::default();
    scene.frame.capture_lost(&mut scene.editor, &mut ctx);
    scene.send(
        InputKind::MouseMove {
            at: pt(200.0, 300.0),
        },
        &mut repaint,
    );
    std::thread::sleep(Duration::from_millis(60));
    scene.send(
        InputKind::MouseMove {
            at: pt(200.0, 320.0),
        },
        &mut repaint,
    );
    scene.editor.poll_layout(&mut ViewContext::default());
    assert_eq!(scene.editor.selection(), before);
}

// ---- Finding 4: more than one button ----

#[test]
fn a_right_click_over_the_text_during_a_map_drag_leaves_no_state_behind() {
    let mut scene = Scene::from_text(&lines(5000), 900.0, 400.0);
    let map = scene.frame.map_area();
    let mut repaint = Vec::new();
    scene.send(
        down(pt(map.x.0 + 20.0, 100.0), MouseButton::Left),
        &mut repaint,
    );
    let text = pt(300.0, 200.0);
    scene.send(InputKind::MouseMove { at: text }, &mut repaint);
    scene.send(down(text, MouseButton::Right), &mut repaint);
    scene.send(up(text, MouseButton::Right), &mut repaint);
    // The drag still holds the map, so the next move still scrolls.
    let before = scene.editor.scroll_offset();
    scene.send(
        InputKind::MouseMove {
            at: pt(300.0, 300.0),
        },
        &mut repaint,
    );
    assert_ne!(scene.editor.scroll_offset(), before, "the drag ended early");
    let mut ctx = ViewContext::default();
    scene.frame.handle(
        &mut scene.editor,
        &InputEvent {
            time_us: 0,
            kind: up(pt(300.0, 300.0), MouseButton::Left),
        },
        &scene.measure,
        &mut ctx,
    );
    assert!(ctx.requests.contains(&ViewRequest::CaptureMouse(false)));
    scene.settle(&mut repaint);
    assert_eq!(
        scene.frame.map_box().unwrap().alpha,
        0.0,
        "the box stuck on"
    );
    // The box follows a later scroll.
    let top = scene.frame.map_box().unwrap().rect.y.0;
    let max = scene.editor.max_scroll_position();
    scene
        .editor
        .set_scroll_position(max * 0.9, &mut ViewContext::default());
    scene.settle(&mut repaint);
    assert_ne!(
        scene.frame.map_box().unwrap().rect.y.0,
        top,
        "the box stuck in place"
    );
}

#[test]
fn a_right_click_inside_the_map_does_not_end_a_left_drag() {
    let mut scene = Scene::from_text(&lines(5000), 900.0, 400.0);
    let map = scene.frame.map_area();
    let mut repaint = Vec::new();
    let at = pt(map.x.0 + 20.0, 100.0);
    scene.send(down(at, MouseButton::Left), &mut repaint);
    scene.send(down(at, MouseButton::Right), &mut repaint);
    scene.send(up(at, MouseButton::Right), &mut repaint);
    let before = scene.editor.scroll_offset();
    scene.send(
        InputKind::MouseMove {
            at: pt(map.x.0 + 20.0, 300.0),
        },
        &mut repaint,
    );
    assert!(scene.editor.scroll_offset() > before);
    scene.send(
        up(pt(map.x.0 + 20.0, 300.0), MouseButton::Left),
        &mut repaint,
    );
}

#[test]
fn a_right_press_in_the_text_released_over_the_map_still_reaches_the_editor() {
    let mut scene = Scene::from_text(&lines(50), 900.0, 400.0);
    let map = scene.frame.map_area();
    let mut repaint = Vec::new();
    scene.send(down(pt(100.0, 10.0), MouseButton::Left), &mut repaint);
    let anchor = scene.editor.selection().start;
    // A left drag in the text carries on over the map after a right click in the map.
    scene.send(
        down(pt(map.x.0 + 5.0, 50.0), MouseButton::Right),
        &mut repaint,
    );
    scene.send(
        up(pt(map.x.0 + 5.0, 50.0), MouseButton::Right),
        &mut repaint,
    );
    scene.send(
        InputKind::MouseMove {
            at: pt(map.x.0 + 5.0, 100.0),
        },
        &mut repaint,
    );
    let selection = scene.editor.selection();
    assert_eq!(selection.start, anchor);
    assert!(selection.end > anchor);
    scene.send(
        up(pt(map.x.0 + 5.0, 100.0), MouseButton::Left),
        &mut repaint,
    );
}

// ---- Finding 9: a job at the moment the map goes off ----

#[test]
fn switching_the_map_off_during_a_job_leaves_no_timer_and_no_job() {
    let mut scene = Scene::from_text(&lines(20_000), 900.0, 400.0);
    start_a_map_job(&mut scene, 0.5);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_document_map(false, &mut scene.editor, &scene.measure, &mut ctx);
    assert!(!scene.frame.map_job_running());
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(30));
        scene.clock.set(scene.clock.get() + 1.0);
        let mut ctx = ViewContext::default();
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        assert!(
            !ctx.requests
                .iter()
                .any(|request| matches!(request, ViewRequest::RequestTimer(_))),
            "{:?}",
            ctx.requests
        );
    }
    // And the map comes back with the right rows.
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_document_map(true, &mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());
    assert!(!scene.frame.map_job_running());
}

// ---- The uncertain item: map jobs on a partial index ----

/// One long line fills the index's first part, so rows on screen are not yet known. A wake
/// after each job could then start the next job at once.
#[test]
fn map_jobs_do_not_loop_while_a_slow_index_runs() {
    let file = temp("sparse.txt");
    {
        let made = std::fs::File::create(&file.0).unwrap();
        made.set_len(1024 * 1024 * 1024).unwrap();
    }
    let store = MappedStore::open(&file.0).unwrap();
    store.set_test_read_delay(Duration::from_millis(5));
    let editor = EditorView::from_mapped(store).unwrap();
    // A small size, so that the map stays on for this large file.
    let mut scene = Scene::with_file_bytes(editor, 900.0, 400.0, Some(1024));
    let start = Instant::now();
    let mut partial_syncs = 0u64;
    while !scene.editor.store_status().complete && start.elapsed() < Duration::from_secs(8) {
        // A host that syncs as often as it is woken, with real time on its clock.
        scene.clock.set(10.0 + start.elapsed().as_secs_f64());
        let mut ctx = ViewContext::default();
        scene.editor.poll_layout(&mut ctx);
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        partial_syncs += 1;
        std::thread::yield_now();
    }
    let seconds = start.elapsed().as_secs_f64();
    let jobs = scene.frame.counts().map_jobs;
    println!("{seconds:.2} s partial, {partial_syncs} syncs, {jobs} map jobs");
    assert!(
        partial_syncs > 100,
        "the index ended too soon to prove this"
    );
    assert!(
        jobs as f64 <= seconds * 10.0 + 3.0,
        "{jobs} jobs in {seconds:.2} s"
    );
}

/// A failed index never grows, and nothing may run after it.
#[test]
fn map_jobs_stop_when_the_index_has_failed() {
    let file = numbered_file(50_000);
    let store = MappedStore::open(&file.0).unwrap();
    let editor = EditorView::from_mapped(store).unwrap();
    let mut scene = Scene::with_file_bytes(editor, 900.0, 400.0, Some(1024));
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .open(&file.0)
        .unwrap();
    opened.set_len(10).unwrap();
    let start = Instant::now();
    let mut jobs_at_half = 0;
    while start.elapsed() < Duration::from_millis(1500) {
        scene.clock.set(10.0 + start.elapsed().as_secs_f64());
        let mut ctx = ViewContext::default();
        scene.editor.poll_layout(&mut ctx);
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        if start.elapsed() < Duration::from_millis(750) {
            jobs_at_half = scene.frame.counts().map_jobs;
        }
        std::thread::yield_now();
    }
    let jobs = scene.frame.counts().map_jobs;
    assert!(jobs <= jobs_at_half + 1, "{jobs_at_half} then {jobs}");
}

// ---- Finding 11: a font id names the font's real data ----

#[test]
fn each_font_file_of_one_family_gets_its_own_id() {
    use qe_api::Layout;
    let engine = qe_layout::DirectWriteLayout::with_font(
        FAMILY,
        10.0,
        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
    )
    .unwrap();
    let store = qe_api::testing::StringStore::new("a \u{645}\u{631}\u{62d}\u{628}\u{627}");
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(500.0))
        .unwrap();
    // Each run's glyph ids must fit the face its id names.
    for run in &layout.rows[0].glyph_runs {
        let face = qe_layout::font_face(run.font_id).unwrap();
        let count = unsafe { face.GetGlyphCount() };
        assert!(run.glyph_ids.iter().all(|glyph| *glyph < count));
    }
    // The ids do not depend on the order, so asking again gives the same ones.
    let again = engine
        .layout_line(&store, LineIndex(0), Dip(500.0))
        .unwrap();
    assert_eq!(
        layout.rows[0].glyph_runs[0].font_id,
        again.rows[0].glyph_runs[0].font_id
    );
}

// ---- The second review ----

/// The review's own numbers: a long wrapped line, then short ones, scrolled inside the long
/// line. Line 1 is at the top, so the box must be at the map's top.
#[test]
fn the_box_shows_the_line_really_at_the_top_when_lines_wrap() {
    let mut text = "word ".repeat(3000);
    text.push('\n');
    text.push_str(&lines(200));
    let mut scene = Scene::from_text(&text, 800.0, 600.0);
    let mut ctx = ViewContext::default();
    scene.editor.set_wrap(true).unwrap();
    scene.editor.set_scroll_position(600.0, &mut ctx);
    scene
        .frame
        .sync(&mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());
    let top = scene.editor.visible_rows().rows[0].line.0;
    assert_eq!(top, 0, "the test must scroll inside the long line");
    let geometry = scene.frame.geometry().unwrap();
    assert_eq!(geometry.first_line, 0.0);
    assert_eq!(geometry.box_top(), 0.0);
}

#[test]
fn the_box_follows_the_top_line_past_the_long_line() {
    let mut text = "word ".repeat(3000);
    text.push('\n');
    text.push_str(&lines(2000));
    let mut scene = Scene::from_text(&text, 800.0, 600.0);
    scene.editor.set_wrap(true).unwrap();
    for share in [0.3, 0.6, 0.9] {
        let max = scene.editor.max_scroll_position();
        scene
            .editor
            .set_scroll_position(max * share, &mut ViewContext::default());
        scene.settle(&mut Vec::new());
        let top = scene.editor.visible_rows().rows[0].line.0 as f64;
        let first = scene.frame.geometry().unwrap().first_line;
        assert!((first - top).abs() <= 1.0, "{share}: {first} and {top}");
    }
}

#[test]
fn a_new_paper_size_wraps_the_page_again_at_once() {
    use qe_views::Paper;
    let text = "x".repeat(400);
    let second_row = |scene: &Scene| scene.editor.visible_rows().rows[1].first_byte;
    let mut letter = Scene::from_text(&text, 900.0, 700.0);
    let mut ctx = ViewContext::default();
    letter
        .frame
        .set_paper(Paper::LETTER, &mut letter.editor, &letter.measure, &mut ctx);
    letter
        .frame
        .set_page_wrap(true, &mut letter.editor, &letter.measure, &mut ctx)
        .unwrap();
    letter.settle(&mut Vec::new());
    let on_letter = second_row(&letter);
    let mut ctx = ViewContext::default();
    letter
        .frame
        .set_paper(Paper::A4, &mut letter.editor, &letter.measure, &mut ctx);
    assert!(
        ctx.requests
            .iter()
            .any(|r| matches!(r, ViewRequest::Invalidate(_)))
    );
    letter.settle(&mut Vec::new());
    let on_a4 = second_row(&letter);
    assert_ne!(on_letter, on_a4);

    let mut fresh = Scene::from_text(&text, 900.0, 700.0);
    let mut ctx = ViewContext::default();
    fresh
        .frame
        .set_paper(Paper::A4, &mut fresh.editor, &fresh.measure, &mut ctx);
    fresh
        .frame
        .set_page_wrap(true, &mut fresh.editor, &fresh.measure, &mut ctx)
        .unwrap();
    fresh.settle(&mut Vec::new());
    assert_eq!(on_a4, second_row(&fresh));
}

// ---- Breaks that first survived ----

#[test]
fn switching_the_map_off_tells_the_worker_to_stop() {
    let mut scene = Scene::from_text(&lines(20_000), 900.0, 400.0);
    start_a_map_job(&mut scene, 0.5);
    let running = scene.frame.map_worker_newest_job();
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_document_map(false, &mut scene.editor, &scene.measure, &mut ctx);
    assert!(scene.frame.map_worker_newest_job() > running);
}

#[test]
fn a_second_button_during_a_map_drag_opens_no_menu() {
    let mut scene = Scene::from_text(&lines(5000), 900.0, 400.0);
    let map = scene.frame.map_area();
    let mut repaint = Vec::new();
    scene.send(
        down(pt(map.x.0 + 20.0, 100.0), MouseButton::Left),
        &mut repaint,
    );
    let mut ctx = ViewContext::default();
    scene.frame.handle(
        &mut scene.editor,
        &InputEvent {
            time_us: 0,
            kind: down(pt(300.0, 200.0), MouseButton::Right),
        },
        &scene.measure,
        &mut ctx,
    );
    assert!(
        !ctx.requests
            .iter()
            .any(|request| matches!(request, ViewRequest::ContextMenu(_))),
        "{:?}",
        ctx.requests
    );
}

#[test]
fn a_map_drag_beats_a_button_held_since_before_it() {
    let mut scene = Scene::from_text(&lines(5000), 900.0, 400.0);
    let map = scene.frame.map_area();
    let mut repaint = Vec::new();
    scene.send(down(pt(300.0, 200.0), MouseButton::Right), &mut repaint);
    scene.send(
        down(pt(map.x.0 + 20.0, 100.0), MouseButton::Left),
        &mut repaint,
    );
    let before = scene.editor.scroll_offset();
    scene.send(
        InputKind::MouseMove {
            at: pt(map.x.0 + 20.0, 300.0),
        },
        &mut repaint,
    );
    assert!(scene.editor.scroll_offset() > before);
}

// ---- The second review: the tab width ----

#[test]
fn a_new_tab_width_reaches_the_map_at_once() {
    let text = "\tx\n".repeat(50);
    let mut scene = Scene::from_text(&text, 900.0, 400.0);
    let left = scene.frame.map_area().x.0;
    let block_x = |scene: &Scene| {
        scene
            .list(ThemeKind::Light)
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRects { rects, .. } => Some(rects.clone()),
                _ => None,
            })
            .flatten()
            .find(|rect| rect.x.0 >= left)
            .map(|rect| rect.x.0)
            .unwrap()
    };
    let at_eight = block_x(&scene);
    let mut ctx = ViewContext::default();
    let jobs = scene.frame.counts().map_jobs;
    scene
        .frame
        .set_tab_width(4, &mut scene.editor, &scene.measure, &mut ctx);
    assert!(
        scene.frame.counts().map_jobs > jobs,
        "the map waited for another call"
    );
    scene.settle(&mut Vec::new());
    assert!(block_x(&scene) < at_eight);
}
