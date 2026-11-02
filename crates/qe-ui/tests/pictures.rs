//! Every control goes through the real renderer. Saved images are compared by pixels,
//! and pixel checks prove that text and icons sit inside their controls, centered.
//!
//! Update saved images with `QE_UI_UPDATE_IMAGES=1 cargo test -p qe-ui --locked --test
//! pictures saved_images`, then look at each changed picture.
mod support;

use qe_api::{
    Bitmap, Color, Dip, DisplayList, DrawCommand, Rect, TextEncoding, Theme, ThemeKind, View,
    WidgetId,
};
use qe_ui::*;
use std::path::PathBuf;
use support::scenes::{self, r};
use support::{RealMeasure, png, render};

fn image_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/images")
}

#[test]
fn saved_images_match_in_all_three_themes() {
    for (theme_name, theme) in scenes::THEMES {
        for scene in scenes::all(&theme()) {
            let image = render(&scene.list, scene.width, scene.height);
            let path = image_dir().join(format!("{}-{theme_name}.png", scene.name));
            if std::env::var_os("QE_UI_UPDATE_IMAGES").is_some() {
                std::fs::create_dir_all(image_dir()).unwrap();
                std::fs::write(&path, png::encode(&image)).unwrap();
            }
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let (w, h, saved) = png::decode(&bytes).unwrap();
            assert_eq!((w, h), (scene.width, scene.height), "{}", path.display());
            let current = png::raw_rows(&image);
            assert_eq!(saved.len(), current.len(), "{}", path.display());
            let different = saved
                .iter()
                .zip(&current)
                .filter(|(a, b)| a.abs_diff(**b) > 3)
                .count();
            assert_eq!(
                different,
                0,
                "{} differs in {different} channels",
                path.display()
            );
        }
    }
}

#[test]
fn png_round_trip_keeps_every_pixel() {
    let image = Bitmap {
        width: 3,
        height: 2,
        bgra: vec![
            1, 2, 3, 255, 1, 2, 3, 255, 9, 8, 7, 255, 1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255,
        ],
    };
    let (w, h, raw) = png::decode(&png::encode(&image)).unwrap();
    assert_eq!((w, h), (3, 2));
    assert_eq!(raw, png::raw_rows(&image));
}

