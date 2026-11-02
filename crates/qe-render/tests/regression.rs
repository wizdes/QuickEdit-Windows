use qe_api::{Color, Dip, DisplayList, DrawCommand, Rect, Renderer};
use qe_render::BitmapRenderer;
use windows::{Win32::System::LibraryLoader::GetModuleHandleW, core::PCWSTR};

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(width),
        height: Dip(height),
    }
}

#[test]
fn scroll_matches_full_redraw() {
    let mut incremental = BitmapRenderer::new(16, 16).unwrap();
    let mut original = DisplayList::default();
    for (row, color) in [
        Color::rgb(255, 0, 0),
        Color::rgb(0, 255, 0),
        Color::rgb(0, 0, 255),
        Color::rgb(255, 255, 0),
    ]
    .into_iter()
    .enumerate()
    {
        original.push(DrawCommand::FillRect {
            rect: rect(0.0, row as f32 * 4.0, 16.0, 4.0),
            color,
        });
    }
    incremental.begin_frame().unwrap();
    incremental.draw(&original).unwrap();
    incremental.end_frame().unwrap();
    incremental
        .scroll(rect(0.0, 0.0, 16.0, 16.0), Dip(4.0))
        .unwrap();
    let mut top = DisplayList::default();
    top.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 16.0, 4.0),
        color: Color::rgb(255, 255, 255),
    });
    incremental.begin_frame().unwrap();
    incremental.draw(&top).unwrap();
    incremental.end_frame().unwrap();
    let mut expected = BitmapRenderer::new(16, 16).unwrap();
    let mut full = DisplayList::default();
    full.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 16.0, 4.0),
        color: Color::rgb(255, 255, 255),
    });
    for (row, color) in [
        Color::rgb(255, 0, 0),
        Color::rgb(0, 255, 0),
        Color::rgb(0, 0, 255),
    ]
    .into_iter()
    .enumerate()
    {
        full.push(DrawCommand::FillRect {
            rect: rect(0.0, (row + 1) as f32 * 4.0, 16.0, 4.0),
            color,
        });
    }
    expected.begin_frame().unwrap();
    expected.draw(&full).unwrap();
    expected.end_frame().unwrap();
    let actual = incremental.bitmap().unwrap();
    let wanted = expected.bitmap().unwrap();
    for (i, (a, b)) in actual.bgra.iter().zip(&wanted.bgra).enumerate() {
        assert_eq!(a, b, "byte {i}, row {}", i / (16 * 4));
    }
}

#[test]
fn ordinary_frame_loads_no_gpu_dll() {
    let mut renderer = BitmapRenderer::new(16, 16).unwrap();
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 16.0, 16.0),
        color: Color::rgb(255, 255, 255),
    });
    list.push(DrawCommand::TextRun {
        at: qe_api::Point {
            x: Dip(0.0),
            y: Dip(13.0),
        },
        text: "Q".into(),
        font: "Cascadia Mono".into(),
        color: Color::rgb(0, 0, 0),
    });
    renderer.begin_frame().unwrap();
    renderer.draw(&list).unwrap();
    renderer.end_frame().unwrap();
    for module in ["d3d11.dll", "dxgi.dll", "d2d1.dll", "dcomp.dll"] {
        let name: Vec<u16> = module.encode_utf16().chain(Some(0)).collect();
        assert!(
            unsafe { GetModuleHandleW(PCWSTR(name.as_ptr())) }.is_err(),
            "{module} loaded"
        );
    }
}

#[test]
fn cascadia_text_changes_pixels_without_a_window() {
    let mut renderer = BitmapRenderer::new(200, 128).unwrap();
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 200.0, 128.0),
        color: Color::rgb(255, 255, 255),
    });
    list.push(DrawCommand::TextRun {
        at: qe_api::Point {
            x: Dip(2.0),
            y: Dip(20.0),
        },
        text: "QuickEdit".into(),
        font: "Cascadia Mono".into(),
        color: Color::rgb(0, 0, 0),
    });
    renderer.begin_frame().unwrap();
    renderer.draw(&list).unwrap();
    renderer.end_frame().unwrap();
    let image = renderer.bitmap().unwrap();
    assert!(
        image.bgra[0..200 * 25 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] < 200),
        "glyphs must be near their requested baseline"
    );
    assert!(
        renderer
            .bitmap()
            .unwrap()
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] < 200)
    );
}

