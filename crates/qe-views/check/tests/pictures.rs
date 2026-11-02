//! Pictures through the real qe-render, and what must be true of their pixels. The test
//! saves them in `output_folder`, for a person to look at as well.
use qe_api::{Bitmap, Dip, InputKind, Point, Theme, ThemeKind, ViewContext};
use qe_views::Paper;
use qe_views_check::*;
use std::collections::HashSet;

fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}

fn save(name: &str, bitmap: &Bitmap) {
    write_png(
        &output_folder().join(format!("{name}.png")),
        bitmap.width,
        bitmap.height,
        &bitmap.bgra,
    );
}

fn pixel(bitmap: &Bitmap, x: u32, y: u32) -> (u8, u8, u8) {
    let at = ((y * bitmap.width + x) * 4) as usize;
    (bitmap.bgra[at + 2], bitmap.bgra[at + 1], bitmap.bgra[at])
}

fn source_text() -> String {
    let mut text = String::new();
    for n in 0..400 {
        let indent = " ".repeat((n % 9) * 2);
        match n % 6 {
            0 => text.push_str(&format!("{indent}fn line_{n}() -> u32 {{\n")),
            1 => text.push_str(&format!("{indent}let value = compute(the_input, {n});\n")),
            2 => text.push_str(&format!(
                "{indent}// a comment that runs for a while and then some more words\n"
            )),
            3 => text.push_str("\n"),
            4 => text.push_str(&format!(
                "{indent}\u{628}\u{628}\u{628} \u{4e2d}\u{6587} e\u{301}\n"
            )),
            _ => text.push_str(&format!("{indent}}}\n")),
        }
    }
    text
}

fn picture(scene: &Scene, kind: ThemeKind, dpi: u32, width: f32, height: f32) -> Bitmap {
    let mut renderer = renderer(width, height, dpi);
    paint(&mut renderer, &scene.list(kind), &mut HashSet::new());
    renderer.bitmap().unwrap()
}

#[test]
fn gutter_map_and_box_in_both_themes_at_96_and_144_dpi() {
    let (width, height) = (820.0, 420.0);
    let mut scene = Scene::from_text(&source_text(), width, height);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_line_numbers(true, &mut scene.editor, &scene.measure, &mut ctx);
    let scroll = scene.editor.max_scroll_position() * 0.4;
    scene.editor.set_scroll_position(scroll, &mut ctx);
    scene.settle(&mut Vec::new());
    let map = scene.frame.map_area();
    let mut ignored = Vec::new();
    scene.send(
        InputKind::MouseMove {
            at: point(map.x.0 + 50.0, 100.0),
        },
        &mut ignored,
    );
    scene.settle(&mut ignored);
    let the_box = scene.frame.map_box().unwrap();
    assert_eq!(the_box.alpha, 1.0);

    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let theme = Theme::new(kind);
        let name = if kind == ThemeKind::Light {
            "light"
        } else {
            "dark"
        };
        for dpi in [96u32, 144] {
            let bitmap = picture(&scene, kind, dpi, width, height);
            save(&format!("views-{name}-{dpi}"), &bitmap);
            let scale = dpi as f32 / 96.0;
            let device = |dips: f32| (dips * scale).round() as u32;
            let background = theme.text_background;
            let background = (background.r, background.g, background.b);
            // The separator is the last column of the gutter, and differs from its background.
            let gutter = scene.frame.gutter_area();
            let separator = pixel(&bitmap, device(gutter.width.0 - 1.0), device(5.0));
            assert_ne!(separator, background, "{name} {dpi}: separator");
            assert_eq!(pixel(&bitmap, device(gutter.width.0 - 3.0), 0), background);
            // The gutter has ink, on its number rows.
            let row_y = device(scene.frame.labels()[1].y + 8.0);
            let has_ink = (device(2.0)..device(gutter.width.0 - 2.0))
                .any(|x| (0..device(10.0)).any(|dy| pixel(&bitmap, x, row_y + dy) != background));
            assert!(has_ink, "{name} {dpi}: no number at row {row_y}");

            // The box tints the map's background, and stops at its bottom edge.
            let rect = the_box.rect;
            let inside = device(rect.y.0 + rect.height.0 / 2.0);
            let right = device(map.x.0 + map.width.0) - device(1.0) - 1;
            let tinted = pixel(&bitmap, right, inside);
            assert_ne!(tinted, background, "{name} {dpi}: the box has no tint");
            let below = device(rect.y.0 + rect.height.0) + device(3.0);
            assert_eq!(
                pixel(&bitmap, right, below),
                background,
                "{name} {dpi}: below the box"
            );
            // The map's blocks show under the tint: some pixel in the box is darker or brighter.
            let map_left = device(map.x.0 + 2.0);
            let has_blocks = (map_left..map_left + device(40.0)).any(|x| {
                (device(rect.y.0 + 1.0)..device(rect.y.0 + rect.height.0 - 1.0))
                    .any(|y| pixel(&bitmap, x, y) != tinted)
            });
            assert!(has_blocks, "{name} {dpi}: no blocks in the box");
        }
    }
}

#[test]
fn the_map_without_its_box_and_with_only_the_map() {
    let (width, height) = (620.0, 300.0);
    let scene = Scene::from_text(&source_text(), width, height);
    for (kind, name) in [(ThemeKind::Light, "light"), (ThemeKind::Dark, "dark")] {
        for dpi in [96u32, 144] {
            let bitmap = picture(&scene, kind, dpi, width, height);
            save(&format!("map-only-{name}-{dpi}"), &bitmap);
        }
    }
    assert_eq!(scene.frame.gutter_area().width.0, 0.0);
    assert_eq!(scene.frame.map_box().unwrap().alpha, 0.0);
}

#[test]
fn paper_under_the_text_in_both_themes() {
    let (width, height) = (1000.0, 420.0);
    let mut scene = Scene::new(
        qe_editor::EditorView::from_text(&"the quick brown fox ".repeat(300)).unwrap(),
        width,
        height,
        Paper::LETTER,
    );
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_page_wrap(true, &mut scene.editor, &scene.measure, &mut ctx)
        .unwrap();
    scene.settle(&mut Vec::new());
    for (kind, name) in [(ThemeKind::Light, "light"), (ThemeKind::Dark, "dark")] {
        let theme = Theme::new(kind);
        for dpi in [96u32, 144] {
            let bitmap = picture(&scene, kind, dpi, width, height);
            save(&format!("paper-{name}-{dpi}"), &bitmap);
            let scale = dpi as f32 / 96.0;
            let device = |dips: f32| (dips * scale).round() as u32;
            // Paper is white in both themes, and past its 816 dips is the page's surround.
            assert_eq!(pixel(&bitmap, device(40.0), device(100.0)), (255, 255, 255));
            let under = theme.under_page;
            assert_eq!(
                pixel(&bitmap, device(900.0), device(100.0)),
                (under.r, under.g, under.b)
            );
            // The frame is light gray at the text's edges.
            assert_eq!(pixel(&bitmap, device(95.0), device(100.0)), (170, 170, 170));
            assert_eq!(
                pixel(&bitmap, device(720.0), device(100.0)),
                (170, 170, 170)
            );
            // Dark text on the paper, in a dark window too.
            let darkest = (device(100.0)..device(300.0))
                .map(|x| pixel(&bitmap, x, device(8.0)).0)
                .min()
                .unwrap();
            assert!(darkest < 100, "{name} {dpi}: {darkest}");
        }
    }
}
