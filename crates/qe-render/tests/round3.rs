//! Tests for the third M5 round: the emoji crash, its neighbours, and tests that catch
//! each deliberate break listed in that round's brief.
mod support;
use qe_api::{Bitmap, Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::{BitmapRenderer, ResizeFault, TextSettings};
use support::*;
use windows::Win32::{
    Foundation::POINT,
    Graphics::{
        DirectWrite::*,
        Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
    },
};

const FONT: u32 = 1;
const NAN: f32 = f32::NAN;
fn renderer(width: u32, height: u32) -> BitmapRenderer {
    let mut r = BitmapRenderer::new(width, height).unwrap();
    r.register_font(FONT, MONO).unwrap();
    r
}
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

// ---- Blocker 1: emoji layers in the text's own color.
#[test]
fn every_emoji_draws_in_every_default_text_color_on_both_paths() {
    const COLORS: [Color; 4] = [
        Color::rgb(0, 0, 0),
        Color::rgb(255, 255, 255),
        Color::rgb(0xe6, 0xe6, 0xe6),
        Color::rgb(0x21, 0x21, 0x21),
    ];
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    let emoji_face = face(EMOJI);
    r.register_font_face(FONT, &emoji_face);
    let mut with_ink = 0;
    for point in (0x1F300u32..=0x1F9FF).chain(0x2600..=0x26FF) {
        let Some(c) = char::from_u32(point) else {
            continue;
        };
        let text = c.to_string();
        let mut id = 0u16;
        unsafe { emoji_face.GetGlyphIndices(&point, 1, &mut id).unwrap() };
        for ink in COLORS {
            let mut commands = vec![text_run(UI, 14.0, &text, point_at(4.0, 30.0), ink)];
            if id != 0 {
                commands.push(DrawCommand::GlyphRun {
                    at: point_at(30.0, 30.0),
                    bidi_level: 0,
                    font_id: FONT,
                    font_em_size: Dip(14.0),
                    glyph_ids: vec![id],
                    advances: vec![Dip(14.0)],
                    offsets: vec![Point::default()],
                    color: ink,
                });
            }
            again(&mut r, &on(64.0, 64.0, Color::rgb(128, 128, 128), commands));
            assert!(
                r.draw_errors().is_empty(),
                "U+{point:X}: {:?}",
                r.draw_errors()
            );
        }
        if inked(&r.bitmap().unwrap(), Color::rgb(128, 128, 128)) > 0 {
            with_ink += 1;
        }
    }
    assert!(with_ink > 1000, "only {with_ink} code points drew");
}
#[test]
fn a_run_that_mixes_color_and_plain_glyphs_keeps_its_ink_after_many_layer_colors() {
    // Layers can push other inks out of the ink list. The plain glyph after them must
    // still have the run's own color.
    let (ids, _) = glyphs(EMOJI, 20.0, "😀1🎮1🏁1🎅1");
    let count = ids.len();
    let ink = Color::rgb(10, 200, 30);
    let run = DrawCommand::GlyphRun {
        at: point_at(4.0, 40.0),
        bidi_level: 0,
        font_id: FONT,
        font_em_size: Dip(20.0),
        glyph_ids: ids,
        advances: vec![Dip(24.0); count],
        offsets: vec![Point::default(); count],
        color: ink,
    };
    let mut r = BitmapRenderer::new(240, 60).unwrap();
    r.register_font(FONT, EMOJI).unwrap();
    let list = on(240.0, 60.0, WHITE, vec![run]);
    frame(&mut r, &list);
    let first = r.bitmap().unwrap();
    // Fill the ink list with other colors, so that every ink slot gets replaced.
    for i in 0..600u32 {
        let other = on(
            240.0,
            60.0,
            WHITE,
            vec![text_run(
                MONO,
                13.0,
                "x",
                point_at(4.0, 20.0),
                Color::rgb((i & 255) as u8, (i >> 3) as u8, 9),
            )],
        );
        again(&mut r, &other);
    }
    again(&mut r, &list);
    assert_eq!(difference(&r.bitmap().unwrap(), &first), 0);
    assert_eq!(difference(&first, &fresh_emoji(&list)), 0);
}
fn fresh_emoji(list: &DisplayList) -> Bitmap {
    fresh(240, 60, 96, list, &[(FONT, EMOJI)])
}

// ---- Item 2: the first baseline of text with a line break.
#[test]
fn text_with_a_line_break_keeps_its_first_baseline() {
    for family in [UI, MONO] {
        let top = |text: &str| {
            let list = on(
                200.0,
                80.0,
                WHITE,
                vec![text_run(family, 13.0, text, point_at(4.0, 20.0), BLACK)],
            );
            ink_box(&fresh(200, 80, 96, &list, &[]), WHITE).unwrap().1
        };
        assert_eq!(top("abc"), top("abc\ndef"), "{family}");
        assert_eq!(top("abc"), top("abc\ndef\nghi\njkl"), "{family}");
    }
}

// ---- Item 3: very large glyphs.
#[test]
fn very_large_glyphs_draw_at_every_zoom_a_window_can_reach() {
    for family in [UI, MONO, EMOJI] {
        let size_at = |em: f32| {
            let list = on(
                2400.0,
                2400.0,
                WHITE,
                vec![text_run(family, em, "Q", point_at(40.0, em), BLACK)],
            );
            ink_box(&fresh(2400, 2400, 96, &list, &[]), WHITE)
                .unwrap_or_else(|| panic!("{family} at {em} dips drew nothing"))
        };
        let base = size_at(300.0);
        let base_height = (base.3 - base.1) as f32;
        for em in [600.0f32, 700.0, 800.0, 1000.0, 1300.0, 1600.0, 1800.0] {
            let found = size_at(em);
            let height = (found.3 - found.1) as f32;
            let want = base_height * em / 300.0;
            assert!(
                (height - want).abs() < want * 0.03,
                "{family} at {em}: {found:?}, want {want}"
            );
        }
    }
}
#[test]
fn a_size_too_large_to_hold_is_named_and_the_rest_draws() {
    let mut r = BitmapRenderer::new(100, 40).unwrap();
    r.register_font(FONT, MONO).unwrap();
    let list = on(
        100.0,
        40.0,
        WHITE,
        vec![
            glyph_run(FONT, MONO, 60000.0, "Q", point_at(2.0, 30.0), BLACK),
            glyph_run(FONT, MONO, 13.0, "ok", point_at(2.0, 30.0), BLACK),
        ],
    );
    r.begin_frame().unwrap();
    let message = r.draw(&list).unwrap_err().message;
    r.end_frame().unwrap();
    assert!(message.contains("too large"), "{message}");
    assert!(inked(&r.bitmap().unwrap(), WHITE) > 0);
}
#[test]
fn a_glyph_larger_than_the_cache_limit_draws_and_stays_out_of_the_cache() {
    let mut r = renderer(600, 600);
    r.set_glyph_cache_limit(10_000);
    let list = on(
        600.0,
        600.0,
        WHITE,
        vec![glyph_run(
            FONT,
            MONO,
            500.0,
            "Q",
            point_at(10.0, 450.0),
            BLACK,
        )],
    );
    frame(&mut r, &list);
    assert!(inked(&r.bitmap().unwrap(), WHITE) > 1000);
    assert!(
        r.glyph_cache_bytes() <= 10_000,
        "{} bytes",
        r.glyph_cache_bytes()
    );
}

// ---- Item 4: a failed resize leaves a safe state.
fn fill_list(w: f32, h: f32, c: Color) -> DisplayList {
    on(w, h, c, vec![])
}
#[test]
fn a_resize_that_fails_in_directwrite_keeps_the_old_bitmap_working() {
    let mut r = renderer(32, 32);
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    r.fail_next_resize(ResizeFault::Resize);
    assert!(r.resize(4000, 4000).is_err());
    let list = on(
        32.0,
        32.0,
        WHITE,
        vec![glyph_run(FONT, MONO, 13.0, "Q", point_at(2.0, 20.0), BLACK)],
    );
    again(&mut r, &list);
    let image = r.bitmap().unwrap();
    assert_eq!((image.width, image.height), (32, 32));
    assert!(inked(&image, WHITE) > 0);
    r.resize(4000, 4000).unwrap();
    frame(&mut r, &fill_list(4000.0, 4000.0, WHITE));
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 0);
}
#[test]
fn a_lost_bitmap_draws_nothing_reports_it_and_recovers_on_the_next_resize() {
    let mut r = renderer(32, 32);
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    r.fail_next_resize(ResizeFault::Lookup);
    assert!(r.resize(4000, 4000).is_err());
    let list = fill_list(4000.0, 4000.0, BLACK);
    r.invalidate_all();
    r.invalidate(rect(0.0, 0.0, 10.0, 10.0));
    r.scroll(rect(0.0, 0.0, 10.0, 10.0), Dip(1.0)).unwrap();
    r.begin_frame().unwrap();
    let message = r.draw(&list).unwrap_err().message;
    r.end_frame().unwrap();
    assert!(message.contains("bitmap"), "{message}");
    assert!(r.bitmap().is_err());
    assert_eq!(r.frame_stats().pixels, 0);
    r.resize(64, 64).unwrap();
    frame(&mut r, &fill_list(64.0, 64.0, WHITE));
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 0);
    assert_eq!(r.bitmap().unwrap().width, 64);
}

