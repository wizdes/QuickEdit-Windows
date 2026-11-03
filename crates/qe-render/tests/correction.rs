//! One test or more for each problem found in the M5 reviews. Each name says what
//! must hold.
#![allow(hidden_glob_reexports)]
mod support;
use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Renderer};
use qe_render::{BitmapRenderer, FontRegistry, TextSettings};
use std::time::Instant;
use support::*;
use windows::{
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::{DirectWrite::*, Gdi::*},
    },
    core::{BOOL, HSTRING, PCWSTR},
};

// \#[implement]\ writes paths that start at \windows_core\, as in qe-layout.
extern crate self as windows_core;
use windows::core::implement;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

const FONT: u32 = 1;
const RED: Color = Color {
    r: 200,
    g: 0,
    b: 0,
    a: 255,
};

fn renderer(width: u32, height: u32) -> BitmapRenderer {
    let mut r = BitmapRenderer::new(width, height).unwrap();
    r.register_font(FONT, MONO).unwrap();
    r
}
fn on_background(
    width: f32,
    height: f32,
    background: Color,
    commands: Vec<DrawCommand>,
) -> DisplayList {
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, width, height), background);
    for c in commands {
        list.push(c);
    }
    list
}

// ---- Item 1: text stays inside the clip and the damage.
#[test]
fn clip_holds_glyph_runs_text_runs_and_emoji() {
    for command in [
        glyph_run(
            FONT,
            MONO,
            13.0,
            "QuickEdit QuickEdit",
            point(4.0, 30.0),
            BLACK,
        ),
        text_run(MONO, 13.0, "QuickEdit QuickEdit", point(4.0, 30.0), BLACK),
        text_run(EMOJI, 24.0, "😀😀😀", point(4.0, 40.0), BLACK),
    ] {
        let list = on_background(
            200.0,
            60.0,
            WHITE,
            vec![
                DrawCommand::PushClip(rect(10.0, 20.0, 20.0, 12.0)),
                command,
                DrawCommand::PopClip,
            ],
        );
        let image = fresh(200, 60, 96, &list, &[(FONT, MONO)]);
        let mut inside = 0;
        let mut outside = 0;
        for y in 0..60 {
            for x in 0..200 {
                if pixel(&image, x, y) != [255, 255, 255] {
                    if (10..30).contains(&x) && (20..32).contains(&y) {
                        inside += 1;
                    } else {
                        outside += 1;
                    }
                }
            }
        }
        assert!(inside > 0, "the clip has no ink");
        assert_eq!(outside, 0, "{outside} inked pixels outside the clip");
    }
}
#[test]
fn repeated_line_redraws_do_not_change_any_pixel() {
    let mut commands = Vec::new();
    for row in 0..8 {
        commands.push(glyph_run(
            FONT,
            MONO,
            13.0,
            "The quick brown fox jumps over",
            point(2.0, 15.0 + row as f32 * 17.0),
            BLACK,
        ));
    }
    let list = on_background(300.0, 150.0, WHITE, commands);
    let mut r = renderer(300, 150);
    frame(&mut r, &list);
    let full = r.bitmap().unwrap();
    for _ in 0..3 {
        r.invalidate(rect(0.0, 34.0, 300.0, 17.0));
        frame(&mut r, &list);
        assert_eq!(difference(&r.bitmap().unwrap(), &full), 0);
    }
}

// ---- Item 2: the glyph cache and full frame speed.
fn full_text_frame(width: u32, height: u32, dark: bool) -> DisplayList {
    let (background, ink) = if dark {
        (DARK_BG, DARK_INK)
    } else {
        (WHITE, BLACK)
    };
    let mut list = DisplayList::default();
    fill(
        &mut list,
        rect(0.0, 0.0, width as f32, height as f32),
        background,
    );
    let line =
        "QuickEdit speed test 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ "
            .repeat(width as usize / 650 + 1);
    for y in (16..height).step_by(17) {
        list.push(glyph_run(
            FONT,
            MONO,
            13.33,
            &line,
            point(2.0, y as f32),
            ink,
        ));
    }
    list
}
#[test]
fn full_frame_of_glyph_runs_stays_under_a_generous_limit() {
    let limit = limit_ms(16.0, 150.0);
    for dark in [false, true] {
        let mut r = renderer(1920, 1080);
        let list = full_text_frame(1920, 1080, dark);
        frame(&mut r, &list);
        let ms = median_ms(|| {
            r.invalidate_all();
            frame(&mut r, &list);
        });
        assert!(ms < limit, "dark {dark}: {ms:.1} ms, limit {limit} ms");
    }
}
#[test]
fn color_is_not_part_of_the_glyph_key() {
    let mut r = renderer(300, 40);
    let draw = |r: &mut BitmapRenderer, ink: Color| {
        let list = on_background(
            300.0,
            40.0,
            WHITE,
            vec![glyph_run(
                FONT,
                MONO,
                13.0,
                "QuickEdit",
                point(2.0, 20.0),
                ink,
            )],
        );
        r.invalidate_all();
        frame(r, &list);
    };
    draw(&mut r, BLACK);
    assert!(r.frame_stats().glyphs_rasterized > 0);
    for ink in [
        RED,
        Color {
            r: 0,
            g: 90,
            b: 0,
            a: 255,
        },
        DARK_INK,
    ] {
        draw(&mut r, ink);
        assert_eq!(
            r.frame_stats().glyphs_rasterized,
            0,
            "a new color made new glyphs"
        );
    }
}
#[test]
fn the_glyph_cache_keeps_the_size_and_subpixel_position_apart() {
    let list = |x: f32, size: f32| {
        on_background(
            60.0,
            40.0,
            WHITE,
            vec![glyph_run(FONT, MONO, size, "Q", point(x, 24.0), BLACK)],
        )
    };
    let draw = |x: f32, size: f32| fresh(60, 40, 96, &list(x, size), &[(FONT, MONO)]);
    let mut r = renderer(60, 40);
    for (x, size) in [(4.0, 13.0), (4.5, 13.0), (4.0, 20.0)] {
        r.invalidate_all();
        frame(&mut r, &list(x, size));
        assert_eq!(difference(&r.bitmap().unwrap(), &draw(x, size)), 0);
    }
    assert_ne!(difference(&draw(4.0, 13.0), &draw(4.5, 13.0)), 0);
}
#[test]
fn the_glyph_cache_stays_under_its_byte_limit_and_keeps_recent_glyphs() {
    let mut r = renderer(600, 60);
    r.set_glyph_cache_limit(40_000);
    let list = |size: f32| {
        on_background(
            600.0,
            60.0,
            WHITE,
            vec![glyph_run(
                FONT,
                MONO,
                size,
                "abcdefgh",
                point(2.0, 40.0),
                BLACK,
            )],
        )
    };
    let mut peak = 0;
    for size in 10..50 {
        r.invalidate_all();
        frame(&mut r, &list(size as f32));
        peak = peak.max(r.glyph_cache_bytes());
        // A byte limit never empties the cache to make room.
        assert!(r.glyph_cache_entries() > 0);
    }
    assert!(peak <= 40_000, "peak {peak} bytes");
    assert!(
        r.glyph_cache_entries() >= 8,
        "the newest glyphs were dropped"
    );
    r.invalidate_all();
    frame(&mut r, &list(49.0));
    assert_eq!(
        r.frame_stats().glyphs_rasterized,
        0,
        "the last glyphs must still be cached"
    );
}

