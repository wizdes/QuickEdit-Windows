//! A mapped file: the map and the gutter say only what the index knows, and the UI thread
//! never waits for the disk.
use qe_api::ViewContext;
use qe_buffer::MappedStore;
use qe_editor::EditorView;
use qe_views::{LARGE_FILE_BYTES, Paper};
use qe_views_check::*;
use std::io::Write;
use std::time::{Duration, Instant};

struct Remove(std::path::PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        // A failed removal leaves a file in temp, and must not hide the test's result.
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write_file(name: &str, lines: usize) -> Remove {
    let path = std::env::temp_dir().join(format!("qe-views-{}-{name}", std::process::id()));
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    for n in 0..lines {
        writeln!(file, "line number {n} of the mapped file").unwrap();
    }
    file.flush().unwrap();
    Remove(path)
}

#[test]
fn numbers_and_map_rows_show_only_lines_the_index_knows() {
    let file = write_file("known.txt", 200_000);
    let store = MappedStore::open(&file.0).unwrap();
    store.set_test_read_delay(Duration::from_millis(5));
    let editor = EditorView::from_mapped(store).unwrap();
    let mut scene = Scene::new(editor, 900.0, 400.0, Paper::LETTER);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_line_numbers(true, &mut scene.editor, &scene.measure, &mut ctx);

    // Every sync returns at once, and every number is a real line the index has found.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut slowest = Duration::ZERO;
    loop {
        scene.clock.set(scene.clock.get() + 0.2);
        let start = Instant::now();
        let mut ctx = ViewContext::default();
        scene.editor.poll_layout(&mut ctx);
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        slowest = slowest.max(start.elapsed());
        let status = scene.editor.store_status();
        for label in scene.frame.labels() {
            assert!(
                label.number <= status.known_lines.max(1),
                "{label:?} of {status:?}"
            );
        }
        assert!(
            scene
                .frame
                .geometry()
                .is_none_or(|g| g.line_count >= status.known_lines.min(1))
        );
        if status.complete && !scene.frame.map_job_running() {
            break;
        }
        assert!(Instant::now() < deadline, "the index never finished");
        std::thread::sleep(Duration::from_millis(1));
    }
    scene.settle(&mut Vec::new());
    assert_eq!(scene.frame.labels()[0].number, 1);
    assert_eq!(scene.frame.geometry().unwrap().line_count, 200_001);
    assert!(!scene.frame.is_large());
    // Seven digits' worth of space is not needed: 200,001 has six.
    let list = scene.list(qe_api::ThemeKind::Light);
    assert!(!list.commands.is_empty());
    assert!(
        slowest < Duration::from_millis(500),
        "a sync took {slowest:?}"
    );
}

#[test]
fn a_mapped_file_over_50_mib_has_no_gutter_and_no_map() {
    // 55 MiB of real file, of which the status shows the size as the index finds it.
    let file = write_file("large.txt", 1_600_000);
    let size = std::fs::metadata(&file.0).unwrap().len();
    assert!(size > LARGE_FILE_BYTES, "{size}");
    let store = MappedStore::open(&file.0).unwrap();
    let editor = EditorView::from_mapped(store).unwrap();
    let mut scene = Scene::with_file_bytes(editor, 900.0, 400.0, Some(size));
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_line_numbers(true, &mut scene.editor, &scene.measure, &mut ctx);
    assert!(scene.frame.is_large());
    assert_eq!(scene.frame.gutter_area().width.0, 0.0);
    assert_eq!(scene.frame.map_area().width.0, 0.0);
    assert_eq!(scene.editor.text_area().width.0, 900.0);
    assert!(scene.frame.map_box().is_none());
    // No job ever starts, while the index runs or after a wait.
    for _ in 0..50 {
        scene.clock.set(scene.clock.get() + 0.2);
        let mut ctx = ViewContext::default();
        scene.editor.poll_layout(&mut ctx);
        scene
            .frame
            .sync(&mut scene.editor, &scene.measure, &mut ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(scene.frame.counts().map_jobs, 0);
}
