//! Icons are grayscale and even, as Windows Terminal draws them.
mod support;
use qe_api::{Bitmap, Color, Dip, DisplayList, DrawCommand, Point, Renderer};
use qe_render::{BitmapRenderer, TextSettings};
use support::*;

const ICON_FONT: &str = "Segoe Fluent Icons";
const PLUS: &str = "E710";
const CARET: &str = "E96E";
const FONT: u32 = 1;
fn gray(v: u8) -> Color {
    Color {
        r: v,
        g: v,
        b: v,
        a: 255,
    }
}
fn icon(at: Point, size: f32, id: &str, color: Color) -> DrawCommand {
    DrawCommand::Icon {
        at,
        icon_id: id.into(),
        font_size: Dip(size),
        color,
    }
}
fn on(background: Color, commands: Vec<DrawCommand>) -> DisplayList {
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 200.0, 100.0), background);
    list.commands.extend(commands);
    list
}
/// The largest gap between the channels of any pixel.
fn largest_fringe(image: &Bitmap) -> u8 {
    image
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            p[0].abs_diff(p[1])
                .max(p[1].abs_diff(p[2]))
                .max(p[0].abs_diff(p[2]))
        })
        .max()
        .unwrap_or(0)
}
#[test]
fn icon_pixels_have_no_color_at_any_scale_or_theme_with_either_settings() {
    let system = BitmapRenderer::new(1, 1).unwrap().text_settings();
    for settings in [system, TextSettings::reference()] {
        for dpi in [96u32, 120, 144, 168, 192] {
            for (background, ink) in [
                (gray(43), gray(230)),
                (gray(240), gray(0)),
                (gray(38), gray(255)),
                (gray(128), gray(20)),
            ] {
                for id in [PLUS, CARET, "E921", "E922", "E923", "E8BB"] {
                    let s = dpi as f32 / 96.0;
                    let mut r =
                        BitmapRenderer::new((200.0 * s) as u32, (100.0 * s) as u32).unwrap();
                    r.set_text_settings(settings).unwrap();
                    r.set_dpi(dpi).unwrap();
                    let list = on(background, vec![icon(point(20.3, 40.7), 12.0, id, ink)]);
                    frame(&mut r, &list);
                    let image = r.bitmap().unwrap();
                    assert!(inked(&image, background) > 0, "{id} drew nothing");
                    let fringe = largest_fringe(&image);
                    assert!(
                        fringe <= 1,
                        "{id} at dpi {dpi} on {background:?}: channels differ by {fringe}"
                    );
                }
            }
        }
    }
}
#[test]
fn the_plus_is_square_and_has_one_size_wherever_it_starts() {
    for (dpi, want) in [(96u32, 11u32), (144, 15), (192, 20)] {
        let boxes: Vec<_> = [(20.0f32, 40.0f32), (20.3, 40.3), (20.7, 40.6), (21.5, 41.5)]
            .into_iter()
            .map(|(x, y)| {
                let s = dpi as f32 / 96.0;
                let mut r = BitmapRenderer::new((200.0 * s) as u32, (100.0 * s) as u32).unwrap();
                r.set_dpi(dpi).unwrap();
                frame(
                    &mut r,
                    &on(gray(43), vec![icon(point(x, y), 12.0, PLUS, gray(230))]),
                );
                ink_box(&r.bitmap().unwrap(), gray(43)).unwrap()
            })
            .collect();
        for found in boxes {
            let (w, h) = (found.2 - found.0, found.3 - found.1);
            assert_eq!((w, h), (want, want), "dpi {dpi}: {found:?}");
        }
    }
}
#[test]
fn a_plus_has_one_even_stem_and_one_even_bar_at_100_percent() {
    let mut r = BitmapRenderer::new(60, 40).unwrap();
    frame(
        &mut r,
        &on(
            gray(43),
            vec![icon(point(10.4, 30.0), 12.0, PLUS, gray(230))],
        ),
    );
    let image = r.bitmap().unwrap();
    let found = ink_box(&image, gray(43)).unwrap();
    let (center_x, center_y) = (found.0 + 5, found.1 + 5);
    // Away from its ends and its center, the stem is one column wide and all one value.
    let stem: Vec<_> = (found.1 + 1..found.3 - 1)
        .filter(|y| *y != center_y)
        .map(|y| pixel(&image, center_x, y))
        .collect();
    assert!(stem.windows(2).all(|p| p[0] == p[1]), "{stem:?}");
    let bar: Vec<_> = (found.0 + 1..found.2 - 1)
        .filter(|x| *x != center_x)
        .map(|x| pixel(&image, x, center_y))
        .collect();
    assert!(bar.windows(2).all(|p| p[0] == p[1]), "{bar:?}");
    assert_eq!(stem[0], bar[0], "the stem and bar have one weight");
    assert_eq!(pixel(&image, center_x - 1, found.1 + 2), [43, 43, 43]);
    assert_eq!(pixel(&image, center_x + 1, found.1 + 2), [43, 43, 43]);
}
#[test]
fn an_icon_and_a_glyph_run_of_the_same_font_and_size_do_not_share_a_cache_entry() {
    let glyph = glyph_run(
        FONT,
        ICON_FONT,
        12.0,
        "\u{E710}",
        point(20.0, 40.0),
        gray(230),
    );
    let plus = icon(point(20.0, 40.0), 12.0, PLUS, gray(230));
    let alone = |command: &DrawCommand| {
        let mut r = BitmapRenderer::new(200, 100).unwrap();
        r.register_font(FONT, ICON_FONT).unwrap();
        frame(&mut r, &on(gray(43), vec![command.clone()]));
        r.bitmap().unwrap()
    };
    let (glyph_alone, icon_alone) = (alone(&glyph), alone(&plus));
    assert!(
        largest_fringe(&glyph_alone) > 10,
        "a GlyphRun keeps ClearType"
    );
    assert_eq!(largest_fringe(&icon_alone), 0);
    for first in [&glyph, &plus] {
        let mut r = BitmapRenderer::new(200, 100).unwrap();
        r.register_font(FONT, ICON_FONT).unwrap();
        frame(&mut r, &on(gray(43), vec![first.clone()]));
        for (command, wanted) in [(&glyph, &glyph_alone), (&plus, &icon_alone)] {
            r.invalidate_all();
            frame(&mut r, &on(gray(43), vec![command.clone()]));
            assert_eq!(difference(&r.bitmap().unwrap(), wanted), 0);
        }
    }
}
#[test]
fn text_runs_and_glyph_runs_stay_on_cleartype() {
    let settings = TextSettings::reference();
    for command in [
        text_run(UI, 13.0, "A longer tab name", point(4.0, 30.0), gray(230)),
        glyph_run(
            FONT,
            MONO,
            13.0,
            "A longer tab name",
            point(4.0, 30.0),
            gray(230),
        ),
    ] {
        let mut r = BitmapRenderer::new(200, 100).unwrap();
        r.set_text_settings(settings).unwrap();
        r.register_font(FONT, MONO).unwrap();
        frame(&mut r, &on(gray(43), vec![command]));
        assert!(largest_fringe(&r.bitmap().unwrap()) > 10);
    }
}
#[test]
fn a_very_large_icon_still_draws_without_color() {
    let mut r = BitmapRenderer::new(1400, 1400).unwrap();
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 1400.0, 1400.0), gray(43));
    list.push(icon(point(20.0, 1200.0), 1200.0, PLUS, gray(230)));
    frame(&mut r, &list);
    let image = r.bitmap().unwrap();
    let found = ink_box(&image, gray(43)).expect("nothing drew");
    assert!(found.2 - found.0 > 900);
    assert_eq!(largest_fringe(&image), 0);
}
