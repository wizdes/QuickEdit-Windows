use qe_api::ViewRequest;
use qe_api::{Dip, DisplayList, DrawCommand, Point, Rect, Renderer, Theme, ThemeKind};
use qe_api::{InputEvent, InputKind, Layout, Modifiers, View, ViewContext};
use qe_buffer::MappedStore;
use qe_editor::{EditorView, MemoryClipboard};
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};

fn save_bmp(name: &str, bitmap: &qe_api::Bitmap) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images");
    std::fs::create_dir_all(&dir).unwrap();
    let size = 54 + bitmap.bgra.len() as u32;
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&54_u32.to_le_bytes());
    bytes.extend_from_slice(&40_u32.to_le_bytes());
    bytes.extend_from_slice(&(bitmap.width as i32).to_le_bytes());
    bytes.extend_from_slice(&(-(bitmap.height as i32)).to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&32_u16.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(bitmap.bgra.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    bytes.extend_from_slice(&bitmap.bgra);
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn key(editor: &mut EditorView, code: u16, ctrl: bool, shift: bool) {
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: code,
                modifiers: Modifiers {
                    ctrl,
                    shift,
                    ..Default::default()
                },
                repeat: false,
            },
        },
        &mut ViewContext::default(),
    );
}
fn character(editor: &mut EditorView, ch: char) {
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Char(ch),
        },
        &mut ViewContext::default(),
    );
}

#[test]
fn typing_backspace_and_undo_keep_crlf_intact() {
    let mut editor = EditorView::from_text("a\r\nb").expect("font");
    key(&mut editor, 0x23, true, false); // Ctrl+End
    key(&mut editor, 0x25, false, false); // Left
    assert_eq!(editor.caret().0, 3);
    key(&mut editor, 0x08, false, false); // Backspace removes the whole CRLF
    assert_eq!(editor.text().as_deref(), Some("ab"));
    key(&mut editor, 0x27, false, false);
    key(&mut editor, 0x25, false, false);
    character(&mut editor, 'x');
    assert_eq!(editor.text().as_deref(), Some("axb"));
    key(&mut editor, b'Z' as u16, true, false);
    assert_eq!(editor.text().as_deref(), Some("ab"));
    assert_eq!(editor.caret().0, 1);
}

#[test]
fn replay_matches_plain_string_for_seeded_edits_in_both_wrap_modes() {
    for wrap in [true, false] {
        let mut editor = EditorView::from_text("").unwrap();
        editor.set_wrap(wrap).unwrap();
        let mut model = String::new();
        let mut caret = 0;
        let mut seed = 0x4f3a_92d1_u32;
        for _ in 0..400 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            match seed % 5 {
                0 | 1 => {
                    let c = (b'a' + (seed % 26) as u8) as char;
                    character(&mut editor, c);
                    model.insert(caret, c);
                    caret += 1;
                }
                2 => {
                    key(&mut editor, 0x25, false, false);
                    caret = caret.saturating_sub(1);
                }
                3 => {
                    key(&mut editor, 0x27, false, false);
                    caret = (caret + 1).min(model.len());
                }
                _ => {
                    key(&mut editor, 0x08, false, false);
                    if caret > 0 {
                        caret -= 1;
                        model.remove(caret);
                    }
                }
            }
            assert_eq!(editor.text().as_deref(), Some(model.as_str()));
            assert_eq!(editor.caret().0, caret);
            assert_eq!(editor.selection().start.0, caret);
        }
    }
}

#[test]
fn prevent_editing_keeps_text_and_allows_selection() {
    let mut editor = EditorView::from_text("hello").unwrap();
    editor.set_prevent_editing(true);
    key(&mut editor, 0x41, true, false);
    assert_eq!(editor.selection().start.0, 0);
    assert_eq!(editor.selection().end.0, 5);
    character(&mut editor, 'x');
    assert_eq!(editor.text().as_deref(), Some("hello"));
}

#[test]
fn return_uses_the_most_common_existing_line_ending() {
    let mut editor = EditorView::from_text("a\r\nb\r\nc\nlast").unwrap();
    key(&mut editor, 0x23, true, false);
    key(&mut editor, 0x0d, false, false);
    assert_eq!(editor.text().as_deref(), Some("a\r\nb\r\nc\nlast\r\n"));
}

