//! Every picture and pixel check here goes through the real `qe-render`, with fonts named
//! directly and fixed text settings.
use qe_api::{Bitmap, Color, Dip, InputEvent, InputKind, Point, Rect, Theme, ThemeKind, View};
use qe_shell::{CaptionHit, ContrastColors, Tab, TabStrip};
use std::path::PathBuf;

mod support;
use support::*;

const WIDTH: f32 = 700.0;
const HEIGHT: f32 = 48.0;

fn contrast() -> ContrastColors {
    ContrastColors {
        window: Color::rgb(0, 0, 0),
        window_text: Color::rgb(255, 255, 255),
        highlight: Color::rgb(26, 235, 255),
        highlight_text: Color::rgb(0, 0, 0),
        gray_text: Color::rgb(63, 242, 63),
    }
}
fn theme_for(name: &str) -> Theme {
    match name {
        "light" => Theme::new(ThemeKind::Light),
        "dark" => Theme::new(ThemeKind::Dark),
        _ => {
            let c = contrast();
            let mut theme = Theme::new(ThemeKind::Dark);
            theme.text_background = c.window;
            theme.tab_background = c.window;
            theme.text = c.window_text;
            theme.dim_text = c.window_text;
            theme.border = c.window_text;
            theme
        }
    }
}
fn sample(name: &str, maximized: bool) -> TabStrip {
    let mut s = TabStrip::new(vec![
        Tab::new("Untitled"),
        Tab::new("Example.rs"),
        Tab::new("A longer tab name"),
    ]);
    s.tabs_mut()[1].dirty = true;
    if name == "contrast" {
        s.set_high_contrast(Some(contrast()));
    }
    s.set_maximized(maximized);
    s.layout(rect(0.0, 0.0, WIDTH, s.row_height()), &RealMeasurer);
    s
}
fn background(c: Color) -> [u8; 3] {
    [c.r, c.g, c.b]
}
fn device(b: Rect, scale: f32) -> (u32, u32, u32, u32) {
    (
        (b.x.0 * scale).round() as u32,
        (b.y.0 * scale).round() as u32,
        ((b.x.0 + b.width.0) * scale).round() as u32,
        ((b.y.0 + b.height.0) * scale).round() as u32,
    )
}
/// The ink's center must be within one device pixel of the box's center, on each axis.
fn assert_centered(image: &Bitmap, b: Rect, bg: [u8; 3], scale: f32, what: &str) {
    let area = device(b, scale);
    let ink = ink_box(image, area, bg, 40).unwrap_or_else(|| panic!("{what}: no ink"));
    assert!(
        ink.0 >= area.0 && ink.1 >= area.1 && ink.2 <= area.2 && ink.3 <= area.3,
        "{what}: ink {ink:?} leaves its box {area:?}"
    );
    let (cx, cy) = ((ink.0 + ink.2) as f32 / 2.0, (ink.1 + ink.3) as f32 / 2.0);
    let (bx, by) = (
        (area.0 + area.2) as f32 / 2.0,
        (area.1 + area.3) as f32 / 2.0,
    );
    let limit = 1.0;
    assert!(
        (cx - bx).abs() <= limit && (cy - by).abs() <= limit,
        "{what}: ink center ({cx}, {cy}) is off box center ({bx}, {by}); ink {ink:?}, box {area:?}"
    );
}

#[test]
fn saved_images_in_three_themes_at_three_scales() {
    for name in ["light", "dark", "contrast"] {
        for dpi in [96, 144, 192] {
            let image = render_strip(&sample(name, false), &theme_for(name), dpi, WIDTH, HEIGHT);
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/images")
                .join(format!("top-row-{name}-{dpi}.png"));
            if std::env::var_os("QE_SHELL_UPDATE_IMAGES").is_some() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, encode_png(&image)).unwrap();
            }
            let saved = decode_own_png(&std::fs::read(&path).unwrap());
            let current = raw_pixels(&image);
            assert_eq!(saved.len(), current.len(), "{}", path.display());
            // In a few runs out of 60, on one PC and one account, the same 3 pixels of one
            // glyph draw differently: 9 values of 302,400 at 150%, the most by 59. The cause
            // is not found, and root has the note. So the count is what decides. A real
            // change moves far more values than this allows, and fails.
            let far: Vec<(usize, u8)> = saved
                .iter()
                .zip(&current)
                .enumerate()
                .map(|(i, (a, b))| (i, a.abs_diff(*b)))
                .filter(|(_, d)| *d > 3)
                .collect();
            let worst = far.iter().map(|(_, d)| *d).max().unwrap_or(0);
            if !far.is_empty() {
                println!(
                    "{}: {} values differ by more than 3, the most by {worst}: {far:?}",
                    path.display(),
                    far.len()
                );
            }
            let allowed = saved.len() / 10_000;
            assert!(
                far.len() <= allowed,
                "{} differs in {} channels (allowed {allowed}), the most by {worst}, first at byte {:?}",
                path.display(),
                far.len(),
                far.first()
            );
        }
    }
}