#[derive(Debug, Clone, Copy)]
struct Ink {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
/// The box around pixels that differ between two pictures of the same size.
fn ink_between(a: &Bitmap, b: &Bitmap) -> Option<Ink> {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut ink: Option<Ink> = None;
    for y in 0..a.height as i32 {
        for x in 0..a.width as i32 {
            let at = (y as usize * a.width as usize + x as usize) * 4;
            let differ = (0..3).any(|c| a.bgra[at + c].abs_diff(b.bgra[at + c]) > 24);
            if differ {
                ink = Some(match ink {
                    None => Ink {
                        left: x,
                        top: y,
                        right: x + 1,
                        bottom: y + 1,
                    },
                    Some(i) => Ink {
                        left: i.left.min(x),
                        top: i.top.min(y),
                        right: i.right.max(x + 1),
                        bottom: i.bottom.max(y + 1),
                    },
                });
            }
        }
    }
    ink
}
const W: u32 = 300;
const H: u32 = 90;
/// A picture of one view on a flat background at a fixed place.
fn shot(view: &mut dyn View, theme: &Theme, bounds: Rect) -> Bitmap {
    let t = UiTokens::from_theme(theme);
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: r(0.0, 0.0, W as f32, H as f32),
        color: t.background,
    });
    view.layout(bounds, &RealMeasure);
    view.draw(&mut list, theme, &RealMeasure);
    render(&list, W, H)
}
fn themes() -> Vec<Theme> {
    vec![
        Theme::new(ThemeKind::Light),
        Theme::new(ThemeKind::Dark),
        SystemPalette::HIGH_CONTRAST_BLACK.theme(),
        SystemPalette::HIGH_CONTRAST_WHITE.theme(),
    ]
}
/// Text ink must lie inside the area, with its middle near the middle of the area.
fn assert_centered(name: &str, ink: Ink, area: Rect, slack: f32) {
    let (top, bottom) = (area.y.0, area.y.0 + area.height.0);
    assert!(
        ink.left as f32 >= area.x.0
            && ink.right as f32 <= area.x.0 + area.width.0
            && ink.top as f32 >= top
            && ink.bottom as f32 <= bottom,
        "{name}: ink {ink:?} leaves its control {area:?}"
    );
    let middle = (ink.top + ink.bottom) as f32 / 2.0;
    let want = (top + bottom) / 2.0;
    assert!(
        (middle - want).abs() <= slack,
        "{name}: ink {ink:?} has its middle at {middle}, and the control's is at {want}"
    );
}
// Text with one rise and one drop, so that its ink is near the middle of the line.
const SAMPLE: &str = "Sphinx";
type Sample = (&'static str, Box<dyn View>, Box<dyn View>);
fn area() -> Rect {
    r(20.0, 20.0, 240.0, 32.0)
}

#[test]
fn text_sits_inside_each_control_and_is_centered() {
    for theme in themes() {
        let t = UiTokens::from_theme(&theme);
        let a = area();
        let with_and_without: Vec<Sample> = vec![
            (
                "button",
                Box::new(Button::new(WidgetId(1), SAMPLE)),
                Box::new(Button::new(WidgetId(1), "")),
            ),
            (
                "checkbox",
                Box::new(Checkbox::new(WidgetId(2), SAMPLE)),
                Box::new(Checkbox::new(WidgetId(2), "")),
            ),
            (
                "label",
                Box::new(Label::new(WidgetId(3), SAMPLE, LabelKind::Plain)),
                Box::new(Label::new(WidgetId(3), "", LabelKind::Plain)),
            ),
            (
                "combo",
                Box::new(ComboBox::new(
                    WidgetId(4),
                    "Encoding",
                    vec![Some(SAMPLE.into())],
                )),
                Box::new(ComboBox::new(
                    WidgetId(4),
                    "Encoding",
                    vec![Some(String::new())],
                )),
            ),
            (
                "field",
                Box::new({
                    let mut f = TextField::new(WidgetId(5), "Name");
                    f.text = SAMPLE.into();
                    f
                }),
                Box::new(TextField::new(WidgetId(5), "Name")),
            ),
            (
                "tab",
                Box::new(TabBar::new(WidgetId(6), vec![SAMPLE.into()])),
                Box::new(TabBar::new(WidgetId(6), vec![String::new()])),
            ),
        ];
        for (name, mut with, mut without) in with_and_without {
            let ink = ink_between(
                &shot(with.as_mut(), &theme, a),
                &shot(without.as_mut(), &theme, a),
            )
            .unwrap_or_else(|| panic!("{name} shows no text"));
            assert_centered(name, ink, a, 2.0);
            assert!(
                ink.right - ink.left > 20,
                "{name} text is too small: {ink:?}"
            );
        }
        // A menu row: the same row with and without its label.
        let bounds = r(20.0, 10.0, 240.0, 44.0);
        let row_of = |label: &str| {
            MenuView::new(
                WidgetId(7),
                MenuModel {
                    items: vec![MenuItem::command(label, qe_api::CommandId::Open)],
                },
            )
        };
        let ink = ink_between(
            &shot(&mut row_of(SAMPLE), &theme, bounds),
            &shot(&mut row_of(""), &theme, bounds),
        )
        .expect("a menu row shows its label");
        assert_centered(
            "menu row",
            ink,
            r(20.0, 10.0 + t.inset.0, 240.0, t.menu_row.0),
            2.0,
        );
        // A table cell in each column, under a heading in the same column.
        let row = |label: &str, detail: &str| TableRow {
            label: label.into(),
            detail: detail.into(),
            section: false,
            indent: 0,
        };
        let table_of = |head0: &str, head1: &str, rows: Vec<TableRow>| {
            Table::new(WidgetId(8), vec![head0.into(), head1.into()], rows)
        };
        let bounds = r(10.0, 4.0, 280.0, 84.0);
        let shortcut_head = ink_between(
            &shot(&mut table_of("", SAMPLE, vec![]), &theme, bounds),
            &shot(&mut table_of("", "", vec![]), &theme, bounds),
        )
        .expect("a table shows its heading");
        let cell = ink_between(
            &shot(&mut table_of("", "", vec![row("", SAMPLE)]), &theme, bounds),
            &shot(&mut table_of("", "", vec![row("", "")]), &theme, bounds),
        )
        .expect("a table shows its cell");
        assert_eq!(
            shortcut_head.left, cell.left,
            "a heading and its column must start at one x"
        );
        assert_centered(
            "table cell",
            cell,
            r(10.0, 4.0 + t.row.0, 280.0, t.row.0),
            2.0,
        );
    }
}

/// The pixels inside `area` that are not its fill.
fn ink_in(image: &Bitmap, area: Rect, fill: Color) -> Option<Ink> {
    let mut flat = image.clone();
    for y in 0..flat.height as usize {
        for x in 0..flat.width as usize {
            let at = (y * flat.width as usize + x) * 4;
            flat.bgra[at] = fill.b;
            flat.bgra[at + 1] = fill.g;
            flat.bgra[at + 2] = fill.r;
        }
    }
    let (x0, y0) = (area.x.0 as usize, area.y.0 as usize);
    let (x1, y1) = (
        (area.x.0 + area.width.0) as usize,
        (area.y.0 + area.height.0) as usize,
    );
    let mut masked = image.clone();
    for y in 0..masked.height as usize {
        for x in 0..masked.width as usize {
            if !(x >= x0 && x < x1 && y >= y0 && y < y1) {
                let at = (y * masked.width as usize + x) * 4;
                masked.bgra[at] = fill.b;
                masked.bgra[at + 1] = fill.g;
                masked.bgra[at + 2] = fill.r;
            }
        }
    }
    ink_between(&masked, &flat)
}

#[test]
fn combo_caret_and_checkmark_are_real_icons_in_the_middle_of_their_boxes() {
    for theme in themes() {
        let t = UiTokens::from_theme(&theme);
        let a = area();
        let mut combo = ComboBox::new(WidgetId(1), "Encoding", vec![Some(String::new())]);
        let image = shot(&mut combo, &theme, a);
        let caret_box = r(
            a.x.0 + a.width.0 - t.caret_box.0,
            a.y.0 + 3.0,
            t.caret_box.0 - 3.0,
            a.height.0 - 6.0,
        );
        let caret = ink_in(&image, caret_box, t.surface).expect("the combo shows a caret");
        // A missing glyph draws a tall box. The small chevron is wider than it is tall.
        let (w, h) = (caret.right - caret.left, caret.bottom - caret.top);
        assert!((6..=13).contains(&w), "caret width {w}: {caret:?}");
        assert!(h >= 3 && h < w, "caret height {h}, width {w}: {caret:?}");
        let middle = |lo: i32, hi: i32| (lo + hi) as f32 / 2.0;
        let box_mid_x = a.x.0 + a.width.0 - t.caret_box.0 / 2.0;
        assert!(
            (middle(caret.left, caret.right) - box_mid_x).abs() <= 1.5,
            "caret {caret:?} is off the middle of its box at x {box_mid_x}"
        );
        assert!(
            (middle(caret.top, caret.bottom) - (a.y.0 + a.height.0 / 2.0)).abs() <= 2.0,
            "caret {caret:?} is off the middle of the control"
        );

        let mut on = Checkbox::new(WidgetId(2), "");
        on.checked = true;
        let box_r = r(
            a.x.0,
            a.y.0 + (a.height.0 - t.check_size.0) / 2.0,
            16.0,
            16.0,
        );
        let image = shot(&mut on, &theme, a);
        // Past the rounded corners, so only the mark differs from the fill.
        let inner = r(box_r.x.0 + 3.0, box_r.y.0 + 3.0, 10.0, 10.0);
        let tick = ink_in(&image, inner, t.accent).expect("a checked box shows a checkmark");
        assert!(tick.right - tick.left >= 6, "tick is too small: {tick:?}");
        assert!(
            (middle(tick.left, tick.right) - (box_r.x.0 + 8.0)).abs() <= 1.5
                && (middle(tick.top, tick.bottom) - (box_r.y.0 + 8.0)).abs() <= 1.5,
            "tick {tick:?} is off the middle of {box_r:?}"
        );
    }
}

/// Each color in a list, laid over nothing, must come from the palette. Translucent
/// colors would mix into values the user did not choose.
fn colors(list: &DisplayList) -> Vec<Color> {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::FillRect { color, .. }
            | DrawCommand::FillRects { color, .. }
            | DrawCommand::StrokeRect { color, .. }
            | DrawCommand::WavyUnderline { color, .. }
            | DrawCommand::RoundedRect { color, .. }
            | DrawCommand::Line { color, .. }
            | DrawCommand::TextRun { color, .. }
            | DrawCommand::GlyphRun { color, .. }
            | DrawCommand::Icon { color, .. } => Some(*color),
            _ => None,
        })
        .collect()
}
#[test]
fn high_contrast_reaches_every_draw_with_system_colors_only() {
    for palette in [
        SystemPalette::HIGH_CONTRAST_BLACK,
        SystemPalette::HIGH_CONTRAST_WHITE,
    ] {
        let theme = palette.theme();
        assert!(is_high_contrast(&theme));
        let allowed = palette.colors();
        for scene in scenes::all(&theme) {
            for color in colors(&scene.list) {
                assert!(
                    allowed.contains(&color),
                    "{} uses {color:?}, which is not a system color",
                    scene.name
                );
            }
        }
    }
    assert!(!is_high_contrast(&Theme::new(ThemeKind::Light)));
    assert!(!is_high_contrast(&Theme::new(ThemeKind::Dark)));
}