#[test]
fn registered_glyph_run_uses_its_font_and_origin() {
    let mut renderer = BitmapRenderer::new(40, 32).unwrap();
    renderer.register_font(7, "Cascadia Mono", 10.0).unwrap();
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 40.0, 32.0),
        color: Color::rgb(255, 255, 255),
    });
    // Cascadia Mono's Q glyph ID on the test PC.
    list.push(DrawCommand::GlyphRun {
        at: qe_api::Point {
            x: Dip(2.0),
            y: Dip(20.0),
        },
        bidi_level: 0,
        font_id: 7,
        glyph_ids: vec![148],
        advances: vec![Dip(8.0)],
        offsets: vec![qe_api::Point::default()],
        color: Color::rgb(0, 0, 0),
    });
    renderer.begin_frame().unwrap();
    renderer.draw(&list).unwrap();
    renderer.end_frame().unwrap();
    let image = renderer.bitmap().unwrap();
    assert!(
        image.bgra[0..25 * 40 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] < 200)
    );
}

#[test]
fn positive_glyph_y_offset_moves_ink_down() {
    fn top_row(offset: f32) -> usize {
        let mut renderer = BitmapRenderer::new(40, 40).unwrap();
        renderer.register_font(7, "Cascadia Mono", 10.0).unwrap();
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: rect(0.0, 0.0, 40.0, 40.0),
            color: Color::rgb(255, 255, 255),
        });
        list.push(DrawCommand::GlyphRun {
            at: qe_api::Point {
                x: Dip(2.0),
                y: Dip(20.0),
            },
            bidi_level: 0,
            font_id: 7,
            glyph_ids: vec![148],
            advances: vec![Dip(8.0)],
            offsets: vec![qe_api::Point {
                x: Dip(0.0),
                y: Dip(offset),
            }],
            color: Color::rgb(0, 0, 0),
        });
        renderer.begin_frame().unwrap();
        renderer.draw(&list).unwrap();
        renderer.end_frame().unwrap();
        renderer
            .bitmap()
            .unwrap()
            .bgra
            .as_chunks::<{ 40 * 4 }>()
            .0
            .iter()
            .position(|row| row.as_chunks::<4>().0.iter().any(|p| p[0] < 200))
            .unwrap()
    }
    assert!(top_row(8.0) > top_row(0.0));
}

#[test]
fn one_line_damage_preserves_other_lines() {
    let mut renderer = BitmapRenderer::new(32, 32).unwrap();
    let mut first = DisplayList::default();
    first.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 32.0, 32.0),
        color: Color::rgb(255, 255, 255),
    });
    renderer.begin_frame().unwrap();
    renderer.draw(&first).unwrap();
    renderer.end_frame().unwrap();
    let before = renderer.bitmap().unwrap();
    let mut second = DisplayList::default();
    second.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 32.0, 32.0),
        color: Color::rgb(0, 0, 0),
    });
    renderer.invalidate(rect(0.0, 8.0, 32.0, 4.0));
    renderer.begin_frame().unwrap();
    renderer.draw(&second).unwrap();
    renderer.end_frame().unwrap();
    let after = renderer.bitmap().unwrap();
    for y in 0..32 {
        for x in 0..32 {
            let i = ((y * 32 + x) * 4) as usize;
            if (8..12).contains(&y) {
                assert_ne!(&before.bgra[i..i + 3], &after.bgra[i..i + 3]);
            } else {
                assert_eq!(&before.bgra[i..i + 3], &after.bgra[i..i + 3]);
            }
        }
    }
}
