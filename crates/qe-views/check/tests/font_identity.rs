//! A glyph run must draw with the face that shaped it. A `TextRun` keeps its own face, so
//! the two pictures below match only when `qe_layout::font_face` returns the true face.
//!
//! This fails on any PC with Windows Terminal installed, until `face_key` in
//! `crates/qe-layout/src/fonts.rs` tells two files of one family apart. The README has the
//! change. Windows and Terminal each bring a Cascadia Mono of a different glyph count, and
//! today both get one font id. Arabic then draws as wrong letters, on screen and in a PDF.
use qe_api::testing::StringStore;
use qe_api::{Color, Dip, DisplayList, DrawCommand, Layout, LineIndex, Point, TextMeasurer};
use qe_layout::DirectWriteLayout;
use qe_views_check::*;
use std::collections::HashSet;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

fn dark_pixels(list: &DisplayList) -> usize {
    let mut paper = renderer(800.0, 100.0, 96);
    paint(&mut paper, list, &mut HashSet::new());
    let bitmap = paper.bitmap().unwrap();
    bitmap
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[1] < 128)
        .count()
}

#[test]
fn arabic_in_cascadia_mono_draws_the_same_through_a_glyph_run_and_a_text_run() {
    let text = "\u{645}\u{631}\u{62d}\u{628}\u{627}";
    let engine = DirectWriteLayout::with_font(FAMILY, 30.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
    let store = StringStore::new(text);
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(700.0))
        .unwrap();
    let background = DrawCommand::FillRect {
        rect: rect(800.0, 100.0),
        color: Color::rgb(255, 255, 255),
    };
    let mut glyphs = DisplayList::default();
    glyphs.push(background.clone());
    for run in &layout.rows[0].glyph_runs {
        glyphs.push(DrawCommand::GlyphRun {
            at: Point {
                x: Dip(10.0 + run.origin.x.0),
                y: Dip(10.0 + run.origin.y.0),
            },
            bidi_level: run.bidi_level,
            font_id: run.font_id,
            font_em_size: run.font_em_size,
            glyph_ids: run.glyph_ids.clone(),
            advances: run.advances.clone(),
            offsets: run.offsets.clone(),
            color: Color::rgb(0, 0, 0),
        });
    }
    let metrics = engine.measure(text, "Cascadia Mono 30");
    let mut plain = DisplayList::default();
    plain.push(background);
    plain.push(DrawCommand::TextRun {
        at: Point {
            x: Dip(10.0),
            y: Dip(10.0 + metrics.baseline.0),
        },
        text: text.into(),
        font: FAMILY.into(),
        font_size: Dip(40.0),
        color: Color::rgb(0, 0, 0),
    });
    let (a, b) = (dark_pixels(&glyphs), dark_pixels(&plain));
    let difference = a.abs_diff(b);
    assert!(
        difference * 10 < b,
        "glyph run {a} dark pixels, text run {b}"
    );
}