// ---- Item 3: shapes write directly to the pixels.
fn time_commands(r: &mut BitmapRenderer, commands: Vec<DrawCommand>) -> f64 {
    let list = DisplayList { commands };
    frame(r, &list);
    median_ms(|| {
        r.invalidate_all();
        frame(r, &list);
    })
}
#[test]
fn shapes_stay_under_generous_limits() {
    for (dpi, width, height, limit) in [
        (96u32, 400u32, 200u32, limit_ms(1.0, 10.0)),
        (192, 800, 400, limit_ms(2.0, 30.0)),
    ] {
        let mut r = BitmapRenderer::new(width, height).unwrap();
        r.set_dpi(dpi).unwrap();
        let ms = time_commands(
            &mut r,
            vec![DrawCommand::RoundedRect {
                rect: rect(8.0, 8.0, 160.0, 32.0),
                radius: Dip(8.0),
                color: BLACK,
            }],
        );
        assert!(ms < limit, "tab at dpi {dpi}: {ms:.3} ms, limit {limit} ms");
    }
    let mut r = BitmapRenderer::new(1920, 1080).unwrap();
    let ms = time_commands(
        &mut r,
        vec![DrawCommand::RoundedRect {
            rect: rect(10.0, 10.0, 800.0, 600.0),
            radius: Dip(8.0),
            color: BLACK,
        }],
    );
    let limit = limit_ms(5.0, 60.0);
    assert!(
        ms < limit,
        "800x600 rounded rectangle: {ms:.3} ms, limit {limit} ms"
    );
    let ms = time_commands(
        &mut r,
        vec![DrawCommand::StrokeRect {
            rect: rect(0.0, 0.0, 1920.0, 1080.0),
            radius: Dip(8.0),
            width: Dip(1.0),
            color: BLACK,
        }],
    );
    let limit = limit_ms(1.0, 10.0);
    assert!(ms < limit, "window stroke: {ms:.3} ms, limit {limit} ms");
}
#[test]
fn rounded_shapes_have_soft_corners_and_a_solid_inside() {
    let list = on_background(
        60.0,
        40.0,
        WHITE,
        vec![
            DrawCommand::RoundedRect {
                rect: rect(5.0, 5.0, 30.0, 20.0),
                radius: Dip(6.0),
                color: BLACK,
            },
            DrawCommand::StrokeRect {
                rect: rect(40.0, 5.0, 15.0, 20.0),
                radius: Dip(3.0),
                width: Dip(2.0),
                color: BLACK,
            },
        ],
    );
    let image = fresh(60, 40, 96, &list, &[]);
    assert_eq!(pixel(&image, 5, 5), [255, 255, 255], "the corner is cut");
    assert_eq!(pixel(&image, 20, 15), [0, 0, 0], "the inside is solid");
    assert_eq!(
        pixel(&image, 34, 15),
        [0, 0, 0],
        "the right side reaches the edge"
    );
    assert_eq!(pixel(&image, 35, 15), [255, 255, 255], "and stops there");
    assert_eq!(
        pixel(&image, 41, 15),
        [0, 0, 0],
        "the stroke has its left side"
    );
    assert_eq!(
        pixel(&image, 47, 15),
        [255, 255, 255],
        "and an empty inside"
    );
    assert_eq!(pixel(&image, 54, 15), [0, 0, 0], "and its right side");
    assert_eq!(pixel(&image, 47, 5), [0, 0, 0], "and its top");
    assert_eq!(pixel(&image, 47, 24), [0, 0, 0], "and its bottom");
    // Some corner pixel has partial coverage.
    let partial = (5..11).any(|i| {
        let v = pixel(&image, i, i)[0];
        v > 0 && v < 255
    });
    assert!(partial, "the corner has no soft edge");
    // Left and right mirror each other, and so do top and bottom.
    for y in 5..25 {
        for x in 5..35 {
            assert_eq!(pixel(&image, x, y), pixel(&image, 39 - x, y), "at {x},{y}");
            assert_eq!(pixel(&image, x, y), pixel(&image, x, 29 - y), "at {x},{y}");
        }
    }
}
#[test]
fn narrow_rounded_shapes_and_thick_strokes_have_the_right_shape() {
    // A pill of odd height, and a stroke so thick that it has no inside.
    let list = on_background(
        80.0,
        40.0,
        WHITE,
        vec![
            DrawCommand::RoundedRect {
                rect: rect(4.0, 4.0, 40.0, 5.0),
                radius: Dip(50.0),
                color: BLACK,
            },
            DrawCommand::StrokeRect {
                rect: rect(50.0, 4.0, 6.0, 20.0),
                radius: Dip(0.0),
                width: Dip(4.0),
                color: BLACK,
            },
        ],
    );
    let image = fresh(80, 40, 96, &list, &[]);
    assert_eq!(pixel(&image, 24, 4), [0, 0, 0]);
    assert_eq!(pixel(&image, 24, 8), [0, 0, 0]);
    assert_eq!(pixel(&image, 24, 9), [255, 255, 255]);
    assert_eq!(pixel(&image, 24, 3), [255, 255, 255]);
    for y in 4..24 {
        for x in 50..56 {
            assert_eq!(pixel(&image, x, y), [0, 0, 0], "at {x},{y}");
        }
    }
    for x in 4..44 {
        assert_eq!(pixel(&image, x, 6), pixel(&image, 47 - x, 6), "pill at {x}");
    }
}

// ---- Item 4: a run with no colored glyph is plain text.
#[test]
fn emoji_font_runs_with_no_color_glyph_draw_as_plain_text() {
    for text in [" ", "1", "a", "#", "1😀a"] {
        let list = on_background(
            80.0,
            40.0,
            WHITE,
            vec![text_run(EMOJI, 16.0, text, point(4.0, 26.0), BLACK)],
        );
        let image = fresh(80, 40, 96, &list, &[]);
        if text != " " {
            assert!(inked(&image, WHITE) > 0, "{text:?} drew nothing");
        }
    }
}

// ---- Item 5: an unknown font names itself and leaves the rest of the frame alone.
#[test]
fn an_unknown_font_is_named_once_and_the_rest_of_the_frame_draws() {
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 200.0, 60.0), WHITE);
    list.push(text_run(
        "No Such Font",
        13.0,
        "one",
        point(2.0, 15.0),
        BLACK,
    ));
    list.push(text_run(
        "No Such Font",
        13.0,
        "two",
        point(2.0, 30.0),
        BLACK,
    ));
    list.push(text_run(
        "Segoe Fluent Icons;10px",
        13.0,
        "x",
        point(2.0, 30.0),
        BLACK,
    ));
    list.push(glyph_run(
        FONT,
        MONO,
        13.0,
        "after",
        point(2.0, 50.0),
        BLACK,
    ));
    let mut r = renderer(200, 60);
    r.begin_frame().unwrap();
    let result = r.draw(&list);
    r.end_frame().unwrap();
    let message = result
        .expect_err("the failure must reach the caller")
        .message;
    assert_eq!(message.matches("No Such Font").count(), 1, "{message}");
    assert!(message.contains("Segoe Fluent Icons;10px"), "{message}");
    assert_eq!(r.draw_errors().len(), 2);
    assert!(
        inked(&r.bitmap().unwrap(), WHITE) > 0,
        "the good run still drew"
    );
    // The next list has no problem, so the error is gone.
    r.invalidate_all();
    frame(&mut r, &DisplayList::default());
    assert!(r.draw_errors().is_empty());
}
#[test]
fn a_glyph_run_with_an_unknown_id_or_bad_lengths_does_not_stop_the_frame() {
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 100.0, 40.0), WHITE);
    let mut bad = glyph_run(FONT, MONO, 13.0, "abc", point(2.0, 15.0), BLACK);
    if let DrawCommand::GlyphRun { advances, .. } = &mut bad {
        advances.pop();
    }
    list.push(bad);
    list.push(glyph_run(99, MONO, 13.0, "abc", point(2.0, 15.0), BLACK));
    list.push(glyph_run(FONT, MONO, 13.0, "good", point(2.0, 30.0), BLACK));
    let mut r = renderer(100, 40);
    r.begin_frame().unwrap();
    let message = r.draw(&list).unwrap_err().message;
    r.end_frame().unwrap();
    assert!(message.contains("font ID 99"), "{message}");
    assert!(message.contains("lengths"), "{message}");
    assert!(inked(&r.bitmap().unwrap(), WHITE) > 0);
}

