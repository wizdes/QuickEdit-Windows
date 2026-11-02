//! Timings for M14's editor work, run by hand in a release build:
//! `cargo test -p qe-editor --release --locked --test m14_measure -- --ignored --nocapture --test-threads=1`
//! They print numbers and check only that the text came out right.
use qe_api::{
    ByteOffset, Dip, DisplayList, InputEvent, InputKind, Modifiers, Rect, Theme, ThemeKind, View,
    ViewContext,
};
use qe_editor::{DecorationKind, EditorView, TextEdit, prepare_edits};
use qe_layout::DirectWriteLayout;
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const BACKSPACE: u16 = 0x08;

fn open(text: &str) -> EditorView {
    let mut e = EditorView::from_text(text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    e.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(800.0),
            height: Dip(600.0),
        },
        &measure,
    );
    settle(&mut e);
    e
}
fn settle(e: &mut EditorView) {
    let mut context = ViewContext::default();
    e.poll_layout(&mut context);
    for _ in 0..200_000 {
        if !e.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_micros(200));
        context.requests.clear();
        e.poll_layout(&mut context);
    }
    panic!("the view did not settle");
}
fn send(e: &mut EditorView, kind: InputKind) {
    e.handle(
        &InputEvent { time_us: 0, kind },
        &mut ViewContext::default(),
    );
}
fn press(e: &mut EditorView, code: u16, ctrl: bool) {
    send(
        e,
        InputKind::KeyDown {
            virtual_key: code,
            modifiers: Modifiers {
                ctrl,
                ..Default::default()
            },
            repeat: false,
        },
    );
}
fn ms(from: Instant) -> f64 {
    from.elapsed().as_secs_f64() * 1000.0
}
fn line() -> &'static str {
    "the quick brown fox jumps over the lazy dog 0123456789 and then some more words go here\n"
}

/// Replace All on a real 99 MB document, seven times, with the median and the worst.
#[test]
#[ignore = "a measurement, not a check"]
fn replace_all_on_100_000_matches_in_99_mb() {
    let lines = 100_000_000 / line().len();
    let text = line().repeat(lines);
    let step = lines / 100_000;
    let edits: Vec<_> = (0..100_000)
        .map(|i| {
            let at = i * step * line().len() + 4;
            TextEdit {
                range: ByteOffset(at)..ByteOffset(at + 5),
                text: "slow".to_string(),
            }
        })
        .collect();
    let mut e = open(&text);
    println!("document: {} MB, {lines} lines", text.len() / 1_000_000);
    let marks: Vec<_> = edits.iter().map(|edit| edit.range.clone()).collect();
    let (mut prepares, mut applies, mut screens, mut undos, mut undo_screens) =
        (vec![], vec![], vec![], vec![], vec![]);
    for _ in 0..7 {
        e.set_decorations(
            DecorationKind::FindMatch,
            marks.clone(),
            &mut ViewContext::default(),
        );
        let snapshot = e.worker_snapshot();
        let started = Instant::now();
        let prepared = prepare_edits(snapshot.as_ref(), &edits, &AtomicBool::new(false)).unwrap();
        prepares.push(ms(started));
        drop(snapshot);
        let mut context = ViewContext::default();
        let started = Instant::now();
        e.apply_edits(&prepared, ByteOffset(0)..ByteOffset(0), &mut context)
            .unwrap();
        drop(prepared);
        applies.push(ms(started));
        settle(&mut e);
        screens.push(ms(started));
        let started = Instant::now();
        press(&mut e, 0x5a, true);
        undos.push(ms(started));
        settle(&mut e);
        undo_screens.push(ms(started));
    }
    assert_eq!(e.line_count(), lines + 1);
    assert_eq!(e.text().unwrap().len(), text.len());
    // Edits that a changed text made useless, dropped where they stand.
    let snapshot = e.worker_snapshot();
    let unused = prepare_edits(snapshot.as_ref(), &edits, &AtomicBool::new(false)).unwrap();
    drop(snapshot);
    let started = Instant::now();
    drop(unused);
    println!(
        "dropping unused edits on this thread: {:.3} ms",
        ms(started)
    );
    for (name, mut samples) in [
        ("prepare, on a worker", prepares),
        (
            "apply and free the prepared edit, on the UI thread",
            applies,
        ),
        ("apply until the screen's layouts are in", screens),
        ("undo, on the UI thread", undos),
        ("undo until the screen's layouts are in", undo_screens),
    ] {
        samples.sort_by(f64::total_cmp);
        println!(
            "{name}: median {:.3} ms, worst {:.3} ms, of {} runs",
            samples[samples.len() / 2],
            samples[samples.len() - 1],
            samples.len()
        );
    }
}

fn key_p99(name: &str, text: &str) {
    let mut e = open(text);
    send(&mut e, InputKind::Focus);
    let measure = DirectWriteLayout::new().unwrap();
    let theme = Theme::new(ThemeKind::Light);
    let mut runs = Vec::new();
    for _ in 0..3 {
        let mut samples = Vec::new();
        for i in 0..400 {
            let started = Instant::now();
            if i % 2 == 0 {
                send(&mut e, InputKind::Char('q'));
            } else {
                press(&mut e, BACKSPACE, false);
            }
            let mut list = DisplayList::default();
            e.draw(&mut list, &theme, &measure);
            samples.push(ms(started));
            e.poll_layout(&mut ViewContext::default());
        }
        samples.sort_by(f64::total_cmp);
        runs.push((
            samples[samples.len() / 2],
            samples[(samples.len() * 99).div_ceil(100) - 1],
            samples[samples.len() - 1],
        ));
    }
    for (p50, p99, max) in runs {
        println!("key press, {name}: p50 {p50:.3} ms, p99 {p99:.3} ms, max {max:.3} ms");
    }
}

#[test]
#[ignore = "a measurement, not a check"]
fn key_press_p99_on_1_mb_and_512_mib() {
    key_p99("1 MB", &"x\n".repeat(500_000));
    let lines = (512usize << 20) / line().len();
    key_p99("512 MiB", &line().repeat(lines));
}
