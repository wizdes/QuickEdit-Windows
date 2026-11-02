//! A partial repaint must match a full one, pixel for pixel, through the real renderer.
use qe_api::{Dip, InputKind, Modifiers, MouseButton, Point, Rect, ThemeKind, ViewContext};
use qe_views_check::*;
use std::collections::HashSet;

fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x.0 < b.x.0 + b.width.0
        && b.x.0 < a.x.0 + a.width.0
        && a.y.0 < b.y.0 + b.height.0
        && b.y.0 < a.y.0 + a.height.0
}

fn check_random_steps(dpi: u32, theme: ThemeKind, seed: u64, wrap_lines: bool) {
    let (width, height) = (900.0, 500.0);
    let text: String = (0..600)
        .map(|n| {
            if wrap_lines && n % 5 == 0 {
                format!("wrapped {n} {}\n", "word ".repeat(60))
            } else {
                format!("{}line number {n}\n", " ".repeat(n % 7))
            }
        })
        .collect();
    let mut scene = Scene::from_text(&text, width, height);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_line_numbers(true, &mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());

    let mut partial = renderer(width, height, dpi);
    let mut full = renderer(width, height, dpi);
    let (mut seen_partial, mut seen_full) = (HashSet::new(), HashSet::new());
    paint(&mut partial, &scene.list(theme), &mut seen_partial);

    let mut steps = Steps(seed);
    let mut partial_pixels = 0u64;
    let mut full_pixels = 0u64;
    for step in 0..120 {
        let mut repaint = Vec::new();
        let map = scene.frame.map_area();
        let kind = steps.next(9);
        match kind {
            0..=2 => {
                let c = char::from(b'a' + steps.next(26) as u8);
                scene.send(InputKind::Char(c), &mut repaint);
            }
            3 => scene.send(
                InputKind::KeyDown {
                    virtual_key: 0x0d,
                    modifiers: Modifiers::default(),
                    repeat: false,
                },
                &mut repaint,
            ),
            4 => {
                let y = steps.next(height as u64) as f32;
                scene.send(
                    InputKind::MouseMove {
                        at: point(map.x.0 + 20.0, y),
                    },
                    &mut repaint,
                );
            }
            5 => scene.send(
                InputKind::MouseMove {
                    at: point(100.0, 50.0),
                },
                &mut repaint,
            ),
            6 => {
                let y = steps.next(height as u64) as f32;
                let press = InputKind::MouseDown {
                    at: point(map.x.0 + 20.0, y),
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                };
                scene.send(press, &mut repaint);
                let to = steps.next(height as u64) as f32;
                scene.send(
                    InputKind::MouseMove {
                        at: point(map.x.0 + 20.0, to),
                    },
                    &mut repaint,
                );
                let release = InputKind::MouseUp {
                    at: point(map.x.0 + 20.0, to),
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                };
                scene.send(release, &mut repaint);
            }
            7 => {
                let notches = 1.0 - steps.next(3) as f32;
                let wheel = InputKind::Wheel {
                    at: point(100.0, 100.0),
                    delta_x: 0.0,
                    delta_y: notches,
                    modifiers: Modifiers::default(),
                };
                scene.send(wheel, &mut repaint);
            }
            _ => {
                let mut ctx = ViewContext::default();
                let on = steps.next(2) == 0;
                scene
                    .frame
                    .set_line_numbers(on, &mut scene.editor, &scene.measure, &mut ctx);
                take_repaints(&ctx, &mut repaint);
            }
        }
        scene.settle(&mut repaint);

        let list = scene.list(theme);
        for rect in &repaint {
            partial.invalidate(*rect);
        }
        paint(&mut partial, &list, &mut seen_partial);
        partial_pixels += partial.frame_stats().pixels as u64;
        full.invalidate_all();
        paint(&mut full, &list, &mut seen_full);
        full_pixels += full.frame_stats().pixels as u64;

        let (a, b) = (partial.bitmap().unwrap(), full.bitmap().unwrap());
        if a != b {
            let folder = output_folder();
            write_png(
                &folder.join("repaint-partial.png"),
                a.width,
                a.height,
                &a.bgra,
            );
            write_png(&folder.join("repaint-full.png"), b.width, b.height, &b.bgra);
            let first = a
                .bgra
                .chunks_exact(4)
                .zip(b.bgra.chunks_exact(4))
                .position(|(x, y)| x != y)
                .unwrap_or(0);
            panic!(
                "step {step} (kind {kind}) at {dpi} DPI: first pixel differs at x {} y {}. Asked: {repaint:?}",
                first % a.width as usize,
                first / a.width as usize
            );
        }
    }
    assert!(
        partial_pixels * 4 < full_pixels * 3,
        "partial repaints painted {partial_pixels} of {full_pixels} pixels"
    );
}