// ---- Item 5: a frame with no draw keeps its work.
#[test]
fn a_frame_with_no_draw_keeps_the_pending_repaint() {
    let mut r = BitmapRenderer::new(16, 16).unwrap();
    frame(&mut r, &fill_list(16.0, 16.0, WHITE));
    r.invalidate(rect(0.0, 0.0, 16.0, 4.0));
    r.begin_frame().unwrap();
    r.end_frame().unwrap();
    frame(&mut r, &fill_list(16.0, 16.0, BLACK));
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 16 * 4, "begin, end");
    r.invalidate(rect(0.0, 8.0, 16.0, 4.0));
    r.begin_frame().unwrap();
    r.begin_frame().unwrap();
    r.draw(&fill_list(16.0, 16.0, BLACK)).unwrap();
    r.end_frame().unwrap();
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 16 * 8, "begin, begin");
    assert_eq!(r.frame_stats().regions, 1);
    frame(&mut r, &fill_list(16.0, 16.0, Color::rgb(9, 9, 9)));
    assert_eq!(
        r.frame_stats().pixels,
        0,
        "a drawn frame leaves nothing behind"
    );
}

// ---- Item 6: pixels that nothing painted yet.
#[test]
fn text_on_never_painted_pixels_blends_with_black_and_does_not_write_black() {
    let blue = Color::rgb(0, 0, 255);
    let commands = |first: Vec<DrawCommand>| {
        let mut list = DisplayList { commands: first };
        fill(&mut list, rect(0.0, 0.0, 30.0, 40.0), blue);
        // Long enough to start on the blue fill and run past it.
        list.push(text_run(
            MONO,
            13.0,
            "White text past blue",
            point_at(2.0, 25.0),
            WHITE,
        ));
        list
    };
    let unpainted = fresh(200, 40, 96, &commands(vec![]), &[]);
    let painted = fresh(
        200,
        40,
        96,
        &commands(vec![DrawCommand::FillRect {
            rect: rect(0.0, 0.0, 200.0, 40.0),
            color: BLACK,
        }]),
        &[],
    );
    assert_eq!(difference(&unpainted, &painted), 0);
    // White on black has bright pixels past the fill.
    let bright = (40..200)
        .flat_map(|x| (0..40).map(move |y| (x, y)))
        .filter(|&(x, y)| pixel(&unpainted, x, y)[1] > 128)
        .count();
    assert!(bright > 100, "{bright} bright pixels");
}