#[test]
fn the_real_renderer_draws_every_token_and_family_without_error() {
    // `render_strip` unwraps `draw`, which fails on a family it does not know.
    for name in ["light", "dark", "contrast"] {
        for maximized in [false, true] {
            render_strip(
                &sample(name, maximized),
                &theme_for(name),
                96,
                WIDTH,
                HEIGHT,
            );
        }
    }
}

#[test]
fn icons_and_text_sit_in_the_center_of_their_boxes() {
    for name in ["light", "dark"] {
        let theme = theme_for(name);
        let row = background(theme.tab_background);
        let active = background(theme.text_background);
        for maximized in [false, true] {
            for dpi in [96, 120, 144, 192] {
                let scale = dpi as f32 / 96.0;
                let s = sample(name, maximized);
                let image = render_strip(&s, &theme, dpi, WIDTH, HEIGHT);
                let tree = s.widgets().unwrap();
                let find = |label: &str| {
                    tree.children
                        .iter()
                        .find(|w| w.name == label)
                        .unwrap_or_else(|| panic!("{label}"))
                        .bounds
                };
                for label in ["Minimize", "Maximize or restore", "Close window"] {
                    assert_centered(
                        &image,
                        find(label),
                        row,
                        scale,
                        &format!("{name} {dpi} {maximized} {label}"),
                    );
                }
                // The tab's X in its close zone, on the active tab and on another.
                let close = |i: usize| tree.children[i].children[0].bounds;
                assert_centered(&image, close(0), active, scale, "active tab X");
                assert_centered(&image, close(2), row, scale, "tab X");
                // Tab text sits on the tab's vertical center.
                let b = s.tab_rects()[0];
                let text = rect(b.x.0 + 16.0, b.y.0, 100.0, b.height.0);
                let area = device(text, scale);
                let ink = ink_box(&image, area, active, 60).expect("tab text");
                let center = (ink.1 + ink.3) as f32 / 2.0;
                let tab_center = (area.1 + area.3) as f32 / 2.0;
                assert!(
                    (center - tab_center).abs() <= 1.5 + scale,
                    "{name} {dpi}: text center {center} is off tab center {tab_center}"
                );
            }
        }
    }
}