#[test]
fn page_down_moves_the_view_with_the_caret() {
    let text = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(200.0),
            height: Dip(85.0),
        },
        &measure,
    );
    key(&mut editor, 0x22, false, false);
    assert!(editor.caret().0 > 0);
    assert!(editor.scroll_offset() > 0.0);
}

#[test]
fn horizontal_wheel_scrolls_unwrapped_text() {
    let mut editor = EditorView::from_text("abcdefghijklmnopqrstuvwxyz").unwrap();
    editor.set_wrap(false).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    // The text must be wider than the view, now that scrolling stops at its end.
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(40.0),
            height: Dip(85.0),
        },
        &measure,
    );
    editor.poll_layout(&mut ViewContext::default());
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Wheel {
                at: Point {
                    x: Dip(1.0),
                    y: Dip(1.0),
                },
                delta_x: 120.0,
                delta_y: 0.0,
                modifiers: Modifiers::default(),
            },
        },
        &mut ViewContext::default(),
    );
    assert!(editor.horizontal_offset() > 0.0);
}

#[test]
fn backspace_uses_cached_cluster_stops() {
    for cluster in ["e\u{301}", "🇺🇸", "👩‍👩‍👧‍👦"] {
        let mut editor = EditorView::from_text(cluster).unwrap();
        let mut ctx = ViewContext::default();
        editor.poll_layout(&mut ctx);
        for _ in 0..200 {
            if !editor.pending_layout() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            editor.poll_layout(&mut ctx);
        }
        key(&mut editor, 0x23, true, false);
        key(&mut editor, 0x08, false, false);
        assert_eq!(editor.text().as_deref(), Some(""), "cluster {cluster:?}");
    }
}

#[test]
fn drag_outside_view_keeps_scrolling_on_timer() {
    let text = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(200.0),
            height: Dip(51.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    editor.handle(
        &InputEvent {
            time_us: 1,
            kind: InputKind::MouseDown {
                at: Point {
                    x: Dip(1.0),
                    y: Dip(1.0),
                },
                button: qe_api::MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        },
        &mut ctx,
    );
    editor.handle(
        &InputEvent {
            time_us: 2,
            kind: InputKind::MouseMove {
                at: Point {
                    x: Dip(1.0),
                    y: Dip(200.0),
                },
            },
        },
        &mut ctx,
    );
    let before = editor.scroll_offset();
    editor.on_timer(&mut ctx);
    assert!(editor.scroll_offset() > before);
}

#[test]
fn injected_clipboard_keeps_pasted_endings_and_cut_copy() {
    let mut editor = EditorView::from_text("before").unwrap();
    editor.set_clipboard(Box::new(MemoryClipboard("\r\n🇺🇸".into())));
    key(&mut editor, 0x23, true, false);
    key(&mut editor, 0x56, true, false);
    assert_eq!(editor.text().as_deref(), Some("before\r\n🇺🇸"));
    key(&mut editor, 0x41, true, false);
    key(&mut editor, 0x58, true, false);
    assert_eq!(editor.text().as_deref(), Some(""));
    key(&mut editor, 0x56, true, false);
    assert_eq!(editor.text().as_deref(), Some("before\r\n🇺🇸"));
}

#[test]
fn unfocused_idle_requests_no_timer() {
    let mut editor = EditorView::from_text("").unwrap();
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    ctx.requests.clear();
    editor.on_timer(&mut ctx);
    assert!(
        !ctx.requests
            .iter()
            .any(|r| matches!(r, ViewRequest::RequestTimer(_)))
    );
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Focus,
        },
        &mut ctx,
    );
    assert!(
        ctx.requests
            .iter()
            .any(|r| matches!(r, ViewRequest::RequestTimer(_)))
    );
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Blur,
        },
        &mut ctx,
    );
    ctx.requests.clear();
    editor.on_timer(&mut ctx);
    assert!(
        !ctx.requests
            .iter()
            .any(|r| matches!(r, ViewRequest::RequestTimer(_)))
    );
}