// ---- Item 6: a full redraw after a DPI change or resize is never lost.
fn lines(width: f32, height: f32) -> DisplayList {
    let mut commands = vec![];
    let mut y = 14.0;
    while y < height {
        commands.push(glyph_run(
            FONT,
            MONO,
            13.0,
            "The quick brown fox",
            point(2.0, y),
            BLACK,
        ));
        y += 17.0;
    }
    on_background(width, height, WHITE, commands)
}
#[test]
fn set_dpi_then_one_invalidated_line_still_redraws_everything() {
    let mut r = renderer(200, 200);
    frame(&mut r, &lines(200.0, 200.0));
    r.set_dpi(144).unwrap();
    r.invalidate(rect(0.0, 0.0, 100.0, 10.0));
    let list = lines(200.0 / 1.5, 200.0 / 1.5);
    frame(&mut r, &list);
    assert_eq!(
        difference(
            &r.bitmap().unwrap(),
            &fresh(200, 200, 144, &list, &[(FONT, MONO)])
        ),
        0
    );
}
#[test]
fn resize_then_one_invalidated_line_still_redraws_everything() {
    let mut r = renderer(120, 120);
    frame(&mut r, &lines(120.0, 120.0));
    r.resize(240, 120).unwrap();
    r.invalidate(rect(0.0, 0.0, 60.0, 10.0));
    let list = lines(240.0, 120.0);
    frame(&mut r, &list);
    assert_eq!(
        difference(
            &r.bitmap().unwrap(),
            &fresh(240, 120, 96, &list, &[(FONT, MONO)])
        ),
        0
    );
}
#[test]
fn set_dpi_after_the_first_draw_changes_the_text_size() {
    let draw = |dpi| {
        let list = on_background(
            600.0,
            200.0,
            WHITE,
            vec![glyph_run(
                FONT,
                MONO,
                13.0,
                "QuickEdit",
                point(2.0, 30.0),
                BLACK,
            )],
        );
        let mut r = renderer(600, 200);
        frame(&mut r, &list);
        r.set_dpi(dpi).unwrap();
        r.invalidate_all();
        frame(&mut r, &list);
        let image = r.bitmap().unwrap();
        ink_box(&image, WHITE).unwrap()
    };
    let small = draw(96);
    let large = draw(192);
    assert!(
        large.2 - large.0 > (small.2 - small.0) * 19 / 10,
        "{small:?} {large:?}"
    );
}

// ---- Item 7: scroll moves what is already waiting to be redrawn.
fn stripes(offset: f32, stale_row: Option<f32>) -> DisplayList {
    let mut list = DisplayList::default();
    for row in -8..24 {
        let top = row as f32 * 4.0 + offset;
        let c = (row + 8) as u8 * 7;
        fill(
            &mut list,
            rect(0.0, top, 64.0, 4.0),
            Color {
                r: c,
                g: 255 - c,
                b: 120,
                a: 255,
            },
        );
    }
    if let Some(y) = stale_row {
        fill(
            &mut list,
            rect(0.0, y + offset, 64.0, 4.0),
            Color {
                r: 255,
                g: 0,
                b: 255,
                a: 255,
            },
        );
    }
    list
}
#[test]
fn two_scrolls_before_one_draw_match_a_full_redraw() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &stripes(0.0, None));
    r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(8.0)).unwrap();
    r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(8.0)).unwrap();
    let list = stripes(16.0, None);
    frame(&mut r, &list);
    assert_eq!(
        difference(&r.bitmap().unwrap(), &fresh(64, 64, 96, &list, &[])),
        0
    );
    r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(-8.0)).unwrap();
    r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(12.0)).unwrap();
    let list = stripes(20.0, None);
    frame(&mut r, &list);
    assert_eq!(
        difference(&r.bitmap().unwrap(), &fresh(64, 64, 96, &list, &[])),
        0
    );
}
#[test]
fn invalidate_then_scroll_redraws_the_moved_rectangle() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &stripes(0.0, None));
    r.invalidate(rect(0.0, 20.0, 64.0, 4.0));
    r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(8.0)).unwrap();
    let list = stripes(8.0, Some(20.0));
    frame(&mut r, &list);
    assert_eq!(
        difference(&r.bitmap().unwrap(), &fresh(64, 64, 96, &list, &[])),
        0
    );
    assert!(
        r.frame_stats().pixels <= 64 * 16,
        "{} pixels",
        r.frame_stats().pixels
    );
}
#[test]
fn a_scroll_of_part_of_the_window_leaves_the_rest_alone_and_splits_pending_rectangles() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &stripes(0.0, None));
    let before = r.bitmap().unwrap();
    // This rectangle lies half inside the scrolled part.
    r.invalidate(rect(0.0, 12.0, 64.0, 8.0));
    r.scroll(rect(0.0, 16.0, 64.0, 32.0), Dip(8.0)).unwrap();
    let list = stripes(0.0, None);
    let mut moved = DisplayList::default();
    moved.push(DrawCommand::PushClip(rect(0.0, 0.0, 64.0, 16.0)));
    moved.commands.extend(list.commands.iter().cloned());
    moved.push(DrawCommand::PopClip);
    moved.push(DrawCommand::PushClip(rect(0.0, 16.0, 64.0, 32.0)));
    moved.commands.extend(stripes(8.0, None).commands);
    moved.push(DrawCommand::PopClip);
    moved.push(DrawCommand::PushClip(rect(0.0, 48.0, 64.0, 16.0)));
    moved.commands.extend(list.commands.iter().cloned());
    moved.push(DrawCommand::PopClip);
    frame(&mut r, &moved);
    let image = r.bitmap().unwrap();
    assert_eq!(difference(&image, &fresh(64, 64, 96, &moved, &[])), 0);
    for y in 48..64 {
        assert_eq!(pixel(&image, 3, y), pixel(&before, 3, y));
    }
}

// ---- Item 8: scrolling does not drift at fractional scale.
#[test]
fn a_shift_that_is_not_a_whole_pixel_redraws_and_does_not_drift() {
    for dpi in [120u32, 144, 168] {
        let s = dpi as f32 / 96.0;
        let scene = |offset: f32| {
            let mut commands = vec![];
            for row in -4..16 {
                commands.push(glyph_run(
                    FONT,
                    MONO,
                    13.0,
                    "The quick brown fox",
                    point(2.0, row as f32 * 17.0 + 14.0 - offset),
                    BLACK,
                ));
            }
            on_background(200.0 / s, 200.0 / s, WHITE, commands)
        };
        let mut r = renderer(200, 200);
        r.set_dpi(dpi).unwrap();
        frame(&mut r, &scene(0.0));
        for step in 1..=4 {
            r.scroll(rect(0.0, 0.0, 200.0 / s, 200.0 / s), Dip(-17.0))
                .unwrap();
            let list = scene(step as f32 * 17.0);
            frame(&mut r, &list);
            let wrong = difference(
                &r.bitmap().unwrap(),
                &fresh(200, 200, dpi, &list, &[(FONT, MONO)]),
            );
            assert_eq!(wrong, 0, "{wrong} pixels at dpi {dpi} after scroll {step}");
        }
    }
}
#[test]
fn small_fractional_scrolls_do_not_lose_movement() {
    // Three scrolls of 0.4 dips used to copy nothing, and 1.2 dips moved one row.
    let scene = |offset: f32| stripes(offset, None);
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &scene(0.0));
    for step in 1..=3 {
        // Other damage is pending, so the old rule "nothing pending repaints all" cannot
        // hide a scroll that moved nothing.
        r.invalidate(rect(0.0, 0.0, 4.0, 4.0));
        r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(0.4)).unwrap();
        let list = scene(step as f32 * 0.4);
        frame(&mut r, &list);
        assert_eq!(
            difference(&r.bitmap().unwrap(), &fresh(64, 64, 96, &list, &[])),
            0,
            "step {step}"
        );
    }
    r.invalidate(rect(0.0, 0.0, 4.0, 4.0));
    r.scroll(rect(0.0, 0.0, 64.0, 64.0), Dip(1.2)).unwrap();
    let list = scene(2.4);
    frame(&mut r, &list);
    assert_eq!(
        difference(&r.bitmap().unwrap(), &fresh(64, 64, 96, &list, &[])),
        0
    );
}
#[test]
fn a_whole_pixel_shift_copies_and_redraws_only_the_revealed_strip() {
    let mut r = renderer(200, 200);
    r.set_dpi(120).unwrap();
    let scene = |offset: f32| {
        let mut commands = vec![];
        for row in -4..16 {
            commands.push(glyph_run(
                FONT,
                MONO,
                13.0,
                "The quick brown fox",
                point(2.0, row as f32 * 16.0 + 14.0 - offset),
                BLACK,
            ));
        }
        on_background(160.0, 160.0, WHITE, commands)
    };
    frame(&mut r, &scene(0.0));
    r.scroll_pixels(rect(0.0, 0.0, 160.0, 160.0), -20).unwrap();
    let list = scene(16.0);
    frame(&mut r, &list);
    assert_eq!(
        difference(
            &r.bitmap().unwrap(),
            &fresh(200, 200, 120, &list, &[(FONT, MONO)])
        ),
        0
    );
    assert_eq!(
        r.frame_stats().pixels,
        200 * 20,
        "only the strip is redrawn"
    );
}

