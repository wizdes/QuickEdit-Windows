//! Tests for the findings of the final M5 review.
mod support;
use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Renderer};
use qe_render::{BitmapRenderer, TextSettings};
use support::*;
use windows::Win32::Graphics::DirectWrite::*;

const FONT: u32 = 1;
const RED: Color = Color {
    r: 255,
    g: 0,
    b: 0,
    a: 255,
};
fn on(width: f32, height: f32, background: Color, commands: Vec<DrawCommand>) -> DisplayList {
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, width, height), background);
    list.commands.extend(commands);
    list
}
fn again(r: &mut BitmapRenderer, list: &DisplayList) {
    r.invalidate_all();
    frame(r, list);
}

// ---- Damage covers the pixels that drawing really touches.
#[test]
fn invalidating_a_thin_rectangle_repaints_where_it_was_drawn() {
    // Drawing rounds 0.6 up to column 1. Rounding outward alone gives column 0.
    for (x, y, w, h) in [
        (0.6, 2.0, 0.1, 10.0),
        (2.0, 0.6, 10.0, 0.1),
        (0.55, 0.55, 0.3, 0.3),
    ] {
        let thin = rect(x, y, w, h);
        let list = |c: Color| {
            on(
                16.0,
                16.0,
                WHITE,
                vec![DrawCommand::FillRect {
                    rect: thin,
                    color: c,
                }],
            )
        };
        let mut r = BitmapRenderer::new(16, 16).unwrap();
        frame(&mut r, &list(BLACK));
        r.invalidate(thin);
        let red = list(RED);
        frame(&mut r, &red);
        assert_eq!(
            difference(&r.bitmap().unwrap(), &fresh(16, 16, 96, &red, &[])),
            0,
            "{thin:?}"
        );
    }
}
#[test]
fn every_rectangle_invalidated_at_every_scale_covers_what_it_draws() {
    for dpi in [96u32, 120, 144, 168, 192] {
        let s = dpi as f32 / 96.0;
        for step in 0..60 {
            let at = 1.0 + step as f32 * 0.137;
            let size = 0.05 + step as f32 * 0.043;
            let shape = rect(at, at * 0.7, size, size * 1.3);
            let list = |c: Color| {
                on(
                    40.0 / s,
                    40.0 / s,
                    WHITE,
                    vec![DrawCommand::FillRect {
                        rect: shape,
                        color: c,
                    }],
                )
            };
            let mut r = BitmapRenderer::new(40, 40).unwrap();
            r.set_dpi(dpi).unwrap();
            frame(&mut r, &list(BLACK));
            r.invalidate(shape);
            let red = list(RED);
            frame(&mut r, &red);
            let wrong = difference(&r.bitmap().unwrap(), &fresh(40, 40, dpi, &red, &[]));
            assert_eq!(wrong, 0, "dpi {dpi} {shape:?}");
        }
    }
}

// ---- A scroll copies only for a shift that is exactly a whole number of pixels.
fn blocks(offset: f32) -> DisplayList {
    on(
        40.0,
        60.0,
        WHITE,
        vec![DrawCommand::FillRect {
            rect: rect(0.0, 20.4985 + offset, 40.0, 7.0),
            color: BLACK,
        }],
    )
}
#[test]
fn a_shift_just_off_a_whole_pixel_matches_a_full_redraw() {
    // Astra's case: at 96 DPI this copy differed by 60 pixels.
    let mut r = BitmapRenderer::new(40, 60).unwrap();
    frame(&mut r, &blocks(0.0));
    r.scroll(rect(0.0, 0.0, 40.0, 60.0), Dip(1.001)).unwrap();
    let list = blocks(1.001);
    frame(&mut r, &list);
    assert_eq!(
        difference(&r.bitmap().unwrap(), &fresh(40, 60, 96, &list, &[])),
        0
    );
}
#[test]
fn a_scroll_copies_only_for_the_exact_result_of_pixels_over_scale() {
    for dpi in [96u32, 120, 144, 168, 192] {
        let s = dpi as f32 / 96.0;
        for pixels in [1i32, 2, 3, 7, 17, 100, -1, -3, -100] {
            let exact = pixels as f32 / s;
            let (w, h) = (200.0 / s, 300.0 / s);
            let full = 200 * 300;
            let mut r = BitmapRenderer::new(200, 300).unwrap();
            r.set_dpi(dpi).unwrap();
            frame(&mut r, &on(w, h, WHITE, vec![]));
            r.scroll(rect(0.0, 0.0, w, h), Dip(exact)).unwrap();
            frame(&mut r, &on(w, h, WHITE, vec![]));
            assert_eq!(
                r.frame_stats().pixels,
                200 * pixels.unsigned_abs() as u64,
                "dpi {dpi} {pixels}"
            );
            // One step of an f32 away is a different shift, and it repaints.
            for near in [exact.next_up(), exact.next_down()] {
                r.scroll(rect(0.0, 0.0, w, h), Dip(near)).unwrap();
                frame(&mut r, &on(w, h, WHITE, vec![]));
                assert_eq!(r.frame_stats().pixels, full, "dpi {dpi} {pixels} {near}");
            }
        }
    }
}