// ---- Item 7: not-a-number positions.
#[test]
fn a_command_with_a_position_that_is_not_a_number_is_skipped_and_named() {
    let bad = [NAN, f32::INFINITY, f32::NEG_INFINITY];
    let black = BLACK;
    for v in bad {
        let at = point_at(v, 10.0);
        let at_y = point_at(10.0, v);
        let mut cases: Vec<(&str, DrawCommand)> = vec![
            (
                "fill x",
                DrawCommand::FillRect {
                    rect: rect(v, 0.0, 20.0, 20.0),
                    color: black,
                },
            ),
            (
                "fill width",
                DrawCommand::FillRect {
                    rect: rect(0.0, 0.0, v, 20.0),
                    color: black,
                },
            ),
            (
                "fills",
                DrawCommand::FillRects {
                    rects: vec![rect(0.0, v, 20.0, 20.0)],
                    color: black,
                },
            ),
            (
                "round",
                DrawCommand::RoundedRect {
                    rect: rect(v, 0.0, 20.0, 20.0),
                    radius: Dip(2.0),
                    color: black,
                },
            ),
            (
                "round radius",
                DrawCommand::RoundedRect {
                    rect: rect(0.0, 0.0, 20.0, 20.0),
                    radius: Dip(v),
                    color: black,
                },
            ),
            (
                "stroke",
                DrawCommand::StrokeRect {
                    rect: rect(0.0, v, 20.0, 20.0),
                    radius: Dip(2.0),
                    width: Dip(1.0),
                    color: black,
                },
            ),
            (
                "stroke width",
                DrawCommand::StrokeRect {
                    rect: rect(0.0, 0.0, 20.0, 20.0),
                    radius: Dip(2.0),
                    width: Dip(v),
                    color: black,
                },
            ),
            (
                "line",
                DrawCommand::Line {
                    from: at,
                    to: point_at(10.0, 10.0),
                    width: Dip(1.0),
                    color: black,
                },
            ),
            (
                "line width",
                DrawCommand::Line {
                    from: point_at(0.0, 0.0),
                    to: point_at(10.0, 10.0),
                    width: Dip(v),
                    color: black,
                },
            ),
            (
                "wavy",
                DrawCommand::WavyUnderline {
                    from: point_at(0.0, 0.0),
                    to: at_y,
                    color: black,
                },
            ),
            ("text", text_run(MONO, 13.0, "Q", at, black)),
            ("text y", text_run(MONO, 13.0, "Q", at_y, black)),
            (
                "text size",
                text_run(MONO, v, "Q", point_at(1.0, 10.0), black),
            ),
            ("glyph", glyph_run(FONT, MONO, 13.0, "Q", at, black)),
            (
                "glyph size",
                glyph_run(FONT, MONO, v, "Q", point_at(1.0, 10.0), black),
            ),
            (
                "icon",
                DrawCommand::Icon {
                    at,
                    icon_id: "E710".into(),
                    font_size: Dip(12.0),
                    color: black,
                },
            ),
            (
                "icon size",
                DrawCommand::Icon {
                    at: point_at(1.0, 10.0),
                    icon_id: "E710".into(),
                    font_size: Dip(v),
                    color: black,
                },
            ),
        ];
        let mut advance = glyph_run(FONT, MONO, 13.0, "QQ", point_at(1.0, 10.0), black);
        if let DrawCommand::GlyphRun { advances, .. } = &mut advance {
            advances[0] = Dip(v);
        }
        cases.push(("advance", advance));
        let mut offset = glyph_run(FONT, MONO, 13.0, "QQ", point_at(1.0, 10.0), black);
        if let DrawCommand::GlyphRun { offsets, .. } = &mut offset {
            offsets[0] = point_at(0.0, v);
        }
        cases.push(("offset", offset));
        for (name, command) in cases {
            let list = on(
                40.0,
                40.0,
                WHITE,
                vec![
                    command,
                    DrawCommand::FillRect {
                        rect: rect(30.0, 30.0, 4.0, 4.0),
                        color: Color::rgb(1, 2, 3),
                    },
                ],
            );
            let mut r = renderer(40, 40);
            r.begin_frame().unwrap();
            let message = r.draw(&list).map_err(|e| e.message).expect_err(name);
            r.end_frame().unwrap();
            assert!(message.contains("not a finite number"), "{name}: {message}");
            let image = r.bitmap().unwrap();
            assert_eq!(
                pixel(&image, 31, 31),
                [3, 2, 1],
                "{name}: the rest still draws"
            );
            assert_eq!(inked(&image, WHITE), 16, "{name}: the bad command drew");
        }
    }
}
#[test]
fn a_bad_clip_or_opacity_hides_its_contents_and_is_named() {
    for open in [
        DrawCommand::PushClip(rect(NAN, 0.0, 40.0, 40.0)),
        DrawCommand::PushOpacity(NAN),
    ] {
        let close = if matches!(open, DrawCommand::PushClip(_)) {
            DrawCommand::PopClip
        } else {
            DrawCommand::PopOpacity
        };
        let list = on(
            40.0,
            40.0,
            WHITE,
            vec![
                open,
                DrawCommand::FillRect {
                    rect: rect(0.0, 0.0, 20.0, 20.0),
                    color: BLACK,
                },
                close,
            ],
        );
        let mut r = renderer(40, 40);
        r.begin_frame().unwrap();
        let message = r.draw(&list).unwrap_err().message;
        r.end_frame().unwrap();
        assert!(message.contains("not a finite number"), "{message}");
        assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 0);
    }
}
#[test]
fn a_bad_rectangle_for_invalidate_or_scroll_is_reported() {
    let mut r = renderer(40, 40);
    frame(&mut r, &fill_list(40.0, 40.0, WHITE));
    assert!(r.scroll(rect(NAN, 0.0, 40.0, 40.0), Dip(4.0)).is_err());
    assert!(r.scroll(rect(0.0, 0.0, 40.0, 40.0), Dip(NAN)).is_err());
    assert!(r.scroll_pixels(rect(0.0, NAN, 40.0, 40.0), 4).is_err());
    r.invalidate(rect(NAN, 0.0, 10.0, 10.0));
    r.begin_frame().unwrap();
    let message = r.draw(&fill_list(40.0, 40.0, BLACK)).unwrap_err().message;
    r.end_frame().unwrap();
    assert!(message.contains("not a finite number"), "{message}");
    // Nothing can say what it meant to repaint, so everything is repainted.
    assert_eq!(inked(&r.bitmap().unwrap(), BLACK), 0);
    frame(&mut r, &fill_list(40.0, 40.0, WHITE));
    assert!(r.draw_errors().is_empty());
}