// ---- Item 9: nothing pending means nothing to do.
#[test]
fn nothing_pending_draws_nothing_and_a_full_repaint_is_asked_for() {
    let mut r = renderer(32, 32);
    let mut white = DisplayList::default();
    fill(&mut white, rect(0.0, 0.0, 32.0, 32.0), WHITE);
    frame(&mut r, &white);
    let mut black = DisplayList::default();
    fill(&mut black, rect(0.0, 0.0, 32.0, 32.0), BLACK);
    frame(&mut r, &black);
    assert_eq!(r.frame_stats().pixels, 0);
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 0);
    // Empty and off-screen rectangles ask for nothing.
    r.invalidate(rect(10.0, 10.0, 0.0, 10.0));
    r.invalidate(rect(10.0, 500.0, 10.0, 10.0));
    r.invalidate(rect(-50.0, 0.0, 10.0, 10.0));
    frame(&mut r, &black);
    assert_eq!(r.frame_stats().regions, 0);
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 0);
    r.invalidate_all();
    frame(&mut r, &black);
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 32 * 32);
}
#[test]
fn a_second_draw_in_one_frame_paints_only_the_same_regions() {
    let mut r = renderer(32, 32);
    let mut white = DisplayList::default();
    fill(&mut white, rect(0.0, 0.0, 32.0, 32.0), WHITE);
    frame(&mut r, &white);
    let mut black = DisplayList::default();
    fill(&mut black, rect(0.0, 0.0, 32.0, 32.0), BLACK);
    r.invalidate(rect(0.0, 0.0, 32.0, 4.0));
    r.begin_frame().unwrap();
    r.draw(&white).unwrap();
    r.draw(&black).unwrap();
    r.end_frame().unwrap();
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 32 * 4);
}
#[test]
fn nearby_pending_rectangles_merge_and_their_number_stays_small() {
    let mut r = renderer(400, 400);
    let mut white = DisplayList::default();
    fill(&mut white, rect(0.0, 0.0, 400.0, 400.0), WHITE);
    frame(&mut r, &white);
    for y in 0..100 {
        r.invalidate(rect(0.0, y as f32 * 4.0, 400.0, 4.0));
        r.invalidate(rect(0.0, y as f32 * 4.0, 400.0, 2.0));
    }
    frame(&mut r, &white);
    assert_eq!(r.frame_stats().regions, 1);
    for i in 0..100 {
        r.invalidate(rect(
            (i % 10) as f32 * 40.0,
            (i / 10) as f32 * 40.0,
            3.0,
            3.0,
        ));
    }
    frame(&mut r, &white);
    assert!(
        r.frame_stats().regions <= 16,
        "{} regions",
        r.frame_stats().regions
    );
}
#[test]
fn overlapping_damage_does_not_blend_translucent_patches_twice() {
    // A review: a half-opacity white patch over black read 192, not 128.
    let mut r = BitmapRenderer::new(40, 40).unwrap();
    let mut black = DisplayList::default();
    fill(&mut black, rect(0.0, 0.0, 40.0, 40.0), BLACK);
    frame(&mut r, &black);
    let patch = DisplayList {
        commands: vec![DrawCommand::FillRect {
            rect: rect(0.0, 0.0, 40.0, 40.0),
            color: Color {
                r: 255,
                g: 255,
                b: 255,
                a: 128,
            },
        }],
    };
    for (first, second) in [
        (rect(5.0, 5.0, 20.0, 20.0), rect(5.0, 5.0, 20.0, 20.0)),
        (rect(5.0, 5.0, 20.0, 20.0), rect(15.0, 15.0, 20.0, 20.0)),
    ] {
        frame_clear(&mut r, &black);
        r.invalidate(first);
        r.invalidate(second);
        frame(&mut r, &patch);
        let image = r.bitmap().unwrap();
        let mut values = std::collections::BTreeSet::new();
        for y in 0..40 {
            for x in 0..40 {
                values.insert(pixel(&image, x, y)[0]);
            }
        }
        assert!(
            values.iter().all(|v| *v == 0 || (127..=129).contains(v)),
            "{values:?}"
        );
        assert_eq!(r.frame_stats().regions, 1);
    }
}
fn frame_clear(r: &mut BitmapRenderer, list: &DisplayList) {
    r.invalidate_all();
    frame(r, list);
}

// ---- Item 10: opacity and color alpha reach every kind of text.
#[test]
fn opacity_zero_inks_no_text_of_any_kind() {
    for dark in [false, true] {
        let bg = if dark { DARK_BG } else { WHITE };
        let list = on_background(
            300.0,
            60.0,
            bg,
            vec![
                DrawCommand::PushOpacity(0.0),
                glyph_run(FONT, MONO, 13.0, "QuickEdit", point(2.0, 20.0), RED),
                text_run(MONO, 13.0, "QuickEdit", point(2.0, 40.0), RED),
                text_run(EMOJI, 18.0, "😀", point(120.0, 30.0), BLACK),
                DrawCommand::Icon {
                    at: point(200.0, 30.0),
                    icon_id: "E710".into(),
                    font_size: Dip(12.0),
                    color: RED,
                },
                DrawCommand::PopOpacity,
            ],
        );
        assert_eq!(inked(&fresh(300, 60, 96, &list, &[(FONT, MONO)]), bg), 0);
    }
}
#[test]
fn half_opacity_and_half_alpha_give_about_half_the_ink() {
    let mass = |commands: Vec<DrawCommand>| {
        let list = on_background(300.0, 60.0, WHITE, commands);
        ink_mass(&fresh(300, 60, 96, &list, &[(FONT, MONO)]), WHITE) as f64
    };
    // Each layer of an emoji fades on its own, so layers that overlap give more than
    // half. There is no off-screen group to fade them as one.
    let makers: [(Maker, f64); 2] = [
        (
            Box::new(|c| glyph_run(FONT, MONO, 13.0, "QuickEdit", point(2.0, 20.0), c)),
            0.65,
        ),
        (
            Box::new(|c| text_run(EMOJI, 24.0, "😀", point(2.0, 30.0), c)),
            0.75,
        ),
    ];
    for (make, most) in makers {
        let whole = mass(vec![make(BLACK)]);
        let faded = mass(vec![
            DrawCommand::PushOpacity(0.5),
            make(BLACK),
            DrawCommand::PopOpacity,
        ]);
        let ratio = faded / whole;
        assert!(ratio > 0.3 && ratio < most, "opacity ratio {ratio}");
    }
    let whole = mass(vec![glyph_run(
        FONT,
        MONO,
        13.0,
        "QuickEdit",
        point(2.0, 20.0),
        BLACK,
    )]);
    let half = mass(vec![glyph_run(
        FONT,
        MONO,
        13.0,
        "QuickEdit",
        point(2.0, 20.0),
        Color { a: 128, ..BLACK },
    )]);
    assert!(
        half / whole > 0.3 && half / whole < 0.65,
        "alpha ratio {}",
        half / whole
    );
}

type Maker = Box<dyn Fn(Color) -> DrawCommand>;

// ---- Item 11: culling uses real extents.
#[test]
fn a_large_run_keeps_its_ink_in_a_strip_redraw() {
    let list = on_background(
        400.0,
        120.0,
        WHITE,
        vec![glyph_run(FONT, MONO, 48.0, "Qgy", point(10.0, 80.0), BLACK)],
    );
    let mut r = renderer(400, 120);
    frame(&mut r, &list);
    let full = r.bitmap().unwrap();
    for y in (0..120).step_by(6) {
        r.invalidate(rect(0.0, y as f32, 400.0, 6.0));
        frame(&mut r, &list);
        assert_eq!(difference(&r.bitmap().unwrap(), &full), 0, "strip {y}");
    }
}
#[test]
fn a_right_to_left_run_starting_past_the_right_edge_draws_leftward() {
    let (ids, advances) = glyphs(MONO, 13.0, "QuickEdit");
    let total: f32 = advances.iter().sum();
    let list = on_background(
        100.0,
        40.0,
        WHITE,
        vec![DrawCommand::GlyphRun {
            at: point(100.0 + total / 2.0, 20.0),
            bidi_level: 1,
            font_id: FONT,
            font_em_size: Dip(13.0),
            glyph_ids: ids,
            advances: advances.into_iter().map(Dip).collect(),
            offsets: vec![Point::default(); 9],
            color: BLACK,
        }],
    );
    let image = fresh(100, 40, 96, &list, &[(FONT, MONO)]);
    let found = ink_box(&image, WHITE).expect("the run drew nothing");
    assert!(found.0 > 100 - (total as u32 / 2) - 8, "{found:?}");
    assert_eq!(found.2, 100, "{found:?}");
}
#[test]
fn a_wide_diagonal_line_and_a_wavy_line_keep_their_ink_in_a_strip_redraw() {
    let list = on_background(
        200.0,
        60.0,
        WHITE,
        vec![
            DrawCommand::Line {
                from: point(0.0, 30.0),
                to: point(200.0, 30.0),
                width: Dip(20.0),
                color: RED,
            },
            DrawCommand::Line {
                from: point(20.0, 0.0),
                to: point(60.0, 60.0),
                width: Dip(12.0),
                color: Color {
                    r: 0,
                    g: 200,
                    b: 0,
                    a: 255,
                },
            },
            DrawCommand::WavyUnderline {
                from: point(10.0, 50.0),
                to: point(150.0, 50.0),
                color: BLACK,
            },
        ],
    );
    let mut r = renderer(200, 60);
    frame(&mut r, &list);
    let full = r.bitmap().unwrap();
    for y in (0..60).step_by(3) {
        r.invalidate(rect(0.0, y as f32, 200.0, 3.0));
        frame(&mut r, &list);
        assert_eq!(difference(&r.bitmap().unwrap(), &full), 0, "strip {y}");
    }
}