// ---- Nothing larger than a cache's limit is kept.
#[test]
fn a_glyph_over_the_cache_limit_is_drawn_and_not_kept() {
    for limit in [0usize, 100] {
        for (family, text) in [(MONO, "Q"), (EMOJI, "😀")] {
            let mut r = BitmapRenderer::new(60, 50).unwrap();
            r.set_glyph_cache_limit(limit);
            let list = on(
                60.0,
                50.0,
                WHITE,
                vec![text_run(family, 24.0, text, point(4.0, 36.0), BLACK)],
            );
            frame(&mut r, &list);
            let image = r.bitmap().unwrap();
            assert!(inked(&image, WHITE) > 0, "{family}: nothing drew");
            assert_eq!(r.glyph_cache_entries(), 0, "{family} with limit {limit}");
            assert_eq!(r.glyph_cache_bytes(), 0, "{family} with limit {limit}");
            again(&mut r, &list);
            assert_eq!(difference(&r.bitmap().unwrap(), &image), 0);
        }
    }
}
#[test]
fn a_string_over_the_shape_cache_limit_is_drawn_and_not_kept() {
    let mut r = BitmapRenderer::new(200, 30).unwrap();
    let short = on(
        200.0,
        30.0,
        WHITE,
        vec![text_run(MONO, 13.0, "kept", point(2.0, 20.0), BLACK)],
    );
    frame(&mut r, &short);
    let kept = r.cache_sizes().shape_bytes;
    assert!(kept > 0);
    let long = "x".repeat(600_000);
    let list = on(
        200.0,
        30.0,
        WHITE,
        vec![text_run(MONO, 13.0, &long, point(2.0, 20.0), BLACK)],
    );
    again(&mut r, &list);
    assert!(inked(&r.bitmap().unwrap(), WHITE) > 0);
    assert_eq!(
        r.cache_sizes().shape_bytes,
        kept,
        "the long string stays out, and nothing else leaves"
    );
}
#[test]
fn corner_masks_are_limited_by_bytes_and_a_huge_one_is_not_kept() {
    let mut r = BitmapRenderer::new(2200, 2200).unwrap();
    let mut peak = 0;
    for radius in (1..400).step_by(7) {
        let list = DisplayList {
            commands: vec![DrawCommand::RoundedRect {
                rect: rect(0.0, 0.0, 900.0, 900.0),
                radius: Dip(radius as f32),
                color: BLACK,
            }],
        };
        again(&mut r, &list);
        peak = peak.max(r.cache_sizes().corner_bytes);
    }
    assert!(peak > 100_000, "{peak} bytes: the test filled too little");
    assert!(peak <= 1024 * 1024, "{peak} bytes");
    let mut r = BitmapRenderer::new(2200, 2200).unwrap();
    let list = DisplayList {
        commands: vec![DrawCommand::RoundedRect {
            rect: rect(0.0, 0.0, 2200.0, 2200.0),
            radius: Dip(1100.0),
            color: BLACK,
        }],
    };
    frame(&mut r, &list);
    assert_eq!(r.cache_sizes().corner_bytes, 0);
    assert_eq!(pixel(&r.bitmap().unwrap(), 1100, 1100), [0, 0, 0]);
}