#[test]
fn plus_and_caret_stay_inside_the_split_button() {
    for name in ["light", "dark", "contrast"] {
        let theme = theme_for(name);
        for (maximized, y) in [(false, 12.0), (true, 4.0)] {
            let s = sample(name, maximized);
            assert_eq!(s.split_rect().y.0, y);
            for dpi in [96, 144, 192] {
                let scale = dpi as f32 / 96.0;
                let image = render_strip(&s, &theme, dpi, WIDTH, HEIGHT);
                let split = s.split_rect();
                let part = |x: f32| rect(split.x.0 + x, split.y.0, 31.0, 24.0);
                let bg = background(theme.tab_background);
                for (label, b) in [("plus", part(0.0)), ("caret", part(32.0))] {
                    let area = device(b, scale);
                    let ink = ink_box(&image, area, bg, 40).unwrap_or_else(|| panic!("{label}"));
                    // Clear of the part's edges on all four sides.
                    let margin = (3.0 * scale) as u32;
                    assert!(
                        ink.0 >= area.0 + margin
                            && ink.2 + margin <= area.2
                            && ink.1 >= area.1 + margin
                            && ink.3 + margin <= area.3,
                        "{name} {dpi} {label}: ink {ink:?} in {area:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn separator_is_one_sharp_column_at_100_percent() {
    let theme = theme_for("light");
    let s = sample("light", false);
    let image = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    let split = s.split_rect();
    let x = (split.x.0 + 31.0).round() as u32;
    let y = 24;
    let bg = background(theme.tab_background);
    let center = pixel(&image, x, y);
    assert_ne!(center, bg, "the separator shows");
    assert_eq!(pixel(&image, x - 1, y), bg, "nothing to the left");
    assert_eq!(pixel(&image, x + 1, y), bg, "nothing to the right");
    assert_eq!(pixel(&image, x, 12), center, "top of the 24-tall button");
    assert_eq!(pixel(&image, x, 35), center, "bottom");
    assert_eq!(pixel(&image, x, 11), bg);
    assert_eq!(pixel(&image, x, 36), bg);
}

#[test]
fn active_tab_joins_the_text_area_with_no_rule() {
    for name in ["light", "dark"] {
        let theme = theme_for(name);
        let s = sample(name, false);
        for dpi in [96, 144, 192] {
            let scale = dpi as f32 / 96.0;
            let image = render_strip(&s, &theme, dpi, WIDTH, HEIGHT);
            let x = (150.0 * scale) as u32;
            let text = background(theme.text_background);
            for y in [39.0, 40.0, 41.0, 47.0] {
                assert_eq!(
                    pixel(&image, x, (y * scale) as u32),
                    text,
                    "{name} {dpi} y={y}"
                );
            }
            // The row color stops at the row: the next tab's bottom meets the text area.
            let other = (300.0 * scale) as u32;
            assert_eq!(
                pixel(&image, other, (39.5 * scale) as u32),
                background(theme.tab_background)
            );
            assert_eq!(pixel(&image, other, (40.5 * scale) as u32), text);
            // Top corners are round, and the tab's bottom corners are square.
            assert_eq!(
                pixel(&image, 0, (8.0 * scale) as u32),
                background(theme.tab_background),
                "round top corner"
            );
            assert_eq!(
                pixel(&image, 0, (39.5 * scale) as u32),
                text,
                "square bottom"
            );
        }
    }
}

#[test]
fn tab_close_x_matches_the_window_close_x() {
    // The same glyph at the same size has the same ink, so its strokes match.
    let theme = theme_for("light");
    let mut s = sample("light", false);
    s.set_focused(None);
    let image = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    let tree = s.widgets().unwrap();
    let window = tree
        .children
        .iter()
        .find(|w| w.name == "Close window")
        .unwrap()
        .bounds;
    let tab = tree.children[2].children[0].bounds;
    let bg = background(theme.tab_background);
    let a = ink_box(&image, device(window, 1.0), bg, 40).unwrap();
    let b = ink_box(&image, device(tab, 1.0), bg, 40).unwrap();
    assert!((a.2 - a.0).abs_diff(b.2 - b.0) <= 1, "{a:?} {b:?}");
    assert!((a.3 - a.1).abs_diff(b.3 - b.1) <= 1, "{a:?} {b:?}");
}

#[test]
fn focus_outline_shows_in_pixels() {
    let theme = theme_for("light");
    let mut s = sample("light", false);
    let before = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    s.set_focused(Some(CaptionHit::NewTab));
    let after = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    let split = s.split_rect();
    let x = split.x.0 as u32 + 10;
    assert_ne!(pixel(&before, x, 12), pixel(&after, x, 12));
}

#[test]
fn hover_shows_in_pixels() {
    let theme = theme_for("dark");
    let mut s = sample("dark", false);
    let before = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    let split = s.split_rect();
    let at = Point {
        x: Dip(split.x.0 + 4.0),
        y: Dip(20.0),
    };
    s.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseMove { at },
        },
        &mut qe_api::ViewContext::default(),
    );
    let after = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    let (x, y) = (split.x.0 as u32 + 4, 20);
    assert_ne!(pixel(&before, x, y), pixel(&after, x, y));
}

/// `SplitButton.xaml`: the caret is 12 x 12, right aligned, with padding 0,0,12,0, in the
/// 31-wide part. `AnimatedChevronDownSmallVisualSource` gives ink about 7.7 x 4.2.
#[test]
fn caret_has_winuis_ink_and_position() {
    for name in ["light", "dark"] {
        let theme = theme_for(name);
        let bg = background(theme.tab_background);
        for dpi in [96, 144, 192] {
            let scale = dpi as f32 / 96.0;
            let s = sample(name, false);
            let image = render_strip(&s, &theme, dpi, WIDTH, HEIGHT);
            let split = s.split_rect();
            let part = rect(split.x.0 + 32.0, split.y.0, 31.0, 24.0);
            let area = device(part, scale);
            let ink = ink_box(&image, area, bg, 12).expect("caret");
            let (w, h) = (
                (ink.2 - ink.0) as f32 / scale,
                (ink.3 - ink.1) as f32 / scale,
            );
            println!("measured {name} {dpi}: caret ink {w:.1} x {h:.1} dips");
            assert!((w - 7.7).abs() <= 1.34, "{name} {dpi}: ink width {w}");
            assert!((h - 4.2).abs() <= 1.0, "{name} {dpi}: ink height {h}");
            let cx = (ink.0 + ink.2) as f32 / 2.0 - area.0 as f32;
            let cy = (ink.1 + ink.3) as f32 / 2.0 - area.1 as f32;
            assert!((cx - 13.0 * scale).abs() <= 1.0, "{name} {dpi}: x {cx}");
            assert!((cy - 12.0 * scale).abs() <= 1.0, "{name} {dpi}: y {cy}");
        }
    }
}

#[test]
fn plus_is_square_and_centered() {
    for name in ["light", "dark"] {
        let theme = theme_for(name);
        let bg = background(theme.tab_background);
        for dpi in [96, 144, 192] {
            let scale = dpi as f32 / 96.0;
            let s = sample(name, false);
            let image = render_strip(&s, &theme, dpi, WIDTH, HEIGHT);
            let split = s.split_rect();
            let part = rect(split.x.0, split.y.0, 31.0, 24.0);
            let area = device(part, scale);
            // ClearType smears ink sideways into red and blue, which is not the glyph's
            // shape, so this reads green alone, the middle of each pixel.
            let green = |x: u32, y: u32| pixel(&image, x, y)[1].abs_diff(bg[1]) > 20;
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
            for y in area.1..area.3 {
                for x in area.0..area.2 {
                    if green(x, y) {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x + 1);
                        y1 = y1.max(y + 1);
                    }
                }
            }
            let (w, h) = (x1 - x0, y1 - y0);
            println!("measured {name} {dpi}: plus ink {w} x {h} px");
            assert!(w.abs_diff(h) <= 1, "{name} {dpi}: {w} x {h}");
            assert_centered(&image, part, bg, scale, &format!("{name} {dpi} plus"));
        }
    }
}

#[test]
fn divider_color_is_winuis() {
    for (name, expected) in [("light", 226u8), ("dark", 53u8)] {
        let theme = theme_for(name);
        let s = sample(name, false);
        let image = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
        let x = (s.split_rect().x.0 + 31.0).round() as u32;
        let got = pixel(&image, x, 24);
        assert!(got[0].abs_diff(expected) <= 1, "{name}: {got:?}");
    }
}

#[test]
fn split_hover_pixels_are_plain_and_cover_the_divider() {
    let theme = theme_for("dark");
    let mut s = sample("dark", false);
    let split = s.split_rect();
    let at = Point {
        x: Dip(split.x.0 + 4.0),
        y: Dip(20.0),
    };
    s.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseMove { at },
        },
        &mut qe_api::ViewContext::default(),
    );
    let image = render_strip(&s, &theme, 96, WIDTH, HEIGHT);
    let (x, top) = (split.x.0.round() as u32, 12);
    let row = background(theme.tab_background);
    assert_ne!(pixel(&image, x + 20, 14), row, "filled");
    assert_ne!(
        pixel(&image, x + 31, top),
        row,
        "the inner corner is square"
    );
    assert_eq!(
        pixel(&image, x + 32, top),
        row,
        "the caret part stays clear"
    );
    assert_eq!(pixel(&image, x, top), row, "the outer corner is round");
}

/// Terminal needs this: the frame reaches under the top of the window, and any pixel
/// with alpha below 255 would let the frame's title bar show through.
#[test]
fn every_pixel_is_opaque_so_that_the_extended_frame_stays_hidden() {
    for name in ["light", "dark", "contrast"] {
        let image = render_strip(&sample(name, false), &theme_for(name), 144, WIDTH, HEIGHT);
        let clear = image
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] != 255)
            .count();
        assert_eq!(clear, 0, "{name}");
    }
}