#[test]
fn random_steps_match_a_full_paint_at_96_dpi_light() {
    check_random_steps(96, ThemeKind::Light, 0x1234_5678_9ABC, false);
}
#[test]
fn random_steps_match_a_full_paint_at_144_dpi_dark() {
    check_random_steps(144, ThemeKind::Dark, 0xDEAD_BEEF_0042, false);
}
#[test]
fn random_steps_on_wrapped_lines_match_a_full_paint() {
    check_random_steps(96, ThemeKind::Dark, 0x0F0F_1234_7777, true);
}
#[test]
fn random_steps_match_at_a_fractional_dpi() {
    check_random_steps(120, ThemeKind::Light, 0x5555_AAAA_1111, false);
}

/// A key inside a line, with both side views on: nothing in their columns repaints on the
/// key, and after the wait only the typed row of the map and no number repaints.
#[test]
fn a_key_repaints_only_what_changed_in_the_gutter_and_the_map() {
    let text: String = (0..300).map(|n| format!("line number {n}\n")).collect();
    let mut scene = Scene::from_text(&text, 900.0, 500.0);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_line_numbers(true, &mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());
    let (gutter, map) = (scene.frame.gutter_area(), scene.frame.map_area());

    let mut on_key = Vec::new();
    scene.send(InputKind::Char('x'), &mut on_key);
    assert!(!on_key.is_empty());
    assert!(
        on_key
            .iter()
            .all(|r| !overlaps(*r, gutter) && !overlaps(*r, map)),
        "{on_key:?}"
    );

    let mut after = Vec::new();
    scene.settle(&mut after);
    let in_gutter: Vec<_> = after.iter().filter(|r| overlaps(**r, gutter)).collect();
    assert!(in_gutter.is_empty(), "no number moved: {in_gutter:?}");
    let in_map: Vec<_> = after.iter().filter(|r| overlaps(**r, map)).collect();
    assert_eq!(in_map.len(), 1, "one row of the map: {in_map:?}");
    assert!(in_map[0].height.0 <= 4.0, "{:?}", in_map[0]);
}

/// Enter moves the numbers below it, and only those.
#[test]
fn a_new_line_repaints_only_the_numbers_below_it() {
    let text: String = (0..20).map(|n| format!("line number {n}\n")).collect();
    let mut scene = Scene::from_text(&text, 900.0, 500.0);
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_line_numbers(true, &mut scene.editor, &scene.measure, &mut ctx);
    scene.settle(&mut Vec::new());
    // The caret goes to the end of line 10.
    let target = scene
        .editor
        .line_range(qe_api::LineIndex(10))
        .unwrap()
        .start;
    let mut ctx = ViewContext::default();
    scene.editor.run_host(
        qe_editor::HostCommand::Select {
            range: target..target,
            reveal: false,
        },
        &mut ctx,
    );
    scene.settle(&mut Vec::new());
    let gutter = scene.frame.gutter_area();
    let row_y = scene
        .editor
        .visible_rows()
        .rows
        .iter()
        .find(|row| row.line.0 == 10)
        .unwrap()
        .y
        .0;
    let mut repaint = Vec::new();
    scene.send(
        InputKind::KeyDown {
            virtual_key: 0x0d,
            modifiers: Modifiers::default(),
            repeat: false,
        },
        &mut repaint,
    );
    scene.settle(&mut repaint);
    let in_gutter: Vec<_> = repaint.iter().filter(|r| overlaps(**r, gutter)).collect();
    assert!(!in_gutter.is_empty());
    for rect in &in_gutter {
        assert!(
            rect.y.0 >= row_y - 2.0,
            "above the edit: {rect:?}, row at {row_y}"
        );
    }
}