#[test]
fn double_and_triple_click_select_word_and_line() {
    let mut editor = EditorView::from_text("hello world\nnext").unwrap();
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    let at = Point {
        x: Dip(2.0),
        y: Dip(2.0),
    };
    for time_us in [1, 1000] {
        editor.handle(
            &InputEvent {
                time_us,
                kind: InputKind::MouseDown {
                    at,
                    button: qe_api::MouseButton::Left,
                    modifiers: Modifiers::default(),
                },
            },
            &mut ctx,
        );
        editor.handle(
            &InputEvent {
                time_us,
                kind: InputKind::MouseUp {
                    at,
                    button: qe_api::MouseButton::Left,
                    modifiers: Modifiers::default(),
                },
            },
            &mut ctx,
        );
    }
    assert_eq!(editor.selection().start.0, 0);
    assert_eq!(editor.selection().end.0, 5);
    editor.handle(
        &InputEvent {
            time_us: 2000,
            kind: InputKind::MouseDown {
                at,
                button: qe_api::MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        },
        &mut ctx,
    );
    assert_eq!(editor.selection().start.0, 0);
    assert_eq!(editor.selection().end.0, 12);
}

#[test]
fn control_up_and_down_move_between_paragraph_starts() {
    let mut editor = EditorView::from_text("a\nbc\nxyz").unwrap();
    key(&mut editor, 0x23, true, false);
    key(&mut editor, 0x25, false, false);
    key(&mut editor, 0x25, false, false);
    key(&mut editor, 0x26, true, false);
    assert_eq!(editor.caret().0, 5);
    key(&mut editor, 0x26, true, false);
    assert_eq!(editor.caret().0, 2);
    key(&mut editor, 0x28, true, false);
    assert_eq!(editor.caret().0, 5);
}

#[test]
fn home_uses_the_visual_row_when_wrapped() {
    let text = "abcdefghijklmnopqrstuvwxyz0123456789";
    let mut editor = EditorView::from_text(text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(40.0),
            height: Dip(200.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    let expected = measure
        .layout_line(
            &qe_buffer::PieceStore::new(text),
            qe_api::LineIndex(0),
            Dip(40.0),
        )
        .unwrap()
        .rows
        .last()
        .unwrap()
        .range
        .start;
    assert!(expected.0 > 0);
    key(&mut editor, 0x23, true, false);
    key(&mut editor, 0x24, false, false);
    assert_eq!(editor.caret(), expected);
}

#[test]
fn page_down_moves_through_rows_of_one_wrapped_line() {
    let text = "abcdefghijklmnopqrstuvwxyz0123456789".repeat(5);
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(40.0),
            height: Dip(85.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    key(&mut editor, 0x22, false, false);
    assert!(editor.caret().0 > 0);
    assert!(editor.scroll_offset() > 0.0);
}

#[test]
fn enter_in_one_megabyte_file_keeps_height_map_compact() {
    let text = "x\n".repeat(500_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    key(&mut editor, 0x0d, false, false);
    assert_eq!(editor.line_count(), 500_002);
    assert!(editor.height_run_count() <= 8);
}

#[test]
fn rapid_typing_queues_each_visible_line_once() {
    let text = "x\n".repeat(500_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(200.0),
            height: Dip(170.0),
        },
        &measure,
    );
    editor.poll_layout(&mut ViewContext::default());
    for _ in 0..100 {
        character(&mut editor, 'q');
    }
    assert!(editor.pending_task_count() <= 50);
}

#[test]
fn measure_key_handling_p99_on_one_megabyte() {
    let text = "x\n".repeat(500_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(200.0),
            height: Dip(170.0),
        },
        &measure,
    );
    let mut samples = Vec::new();
    for _ in 0..300 {
        let started = std::time::Instant::now();
        character(&mut editor, 'q');
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = std::time::Instant::now();
        key(&mut editor, 0x08, false, false);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let p99 = samples[(samples.len() as f64 * 0.99).ceil() as usize - 1];
    eprintln!(
        "1 MiB key handling p99 on this PC, load uncontrolled: {p99:.3} ms ({} samples)",
        samples.len()
    );
    assert_eq!(editor.line_count(), 500_001);
    assert!(editor.pending_task_count() <= 100);
}

#[test]
fn a_long_wrapped_line_scrolls_to_its_last_segment() {
    let text = "x".repeat(100_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(40.0),
            height: Dip(85.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    key(&mut editor, 0x23, true, false);
    assert!(editor.scroll_offset() > 100_000.0);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Focus,
        },
        &mut ctx,
    );
    let mut list = DisplayList::default();
    editor.draw(&mut list, &Theme::new(ThemeKind::Light), &measure);
    assert!(
        list.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. }))
    );
    editor.handle(
        &InputEvent {
            time_us: 1,
            kind: InputKind::MouseDown {
                at: Point {
                    x: Dip(1.0),
                    y: Dip(1.0),
                },
                button: qe_api::MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        },
        &mut ctx,
    );
    assert!(editor.caret().0 < text.len());
    assert!(editor.caret().0 > 80_000);
}

#[test]
fn unwrapped_long_line_reveals_the_caret_horizontally() {
    let text = "x".repeat(100_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    editor.set_wrap(false).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(80.0),
            height: Dip(85.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    key(&mut editor, 0x23, true, false);
    let engine = DirectWriteLayout::new().unwrap().with_wrap(false);
    let map = engine
        .long_line_map(
            &qe_buffer::PieceStore::new(&text),
            qe_api::LineIndex(0),
            Dip(1_000_000.0),
        )
        .unwrap();
    let expected = map.total_height() - 80.0 + 2.0;
    assert!(
        (f64::from(editor.horizontal_offset()) - expected).abs() < 20.0,
        "got {}, expected {expected}",
        editor.horizontal_offset()
    );
}

#[test]
fn changing_wrap_rebuilds_the_long_line_map() {
    let text = "x".repeat(100_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(80.0),
            height: Dip(85.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    editor.set_wrap(false).unwrap();
    editor.poll_layout(&mut ctx);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    key(&mut editor, 0x23, true, false);
    let engine = DirectWriteLayout::new().unwrap().with_wrap(false);
    let expected = engine
        .long_line_map(
            &qe_buffer::PieceStore::new(&text),
            qe_api::LineIndex(0),
            Dip(1_000_000.0),
        )
        .unwrap()
        .total_height()
        - 78.0;
    assert!((f64::from(editor.horizontal_offset()) - expected).abs() < 20.0);
}

#[test]
fn edit_inside_long_line_updates_its_segment_count() {
    let text = "x".repeat(100_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(80.0),
            height: Dip(85.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    editor.set_clipboard(Box::new(MemoryClipboard("y".repeat(4096))));
    key(&mut editor, 0x56, true, false);
    key(&mut editor, 0x23, true, false);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    editor.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Focus,
        },
        &mut ctx,
    );
    let mut list = DisplayList::default();
    editor.draw(&mut list, &Theme::new(ThemeKind::Light), &measure);
    assert_eq!(editor.caret().0, 104_096);
    assert!(
        list.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. }))
    );
    assert!(
        list.commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::FillRect { .. }))
            .count()
            > 1
    );
}

#[test]
fn backspace_at_end_of_long_line_deletes_one_caret_stop() {
    let text = "x".repeat(100_000);
    let mut editor = EditorView::from_text(&text).unwrap();
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    key(&mut editor, 0x23, true, false);
    for _ in 0..300 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    key(&mut editor, 0x08, false, false);
    assert_eq!(editor.text().unwrap().len(), 99_999);
}

#[test]
fn mapped_unknown_long_line_paints_its_known_prefix() {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("qe-editor-known-{}.txt", std::process::id()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for _ in 0..50 {
                match std::fs::remove_file(&self.0) {
                    Ok(()) => return,
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
            eprintln!("could not remove {}", self.0.display());
        }
    }
    let _cleanup = Cleanup(path.clone());
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&vec![b'x'; 64 * 1024]).unwrap();
    file.set_len(64 * 1024 * 1024).unwrap();
    drop(file);
    let mapped = MappedStore::open(&path).unwrap();
    let mut editor = EditorView::from_mapped(mapped).unwrap();
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    let mut list = DisplayList::default();
    editor.draw(
        &mut list,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    assert!(
        list.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. }))
    );
    editor.set_wrap(false).unwrap();
    let mut after_change = DisplayList::default();
    editor.draw(
        &mut after_change,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    assert!(
        !after_change
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. })),
        "a wrapped partial layout must not paint after wrap is disabled"
    );
}

