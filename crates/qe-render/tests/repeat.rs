//! The same list gives the same pixels on every thread and in every order.
//!
//! This test compares images inside one process. DirectWrite itself sometimes gives a
//! glyph different coverage in a new process, as the README says, so this test cannot
//! compare across processes. The example `repeat_stress` does that.
mod support;
use qe_api::{Color, Dip, DisplayList, DrawCommand, Renderer};
use qe_render::{BitmapRenderer, TextSettings};
use std::{sync::Arc, thread};
use support::*;

fn commands(dark: bool) -> DisplayList {
    let (bg, ink) = if dark {
        (DARK_BG, WHITE)
    } else {
        (WHITE, BLACK)
    };
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 600.0, 120.0), bg);
    for (x, title, alpha) in [
        (16.0f32, "Untitled", 255u8),
        (215.667, "Example.rs", 0x9e),
        (415.333, "A longer tab name", 0x9e),
    ] {
        list.push(text_run(
            UI,
            13.0,
            title,
            point(x, 28.0),
            Color { a: alpha, ..ink },
        ));
        list.push(text_run(MONO, 13.0, title, point(x, 58.0), ink));
        list.push(text_run(UI, 13.0, "日本語", point(x, 88.0), ink));
        list.push(text_run(EMOJI, 13.0, "😀", point(x + 60.0, 88.0), BLACK));
        list.push(DrawCommand::Icon {
            at: point(x + 120.0, 88.0),
            icon_id: "E710".into(),
            font_size: Dip(12.0),
            color: ink,
        });
    }
    list
}
fn draw(dpi: u32, dark: bool, reference: bool) -> Vec<u8> {
    let s = dpi as f32 / 96.0;
    let mut r = BitmapRenderer::new((600.0 * s) as u32, (120.0 * s) as u32).unwrap();
    r.set_dpi(dpi).unwrap();
    if reference {
        r.set_text_settings(TextSettings::reference()).unwrap();
    }
    frame(&mut r, &commands(dark));
    r.bitmap().unwrap().bgra
}
const CASES: [(u32, bool, bool); 8] = [
    (96, false, true),
    (96, true, true),
    (144, false, true),
    (144, true, true),
    (192, false, true),
    (192, true, true),
    (144, false, false),
    (144, true, false),
];
#[test]
fn every_thread_and_order_draws_the_same_pixels() {
    let wanted: Arc<Vec<Vec<u8>>> = Arc::new(CASES.iter().map(|c| draw(c.0, c.1, c.2)).collect());
    let threads: Vec<_> = (0..8)
        .map(|order| {
            let wanted = wanted.clone();
            thread::spawn(move || {
                for round in 0..6 {
                    for step in 0..CASES.len() {
                        // Each thread and round takes the cases in its own order, so
                        // each new renderer fills its caches in its own order.
                        let at = (step * (2 * order + 1) + round) % CASES.len();
                        let case = CASES[at];
                        assert!(
                            draw(case.0, case.1, case.2) == wanted[at],
                            "thread {order} case {case:?}"
                        );
                    }
                }
            })
        })
        .collect();
    for one in threads {
        one.join().unwrap();
    }
}
#[test]
fn a_warm_renderer_and_a_new_one_agree_in_any_order_of_glyphs() {
    // A long-lived renderer that has seen other text first.
    let mut warm = BitmapRenderer::new(600, 120).unwrap();
    warm.set_text_settings(TextSettings::reference()).unwrap();
    for text in ["longer tab name A", "name tab A longer", "A", "AAAA"] {
        let mut other = DisplayList::default();
        fill(&mut other, rect(0.0, 0.0, 600.0, 120.0), WHITE);
        other.push(text_run(UI, 13.0, text, point(13.7, 28.0), BLACK));
        warm.invalidate_all();
        frame(&mut warm, &other);
    }
    let list = commands(false);
    warm.invalidate_all();
    frame(&mut warm, &list);
    assert!(warm.bitmap().unwrap().bgra == draw(96, false, true));
}
