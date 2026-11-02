use qe_api::*;
use qe_editor::{EditorView, MemoryClipboard};
use qe_layout::DirectWriteLayout;
fn event(e: &mut EditorView, kind: InputKind) -> ViewContext {
    let mut c = ViewContext::default();
    e.handle(
        &InputEvent {
            time_us: 1_000_000,
            kind,
        },
        &mut c,
    );
    c
}
fn key(e: &mut EditorView, k: u16, ctrl: bool, shift: bool) -> ViewContext {
    event(
        e,
        InputKind::KeyDown {
            virtual_key: k,
            modifiers: Modifiers {
                ctrl,
                shift,
                ..Default::default()
            },
            repeat: false,
        },
    )
}
fn warm(e: &mut EditorView) {
    let start = std::time::Instant::now();
    loop {
        e.poll_layout(&mut ViewContext::default());
        if !e.pending_layout() {
            break;
        }
        assert!(start.elapsed().as_secs() < 5, "layout timeout");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
fn size(e: &mut EditorView, w: f32, h: f32) {
    e.layout(
        Rect {
            x: Dip(0.),
            y: Dip(0.),
            width: Dip(w),
            height: Dip(h),
        },
        &DirectWriteLayout::new().unwrap(),
    );
}
#[test]
fn up_from_second_line() {
    let mut e = EditorView::from_text("abc\ndef").unwrap();
    warm(&mut e);
    key(&mut e, 0x23, true, false);
    key(&mut e, 0x26, false, false);
    assert_eq!(e.caret().0, 3);
}
#[test]
fn cold_delete_long_line() {
    let text = format!("{}\ntail", "x".repeat(9000));
    let mut e = EditorView::from_text(&text).unwrap();
    key(&mut e, 0x2e, false, false);
    assert_eq!(e.text().unwrap().len(), text.len() - 1);
}
#[test]
fn warm_long_edit_then_delete() {
    let text = format!("{}\ntail", "x".repeat(100000));
    let mut e = EditorView::from_text(&text).unwrap();
    warm(&mut e);
    event(&mut e, InputKind::Char('z'));
    key(&mut e, 0x2e, false, false);
    assert_eq!(e.text().unwrap().len(), text.len());
}
#[test]
fn cold_cluster_backspace() {
    let mut e = EditorView::from_text("e\u{301}").unwrap();
    key(&mut e, 0x23, true, false);
    key(&mut e, 0x08, false, false);
    assert_eq!(e.text().unwrap(), "");
}
#[test]
fn prevent_editing_blocks_undo() {
    let mut e = EditorView::from_text("").unwrap();
    event(&mut e, InputKind::Char('x'));
    e.set_prevent_editing(true);
    key(&mut e, 0x5a, true, false);
    assert_eq!(e.text().unwrap(), "x");
}
#[test]
fn deleting_join_keeps_caret_out_of_crlf() {
    let mut e = EditorView::from_text("\rX\n").unwrap();
    key(&mut e, 0x27, false, false);
    assert_eq!(e.caret().0, 1);
    key(&mut e, 0x2e, false, false);
    assert_eq!(e.text().unwrap(), "\r\n");
    assert_ne!(e.caret().0, 1);
}
#[test]
fn vertical_does_not_enter_crlf() {
    let mut e = EditorView::from_text("ab\nx\r\ny").unwrap();
    key(&mut e, 0x23, false, false);
    key(&mut e, 0x28, false, false);
    assert_ne!(e.caret().0, 5, "between CR and LF");
}
#[test]
fn left_collapses_selection() {
    let mut e = EditorView::from_text("hello").unwrap();
    key(&mut e, 0x41, true, false);
    key(&mut e, 0x25, false, false);
    assert_eq!(e.caret().0, 0);
}
#[test]
fn typing_reveals_caret() {
    let mut e = EditorView::from_text("").unwrap();
    size(&mut e, 80., 34.);
    warm(&mut e);
    for _ in 0..8 {
        key(&mut e, 0x0d, false, false);
        warm(&mut e);
    }
    assert!(
        e.scroll_offset() > 0.,
        "caret {} scroll {}",
        e.caret().0,
        e.scroll_offset()
    );
}
#[test]
fn select_all_invalidates_middle() {
    let mut e = EditorView::from_text("a\nb\nc").unwrap();
    size(&mut e, 200., 200.);
    warm(&mut e);
    let c = key(&mut e, 0x41, true, false);
    assert!(
        c.requests.iter().any(|r| match r {
            ViewRequest::Invalidate(r) => r.y.0 <= 20. && r.y.0 + r.height.0 > 25.,
            _ => false,
        }),
        "{:?}",
        c.requests
    );
}
#[test]
fn long_click_does_not_scroll_to_end() {
    let mut e = EditorView::from_text(&"x".repeat(100000)).unwrap();
    size(&mut e, 80., 85.);
    warm(&mut e);
    event(
        &mut e,
        InputKind::MouseDown {
            at: Point {
                x: Dip(1.),
                y: Dip(1.),
            },
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    );
    assert!(e.scroll_offset() < 100., "{}", e.scroll_offset());
}
#[test]
fn cold_right_large_line_does_not_skip() {
    let mut e = EditorView::from_text(&format!("{}\ntail", "x".repeat(9000))).unwrap();
    key(&mut e, 0x27, false, false);
    assert_eq!(e.caret().0, 1);
}
#[test]
fn undo_keeps_cached_line_heights() {
    let mut e = EditorView::from_text("abcdefghijklmnopqrstuvwxyz0123456789\nnext").unwrap();
    size(&mut e, 40., 400.);
    warm(&mut e);
    key(&mut e, 0x23, true, false);
    event(&mut e, InputKind::Char('z'));
    warm(&mut e);
    key(&mut e, 0x5a, true, false);
    warm(&mut e);
    let mut l = DisplayList::default();
    e.draw(
        &mut l,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    let ys: Vec<_> = l
        .commands
        .iter()
        .filter_map(|c| {
            if let DrawCommand::GlyphRun { at, .. } = c {
                Some(at.y.0)
            } else {
                None
            }
        })
        .collect();
    assert!(
        ys.windows(2).all(|v| v[1] >= v[0]),
        "overlapping lines: {ys:?}"
    );
}
#[test]
fn paste_preserves_text() {
    let mut e = EditorView::from_text("a").unwrap();
    e.set_clipboard(Box::new(MemoryClipboard("\r\nz".into())));
    key(&mut e, 0x56, true, false);
    assert_eq!(e.text().unwrap(), "\r\nza");
}
#[test]
fn shrinking_long_line_still_paints() {
    let mut e = EditorView::from_text(&"x".repeat(100000)).unwrap();
    size(&mut e, 200., 200.);
    warm(&mut e);
    key(&mut e, 0x41, true, false);
    event(&mut e, InputKind::Char('z'));
    warm(&mut e);
    let mut l = DisplayList::default();
    e.draw(
        &mut l,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    assert!(
        l.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::GlyphRun { .. })),
        "{:?}",
        l.commands
    );
}
#[test]
fn vertical_avoids_cluster_in_warm_layout() {
    let mut e = EditorView::from_text("a\ne\u{301}x").unwrap();
    warm(&mut e);
    key(&mut e, 0x23, false, false);
    key(&mut e, 0x28, false, false);
    assert_ne!(e.caret().0, 3, "inside e + accent");
}
#[test]
fn home_at_end_affinity_stays_on_row() {
    let text = "abcdefghijklmnopqrstuvwxyz";
    let mut e = EditorView::from_text(text).unwrap();
    size(&mut e, 40., 200.);
    warm(&mut e);
    key(&mut e, 0x28, false, false);
    key(&mut e, 0x23, false, false);
    key(&mut e, 0x24, false, false);
    let measure = DirectWriteLayout::new().unwrap();
    let s = qe_buffer::PieceStore::new(text);
    let l = measure.layout_line(&s, LineIndex(0), Dip(40.)).unwrap();
    assert_eq!(e.caret(), l.rows[1].range.start);
}
#[test]
fn key_completion_does_not_repaint_whole_view() {
    let mut e = EditorView::from_text("a\nb\nc").unwrap();
    size(&mut e, 200., 200.);
    warm(&mut e);
    event(&mut e, InputKind::Char('x'));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut c = ViewContext::default();
    e.poll_layout(&mut c);
    assert!(
        !c.requests
            .iter()
            .any(|r| matches!(r,ViewRequest::Invalidate(r) if r.height.0==200.)),
        "{:?}",
        c.requests
    );
}
#[test]
fn clipboard_failure_requests_paint() {
    struct Fail;
    impl qe_editor::Clipboard for Fail {
        fn read(&mut self) -> Result<Option<String>, String> {
            Err("clipboard denied".into())
        }
        fn write(&mut self, _: &str) -> Result<(), String> {
            Err("clipboard denied".into())
        }
    }
    let mut e = EditorView::from_text("a").unwrap();
    warm(&mut e);
    e.set_clipboard(Box::new(Fail));
    let c = key(&mut e, 0x56, true, false);
    assert!(
        c.requests
            .iter()
            .any(|r| matches!(r, ViewRequest::Invalidate(_))),
        "{:?}",
        c.requests
    );
}
#[test]
fn word_movement_crosses_line() {
    let mut e = EditorView::from_text("abc\ndef").unwrap();
    key(&mut e, 0x23, false, false);
    key(&mut e, 0x27, true, false); /* AppKit goes to the end of the next word. */
    assert_eq!(e.caret().0, 7);
}
#[test]
fn distant_clicks_do_not_select_word() {
    let mut e = EditorView::from_text("hello world").unwrap();
    warm(&mut e);
    for (time, x) in [(1_000_000, 1.), (1_100_000, 50.)] {
        let mut c = ViewContext::default();
        e.handle(
            &InputEvent {
                time_us: time,
                kind: InputKind::MouseDown {
                    at: Point {
                        x: Dip(x),
                        y: Dip(1.),
                    },
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                },
            },
            &mut c,
        );
        e.handle(
            &InputEvent {
                time_us: time,
                kind: InputKind::MouseUp {
                    at: Point {
                        x: Dip(x),
                        y: Dip(1.),
                    },
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                },
            },
            &mut c,
        );
    }
    assert!(e.selection().is_empty(), "{:?}", e.selection());
}
#[test]
fn short_font_fills_view() {
    let mut e = EditorView::from_text(&"x\n".repeat(100)).unwrap();
    e.set_font("Consolas", 5.).unwrap();
    size(&mut e, 200., 100.);
    warm(&mut e);
    let mut l = DisplayList::default();
    e.draw(
        &mut l,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    let y = l
        .commands
        .iter()
        .filter_map(|c| {
            if let DrawCommand::GlyphRun { at, .. } = c {
                Some(at.y.0)
            } else {
                None
            }
        })
        .fold(0f32, f32::max);
    assert!(y > 90., "last baseline {y}");
}
#[test]
fn shifted_worker_result_keeps_line_height() {
    let text = format!("a\n{}\nz", "x".repeat(36));
    let mut e = EditorView::from_text(&text).unwrap();
    size(&mut e, 40., 400.);
    e.poll_layout(&mut ViewContext::default());
    std::thread::sleep(std::time::Duration::from_millis(20));
    key(&mut e, 0x0d, false, false);
    warm(&mut e);
    let mut l = DisplayList::default();
    e.draw(
        &mut l,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    let ys: Vec<_> = l
        .commands
        .iter()
        .filter_map(|c| {
            if let DrawCommand::GlyphRun { at, .. } = c {
                Some(at.y.0)
            } else {
                None
            }
        })
        .collect();
    assert!(
        ys.windows(2).all(|v| v[1] >= v[0]),
        "overlapping shifted lines {ys:?}"
    );
}
#[test]
fn page_down_counts_visual_rows_across_lines() {
    let text = format!("a\n{}\nz", "x".repeat(200));
    let mut e = EditorView::from_text(&text).unwrap();
    size(&mut e, 40., 85.);
    warm(&mut e);
    key(&mut e, 0x22, false, false);
    assert!(
        e.caret().0 > 2 && e.caret().0 < 202,
        "caret {} skipped wrapped line",
        e.caret().0
    );
}