#[test]
fn bitmap_renderer_paints_real_glyphs_in_six_conditions() {
    let mut editor = EditorView::from_text("Hello\nمرحبا 😀").unwrap();
    editor.set_font("Consolas", 10.0).unwrap();
    let measure = DirectWriteLayout::with_font(
        "Consolas",
        10.0,
        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
    )
    .unwrap();
    editor.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(300.0),
            height: Dip(90.0),
        },
        &measure,
    );
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    assert!(
        !editor.pending_layout(),
        "layout worker must fill the first screen"
    );
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let mut list = DisplayList::default();
        editor.draw(&mut list, &Theme::new(kind), &measure);
        let runs: Vec<_> = list
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::GlyphRun { font_id, .. } => Some(*font_id),
                _ => None,
            })
            .collect();
        assert!(!runs.is_empty());
        for dpi in [96, 144, 192] {
            let mut renderer = BitmapRenderer::new(300, 90).unwrap();
            renderer
                .set_text_settings(TextSettings::reference())
                .unwrap();
            renderer.set_dpi(dpi).unwrap();
            for id in &runs {
                renderer.register_font_face(*id, &qe_layout::font_face(*id).unwrap());
            }
            let bitmap = renderer.render_to_bitmap(&list, 300, 90).unwrap();
            let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/images/editor-{kind:?}-{dpi}.bmp"));
            if std::env::var_os("QE_EDITOR_DUMP_IMAGES").is_some() {
                save_bmp(&format!("editor-{kind:?}-{dpi}.bmp"), &bitmap);
            } else {
                let baseline = std::fs::read(&file).unwrap();
                assert_eq!(baseline.len(), 54 + bitmap.bgra.len());
                let changed = bitmap
                    .bgra
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(baseline[54..].as_chunks::<4>().0.iter())
                    .filter(|(actual, expected)| {
                        actual
                            .iter()
                            .zip(expected.iter())
                            .any(|(a, b)| a.abs_diff(*b) > 8)
                    })
                    .count();
                assert!(
                    changed <= 135,
                    "{changed} changed pixels in {kind:?} {dpi} DPI baseline"
                );
            }
            let background = bitmap.bgra[0..4].to_vec();
            assert!(
                bitmap
                    .bgra
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel != background.as_slice()),
                "no ink at {kind:?} {dpi} dpi"
            );
        }
    }
}