#[test]
fn control_edges_and_the_keyboard_highlight_reach_three_to_one() {
    for (name, theme) in [
        ("light", Theme::new(ThemeKind::Light)),
        ("dark", Theme::new(ThemeKind::Dark)),
    ] {
        let t = UiTokens::from_theme(&theme);
        let measured = [
            ("border on surface", contrast_ratio(t.border, t.surface)),
            ("border on page", contrast_ratio(t.border, t.background)),
            ("accent on page", contrast_ratio(t.accent, t.background)),
            ("focus ring on surface", contrast_ratio(t.focus, t.surface)),
            ("focus ring on page", contrast_ratio(t.focus, t.background)),
            (
                "focus ring on a hovered row",
                contrast_ratio(t.focus, over(t.hover, t.surface)),
            ),
        ];
        for (what, ratio) in measured {
            println!("{name}: {what} {ratio:.2}:1");
            assert!(ratio >= 3.0, "{name}: {what} is only {ratio:.2}:1");
        }
        let text = [
            ("text on surface", contrast_ratio(t.text, t.surface)),
            (
                "text on a selected row",
                contrast_ratio(t.selected_text, t.selected),
            ),
            ("mark on accent", contrast_ratio(t.on_accent, t.accent)),
            ("dim text on page", contrast_ratio(t.disabled, t.background)),
        ];
        for (what, ratio) in text {
            println!("{name}: {what} {ratio:.2}:1");
            assert!(ratio >= 4.5, "{name}: {what} is only {ratio:.2}:1");
        }
    }
}