// ---- Item 12: no shortcut trusts a white fill that never painted white.
#[test]
fn text_after_a_clipped_or_invisible_white_fill_blends_with_the_real_background() {
    for white in [
        vec![
            DrawCommand::PushClip(rect(0.0, 0.0, 2.0, 2.0)),
            DrawCommand::FillRect {
                rect: rect(0.0, 0.0, 200.0, 60.0),
                color: WHITE,
            },
            DrawCommand::PopClip,
        ],
        vec![
            DrawCommand::PushOpacity(0.0),
            DrawCommand::FillRect {
                rect: rect(0.0, 0.0, 200.0, 60.0),
                color: WHITE,
            },
            DrawCommand::PopOpacity,
        ],
    ] {
        let mut commands = white;
        commands.push(text_run(MONO, 13.0, "QuickEdit", point(4.0, 30.0), BLACK));
        let list = on_background(200.0, 60.0, DARK_BG, commands);
        let image = fresh(200, 60, 96, &list, &[]);
        for y in 10..50 {
            for x in 3..190 {
                let p = pixel(&image, x, y);
                assert!(p[0] <= 30 && p[1] <= 30 && p[2] <= 30, "{p:?} at {x},{y}");
            }
        }
    }
}
#[test]
fn overlapping_text_on_white_does_not_paste_white_over_earlier_text() {
    // A review: 222 wrong pixels. Black over black can only add ink, so
    // every pixel of both words alone must stay at least as dark.
    let word = |x: f32| text_run(MONO, 13.0, "QuickEdit", point(x, 20.0), BLACK);
    let alone = fresh(
        120,
        30,
        96,
        &on_background(120.0, 30.0, WHITE, vec![word(4.0)]),
        &[],
    );
    let both = fresh(
        120,
        30,
        96,
        &on_background(120.0, 30.0, WHITE, vec![word(4.0), word(6.0)]),
        &[],
    );
    for y in 0..30 {
        for x in 0..120 {
            let (a, b) = (pixel(&alone, x, y), pixel(&both, x, y));
            for c in 0..3 {
                assert!(b[c] <= a[c], "at {x},{y}: alone {a:?}, both {b:?}");
            }
        }
    }
}

// ---- Item 13: touching rectangles do not overlap at fractional scale.
#[test]
fn translucent_rectangles_that_touch_show_no_bands_at_any_scale() {
    for dpi in [120u32, 144, 168] {
        let s = dpi as f32 / 96.0;
        let list = on_background(
            200.0 / s,
            200.0 / s,
            WHITE,
            vec![DrawCommand::FillRects {
                rects: (0..8)
                    .map(|i| rect(5.0, 3.3 + i as f32 * 17.3, 90.0, 17.3))
                    .collect(),
                color: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 80,
                },
            }],
        );
        let image = fresh(200, 200, dpi, &list, &[]);
        let mut values = std::collections::BTreeSet::new();
        for y in 0..200 {
            values.insert(pixel(&image, 40, y));
        }
        assert_eq!(values.len(), 2, "dpi {dpi}: {values:?}");
    }
}
#[test]
fn a_thin_rectangle_never_disappears() {
    let list = on_background(
        40.0,
        40.0,
        WHITE,
        vec![DrawCommand::FillRect {
            rect: rect(10.2, 10.2, 0.1, 10.0),
            color: BLACK,
        }],
    );
    assert!(inked(&fresh(40, 40, 96, &list, &[]), WHITE) >= 10);
}