#[test]
fn cache_miss_paints_blank_until_worker_result_arrives() {
    let mut editor = EditorView::from_text("Wide 漢字 text").unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    let theme = Theme::new(ThemeKind::Light);
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    let mut before = DisplayList::default();
    editor.draw(&mut before, &theme, &measure);
    assert!(
        !before
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. }))
    );
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    let mut after = DisplayList::default();
    editor.draw(&mut after, &theme, &measure);
    assert!(
        after
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. }))
    );
}

#[test]
fn mapped_store_can_paint_and_cannot_edit() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mapped.txt");
    let store = MappedStore::open(path).unwrap();
    let mut editor = EditorView::from_mapped(store).unwrap();
    let mut ctx = ViewContext::default();
    editor.poll_layout(&mut ctx);
    for _ in 0..200 {
        if !editor.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        editor.poll_layout(&mut ctx);
    }
    assert!(editor.text().unwrap().starts_with("Read only sample."));
    character(&mut editor, 'X');
    assert_eq!(editor.caret().0, 0);
    assert!(editor.text().unwrap().starts_with("Read only sample."));
    assert!(editor.widgets().unwrap().states.read_only);
}

#[test]
fn qe_api_replay_file_checks_text_caret_and_selection() {
    let steps = qe_api::parse_replay(include_str!("replays/basic.txt")).unwrap();
    for wrap in [true, false] {
        let mut editor = EditorView::from_text("").unwrap();
        editor.set_wrap(wrap).unwrap();
        for step in &steps {
            match step {
                qe_api::ReplayStep::Event(input) => {
                    editor.handle(input, &mut ViewContext::default());
                }
                qe_api::ReplayStep::Expect { name, value } => {
                    let actual = match name.as_str() {
                        "text" => editor.text().unwrap(),
                        "caret" => editor.caret().0.to_string(),
                        "selection" => format!(
                            "{}:{}",
                            editor.selection().start.0,
                            editor.selection().end.0
                        ),
                        other => panic!("unknown replay check {other}"),
                    };
                    assert_eq!(&actual, value);
                }
            }
        }
    }
}