#[test]
fn keyboard_menu_highlight_shows_a_ring_in_the_picture() {
    for theme in themes() {
        let bounds = r(20.0, 8.0, 240.0, 44.0);
        let model = MenuModel {
            items: vec![MenuItem::command("", qe_api::CommandId::Open)],
        };
        let mut mouse = MenuView::new(WidgetId(1), model.clone());
        mouse.selected = Some(0);
        let mut keyboard = MenuView::new(WidgetId(1), model);
        keyboard.open_keyboard();
        let ring = ink_between(
            &shot(&mut keyboard, &theme, bounds),
            &shot(&mut mouse, &theme, bounds),
        )
        .expect("the keyboard row has a ring that the mouse row does not");
        assert!(ring.right - ring.left > 200, "ring {ring:?}");
    }
}

#[test]
fn encoding_panel_is_336_by_187_and_stays_inside() {
    let bounds = r(0.0, 0.0, ENCODING_PANEL_WIDTH, ENCODING_PANEL_HEIGHT);
    let panel = EncodingPanel::new(WidgetId(1), TextEncoding::DEFAULT_LIST, bounds);
    assert_eq!((bounds.width, bounds.height), (Dip(336.0), Dip(187.0)));
    let tree = panel.widgets().unwrap();
    for child in &tree.children {
        let b = child.bounds;
        assert!(
            b.x.0 >= 0.0
                && b.y.0 >= 0.0
                && b.x.0 + b.width.0 <= 336.0
                && b.y.0 + b.height.0 <= 187.0,
            "{} leaves the panel: {b:?}",
            child.name
        );
    }
    // Every button label fits its button in the real font.
    for child in tree.children.iter().skip(1) {
        let width = RealMeasure.measure(&child.name, UI_MEASURE_TOKEN).width.0;
        assert!(
            width <= child.bounds.width.0 - 8.0,
            "{} does not fit: {width} in {}",
            child.name,
            child.bounds.width.0
        );
    }
}
use qe_api::TextMeasurer;