// ---- Item 8: the ends of the light table.
#[test]
fn full_coverage_gives_the_exact_ink_and_no_coverage_keeps_the_exact_background() {
    for settings in [
        TextSettings::reference(),
        BitmapRenderer::new(1, 1).unwrap().text_settings(),
    ] {
        for level in [0u8, 1, 2, 3, 4, 5, 8, 128, 254, 255] {
            let ink = Color::rgb(level, level, level);
            for bg in [
                Color::rgb(255 - level, 255 - level, 255 - level),
                Color::rgb(90, 90, 90),
            ] {
                let mut r = BitmapRenderer::new(200, 200).unwrap();
                r.set_text_settings(settings).unwrap();
                r.register_font(FONT, UI).unwrap();
                // The bar of a large capital I has a solid middle.
                let list = on(
                    200.0,
                    200.0,
                    bg,
                    vec![glyph_run(FONT, UI, 160.0, "I", point_at(40.0, 170.0), ink)],
                );
                frame(&mut r, &list);
                let image = r.bitmap().unwrap();
                let found =
                    ink_box(&image, bg).unwrap_or_else(|| panic!("level {level}: nothing drew"));
                let (x, y) = ((found.0 + found.2) / 2, (found.1 + found.3) / 2);
                assert_eq!(pixel(&image, x, y), [level; 3], "level {level} on {bg:?}");
                assert_eq!(pixel(&image, 2, 2), [bg.b, bg.g, bg.r]);
            }
        }
    }
}
#[test]
fn a_partly_covered_pixel_keeps_a_channel_with_no_coverage_exact() {
    // Near-black backgrounds share one light value at gamma 2.2. A channel that has no
    // coverage at a glyph's edge must not lose its level.
    let bg = Color::rgb(2, 1, 3);
    let mut r = BitmapRenderer::new(100, 60).unwrap();
    r.set_text_settings(TextSettings::reference()).unwrap();
    r.register_font(FONT, MONO).unwrap();
    frame(
        &mut r,
        &on(
            100.0,
            60.0,
            bg,
            vec![glyph_run(
                FONT,
                MONO,
                30.0,
                "Q",
                point_at(10.0, 40.0),
                WHITE,
            )],
        ),
    );
    let image = r.bitmap().unwrap();
    for y in 0..60 {
        for x in 0..100 {
            let p = pixel(&image, x, y);
            assert!(p[0] >= 3 && p[1] >= 1 && p[2] >= 2, "{p:?} at {x},{y}");
        }
    }
}

// ---- Deliberate breaks: least recently used, on the plain and color paths.
fn cache_keeps_a_run_that_stays_in_use(make: impl Fn(f32) -> DrawCommand) {
    let mut r = BitmapRenderer::new(400, 200).unwrap();
    r.register_font(FONT, EMOJI).unwrap();
    let list = |size: f32| on(400.0, 200.0, WHITE, vec![make(size)]);
    frame(&mut r, &list(30.0));
    let bytes = r.glyph_cache_bytes();
    assert!(bytes > 0);
    r.set_glyph_cache_limit(bytes * 5);
    for step in 0..30 {
        // Other sizes fill the cache, and the size in use comes back between them.
        again(&mut r, &list(31.0 + step as f32 / 4.0));
        again(&mut r, &list(30.0));
        assert_eq!(
            r.frame_stats().glyphs_rasterized,
            0,
            "step {step}: a glyph in use left the cache"
        );
    }
}
#[test]
fn the_glyph_cache_keeps_plain_glyphs_that_stay_in_use() {
    cache_keeps_a_run_that_stays_in_use(|size| {
        glyph_run(FONT, EMOJI, size, "12345", point_at(4.0, 100.0), BLACK)
    });
}
#[test]
fn the_glyph_cache_keeps_color_layers_that_stay_in_use() {
    cache_keeps_a_run_that_stays_in_use(|size| {
        glyph_run(FONT, EMOJI, size, "😀🎮🏁", point_at(4.0, 100.0), BLACK)
    });
}