// ---- Item 14: offsets from qe-layout keep DirectWrite's meaning.
#[derive(Clone)]
struct Collected {
    face: IDWriteFontFace,
    em: f32,
    ids: Vec<u16>,
    advances: Vec<f32>,
    offsets: Vec<DWRITE_GLYPH_OFFSET>,
    bidi: u8,
    x: f32,
    y: f32,
}
/// Collects runs in the way that crates/qe-layout/src/shaped.rs does. qe-render
/// cannot depend on qe-layout here, because that would change Cargo.lock.
#[implement(IDWriteTextRenderer)]
struct Collector {
    runs: std::sync::Arc<std::sync::Mutex<Vec<Collected>>>,
}
impl IDWritePixelSnapping_Impl for Collector_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const core::ffi::c_void) -> windows::core::Result<BOOL> {
        Ok(BOOL(1))
    }
    fn GetCurrentTransform(
        &self,
        _: *const core::ffi::c_void,
        out: *mut DWRITE_MATRIX,
    ) -> windows::core::Result<()> {
        unsafe {
            *out = DWRITE_MATRIX {
                m11: 1.0,
                m22: 1.0,
                m12: 0.0,
                m21: 0.0,
                dx: 0.0,
                dy: 0.0,
            }
        };
        Ok(())
    }
    fn GetPixelsPerDip(&self, _: *const core::ffi::c_void) -> windows::core::Result<f32> {
        Ok(1.0)
    }
}
impl IDWriteTextRenderer_Impl for Collector_Impl {
    fn DrawGlyphRun(
        &self,
        _: *const core::ffi::c_void,
        x: f32,
        y: f32,
        _: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        _: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        let run = unsafe { &*run };
        let n = run.glyphCount as usize;
        let collected = Collected {
            face: (*run.fontFace).clone().unwrap(),
            em: run.fontEmSize,
            ids: unsafe { std::slice::from_raw_parts(run.glyphIndices, n) }.to_vec(),
            advances: unsafe { std::slice::from_raw_parts(run.glyphAdvances, n) }.to_vec(),
            offsets: unsafe { std::slice::from_raw_parts(run.glyphOffsets, n) }.to_vec(),
            bidi: run.bidiLevel as u8,
            x,
            y,
        };
        self.this.runs.lock().unwrap().push(collected);
        Ok(())
    }
    fn DrawUnderline(
        &self,
        _: *const core::ffi::c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_UNDERLINE,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(
        &self,
        _: *const core::ffi::c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_STRIKETHROUGH,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawInlineObject(
        &self,
        _: *const core::ffi::c_void,
        _: f32,
        _: f32,
        _: windows::core::Ref<IDWriteInlineObject>,
        _: BOOL,
        _: BOOL,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}
fn shape(family: &str, em: f32, text: &str) -> Vec<Collected> {
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let format = unsafe {
        factory
            .CreateTextFormat(
                PCWSTR(HSTRING::from(family).as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                em,
                PCWSTR(HSTRING::from("en-US").as_ptr()),
            )
            .unwrap()
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe {
        factory
            .CreateTextLayout(&units, &format, 10000.0, 1000.0)
            .unwrap()
    };
    let runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let renderer: IDWriteTextRenderer = Collector { runs: runs.clone() }.into();
    unsafe { layout.Draw(None, &renderer, 0.0, 0.0).unwrap() };
    runs.lock().unwrap().clone()
}
#[test]
fn shaped_combining_marks_land_where_directwrite_puts_them() {
    for (family, text, right_to_left) in [
        (UI, "x\u{0301}\u{0323}q\u{0300}\u{0316}", false),
        (UI, "\u{0628}\u{064E}\u{0651}\u{0628}\u{0650}\u{0628}", true),
    ] {
        let runs = shape(family, 28.0, text);
        assert!(
            runs.iter()
                .any(|r| r.offsets.iter().any(|o| o.ascenderOffset != 0.0)),
            "{text:?} has no vertical offset"
        );
        assert_eq!(runs.iter().any(|r| r.bidi % 2 == 1), right_to_left);
        for dark in [false, true] {
            let (bg, ink) = if dark {
                (DARK_BG, DARK_INK)
            } else {
                (WHITE, BLACK)
            };
            let settings = TextSettings::reference();
            let mut r = BitmapRenderer::new(200, 100).unwrap();
            r.set_text_settings(settings).unwrap();
            let direct = Direct::new(200, 100, 96, settings, bg);
            let mut list = DisplayList::default();
            fill(&mut list, rect(0.0, 0.0, 200.0, 100.0), bg);
            let top = runs[0].y;
            for (index, run) in runs.iter().enumerate() {
                let id = 100 + index as u32;
                r.register_font_face(id, &run.face);
                let (x, y) = (20.0 + run.x, 60.0 + run.y - top);
                list.push(DrawCommand::GlyphRun {
                    at: point(x, y),
                    bidi_level: run.bidi,
                    font_id: id,
                    font_em_size: Dip(run.em),
                    glyph_ids: run.ids.clone(),
                    advances: run.advances.iter().copied().map(Dip).collect(),
                    // The same mapping as RunCollector in qe-layout's shaped.rs.
                    offsets: run
                        .offsets
                        .iter()
                        .map(|o| point(o.advanceOffset, o.ascenderOffset))
                        .collect(),
                    color: ink,
                });
                direct.draw(
                    &run.face,
                    run.em,
                    &run.ids,
                    &run.advances,
                    &run.offsets,
                    run.bidi,
                    (x, y),
                    ink,
                );
            }
            frame(&mut r, &list);
            let ours = r.bitmap().unwrap();
            let theirs = direct.bitmap();
            let (a, b) = (ink_box(&ours, bg).unwrap(), ink_box(&theirs, bg).unwrap());
            for (name, p, q) in [
                ("left", a.0, b.0),
                ("top", a.1, b.1),
                ("right", a.2, b.2),
                ("bottom", a.3, b.3),
            ] {
                assert!(
                    p.abs_diff(q) <= 2,
                    "{text:?} {name}: ours {a:?}, DirectWrite {b:?}"
                );
            }
        }
    }
}

// ---- Item 15: the same weight as DirectWrite on both paths and both themes.
#[test]
fn text_has_directwrites_own_weight_in_both_themes_and_on_both_paths() {
    let line = "The quick brown fox jumps over the lazy dog";
    let (ids, advances) = glyphs(MONO, 13.0, line);
    let offsets = vec![DWRITE_GLYPH_OFFSET::default(); ids.len()];
    let font = face(MONO);
    let system = BitmapRenderer::new(1, 1).unwrap().text_settings();
    for settings in [TextSettings::reference(), system] {
        for (bg, ink) in [
            (WHITE, BLACK),
            (DARK_BG, WHITE),
            (DARK_BG, DARK_INK),
            (
                Color {
                    r: 240,
                    g: 240,
                    b: 240,
                    a: 255,
                },
                Color {
                    r: 60,
                    g: 60,
                    b: 60,
                    a: 255,
                },
            ),
            (
                WHITE,
                Color {
                    r: 0,
                    g: 90,
                    b: 200,
                    a: 255,
                },
            ),
            (
                DARK_BG,
                Color {
                    r: 90,
                    g: 180,
                    b: 255,
                    a: 255,
                },
            ),
        ] {
            for dpi in [96u32, 144] {
                let s = dpi as f32 / 96.0;
                let direct = Direct::new(800, 40, dpi, settings, bg);
                direct.draw(&font, 13.0, &ids, &advances, &offsets, 0, (4.0, 18.0), ink);
                let theirs = ink_mass(&direct.bitmap(), bg) as f64;
                let mut r = BitmapRenderer::new(800, 40).unwrap();
                r.set_text_settings(settings).unwrap();
                r.set_dpi(dpi).unwrap();
                r.register_font(FONT, MONO).unwrap();
                let glyph_list = on_background(
                    800.0 / s,
                    40.0 / s,
                    bg,
                    vec![glyph_run(FONT, MONO, 13.0, line, point(4.0, 18.0), ink)],
                );
                frame(&mut r, &glyph_list);
                let glyph_image = r.bitmap().unwrap();
                let ours = ink_mass(&glyph_image, bg) as f64;
                let ratio = ours / theirs;
                assert!(
                    (0.95..1.05).contains(&ratio),
                    "bg {bg:?} ink {ink:?} dpi {dpi} {settings:?}: ours {ours}, DirectWrite {theirs}, ratio {ratio:.3}"
                );
                let text_list = on_background(
                    800.0 / s,
                    40.0 / s,
                    bg,
                    vec![text_run(MONO, 13.0, line, point(4.0, 18.0), ink)],
                );
                r.invalidate_all();
                frame(&mut r, &text_list);
                assert_eq!(
                    difference(&r.bitmap().unwrap(), &glyph_image),
                    0,
                    "the two paths differ"
                );
            }
        }
    }
}

#[test]
fn one_glyph_on_a_whole_pixel_matches_directwrite_within_rounding() {
    let (ids, advances) = glyphs(MONO, 24.0, "Qg");
    let offsets = vec![DWRITE_GLYPH_OFFSET::default(); ids.len()];
    let font = face(MONO);
    let system = BitmapRenderer::new(1, 1).unwrap().text_settings();
    for settings in [TextSettings::reference(), system] {
        for (bg, ink) in [
            (WHITE, BLACK),
            (DARK_BG, WHITE),
            (DARK_BG, DARK_INK),
            (
                Color {
                    r: 100,
                    g: 100,
                    b: 100,
                    a: 255,
                },
                WHITE,
            ),
            (
                WHITE,
                Color {
                    r: 128,
                    g: 128,
                    b: 128,
                    a: 255,
                },
            ),
            (
                WHITE,
                Color {
                    r: 0,
                    g: 90,
                    b: 200,
                    a: 255,
                },
            ),
            (
                DARK_BG,
                Color {
                    r: 90,
                    g: 180,
                    b: 255,
                    a: 255,
                },
            ),
        ] {
            // Only the first glyph, so that the second one's fractional place is no factor.
            let direct = Direct::new(60, 60, 96, settings, bg);
            direct.draw(
                &font,
                24.0,
                &ids[..1],
                &advances[..1],
                &offsets[..1],
                0,
                (10.0, 40.0),
                ink,
            );
            let mut r = BitmapRenderer::new(60, 60).unwrap();
            r.set_text_settings(settings).unwrap();
            r.register_font(FONT, MONO).unwrap();
            let list = on_background(
                60.0,
                60.0,
                bg,
                vec![glyph_run(FONT, MONO, 24.0, "Q", point(10.0, 40.0), ink)],
            );
            frame(&mut r, &list);
            let wrong = difference_over(&r.bitmap().unwrap(), &direct.bitmap(), 2);
            assert_eq!(wrong, 0, "bg {bg:?} ink {ink:?} {settings:?}");
        }
    }
}

// ---- Item 16: TextRun finds a font for every character.
#[test]
fn text_runs_fall_back_and_shape_like_directwrite() {
    let draw = |text: &str| {
        let list = on_background(
            120.0,
            40.0,
            WHITE,
            vec![text_run(UI, 16.0, text, point(4.0, 26.0), BLACK)],
        );
        fresh(120, 40, 96, &list, &[])
    };
    let (sun, book) = (draw("日"), draw("本"));
    assert!(inked(&sun, WHITE) > 0);
    assert!(
        difference(&sun, &book) > 20,
        "the two characters drew the same"
    );
    // Mixed text keeps every part.
    let mixed = ink_box(&draw("ab日本cd"), WHITE).unwrap();
    let plain = ink_box(&draw("abcd"), WHITE).unwrap();
    assert!(mixed.2 > plain.2 + 20, "{mixed:?} {plain:?}");
    // Arabic runs right to left and stays right of the start point.
    let arabic = ink_box(&draw("\u{0628}\u{0644}"), WHITE).unwrap();
    assert!(arabic.0 >= 3, "{arabic:?}");
}
#[test]
fn very_tall_glyphs_are_not_cropped() {
    // The old cache drew each glyph in a strip 64 pixels high, at a fixed size.
    let list = on_background(
        80.0,
        60.0,
        WHITE,
        vec![text_run(MONO, 30.0, "Qg", point(2.0, 30.0), BLACK)],
    );
    let image = fresh(400, 300, 480, &list, &[]);
    let found = ink_box(&image, WHITE).unwrap();
    // The baseline is at row 150, and the size is 150 pixels. Capitals reach about 100
    // rows above the baseline, and the tail of g hangs about 30 rows below it.
    assert!(found.1 < 150 - 80, "{found:?}");
    assert!(found.3 > 150 + 20, "{found:?}");
    assert!(found.3 - found.1 > 100, "{found:?}");
}
#[test]
fn cached_shapes_are_the_same_as_new_ones() {
    let list = on_background(
        120.0,
        40.0,
        WHITE,
        vec![text_run(UI, 16.0, "ab日本cd", point(4.0, 26.0), BLACK)],
    );
    let mut r = renderer(120, 40);
    frame(&mut r, &list);
    let first = r.bitmap().unwrap();
    r.invalidate_all();
    frame(&mut r, &list);
    assert_eq!(r.frame_stats().glyphs_rasterized, 0);
    assert_eq!(difference(&first, &r.bitmap().unwrap()), 0);
}

// ---- Item 17: icons use their size and baseline.
fn icon(at: Point, size: f32, id: &str) -> DisplayList {
    on_background(
        80.0,
        80.0,
        WHITE,
        vec![DrawCommand::Icon {
            at,
            icon_id: id.into(),
            font_size: Dip(size),
            color: BLACK,
        }],
    )
}
#[test]
fn icons_are_drawn_at_their_size_and_baseline() {
    let small = ink_box(
        &fresh(80, 80, 96, &icon(point(10.0, 40.0), 10.0, "E710"), &[]),
        WHITE,
    )
    .unwrap();
    let large = ink_box(
        &fresh(80, 80, 96, &icon(point(10.0, 40.0), 40.0, "E710"), &[]),
        WHITE,
    )
    .unwrap();
    let moved = ink_box(
        &fresh(80, 80, 96, &icon(point(10.0, 50.0), 10.0, "E710"), &[]),
        WHITE,
    )
    .unwrap();
    assert!(
        large.2 - large.0 >= (small.2 - small.0) * 3,
        "{small:?} {large:?}"
    );
    assert_eq!((moved.0, moved.1), (small.0, small.1 + 10));
    // The other spelling of an id works, and a bad one is named.
    let spelled = fresh(80, 80, 96, &icon(point(10.0, 40.0), 10.0, "U+E710"), &[]);
    assert_eq!(ink_box(&spelled, WHITE).unwrap(), small);
    let mut r = BitmapRenderer::new(80, 80).unwrap();
    r.begin_frame().unwrap();
    let message = r
        .draw(&icon(point(10.0, 40.0), 10.0, "not hex"))
        .unwrap_err()
        .message;
    assert!(message.contains("not hex"), "{message}");
}
#[test]
fn an_icon_at_the_measured_baseline_is_centered_in_its_box() {
    // What the shell and UI must do: box top + (box height - height) / 2 + baseline,
    // where height and baseline come from the measurer for the icon font and size.
    // These are the same DirectWrite calls that qe-layout's measure_text makes.
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    for (size, width) in [(12.0f32, 31.0f32), (10.0, 46.0)] {
        let format = unsafe {
            factory
                .CreateTextFormat(
                    PCWSTR(HSTRING::from("Segoe Fluent Icons").as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(HSTRING::from("en-US").as_ptr()),
                )
                .unwrap()
        };
        let units: Vec<u16> = "\u{E710}".encode_utf16().collect();
        let layout = unsafe {
            factory
                .CreateTextLayout(&units, &format, 1000.0, 1000.0)
                .unwrap()
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        let mut line = [DWRITE_LINE_METRICS::default(); 1];
        let mut count = 0;
        unsafe {
            layout.GetMetrics(&mut metrics).unwrap();
            layout.GetLineMetrics(Some(&mut line), &mut count).unwrap();
        }
        let box_top = 20.0;
        let box_height = 24.0;
        let baseline = box_top + (box_height - metrics.height) / 2.0 + line[0].baseline;
        let x = 10.0 + (width - metrics.widthIncludingTrailingWhitespace) / 2.0;
        let list = on_background(
            80.0,
            80.0,
            WHITE,
            vec![DrawCommand::Icon {
                at: point(x, baseline),
                icon_id: "E710".into(),
                font_size: Dip(size),
                color: BLACK,
            }],
        );
        let found = ink_box(&fresh(80, 80, 96, &list, &[]), WHITE).unwrap();
        let middle = (found.1 + found.3) as f32 / 2.0;
        assert!(
            (middle - (box_top + box_height / 2.0)).abs() <= 1.5,
            "size {size}: ink {found:?}, height {}, baseline {}",
            metrics.height,
            line[0].baseline
        );
        let center = (found.0 + found.2) as f32 / 2.0;
        assert!(
            (center - (10.0 + width / 2.0)).abs() <= 1.5,
            "size {size}: ink {found:?}"
        );
    }
}

// ---- Item 18: one drawing path.
#[test]
fn render_to_bitmap_matches_live_drawing_with_either_settings() {
    let commands = vec![
        glyph_run(FONT, MONO, 13.0, "QuickEdit", point(2.0, 18.0), BLACK),
        text_run(UI, 13.0, "Notes", point(2.0, 40.0), BLACK),
        text_run(EMOJI, 16.0, "😀", point(100.0, 40.0), BLACK),
    ];
    let list = on_background(200.0, 60.0, WHITE, commands);
    for reference in [false, true] {
        let mut r = renderer(200, 60);
        if reference {
            r.set_text_settings(TextSettings::reference()).unwrap();
        }
        frame(&mut r, &list);
        let live = r.bitmap().unwrap();
        let off_screen = r.render_to_bitmap(&list, 200, 60).unwrap();
        assert_eq!(difference(&live, &off_screen), 0, "reference {reference}");
    }
}

// ---- Item 19: register_fonts, the right font and the right place.
struct Names;
impl FontRegistry for Names {
    fn font(&self, id: u32) -> Option<&str> {
        match id {
            1 => Some(MONO),
            2 => Some("Consolas"),
            _ => None,
        }
    }
}
#[test]
fn register_fonts_maps_each_id_and_names_a_missing_one() {
    let mut r = BitmapRenderer::new(200, 40).unwrap();
    r.register_fonts(&Names, &[1, 2]).unwrap();
    let message = r.register_fonts(&Names, &[3]).unwrap_err().message;
    assert!(message.contains('3'), "{message}");
    // Each id draws with its own font.
    let draw = |r: &mut BitmapRenderer, id: u32| {
        let (ids, advances) = glyphs(if id == 1 { MONO } else { "Consolas" }, 16.0, "Rag");
        let list = on_background(
            200.0,
            40.0,
            WHITE,
            vec![DrawCommand::GlyphRun {
                at: point(4.0, 26.0),
                bidi_level: 0,
                font_id: id,
                font_em_size: Dip(16.0),
                glyph_ids: ids,
                advances: advances.into_iter().map(Dip).collect(),
                offsets: vec![Point::default(); 3],
                color: BLACK,
            }],
        );
        r.invalidate_all();
        frame(r, &list);
        r.bitmap().unwrap()
    };
    let (cascadia, consolas) = (draw(&mut r, 1), draw(&mut r, 2));
    assert!(difference(&cascadia, &consolas) > 30);
}
#[test]
fn resize_keeps_drawing_at_the_new_size_and_grows_past_its_capacity() {
    let mut r = BitmapRenderer::new(32, 32).unwrap();
    let color = Color {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    };
    let mut grown = DisplayList::default();
    fill(&mut grown, rect(0.0, 0.0, 1000.0, 700.0), color);
    frame(&mut r, &grown);
    r.resize(1000, 700).unwrap();
    frame(&mut r, &grown);
    let image = r.bitmap().unwrap();
    assert_eq!((image.width, image.height), (1000, 700));
    assert_eq!(inked(&image, color), 0);
    r.resize(10, 10).unwrap();
    frame(&mut r, &grown);
    assert_eq!(r.bitmap().unwrap().bgra.len(), 400);
    assert!(r.resize(0, 10).is_err());
}

// ---- Item 20: any rectangle can be presented with no new draw.
struct MemoryDc {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}
impl MemoryDc {
    fn new(width: i32, height: i32) -> Self {
        let dc = unsafe { CreateCompatibleDC(None) };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = unsafe {
            CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap()
        };
        let old = unsafe { SelectObject(dc, bitmap.into()) };
        Self { dc, bitmap, old }
    }
    fn pixel(&self, x: i32, y: i32) -> u32 {
        unsafe { GetPixel(self.dc, x, y) }.0
    }
}
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            assert!(DeleteObject(self.bitmap.into()).as_bool());
            assert!(DeleteDC(self.dc).as_bool());
        }
    }
}
#[test]
fn present_rect_copies_any_rectangle_without_a_new_draw() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    let mut list = DisplayList::default();
    fill(
        &mut list,
        rect(0.0, 0.0, 64.0, 64.0),
        Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        },
    );
    frame(&mut r, &list);
    let target = MemoryDc::new(64, 64);
    r.present_rect_to_dc(
        target.dc,
        RECT {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        },
    )
    .unwrap();
    assert_eq!(target.pixel(12, 12), 0xFF0000, "inside the rectangle");
    assert_eq!(target.pixel(30, 30), 0, "outside it");
    assert_eq!(target.pixel(9, 12), 0, "just outside it");
    // A rectangle past the edge is cut to the bitmap.
    r.present_rect_to_dc(
        target.dc,
        RECT {
            left: 60,
            top: -5,
            right: 500,
            bottom: 500,
        },
    )
    .unwrap();
    assert_eq!(target.pixel(63, 63), 0xFF0000);
}
#[test]
fn present_copies_all_damage_since_the_last_present() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    let blue = Color {
        r: 0,
        g: 0,
        b: 255,
        a: 255,
    };
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 64.0, 64.0), blue);
    frame(&mut r, &list);
    let target = MemoryDc::new(64, 64);
    r.present_to_dc(target.dc).unwrap();
    assert_eq!(target.pixel(63, 63), 0xFF0000);
    assert!(r.damage_rects().is_empty());
    let other = MemoryDc::new(64, 64);
    r.invalidate(rect(0.0, 0.0, 8.0, 8.0));
    frame(&mut r, &list);
    r.invalidate(rect(40.0, 40.0, 8.0, 8.0));
    frame(&mut r, &list);
    frame(&mut r, &list);
    assert_eq!(
        r.damage_rects().len(),
        2,
        "a later frame must not lose earlier damage"
    );
    r.present_to_dc(other.dc).unwrap();
    assert_eq!(other.pixel(1, 1), 0xFF0000);
    assert_eq!(other.pixel(41, 41), 0xFF0000);
    assert_eq!(other.pixel(20, 20), 0);
    r.scroll_pixels(rect(0.0, 16.0, 64.0, 8.0), 2).unwrap();
    assert_eq!(r.damage_rects().len(), 1, "a scroll changes pixels");
}
#[test]
fn present_reports_a_bad_window_and_keeps_its_damage() {
    let mut r = BitmapRenderer::new(16, 16).unwrap();
    frame(&mut r, &DisplayList::default());
    // A window handle that does not exist.
    let bad = HWND(0x7FFF_FFF0usize as *mut core::ffi::c_void);
    assert!(r.present(bad).is_err());
    assert!(
        r.present_rect(
            bad,
            RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4
            }
        )
        .is_err()
    );
    assert!(!r.damage_rects().is_empty());
}