// ---- A run costs only the part near the clip.
fn long_run(y: f32, bidi: u8, x: f32) -> DrawCommand {
    let (ids, advances) = glyphs(MONO, 13.0, &"abcdefghij".repeat(10_000));
    let count = ids.len();
    DrawCommand::GlyphRun {
        at: point(x, y),
        bidi_level: bidi,
        font_id: FONT,
        font_em_size: Dip(13.0),
        glyph_ids: ids,
        advances: advances.into_iter().map(Dip).collect(),
        offsets: vec![Point::default(); count],
        color: BLACK,
    }
}
#[test]
fn a_long_run_walks_only_the_glyphs_near_the_clip() {
    let mut r = BitmapRenderer::new(200, 40).unwrap();
    r.register_font(FONT, MONO).unwrap();
    for (name, command) in [
        ("above", long_run(-500.0, 0, 2.0)),
        ("below", long_run(5000.0, 0, 2.0)),
        ("on screen", long_run(20.0, 0, 2.0)),
        ("right to left", long_run(20.0, 1, 198.0)),
    ] {
        again(
            &mut r,
            &DisplayList {
                commands: vec![command],
            },
        );
        let walked = r.frame_stats().glyphs_walked;
        assert!(walked < 300, "{name}: walked {walked} of 100000 glyphs");
    }
}
#[test]
fn a_mark_far_from_its_row_still_draws_in_a_strip_that_only_it_reaches() {
    let mut run = glyph_run(FONT, MONO, 13.0, "xQx", point(4.0, 90.0), BLACK);
    if let DrawCommand::GlyphRun { offsets, .. } = &mut run {
        offsets[1] = point(0.0, 70.0);
    }
    let list = on(60.0, 100.0, WHITE, vec![run]);
    let mut r = BitmapRenderer::new(60, 100).unwrap();
    r.register_font(FONT, MONO).unwrap();
    frame(&mut r, &list);
    let full = r.bitmap().unwrap();
    assert!(
        ink_box(&full, WHITE).unwrap().1 < 15,
        "the mark is near the top"
    );
    r.invalidate(rect(0.0, 0.0, 60.0, 30.0));
    frame(&mut r, &list);
    assert_eq!(difference(&r.bitmap().unwrap(), &full), 0);
}

// ---- What the switch from an outline mode looks like.
#[test]
fn large_text_has_the_weight_and_shape_of_directwrites_own_drawing() {
    let system = BitmapRenderer::new(1, 1).unwrap().text_settings();
    let (ids, advances) = glyphs(UI, 1.0, "Q");
    let offsets = [DWRITE_GLYPH_OFFSET::default()];
    let font = face(UI);
    for settings in [system, TextSettings::reference()] {
        for em in [40.0f32, 100.0, 300.0, 600.0, 800.0, 1200.0] {
            let side = (em * 1.5) as u32;
            let at = (em * 0.1, em * 1.1);
            let direct = Direct::new(side, side, 96, settings, WHITE);
            let scaled = [advances[0] * em];
            direct.draw(&font, em, &ids, &scaled, &offsets, 0, at, BLACK);
            let theirs = direct.bitmap();
            let list = on(
                side as f32,
                side as f32,
                WHITE,
                vec![DrawCommand::GlyphRun {
                    at: point(at.0, at.1),
                    bidi_level: 0,
                    font_id: FONT,
                    font_em_size: Dip(em),
                    glyph_ids: ids.clone(),
                    advances: vec![Dip(scaled[0])],
                    offsets: vec![Point::default()],
                    color: BLACK,
                }],
            );
            let mut r = BitmapRenderer::new(side, side).unwrap();
            r.set_text_settings(settings).unwrap();
            r.register_font(FONT, UI).unwrap();
            frame(&mut r, &list);
            let ours = r.bitmap().unwrap();
            let ratio = ink_mass(&ours, WHITE) as f64 / ink_mass(&theirs, WHITE) as f64;
            let far = difference_over(&ours, &theirs, 96);
            let pixels = inked(&theirs, WHITE).max(1);
            println!(
                "em {em} mode {:?}: ink ratio {ratio:.4}, {far} of {pixels} pixels far off",
                settings.mode
            );
            assert!((0.97..1.03).contains(&ratio), "em {em}: ratio {ratio}");
            // Up to 300 pixels the two match. Past that, DirectWrite's default mode draws its
            // own outlines, and edge pixels differ by up to 4% of the ink's pixels here.
            let allowed = if em <= 300.0 || settings == TextSettings::reference() {
                0
            } else {
                pixels * 4 / 100
            };
            assert!(
                far <= allowed,
                "em {em}: {far} of {pixels} pixels are far off"
            );
        }
    }
}