// ---- Deliberate breaks: every other cache has a limit.
#[test]
fn the_shape_cache_has_a_byte_limit() {
    let mut r = BitmapRenderer::new(32, 32).unwrap();
    let mut peak = 0;
    for i in 0..40 {
        let text = format!("{i:03}{}", "x".repeat(40_000));
        again(
            &mut r,
            &DisplayList {
                commands: vec![text_run(MONO, 13.0, &text, point_at(1.0, 20.0), BLACK)],
            },
        );
        peak = peak.max(r.cache_sizes().shape_bytes);
    }
    assert!(peak > 1_000_000, "{peak} bytes: the test filled too little");
    assert!(peak <= 8 * 1024 * 1024, "{peak} bytes");
}
#[test]
fn the_ink_list_has_a_limit() {
    let mut r = BitmapRenderer::new(32, 32).unwrap();
    for i in 0..600u32 {
        let c = Color::rgb((i & 255) as u8, (i >> 2) as u8, 7);
        again(
            &mut r,
            &DisplayList {
                commands: vec![text_run(MONO, 13.0, "Q", point_at(1.0, 20.0), c)],
            },
        );
    }
    let inks = r.cache_sizes().inks;
    assert!(inks > 10 && inks <= 256, "{inks} inks");
}
#[test]
fn the_color_layer_map_has_a_limit() {
    let mut r = BitmapRenderer::new(32, 32).unwrap();
    r.register_font(FONT, EMOJI).unwrap();
    r.set_glyph_cache_limit(100_000);
    let (ids, _) = glyphs(EMOJI, 1.0, "1");
    let mut peak = 0;
    for i in 0..4_500u32 {
        // Each size is a new key, and a digit has no layers to draw.
        let em = 3.0 + i as f32 / 64.0;
        let list = DisplayList {
            commands: vec![DrawCommand::GlyphRun {
                at: point_at(1.0, 20.0),
                bidi_level: 0,
                font_id: FONT,
                font_em_size: Dip(em),
                glyph_ids: ids.clone(),
                advances: vec![Dip(0.0)],
                offsets: vec![Point::default()],
                color: BLACK,
            }],
        };
        again(&mut r, &list);
        peak = peak.max(r.cache_sizes().color_glyphs);
    }
    assert!(peak > 1000, "{peak}: the test filled too little");
    assert!(peak <= 4096, "{peak} color entries");
}

// ---- Deliberate breaks: new settings leave nothing old behind.
#[test]
fn new_text_settings_match_a_new_renderer_with_those_settings() {
    let cases = [
        // New contrast and gamma change only the inks.
        TextSettings {
            gamma: 1.2,
            enhanced_contrast: 0.0,
            ..TextSettings::reference()
        },
        // A new geometry changes only the glyphs.
        TextSettings {
            geometry: DWRITE_PIXEL_GEOMETRY_BGR,
            ..TextSettings::reference()
        },
        TextSettings {
            cleartype_level: 0.0,
            ..TextSettings::reference()
        },
    ];
    for ink in [BLACK, DARK_INK] {
        for settings in cases {
            let list = on(
                200.0,
                40.0,
                DARK_BG,
                vec![glyph_run(
                    FONT,
                    MONO,
                    13.0,
                    "QuickEdit",
                    point_at(2.0, 20.0),
                    ink,
                )],
            );
            let mut r = renderer(200, 40);
            r.set_text_settings(TextSettings::reference()).unwrap();
            frame(&mut r, &list);
            let old = r.bitmap().unwrap();
            r.set_text_settings(settings).unwrap();
            again(&mut r, &list);
            let new = r.bitmap().unwrap();
            let mut other = renderer(200, 40);
            other.set_text_settings(settings).unwrap();
            frame(&mut other, &list);
            assert_eq!(
                difference(&new, &other.bitmap().unwrap()),
                0,
                "{settings:?}"
            );
            assert_ne!(difference(&new, &old), 0, "{settings:?} changed nothing");
        }
    }
}