// ---- Item 23.
#[test]
fn a_stroke_of_width_zero_inks_nothing() {
    let list = on_background(
        64.0,
        64.0,
        WHITE,
        vec![
            DrawCommand::StrokeRect {
                rect: rect(4.0, 4.0, 40.0, 40.0),
                radius: Dip(4.0),
                width: Dip(0.0),
                color: BLACK,
            },
            DrawCommand::StrokeRect {
                rect: rect(4.0, 4.0, 40.0, 40.0),
                radius: Dip(0.0),
                width: Dip(-1.0),
                color: BLACK,
            },
        ],
    );
    assert_eq!(inked(&fresh(64, 64, 96, &list, &[]), WHITE), 0);
}
#[test]
fn a_very_long_run_stops_at_the_right_edge() {
    let mut r = renderer(200, 40);
    let line = "abcdefghij".repeat(50_000);
    let (ids, advances) = glyphs(MONO, 13.0, &line);
    let count = ids.len();
    let list = on_background(
        200.0,
        40.0,
        WHITE,
        vec![DrawCommand::GlyphRun {
            at: point(2.0, 20.0),
            bidi_level: 0,
            font_id: FONT,
            font_em_size: Dip(13.0),
            glyph_ids: ids,
            advances: advances.into_iter().map(Dip).collect(),
            offsets: vec![Point::default(); count],
            color: BLACK,
        }],
    );
    frame(&mut r, &list);
    r.invalidate_all();
    let start = Instant::now();
    frame(&mut r, &list);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let limit = limit_ms(4.0, 30.0);
    assert!(
        ms < limit,
        "{ms:.2} ms for a run of {count} glyphs, limit {limit} ms"
    );
    assert!(
        r.frame_stats().glyphs_drawn < 100,
        "{} glyphs",
        r.frame_stats().glyphs_drawn
    );
}
#[test]
fn a_very_long_right_to_left_run_stops_at_the_left_edge() {
    let mut r = renderer(200, 40);
    let line = "abcdefghij".repeat(5_000);
    let (ids, advances) = glyphs(MONO, 13.0, &line);
    let count = ids.len();
    let list = on_background(
        200.0,
        40.0,
        WHITE,
        vec![DrawCommand::GlyphRun {
            at: point(198.0, 20.0),
            bidi_level: 1,
            font_id: FONT,
            font_em_size: Dip(13.0),
            glyph_ids: ids,
            advances: advances.into_iter().map(Dip).collect(),
            offsets: vec![Point::default(); count],
            color: BLACK,
        }],
    );
    frame(&mut r, &list);
    assert!(
        r.frame_stats().glyphs_drawn < 100,
        "{} glyphs",
        r.frame_stats().glyphs_drawn
    );
}
#[test]
fn stacks_that_do_not_balance_are_reported_and_the_rest_draws() {
    let mut list = DisplayList::default();
    list.push(DrawCommand::PopClip);
    fill(&mut list, rect(0.0, 0.0, 10.0, 10.0), BLACK);
    list.push(DrawCommand::PushOpacity(0.5));
    let mut r = BitmapRenderer::new(10, 10).unwrap();
    r.begin_frame().unwrap();
    let message = r.draw(&list).unwrap_err().message;
    r.end_frame().unwrap();
    assert!(message.contains("clip"), "{message}");
    assert!(message.contains("opacity"), "{message}");
    assert_eq!(pixel(&r.bitmap().unwrap(), 5, 5), [0, 0, 0]);
}