// ---- Deliberate breaks: regions.
#[test]
fn a_scroll_between_two_draws_of_one_frame_moves_the_frame_regions() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &fill_list(64.0, 64.0, WHITE));
    r.invalidate(rect(0.0, 20.0, 64.0, 8.0));
    r.begin_frame().unwrap();
    r.draw(&fill_list(64.0, 64.0, WHITE)).unwrap();
    r.scroll_pixels(rect(0.0, 0.0, 64.0, 64.0), 16).unwrap();
    // A second layer paints only where the frame's regions are now.
    r.draw(&fill_list(64.0, 64.0, BLACK)).unwrap();
    r.end_frame().unwrap();
    let image = r.bitmap().unwrap();
    assert_eq!(
        pixel(&image, 3, 22),
        [255, 255, 255],
        "the old place of the region"
    );
    assert_eq!(
        pixel(&image, 3, 38),
        [0, 0, 0],
        "the new place of the region"
    );
    assert_eq!(pixel(&image, 3, 50), [255, 255, 255], "below the region");
    assert_eq!(pixel(&image, 3, 8), [0, 0, 0], "the revealed strip");
}
/// A row of stripes per column band: `left` and `right` stay, `middle` scrolls.
fn bands(
    middle_offset: f32,
    edit_left_right: Option<f32>,
    edit_middle: Option<f32>,
) -> DisplayList {
    let mut list = DisplayList::default();
    let stripes = |list: &mut DisplayList, x: f32, w: f32, offset: f32| {
        for row in -8..24 {
            let top = row as f32 * 4.0 + offset;
            let c = (row + 8) as u8 * 7;
            fill(list, rect(x, top, w, 4.0), Color::rgb(c, 255 - c, 120));
        }
    };
    list.push(DrawCommand::PushClip(rect(0.0, 0.0, 20.0, 64.0)));
    stripes(&mut list, 0.0, 20.0, 0.0);
    list.push(DrawCommand::PopClip);
    list.push(DrawCommand::PushClip(rect(44.0, 0.0, 20.0, 64.0)));
    stripes(&mut list, 44.0, 20.0, 0.0);
    list.push(DrawCommand::PopClip);
    list.push(DrawCommand::PushClip(rect(20.0, 0.0, 24.0, 64.0)));
    stripes(&mut list, 20.0, 24.0, middle_offset);
    if let Some(y) = edit_middle {
        fill(&mut list, rect(20.0, y, 24.0, 8.0), Color::rgb(255, 0, 255));
    }
    list.push(DrawCommand::PopClip);
    if let Some(y) = edit_left_right {
        fill(&mut list, rect(0.0, y, 20.0, 8.0), Color::rgb(255, 0, 255));
        fill(&mut list, rect(44.0, y, 20.0, 8.0), Color::rgb(255, 0, 255));
    }
    list
}
#[test]
fn a_pending_rectangle_keeps_its_left_and_right_parts_outside_a_scroll() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &bands(0.0, None, None));
    // The row changes across the whole width. Then only the middle scrolls.
    r.invalidate(rect(0.0, 20.0, 64.0, 8.0));
    r.scroll(rect(20.0, 0.0, 24.0, 64.0), Dip(8.0)).unwrap();
    let list = bands(8.0, Some(20.0), Some(28.0));
    frame(&mut r, &list);
    assert_eq!(
        difference(&r.bitmap().unwrap(), &fresh(64, 64, 96, &list, &[])),
        0
    );
}
#[test]
fn a_pending_rectangle_moved_by_a_scroll_stays_inside_the_scrolled_part() {
    let mut r = BitmapRenderer::new(64, 64).unwrap();
    frame(&mut r, &fill_list(64.0, 64.0, WHITE));
    // Near the bottom of the scrolled part, so the move would carry it out past the end.
    r.invalidate(rect(0.0, 28.0, 64.0, 4.0));
    r.scroll(rect(0.0, 0.0, 64.0, 32.0), Dip(8.0)).unwrap();
    frame(&mut r, &fill_list(64.0, 64.0, BLACK));
    let image = r.bitmap().unwrap();
    for y in 32..64 {
        assert_eq!(
            pixel(&image, 3, y),
            [255, 255, 255],
            "row {y} is outside the scrolled part"
        );
    }
}
#[test]
fn a_pending_rectangle_at_the_bottom_of_the_bitmap_moved_by_a_scroll_is_safe() {
    let mut r = BitmapRenderer::new(32, 32).unwrap();
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    r.invalidate(rect(0.0, 28.0, 32.0, 4.0));
    r.scroll(rect(0.0, 0.0, 32.0, 32.0), Dip(10.0)).unwrap();
    frame(&mut r, &fill_list(32.0, 32.0, BLACK));
    assert_eq!(inked(&r.bitmap().unwrap(), WHITE), 32 * 10);
}
#[test]
fn resize_forgets_damage_from_the_old_size() {
    let mut r = BitmapRenderer::new(100, 100).unwrap();
    frame(&mut r, &fill_list(100.0, 100.0, WHITE));
    assert!(!r.damage_rects().is_empty());
    r.resize(40, 30).unwrap();
    assert!(r.damage_rects().is_empty(), "{:?}", r.damage_rects());
    frame(&mut r, &fill_list(40.0, 30.0, WHITE));
    for d in r.damage_rects() {
        assert!(d.right <= 40 && d.bottom <= 30, "{d:?}");
    }
}
#[test]
fn invalidate_rounds_outward_so_every_touched_pixel_repaints() {
    let mut r = BitmapRenderer::new(40, 40).unwrap();
    r.set_dpi(120).unwrap();
    let scale = 1.25;
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    // In device pixels this runs from 5.25 to 8.25, and each edge would round inward.
    r.invalidate(rect(5.25 / scale, 5.25 / scale, 3.0 / scale, 3.0 / scale));
    frame(&mut r, &fill_list(32.0, 32.0, BLACK));
    let image = r.bitmap().unwrap();
    assert_eq!(pixel(&image, 8, 8), [0, 0, 0], "the last touched pixel");
    assert_eq!(pixel(&image, 5, 5), [0, 0, 0], "the first touched pixel");
    assert_eq!(pixel(&image, 9, 9), [255, 255, 255]);
    assert_eq!(pixel(&image, 4, 4), [255, 255, 255]);
}

// ---- The caller's rule for ink that overhangs its row.
fn tall_mark_row(mark: bool) -> DrawCommand {
    let mut run = glyph_run(FONT, UI, 13.0, "x\u{00B4}x", point_at(4.0, 46.0), BLACK);
    if let DrawCommand::GlyphRun { offsets, .. } = &mut run
        && mark
    {
        // Shaped marks stack like this. It reaches well into the row above.
        offsets[1] = point_at(0.0, 18.0);
    }
    run
}
#[test]
fn ink_bounds_gives_the_rectangle_a_row_redraw_must_invalidate() {
    let row_1_text = glyph_run(FONT, UI, 13.0, "above", point_at(4.0, 20.0), BLACK);
    let list = |mark: bool| {
        on(
            120.0,
            60.0,
            WHITE,
            vec![row_1_text.clone(), tall_mark_row(mark)],
        )
    };
    let draw = |r: &mut BitmapRenderer, l: &DisplayList| {
        r.register_font(FONT, UI).unwrap();
        frame(r, l);
    };
    let (old, new) = (list(true), list(false));
    let wanted = fresh(120, 60, 96, &new, &[(FONT, UI)]);
    // Invalidating only the row of the edited text leaves the old mark behind.
    let mut r = BitmapRenderer::new(120, 60).unwrap();
    draw(&mut r, &old);
    r.invalidate(rect(0.0, 34.0, 120.0, 17.0));
    draw(&mut r, &new);
    assert_ne!(
        difference(&r.bitmap().unwrap(), &wanted),
        0,
        "the row alone should go stale"
    );
    // The rule: invalidate the ink bounds of the command before and after the edit.
    let mut r = BitmapRenderer::new(120, 60).unwrap();
    draw(&mut r, &old);
    for command in [tall_mark_row(true), tall_mark_row(false)] {
        let bounds: Rect = r.ink_bounds(&command).unwrap().unwrap();
        r.invalidate(bounds);
    }
    draw(&mut r, &new);
    assert_eq!(difference(&r.bitmap().unwrap(), &wanted), 0);
}
#[test]
fn ink_bounds_holds_all_ink_of_each_kind_of_text_at_every_scale() {
    let commands = [
        glyph_run(FONT, MONO, 30.0, "Qgy", point_at(20.0, 60.0), BLACK),
        text_run(UI, 30.0, "Qgy日本", point_at(20.0, 60.0), BLACK),
        text_run(EMOJI, 30.0, "😀🎮", point_at(20.0, 60.0), BLACK),
        DrawCommand::Icon {
            at: point_at(20.0, 60.0),
            icon_id: "E710".into(),
            font_size: Dip(30.0),
            color: BLACK,
        },
    ];
    for dpi in [96u32, 120, 144, 168, 192] {
        let s = dpi as f32 / 96.0;
        for command in &commands {
            let mut r = renderer((300.0 * s) as u32, (120.0 * s) as u32);
            r.set_dpi(dpi).unwrap();
            let bounds = r.ink_bounds(command).unwrap().unwrap();
            let list = on(300.0, 120.0, WHITE, vec![command.clone()]);
            frame(&mut r, &list);
            let found = ink_box(&r.bitmap().unwrap(), WHITE).unwrap();
            let (l, t) = (bounds.x.0 * s, bounds.y.0 * s);
            let (right, bottom) = (
                (bounds.x.0 + bounds.width.0) * s,
                (bounds.y.0 + bounds.height.0) * s,
            );
            assert!(
                l <= found.0 as f32 && t <= found.1 as f32,
                "{command:?} {bounds:?} {found:?}"
            );
            assert!(
                right >= found.2 as f32 && bottom >= found.3 as f32,
                "{command:?} {bounds:?} {found:?}"
            );
        }
    }
    let mut r = renderer(32, 32);
    assert!(
        r.ink_bounds(&text_run(
            "No Such Font",
            13.0,
            "x",
            point_at(0.0, 0.0),
            BLACK
        ))
        .is_err()
    );
    assert!(r.ink_bounds(&DrawCommand::PopClip).unwrap().is_none());
}

// ---- A scroll that is nearly a whole pixel.
fn text_rows(offset: f32, width: f32, height: f32) -> DisplayList {
    let mut commands = vec![];
    let first = (offset / 17.0).floor() as i32 - 2;
    for row in first..first + (height / 17.0) as i32 + 5 {
        commands.push(glyph_run(
            FONT,
            MONO,
            13.0,
            "The quick brown fox",
            point_at(2.0, row as f32 * 17.0 + 14.0 - offset),
            BLACK,
        ));
    }
    on(width, height, WHITE, commands)
}
#[test]
fn a_shift_near_a_whole_pixel_does_not_add_up_to_a_tear() {
    // 0.802 dips at 125% is 1.0025 pixels: close to whole, but it adds up.
    let mut r = renderer(200, 200);
    r.set_dpi(120).unwrap();
    let (w, h) = (160.0, 160.0);
    frame(&mut r, &text_rows(0.0, w, h));
    let mut offset = 0.0f32;
    for step in 1..=240 {
        r.scroll(rect(0.0, 0.0, w, h), Dip(-0.802)).unwrap();
        offset = step as f32 * 0.802;
        frame(&mut r, &text_rows(offset, w, h));
        if step % 20 == 0 {
            let wrong = difference(
                &r.bitmap().unwrap(),
                &fresh(200, 200, 120, &text_rows(offset, w, h), &[(FONT, MONO)]),
            );
            assert_eq!(wrong, 0, "{wrong} pixels at step {step}");
        }
    }
    assert!(offset > 190.0);
}
#[test]
fn whole_pixel_scrolls_stay_exact_for_a_long_time_at_every_fractional_scale() {
    for dpi in [120u32, 144, 168] {
        let s = dpi as f32 / 96.0;
        let (w, h) = (200.0 / s, 200.0 / s);
        let mut r = renderer(200, 200);
        r.set_dpi(dpi).unwrap();
        frame(&mut r, &text_rows(0.0, w, h));
        for step in 1..=1500u32 {
            // The rule from the README: keep a count of pixels, and divide each time.
            r.scroll(rect(0.0, 0.0, w, h), Dip(-1.0 / s)).unwrap();
            let offset = step as f32 / s;
            let list = text_rows(offset, w, h);
            frame(&mut r, &list);
            assert_eq!(
                r.frame_stats().pixels,
                200,
                "dpi {dpi}: step {step} must copy"
            );
            if step % 250 == 0 {
                let wrong = difference(
                    &r.bitmap().unwrap(),
                    &fresh(200, 200, dpi, &list, &[(FONT, MONO)]),
                );
                assert_eq!(wrong, 0, "{wrong} pixels at dpi {dpi} step {step}");
            }
        }
    }
}