// ---- Item 22: the example window shows its failures.
fn example(name: &str) -> std::path::PathBuf {
    // Test binaries live in target/<profile>/deps, and examples in target/<profile>/examples.
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join(format!("{name}.exe"))
}
/// Runs the example window. `None` means this account cannot run it: the test account
/// with no desktop can get Access Denied (0x80070005) with no desktop to make a window on.
/// Only that exact failure skips, and it says so.
fn run_window(args: &[&str]) -> Option<std::process::Output> {
    let path = example("render_window");
    let output = match std::process::Command::new(&path).args(args).output() {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            println!(
                "skipped: this account may not start {}: {e}",
                path.display()
            );
            return None;
        }
        Err(e) => panic!("{} did not start: {e}", path.display()),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let denied = stderr.contains("0x80070005") && !stderr.contains("DPI awareness was not changed");
    if denied {
        println!("skipped: this account may not make a window here: {stderr}");
        return None;
    }
    Some(output)
}
#[test]
fn render_window_reports_failures_and_its_exit_code() {
    let Some(output) = run_window(&[
        "--hidden",
        "--exit-after-first-frame",
        "--font",
        "No Such Font",
    ]) else {
        return;
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("No Such Font"), "stderr: {stderr}");
    let Some(output) = run_window(&["--no-such-option"]) else {
        return;
    };
    assert_ne!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--no-such-option"));
}
#[test]
fn render_window_follows_the_suggested_rectangle_and_starts_at_the_window_dpi() {
    let Some(output) = run_window(&[
        "--hidden",
        "--exit-after-first-frame",
        "--simulate-dpi-change",
        "144",
    ]) else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("dpi 144"), "{stdout}");
    assert!(stdout.contains("window 900x600"), "{stdout}");
    assert!(stdout.contains("start dpi "), "{stdout}");
}
#[test]
fn render_window_keeps_supplementary_characters_typed_as_two_messages() {
    // A review: char::from_u32 rejected each half of a surrogate pair.
    let Some(output) = run_window(&[
        "--hidden",
        "--exit-after-first-frame",
        "--simulate-char",
        "1F600",
    ]) else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("typed 😀"), "{stdout}");
}