// ---- Settings from the system and its monitors.
#[test]
fn text_settings_refresh_from_the_monitor_and_repaint_only_on_a_change() {
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut r = renderer(32, 32);
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    let system = r.text_settings();
    assert!(!r.refresh_text_settings_for_monitor(monitor).unwrap() || r.text_settings() != system);
    let actual = r.text_settings();
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    // No change: no repaint.
    assert!(!r.refresh_text_settings_for_monitor(monitor).unwrap());
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    assert_eq!(r.frame_stats().pixels, 0);
    // A change brings back the monitor's values and asks for a repaint.
    r.set_text_settings(TextSettings {
        gamma: 1.1,
        ..actual
    })
    .unwrap();
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    assert!(r.refresh_text_settings_for_monitor(monitor).unwrap());
    assert_eq!(r.text_settings(), actual);
    frame(&mut r, &fill_list(32.0, 32.0, WHITE));
    assert_eq!(r.frame_stats().pixels, 32 * 32);
}
#[test]
fn text_settings_refresh_works_for_a_window() {
    let mut r = renderer(32, 32);
    let window = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow() };
    r.refresh_text_settings_for_window(window).unwrap();
    assert!(!r.refresh_text_settings_for_window(window).unwrap());
}

// ---- Ligatures: the same answer as qe-layout on both paths.
#[test]
fn ascii_in_a_fixed_pitch_font_has_no_ligatures_on_either_path() {
    let family = "Cascadia Code";
    let mut probe = BitmapRenderer::new(4, 4).unwrap();
    if probe.register_font(FONT, family).is_err() {
        println!("skipped: {family} is not installed");
        return;
    }
    // Direct glyph lookup draws each character on its own, with no ligature. Non-ASCII
    // text on the end sends the same characters through text layout.
    let draw = |text: &str| {
        let list = on(
            120.0,
            30.0,
            WHITE,
            vec![
                DrawCommand::PushClip(rect(0.0, 0.0, 60.0, 30.0)),
                text_run(family, 16.0, text, point_at(2.0, 20.0), BLACK),
                DrawCommand::PopClip,
            ],
        );
        fresh(120, 30, 96, &list, &[])
    };
    assert_eq!(
        difference(&draw("=> != ->"), &draw("=> != ->日本語日本語日本語")),
        0
    );
}
fn point_at(x: f32, y: f32) -> Point {
    point(x, y)
}

// ---- Does the filter's margin matter? A strip holding only a glyph's outermost row or
// column of ink must still draw that ink. Set QE_RENDER_ALL_GLYPHS to try every glyph.
#[test]
fn a_strip_with_only_the_outermost_ink_of_a_glyph_still_draws_it() {
    let every = std::env::var_os("QE_RENDER_ALL_GLYPHS").is_some();
    for family in [UI, MONO, EMOJI, "Segoe Fluent Icons"] {
        let font = face(family);
        let total = unsafe { font.GetGlyphCount() };
        let step = if every { 1 } else { (total / 150).max(1) };
        let mut r = BitmapRenderer::new(160, 100).unwrap();
        r.register_font(FONT, family).unwrap();
        r.set_glyph_cache_limit(1 << 20);
        // Segoe UI glyph 5220 needs the margin for the ClearType filter: with no margin,
        // its outermost column of ink is lost. Found by trying every glyph.
        let known = (family == UI && total > 5220).then_some(5220u16);
        for id in (0..total).step_by(step as usize).chain(known) {
            for (em, x, y) in [(13.0f32, 50.5f32, 50.5f32), (20.0, 50.25, 50.0)] {
                let list = on(
                    160.0,
                    100.0,
                    WHITE,
                    vec![DrawCommand::GlyphRun {
                        at: point_at(x, y),
                        bidi_level: 0,
                        font_id: FONT,
                        font_em_size: Dip(em),
                        glyph_ids: vec![id],
                        advances: vec![Dip(0.0)],
                        offsets: vec![Point::default()],
                        color: BLACK,
                    }],
                );
                again(&mut r, &list);
                let full = r.bitmap().unwrap();
                let Some(found) = ink_box(&full, WHITE) else {
                    continue;
                };
                let (l, t, right, b) = (
                    found.0 as f32,
                    found.1 as f32,
                    found.2 as f32,
                    found.3 as f32,
                );
                for strip in [
                    rect(0.0, t, 160.0, 1.0),
                    rect(0.0, b - 1.0, 160.0, 1.0),
                    rect(l, 0.0, 1.0, 100.0),
                    rect(right - 1.0, 0.0, 1.0, 100.0),
                ] {
                    r.invalidate(strip);
                    frame(&mut r, &list);
                    let wrong = difference(&r.bitmap().unwrap(), &full);
                    assert_eq!(wrong, 0, "{family} glyph {id} at {em}: strip {strip:?}");
                }
            }
        }
    }
}
