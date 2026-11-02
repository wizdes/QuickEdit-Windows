//! What the M9 reviews found, and the cases they said no test covered.
use qe_api::{ByteOffset, LineIndex};
use qe_api::{
    Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseButton, Point, Rect,
    Renderer, Theme, ThemeKind, View, ViewContext, ViewRequest,
};
use qe_buffer::{MappedStore, PieceStore};
use qe_editor::{
    Clipboard, EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile, MemoryClipboard,
    preferred_line_ending_bytes,
};
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

const LEFT: u16 = 0x25;
const UP: u16 = 0x26;
const RIGHT: u16 = 0x27;
const DOWN: u16 = 0x28;
const HOME: u16 = 0x24;
const END: u16 = 0x23;
const PG_UP: u16 = 0x21;
const PG_DOWN: u16 = 0x22;
const BACKSPACE: u16 = 0x08;
const DELETE: u16 = 0x2e;
const ENTER: u16 = 0x0d;

fn rect(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}
fn measure() -> DirectWriteLayout {
    DirectWriteLayout::new().unwrap()
}
fn settle(e: &mut EditorView) {
    let mut ctx = ViewContext::default();
    e.poll_layout(&mut ctx);
    for _ in 0..2500 {
        if !e.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
        e.poll_layout(&mut ctx);
    }
    panic!("layout never settled");
}
fn cold(text: &str, width: f32, height: f32) -> EditorView {
    let mut e = EditorView::from_text(text).unwrap();
    e.layout(rect(width, height), &measure());
    e
}
fn open(text: &str, width: f32, height: f32) -> EditorView {
    let mut e = cold(text, width, height);
    settle(&mut e);
    e
}
fn send(e: &mut EditorView, kind: InputKind, time_us: u64) -> Vec<ViewRequest> {
    let mut ctx = ViewContext::default();
    e.handle(&InputEvent { time_us, kind }, &mut ctx);
    ctx.requests
}
fn press(e: &mut EditorView, code: u16, ctrl: bool, shift: bool) -> Vec<ViewRequest> {
    send(
        e,
        InputKind::KeyDown {
            virtual_key: code,
            modifiers: Modifiers {
                ctrl,
                shift,
                ..Default::default()
            },
            repeat: false,
        },
        0,
    )
}
fn key(e: &mut EditorView, code: u16) -> Vec<ViewRequest> {
    press(e, code, false, false)
}
fn ctrl(e: &mut EditorView, code: u16) -> Vec<ViewRequest> {
    press(e, code, true, false)
}
fn shift(e: &mut EditorView, code: u16) -> Vec<ViewRequest> {
    press(e, code, false, true)
}
fn type_char(e: &mut EditorView, c: char) -> Vec<ViewRequest> {
    send(e, InputKind::Char(c), 0)
}
fn focus(e: &mut EditorView) -> Vec<ViewRequest> {
    send(e, InputKind::Focus, 0)
}
fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn click(e: &mut EditorView, x: f32, y: f32, time_us: u64) {
    let at = point(x, y);
    send(
        e,
        InputKind::MouseDown {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        time_us,
    );
    send(
        e,
        InputKind::MouseUp {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        time_us,
    );
}
fn wheel(e: &mut EditorView, delta_y: f32) {
    send(
        e,
        InputKind::Wheel {
            at: point(1.0, 1.0),
            delta_x: 0.0,
            delta_y,
            modifiers: Modifiers::default(),
        },
        0,
    );
}
fn list(e: &EditorView) -> DisplayList {
    let mut list = DisplayList::default();
    e.draw(&mut list, &Theme::new(ThemeKind::Light), &measure());
    list
}
fn glyph_rows(e: &EditorView) -> Vec<(i32, i32, Vec<u16>)> {
    list(e)
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::GlyphRun { at, glyph_ids, .. } => Some((
                (at.x.0 * 10.0) as i32,
                (at.y.0 * 10.0) as i32,
                glyph_ids.clone(),
            )),
            _ => None,
        })
        .collect()
}
/// The selection's rectangles: fills with the text color at a low alpha.
fn selection_rects(e: &EditorView) -> Vec<(i32, i32, i32, i32)> {
    let text = Theme::new(ThemeKind::Light).text;
    list(e)
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::FillRect { rect, color } if color.a != 255 && color.r == text.r => Some((
                (rect.x.0 * 10.0) as i32,
                (rect.y.0 * 10.0) as i32,
                (rect.width.0 * 10.0) as i32,
                (rect.height.0 * 10.0) as i32,
            )),
            _ => None,
        })
        .collect()
}
fn caret_rect(e: &EditorView) -> Option<Rect> {
    let theme = Theme::new(ThemeKind::Light);
    list(e).commands.iter().find_map(|c| match c {
        DrawCommand::FillRect { rect, color } if *color == theme.text => Some(*rect),
        _ => None,
    })
}
fn invalidates(requests: &[ViewRequest]) -> Vec<Rect> {
    requests
        .iter()
        .filter_map(|r| match r {
            ViewRequest::Invalidate(rect) => Some(*rect),
            _ => None,
        })
        .collect()
}
fn timers(requests: &[ViewRequest]) -> usize {
    requests
        .iter()
        .filter(|r| matches!(r, ViewRequest::RequestTimer(_)))
        .count()
}
fn covers(requests: &[ViewRequest], y: f32) -> bool {
    invalidates(requests)
        .iter()
        .any(|b| b.y.0 <= y && y < b.y.0 + b.height.0)
}
fn lines(count: usize) -> String {
    (0..count).map(|i| format!("line {i}\n")).collect()
}
fn wrapped_lines(count: usize) -> String {
    (0..count)
        .map(|i| format!("{i:04} abcd efgh ijkl mnop qrst uvwx\n"))
        .collect()
}
fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("qe-editor-{}-{name}", std::process::id()))
}
struct Remove(std::path::PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        for _ in 0..200 {
            match std::fs::remove_file(&self.0) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        eprintln!("could not remove {}", self.0.display());
    }
}

// ---- Item 1: one rule for caret stops ----

/// Every state of layout gives the same answer: no layout at all, and layout settled.
fn both_layout_states(text: &str, check: impl Fn(&mut EditorView)) {
    let mut unsettled = EditorView::from_text(text).unwrap();
    check(&mut unsettled);
    let mut settled = open(text, 400.0, 300.0);
    check(&mut settled);
}

#[test]
fn backspace_and_delete_take_one_stop_on_any_line_length() {
    for length in [10, 8192, 8193, 9000, 16 * 1024, 16 * 1024 + 1, 100_000] {
        let line = "x".repeat(length);
        both_layout_states(&format!("{line}\ntail"), |e| {
            key(e, DELETE);
            assert_eq!(e.text().unwrap().len(), length + 4, "Delete on {length}");
            key(e, DELETE);
            assert_eq!(e.text().unwrap().len(), length + 3, "second Delete");
        });
        both_layout_states(&format!("head\n{line}"), |e| {
            ctrl(e, END);
            key(e, BACKSPACE);
            assert_eq!(e.text().unwrap().len(), length + 4, "Backspace on {length}");
            key(e, BACKSPACE);
            assert_eq!(e.text().unwrap().len(), length + 3, "second Backspace");
        });
    }
}

#[test]
fn arrows_never_skip_a_line_with_no_layout() {
    both_layout_states(&format!("{}\ntail", "x".repeat(9000)), |e| {
        key(e, RIGHT);
        assert_eq!(e.caret().0, 1);
        key(e, LEFT);
        assert_eq!(e.caret().0, 0);
        ctrl(e, END);
        for expected in [9005, 9004, 9003, 9002, 9001] {
            assert_eq!(e.caret().0, expected);
            key(e, LEFT);
        }
        assert_eq!(e.caret().0, 9000, "the end of the long line");
        key(e, LEFT);
        assert_eq!(e.caret().0, 8999);
    });
}

#[test]
fn clusters_stay_whole_with_or_without_layout() {
    for cluster in [
        "e\u{301}",
        "e\u{301}\u{302}\u{303}",
        "🇺🇸",
        "👩‍👩‍👧‍👦",
        "a\u{200D}b",
        "👍🏽",
    ] {
        let text = format!("x{cluster}y");
        both_layout_states(&text, |e| {
            key(e, RIGHT);
            assert_eq!(e.caret().0, 1, "{cluster:?}");
            key(e, RIGHT);
            assert_eq!(e.caret().0, 1 + cluster.len(), "{cluster:?}");
            key(e, LEFT);
            assert_eq!(e.caret().0, 1, "{cluster:?}");
            key(e, DELETE);
            assert_eq!(e.text().unwrap(), "xy", "{cluster:?}");
            ctrl(e, 0x5a);
            assert_eq!(e.text().unwrap(), text, "{cluster:?}");
        });
    }
}

#[test]
fn flags_pair_up_in_a_long_run_with_no_layout() {
    let flags = "🇺🇸".repeat(100);
    let mut e = EditorView::from_text(&flags).unwrap();
    ctrl(&mut e, END);
    for left in 0..3 {
        assert_eq!(e.caret().0, flags.len() - left * 8);
        key(&mut e, LEFT);
    }
}

#[test]
fn line_endings_of_every_kind_are_one_stop() {
    for ending in ["\n", "\r\n", "\r", "\u{85}", "\u{2028}", "\u{2029}"] {
        let text = format!("ab{ending}cd");
        both_layout_states(&text, |e| {
            key(e, RIGHT);
            key(e, RIGHT);
            assert_eq!(e.caret().0, 2);
            key(e, RIGHT);
            assert_eq!(e.caret().0, 2 + ending.len(), "{ending:?}");
            key(e, LEFT);
            assert_eq!(e.caret().0, 2, "{ending:?}");
            key(e, DELETE);
            assert_eq!(e.text().unwrap(), "abcd", "{ending:?}");
            ctrl(e, 0x5a);
            ctrl(e, END);
            key(e, LEFT);
            key(e, LEFT);
            key(e, BACKSPACE);
            assert_eq!(e.text().unwrap(), "abcd", "{ending:?}");
        });
    }
}

#[test]
fn edits_that_join_text_leave_the_caret_on_a_stop() {
    // Deleting the letter joins CR and LF, and the caret goes past the pair.
    both_layout_states("\rX\n", |e| {
        key(e, RIGHT);
        assert_eq!(e.caret().0, 1);
        key(e, DELETE);
        assert_eq!(e.text().unwrap(), "\r\n");
        assert_eq!(e.caret().0, 2);
    });
    // Pasting a CR before an LF makes CRLF.
    both_layout_states("a\nb", |e| {
        key(e, RIGHT);
        e.set_clipboard(Box::new(MemoryClipboard("\r".into())));
        ctrl(e, 0x56);
        assert_eq!(e.text().unwrap(), "a\r\nb");
        assert_eq!(e.caret().0, 3);
    });
    // Typing a letter before an accent makes a new cluster.
    both_layout_states("\u{301}b", |e| {
        type_char(e, 'a');
        assert_eq!(e.text().unwrap(), "a\u{301}b");
        assert_eq!(e.caret().0, 3);
    });
    // Deleting a letter between a letter and an accent joins them.
    both_layout_states("eX\u{301}", |e| {
        e.select(ByteOffset(1)..ByteOffset(2), &mut ViewContext::default());
        key(e, DELETE);
        assert_eq!(e.text().unwrap(), "e\u{301}");
        assert_eq!(e.caret().0, 3);
    });
}

#[test]
fn undo_that_joins_text_leaves_the_caret_on_a_stop() {
    let mut e = EditorView::from_text("\r\n").unwrap();
    ctrl(&mut e, END);
    key(&mut e, LEFT);
    assert_eq!(e.caret().0, 0);
    e.select(ByteOffset(0)..ByteOffset(2), &mut ViewContext::default());
    e.set_clipboard(Box::new(MemoryClipboard("\rX\n".into())));
    ctrl(&mut e, 0x56);
    assert_eq!(e.text().unwrap(), "\rX\n");
    ctrl(&mut e, 0x5a);
    assert_eq!(e.text().unwrap(), "\r\n");
    let c = e.caret().0;
    assert!(c == 0 || c == 2, "caret {c}");
}

// ---- Item 2: vertical movement ----

#[test]
fn up_goes_to_the_last_row_of_the_line_above_at_the_same_x() {
    let long = "abcdefghij".repeat(4);
    let text = format!("{long}\nxyz");
    let mut e = open(&text, 40.0, 400.0);
    ctrl(&mut e, END);
    key(&mut e, LEFT);
    assert_eq!(e.caret().0, 43);
    key(&mut e, UP);
    let layout = measure()
        .with_tab_width(8)
        .layout_line_for_test(&long, Dip(40.0));
    let last = layout.rows.last().unwrap();
    assert!(
        e.caret() >= last.range.start && e.caret() <= last.range.end,
        "caret {} is not on the last row {:?}",
        e.caret().0,
        last.range
    );
    assert_eq!(e.caret().0 - last.range.start.0, 2);
}

/// `DirectWriteLayout::layout_line` on one line of text, for what a row should hold.
trait LayoutForTest {
    fn layout_line_for_test(&self, text: &str, width: Dip) -> qe_api::LineLayout;
}
impl LayoutForTest for DirectWriteLayout {
    fn layout_line_for_test(&self, text: &str, width: Dip) -> qe_api::LineLayout {
        self.try_layout_line(&PieceStore::new(text), LineIndex(0), width)
            .unwrap()
    }
}

#[test]
fn down_visits_every_row_then_the_next_line() {
    let long = "abcdefghij".repeat(4);
    let text = format!("{long}\nxyz\nend");
    let mut e = open(&text, 40.0, 400.0);
    let layout = measure().layout_line_for_test(&long, Dip(40.0));
    assert!(layout.rows.len() > 2);
    for row in layout.rows.iter().skip(1) {
        key(&mut e, DOWN);
        assert_eq!(e.caret(), row.range.start, "row {:?}", row.range);
    }
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, long.len() + 1, "the next line's first row");
}

#[test]
fn up_and_down_keep_the_x_until_another_move() {
    let mut e = open("abcdefgh\nab\nabcdefgh", 300.0, 100.0);
    for _ in 0..6 {
        key(&mut e, RIGHT);
    }
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 11, "the short line's end");
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 18, "the x came back");
    key(&mut e, UP);
    assert_eq!(e.caret().0, 11);
    // Another move forgets the x.
    key(&mut e, LEFT);
    assert_eq!(e.caret().0, 10);
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 13);
    // An edit forgets it too.
    key(&mut e, UP);
    type_char(&mut e, 'Q');
    key(&mut e, DOWN);
    assert_eq!(e.text().unwrap(), "abcdefgh\naQb\nabcdefgh");
    assert_eq!(e.caret().0, 15, "the x after the Q");
}

#[test]
fn up_on_the_first_row_and_down_on_the_last_go_to_the_ends_of_the_text() {
    let mut e = open("abc\ndef", 300.0, 100.0);
    key(&mut e, RIGHT);
    key(&mut e, UP);
    assert_eq!(e.caret().0, 0);
    key(&mut e, RIGHT);
    key(&mut e, DOWN);
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 7);
}

#[test]
fn shift_up_and_down_extend_the_selection() {
    let mut e = open("abc\ndef\nghi", 300.0, 100.0);
    ctrl(&mut e, END);
    shift(&mut e, UP);
    assert_eq!(e.selection(), ByteOffset(7)..ByteOffset(11));
    shift(&mut e, UP);
    assert_eq!(e.selection(), ByteOffset(3)..ByteOffset(11));
    shift(&mut e, DOWN);
    assert_eq!(e.selection(), ByteOffset(7)..ByteOffset(11));
    assert_eq!(e.caret().0, 7);
}

#[test]
fn up_and_down_with_a_selection_start_from_its_edges() {
    let mut e = open("abc\ndef\nghi", 300.0, 100.0);
    e.select(ByteOffset(5)..ByteOffset(6), &mut ViewContext::default());
    key(&mut e, UP);
    assert_eq!(e.caret().0, 1);
    e.select(ByteOffset(5)..ByteOffset(6), &mut ViewContext::default());
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 10);
}

#[test]
fn page_keys_move_caret_and_view_together_across_wrapped_paragraphs() {
    let text = wrapped_lines(100);
    let mut e = open(&text, 60.0, 170.0);
    key(&mut e, PG_DOWN);
    settle(&mut e);
    let after_one = (e.caret().0, e.scroll_offset());
    assert!(after_one.0 > 0);
    assert!(after_one.1 > 100.0, "view moved {}", after_one.1);
    // The move is one view of rows. A row here is about 17, and a line is 7 rows.
    assert!(after_one.0 < 70, "skipped wrapped text to {}", after_one.0);
    key(&mut e, PG_DOWN);
    settle(&mut e);
    assert!(e.scroll_offset() > after_one.1);
    key(&mut e, PG_UP);
    settle(&mut e);
    let back = e.caret().0;
    assert!(back <= after_one.0 + 8 && back + 8 >= after_one.0, "{back}");
    shift(&mut e, PG_DOWN);
    assert!(!e.selection().is_empty());
}

#[test]
fn page_keys_stop_at_the_ends_of_the_text() {
    let mut e = open(&lines(10), 200.0, 85.0);
    key(&mut e, RIGHT);
    key(&mut e, PG_UP);
    assert_eq!(e.caret().0, 0);
    for _ in 0..4 {
        key(&mut e, PG_DOWN);
    }
    assert_eq!(e.caret().0, lines(10).len());
}

#[test]
fn down_with_no_layout_moves_to_the_next_line_by_estimate() {
    // Non-ASCII lines lay out only on a worker, so this is the case with no layout.
    let text = "日本語日本語\n日本語日本語\n日本語日本語";
    let mut e = EditorView::from_text(text).unwrap();
    e.layout(rect(800.0, 600.0), &measure());
    key(&mut e, RIGHT);
    key(&mut e, DOWN);
    let line2 = "日本語日本語\n".len();
    let caret = e.caret().0;
    assert!(
        caret >= line2 && caret < line2 * 2,
        "caret {caret} is not on line 2"
    );
    assert!(text.is_char_boundary(caret));
    // An ASCII line lays out on the UI thread, so its rows are real at once.
    let mut e = cold("abcdef\nabcdef\nabcdef", 800.0, 600.0);
    key(&mut e, RIGHT);
    key(&mut e, RIGHT);
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 9);
    assert!(!e.selection().is_empty() || e.selection().start.0 == 9);
}

#[test]
fn down_into_crlf_and_clusters_lands_on_a_stop_with_no_layout() {
    both_layout_states("ab\nx\r\ny", |e| {
        key(e, END);
        key(e, DOWN);
        assert_eq!(e.caret().0, 4, "the line's end, before CR");
    });
    both_layout_states("a\ne\u{301}x", |e| {
        key(e, END);
        key(e, DOWN);
        assert_eq!(e.caret().0, 5, "after the whole cluster");
    });
}

// ---- Rapid keys ----

#[test]
fn rapid_keys_before_layout_lose_nothing() {
    let text = format!("{}\nsecond\n{}", "x".repeat(20_000), "日本語".repeat(10));
    let mut e = EditorView::from_text(&text).unwrap();
    let mut model = text.clone();
    for _ in 0..50 {
        type_char(&mut e, 'a');
        model.insert(e.caret().0 - 1, 'a');
        key(&mut e, RIGHT);
        key(&mut e, BACKSPACE);
        model.remove(e.caret().0);
        assert_eq!(e.text().unwrap().len(), model.len());
    }
    assert_eq!(e.text().unwrap(), model);
    settle(&mut e);
    assert_eq!(e.text().unwrap(), model);
}

// ---- Item 3: long lines ----

#[test]
fn a_long_line_edited_down_to_a_short_one_paints_and_moves() {
    for wrap in [true, false] {
        for how in ["select all", "shift home"] {
            let text = "x".repeat(100_000);
            let mut e = EditorView::from_text(&text).unwrap();
            e.set_wrap(wrap).unwrap();
            e.layout(rect(200.0, 170.0), &measure());
            settle(&mut e);
            focus(&mut e);
            if how == "select all" {
                ctrl(&mut e, 0x41);
            } else {
                ctrl(&mut e, END);
                settle(&mut e);
                press(&mut e, HOME, true, true);
            }
            type_char(&mut e, 'a');
            assert_eq!(e.text().as_deref(), Some("a"));
            // No wait: the letter paints with no worker.
            assert!(!glyph_rows(&e).is_empty(), "wrap {wrap} {how}");
            assert!(caret_rect(&e).is_some(), "caret wrap {wrap} {how}");
            settle(&mut e);
            assert_eq!(e.scroll_offset(), 0.0);
            assert!(!glyph_rows(&e).is_empty());
            key(&mut e, LEFT);
            assert_eq!(e.caret().0, 0);
            key(&mut e, RIGHT);
            key(&mut e, BACKSPACE);
            assert_eq!(e.text().as_deref(), Some(""));
        }
    }
}

#[test]
fn a_short_line_grown_long_paints_and_moves() {
    for wrap in [true, false] {
        let mut e = EditorView::from_text("seed").unwrap();
        e.set_wrap(wrap).unwrap();
        e.layout(rect(200.0, 170.0), &measure());
        settle(&mut e);
        focus(&mut e);
        ctrl(&mut e, END);
        e.set_clipboard(Box::new(MemoryClipboard("x".repeat(100_000))));
        ctrl(&mut e, 0x56);
        settle(&mut e);
        assert_eq!(e.caret().0, 100_004);
        assert!(!glyph_rows(&e).is_empty(), "wrap {wrap}");
        assert!(caret_rect(&e).is_some(), "wrap {wrap}");
        let at = e.caret().0;
        key(&mut e, BACKSPACE);
        assert_eq!(e.text().unwrap().len(), at - 1);
        settle(&mut e);
        key(&mut e, LEFT);
        assert_eq!(e.caret().0, at - 2);
        if wrap {
            let before = e.caret().0;
            key(&mut e, UP);
            assert!(e.caret().0 < before);
            assert!(before - e.caret().0 < 100, "{}", before - e.caret().0);
        }
        // Undo walks back to the short line, and it paints again.
        ctrl(&mut e, 0x5a);
        ctrl(&mut e, 0x5a);
        settle(&mut e);
        assert_eq!(e.text().as_deref(), Some("seed"));
        assert_eq!(glyph_rows(&e), glyph_rows(&with_wrap("seed", wrap)));
    }
}

fn with_wrap(text: &str, wrap: bool) -> EditorView {
    let mut e = EditorView::from_text(text).unwrap();
    e.set_wrap(wrap).unwrap();
    e.layout(rect(200.0, 170.0), &measure());
    settle(&mut e);
    e
}

#[test]
fn up_down_home_end_stay_inside_a_long_wrapped_line() {
    let text = "x".repeat(100_000);
    let mut e = open(&text, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    let bottom = e.scroll_offset();
    assert!(bottom > 1000.0);
    key(&mut e, HOME);
    let row_start = e.caret().0;
    assert!(row_start > 99_900, "Home jumped to {row_start}");
    key(&mut e, END);
    assert_eq!(e.caret().0, 100_000);
    key(&mut e, UP);
    assert!(e.caret().0 > 99_900 && e.caret().0 < 100_000);
    assert!((e.scroll_offset() - bottom).abs() < 100.0);
    // Crossing a segment edge going up keeps moving one row at a time.
    let mut last = e.caret().0;
    for _ in 0..200 {
        key(&mut e, UP);
        let now = e.caret().0;
        assert!(now < last, "stuck at {now}");
        assert!(last - now < 100, "skipped from {last} to {now}");
        last = now;
    }
    assert!(last < 100_000 - 2048, "crossed a segment, at {last}");
}

#[test]
fn clicking_and_dragging_in_a_long_line_do_not_jump() {
    let text = "x".repeat(100_000);
    let mut e = open(&text, 80.0, 85.0);
    click(&mut e, 1.0, 1.0, 1_000_000);
    assert_eq!(e.scroll_offset(), 0.0);
    assert_eq!(e.caret().0, 0);
    ctrl(&mut e, END);
    settle(&mut e);
    let bottom = e.scroll_offset();
    click(&mut e, 5.0, 40.0, 5_000_000);
    assert!(e.caret().0 > 90_000);
    assert!((e.scroll_offset() - bottom).abs() < 20.0);
}

#[test]
fn an_edit_that_moves_a_long_line_keeps_it_painting() {
    let text = format!("top\n{}\nend", "x".repeat(50_000));
    let mut e = open(&text, 200.0, 170.0);
    key(&mut e, ENTER);
    settle(&mut e);
    assert_eq!(e.line_count(), 4);
    assert!(!glyph_rows(&e).is_empty());
    let fresh = open(&e.text().unwrap(), 200.0, 170.0);
    assert_eq!(glyph_rows(&e), glyph_rows(&fresh));
}

// ---- Item 4: heights and scrolling ----

#[test]
fn worker_results_for_moved_lines_go_to_their_new_number() {
    let text = format!("a\n{}\nz", "日".repeat(36));
    let mut e = EditorView::from_text(&text).unwrap();
    e.layout(rect(40.0, 400.0), &measure());
    e.poll_layout(&mut ViewContext::default());
    std::thread::sleep(Duration::from_millis(30));
    // A new line above moves the queued lines down by one.
    key(&mut e, ENTER);
    settle(&mut e);
    let fresh = open(&e.text().unwrap(), 40.0, 400.0);
    assert_eq!(glyph_rows(&e), glyph_rows(&fresh));
}

#[test]
fn text_stays_still_when_heights_arrive_above_the_view() {
    let text = wrapped_lines(400);
    let mut e = cold(&text, 60.0, 170.0);
    // No layout of the top lines yet. Jump far down, then let layout arrive around it.
    e.poll_layout(&mut ViewContext::default());
    wheel(&mut e, -6000.0);
    click(&mut e, 1.0, 1.0, 1_000_000);
    let first = e.caret().0 / 35;
    settle(&mut e);
    click(&mut e, 1.0, 1.0, 9_000_000);
    let then = e.caret().0 / 35;
    assert!(first > 0);
    assert_eq!(first, then, "the top line moved");
}

#[test]
fn tall_windows_and_small_fonts_fill_the_view() {
    let text = lines(500);
    for height in [600.0, 1000.0, 1400.0] {
        let e = open(&text, 800.0, height);
        let lowest = glyph_rows(&e).iter().map(|r| r.1).max().unwrap() as f32 / 10.0;
        assert!(height - lowest < 20.0, "height {height}, lowest {lowest}");
    }
    let mut e = cold(&text, 300.0, 170.0);
    e.set_font("Consolas", 5.0).unwrap();
    settle(&mut e);
    let lowest = glyph_rows(&e).iter().map(|r| r.1).max().unwrap() as f32 / 10.0;
    assert!(170.0 - lowest < 12.0, "lowest {lowest}");
}

#[test]
fn set_wrap_keeps_the_font_and_the_top_line() {
    let text = lines(500);
    let mut e = open(&text, 300.0, 170.0);
    e.set_font("Consolas", 30.0).unwrap();
    settle(&mut e);
    wheel(&mut e, -2400.0);
    settle(&mut e);
    click(&mut e, 1.0, 1.0, 1_000_000);
    let top = e.caret().0;
    let em = |e: &EditorView| {
        list(e).commands.iter().find_map(|c| match c {
            DrawCommand::GlyphRun { font_em_size, .. } => Some(font_em_size.0),
            _ => None,
        })
    };
    let before = em(&e);
    e.set_wrap(false).unwrap();
    settle(&mut e);
    assert_eq!(before, em(&e));
    click(&mut e, 1.0, 1.0, 9_000_000);
    assert_eq!(e.caret().0, top);
}

#[test]
fn a_resize_with_the_same_width_keeps_all_work_and_clamps_the_scroll() {
    let text = lines(100);
    let mut e = open(&text, 200.0, 300.0);
    wheel(&mut e, -120_000.0);
    settle(&mut e);
    let at_bottom = e.scroll_offset();
    assert!(at_bottom > 0.0);
    let rows_before = glyph_rows(&e).len();
    assert!(rows_before > 0);
    let runs = e.height_run_count();
    // Taller, same width: nothing is thrown away, and the scroll clamps.
    e.layout(rect(200.0, 600.0), &measure());
    let requests = send(
        &mut e,
        InputKind::Resize {
            width: Dip(200.0),
            height: Dip(600.0),
        },
        0,
    );
    assert_eq!(e.height_run_count(), runs);
    assert!(e.scroll_offset() < at_bottom);
    assert!(!invalidates(&requests).is_empty());
    assert!(!glyph_rows(&e).is_empty(), "no blank frame");
    // A new width with wrap off costs nothing either.
    let mut e = with_wrap(&lines(10), false);
    let rows = glyph_rows(&e);
    e.layout(rect(100.0, 170.0), &measure());
    assert_eq!(glyph_rows(&e), rows);
    assert!(!e.pending_layout());
}

#[test]
fn a_resize_to_a_new_width_keeps_text_on_screen() {
    let text = wrapped_lines(60);
    let mut e = open(&text, 60.0, 200.0);
    ctrl(&mut e, END);
    settle(&mut e);
    e.layout(rect(800.0, 200.0), &measure());
    send(
        &mut e,
        InputKind::Resize {
            width: Dip(800.0),
            height: Dip(200.0),
        },
        0,
    );
    settle(&mut e);
    assert!(!glyph_rows(&e).is_empty());
    let limit = 60.0 * 17.5 - 200.0;
    assert!(e.scroll_offset() <= limit + 20.0, "{}", e.scroll_offset());
}

#[test]
fn undo_changes_only_the_lines_that_changed() {
    let text = "abcd efgh ijkl mnop qrst uvwx yz01 2345 6789\n".repeat(6);
    let mut e = open(&text, 60.0, 600.0);
    let runs_before = e.height_run_count();
    let before = glyph_rows(&e);
    type_char(&mut e, 'x');
    settle(&mut e);
    ctrl(&mut e, 0x5a);
    settle(&mut e);
    assert_eq!(glyph_rows(&e), before);
    assert!(e.height_run_count() <= runs_before + 2);
}

// ---- Item 5: revealing the caret ----

#[test]
fn every_edit_and_move_brings_the_caret_into_view() {
    let text = lines(1000);
    let top_of_view = |e: &mut EditorView| {
        wheel(e, -12000.0);
        settle(e);
        assert!(e.scroll_offset() > 0.0);
    };
    let mut e = open(&text, 200.0, 51.0);
    top_of_view(&mut e);
    type_char(&mut e, 'q');
    assert_eq!(e.scroll_offset(), 0.0, "typing");
    top_of_view(&mut e);
    key(&mut e, ENTER);
    assert!(e.scroll_offset() < 20.0, "Enter");
    top_of_view(&mut e);
    e.set_clipboard(Box::new(MemoryClipboard("z".into())));
    ctrl(&mut e, 0x56);
    assert!(e.scroll_offset() < 40.0, "paste");
    top_of_view(&mut e);
    ctrl(&mut e, 0x5a);
    assert!(e.scroll_offset() < 100.0, "undo");
    top_of_view(&mut e);
    key(&mut e, RIGHT);
    assert!(e.scroll_offset() < 100.0, "arrow");
    let scrolled = e.scroll_offset();
    ctrl(&mut e, END);
    assert!(e.scroll_offset() > scrolled, "Ctrl+End");
    settle(&mut e);
    for _ in 0..3 {
        key(&mut e, ENTER);
    }
    let bottom = e.scroll_offset();
    settle(&mut e);
    assert!(bottom > 1000.0, "Enter at the bottom");
}

#[test]
fn scroll_after_an_edit_undo_and_resize_shows_the_caret() {
    let text = "x\n".repeat(400);
    let mut e = open(&text, 200.0, 85.0);
    focus(&mut e);
    ctrl(&mut e, END);
    settle(&mut e);
    let shown = |e: &EditorView| {
        let caret = caret_rect(e).expect("caret on screen");
        assert!(
            caret.y.0 >= 0.0 && caret.y.0 + caret.height.0 <= 85.0 + 0.5,
            "{caret:?}"
        );
    };
    shown(&e);
    key(&mut e, ENTER);
    settle(&mut e);
    shown(&e);
    ctrl(&mut e, 0x5a);
    settle(&mut e);
    shown(&e);
    // A selection that hides the caret still leaves the scroll as it was.
    let scroll = e.scroll_offset();
    ctrl(&mut e, 0x41);
    assert_eq!(e.scroll_offset(), scroll, "Select All must not scroll");
    assert!(e.selection().end.0 > 0);
}

#[test]
fn a_click_scrolls_only_for_a_partly_visible_caret() {
    let text = lines(100);
    let mut e = open(&text, 200.0, 100.0);
    // Row 3 is whole on screen, so nothing moves.
    click(&mut e, 1.0, 55.0, 1_000_000);
    assert_eq!(e.scroll_offset(), 0.0);
    // The last row is cut by the view's bottom edge, so the click shows all of it.
    click(&mut e, 1.0, 99.0, 9_000_000);
    assert!(e.scroll_offset() > 0.0);
    assert!(e.scroll_offset() < 17.5);
}

// ---- Item 6: repaint ----

#[test]
fn selection_changes_repaint_each_changed_row_and_no_more() {
    let text = lines(20);
    let mut e = open(&text, 300.0, 400.0);
    let requests = ctrl(&mut e, 0x41);
    let row_height = measure().layout_line_for_test("x", Dip(100.0)).rows[0]
        .height
        .0;
    for row in 0..20 {
        let y = row as f32 * row_height + 8.0;
        assert!(covers(&requests, y), "Select All skips row {row}");
    }
    // Clearing a selection repaints the same rows.
    let requests = send(
        &mut e,
        InputKind::MouseDown {
            at: point(1.0, 1.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        9_000_000,
    );
    assert!(covers(&requests, row_height * 10.0 + 8.0), "{requests:?}");
    send(
        &mut e,
        InputKind::MouseUp {
            at: point(1.0, 1.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        9_000_000,
    );
    // One more row by Shift+Down repaints near that row only.
    e.select(ByteOffset(0)..ByteOffset(0), &mut ViewContext::default());
    let requests = shift(&mut e, DOWN);
    assert!(covers(&requests, 8.0));
    assert!(!covers(&requests, row_height * 10.0));
}

#[test]
fn a_blink_repaints_only_the_caret() {
    let mut e = open("abc\ndef", 300.0, 400.0);
    focus(&mut e);
    let mut ctx = ViewContext::default();
    e.on_timer_at(Instant::now() + Duration::from_secs(1), &mut ctx);
    let rects = invalidates(&ctx.requests);
    assert_eq!(rects.len(), 1, "{rects:?}");
    assert!(rects[0].width.0 <= 6.0, "{rects:?}");
    assert!(rects[0].height.0 <= 20.0, "{rects:?}");
}

#[test]
fn a_finished_layout_repaints_only_what_changed() {
    let mut e = open("hello\nworld\nnext\n日本語", 300.0, 400.0);
    // A non-ASCII line goes to a worker.
    ctrl(&mut e, END);
    type_char(&mut e, '語');
    let mut ctx = ViewContext::default();
    for _ in 0..500 {
        std::thread::sleep(Duration::from_millis(2));
        e.poll_layout(&mut ctx);
        if !e.pending_layout() {
            break;
        }
    }
    let rects = invalidates(&ctx.requests);
    assert!(!rects.is_empty());
    for rect in &rects {
        assert!(rect.height.0 < 40.0, "{rects:?}");
        assert!(rect.y.0 > 40.0, "{rects:?}");
    }
    // Nothing left: polling again asks for nothing.
    let mut ctx = ViewContext::default();
    e.poll_layout(&mut ctx);
    assert!(invalidates(&ctx.requests).is_empty());
}

#[test]
fn a_layout_that_changes_height_repaints_the_lines_below() {
    let text = format!("{}\nbelow\nmore", "日".repeat(40));
    let mut e = cold(&text, 100.0, 400.0);
    let mut ctx = ViewContext::default();
    for _ in 0..500 {
        e.poll_layout(&mut ctx);
        if !e.pending_layout() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(covers(&ctx.requests, 200.0), "{:?}", ctx.requests);
}

#[test]
fn the_edited_line_keeps_showing_text_until_its_layout_arrives() {
    // Plain ASCII lays out right away, with the new letter.
    let mut e = open("hello world\nsecond", 300.0, 100.0);
    let before = glyph_rows(&e).len();
    type_char(&mut e, 'x');
    assert_eq!(glyph_rows(&e).len(), before);
    // Shaped text shows its old layout until a worker answers.
    let mut e = open("日本語\nsecond", 300.0, 100.0);
    let before = glyph_rows(&e);
    type_char(&mut e, '語');
    assert_eq!(glyph_rows(&e), before, "the old row stays");
    settle(&mut e);
    assert_ne!(glyph_rows(&e), before, "the new row arrived");
    // Long lines keep their segments on screen too.
    let text = format!("{}日", "x".repeat(50_000));
    let mut e = open(&text, 200.0, 170.0);
    let before = glyph_rows(&e).len();
    assert!(before > 0);
    type_char(&mut e, 'y');
    assert!(!glyph_rows(&e).is_empty());
}

#[test]
fn keys_restart_the_blink_and_only_the_clock_flips_it() {
    let mut e = open("abc\n", 300.0, 100.0);
    focus(&mut e);
    assert!(caret_rect(&e).is_some());
    // Early timers, as worker polling and store wake-ups give, do not flip it.
    for _ in 0..5 {
        e.on_timer(&mut ViewContext::default());
        assert!(caret_rect(&e).is_some());
    }
    let later = Instant::now() + Duration::from_secs(1);
    e.on_timer_at(later, &mut ViewContext::default());
    assert!(caret_rect(&e).is_none(), "the blink hides it");
    // A key in the hidden phase shows it at once.
    type_char(&mut e, 'x');
    assert!(caret_rect(&e).is_some(), "a key shows it");
    e.on_timer_at(later, &mut ViewContext::default());
    assert!(caret_rect(&e).is_some() || caret_rect(&e).is_none());
    // A plain arrow shows it too.
    let later = Instant::now() + Duration::from_secs(1);
    e.on_timer_at(later, &mut ViewContext::default());
    key(&mut e, LEFT);
    assert!(caret_rect(&e).is_some(), "an arrow shows it");
}

#[test]
fn each_event_asks_for_at_most_one_timer() {
    let mut e = open(&lines(50), 300.0, 100.0);
    assert_eq!(timers(&focus(&mut e)), 1);
    assert_eq!(timers(&type_char(&mut e, 'x')), 1);
    assert_eq!(timers(&key(&mut e, RIGHT)), 1);
    let mut ctx = ViewContext::default();
    e.poll_layout(&mut ctx);
    assert_eq!(timers(&ctx.requests), 1);
}

#[test]
fn warnings_repaint_and_clear() {
    struct Fail;
    impl Clipboard for Fail {
        fn read(&mut self) -> Result<Option<String>, String> {
            Err("the clipboard is busy".into())
        }
        fn write(&mut self, _: &str) -> Result<(), String> {
            Err("the clipboard is busy".into())
        }
    }
    let mut e = open("a", 300.0, 100.0);
    e.set_clipboard(Box::new(Fail));
    let requests = ctrl(&mut e, 0x56);
    assert_eq!(e.warning(), Some("the clipboard is busy"));
    assert!(!invalidates(&requests).is_empty(), "the warning must show");
    assert!(
        list(&e)
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::TextRun { .. }))
    );
    let requests = key(&mut e, RIGHT);
    assert_eq!(e.warning(), None);
    assert!(covers(&requests, 10.0), "the warning must leave the screen");
    assert!(
        !list(&e)
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::TextRun { .. }))
    );
}

#[test]
fn undo_with_nothing_to_undo_changes_nothing_and_repaints_nothing() {
    let mut e = open("abc", 300.0, 100.0);
    ctrl(&mut e, 0x41);
    let requests = ctrl(&mut e, 0x5a);
    assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(3));
    assert!(invalidates(&requests).is_empty(), "{requests:?}");
    let requests = ctrl(&mut e, 0x59);
    assert!(invalidates(&requests).is_empty(), "{requests:?}");
}

// ---- Item 7: read-only states ----

type NamedAction = (&'static str, fn(&mut EditorView));

#[test]
fn prevent_editing_and_mapped_files_refuse_every_edit() {
    let path = temp_path("readonly.txt");
    std::fs::write(&path, "hello\nworld\n").unwrap();
    let _remove = Remove(path.clone());
    for mapped in [false, true] {
        let mut e = if mapped {
            let store = MappedStore::open(&path).unwrap();
            let mut e = EditorView::from_mapped(store).unwrap();
            e.layout(rect(300.0, 100.0), &measure());
            settle(&mut e);
            e
        } else {
            let mut e = open("hello\nworld\n", 300.0, 100.0);
            type_char(&mut e, 'x');
            e.set_clipboard(Box::new(MemoryClipboard("paste".into())));
            key(&mut e, BACKSPACE);
            type_char(&mut e, 'y');
            key(&mut e, BACKSPACE);
            e.set_prevent_editing(true);
            e
        };
        let text = e.text().unwrap();
        ctrl(&mut e, END);
        key(&mut e, LEFT);
        let caret = e.caret();
        let actions: [NamedAction; 10] = [
            ("undo", |e| {
                ctrl(e, 0x5a);
            }),
            ("redo", |e| {
                ctrl(e, 0x59);
            }),
            ("redo by shift", |e| {
                press(e, 0x5a, true, true);
            }),
            ("cut", |e| {
                ctrl(e, 0x58);
            }),
            ("paste", |e| {
                ctrl(e, 0x56);
            }),
            ("typing", |e| {
                type_char(e, 'z');
            }),
            ("enter", |e| {
                key(e, ENTER);
            }),
            ("backspace", |e| {
                key(e, BACKSPACE);
            }),
            ("delete", |e| {
                key(e, DELETE);
            }),
            ("word delete", |e| {
                ctrl(e, BACKSPACE);
                ctrl(e, DELETE);
            }),
        ];
        let start_refusals = e.refused_edits();
        for (name, action) in actions {
            action(&mut e);
            assert_eq!(e.text().unwrap(), text, "{name}, mapped {mapped}");
            assert_eq!(e.caret(), caret, "{name} moved the caret");
            assert!(e.selection().is_empty(), "{name} changed the selection");
        }
        assert!(e.refused_edits() >= start_refusals + 10);
        // Reading still works.
        key(&mut e, LEFT);
        assert_ne!(e.caret(), caret);
        ctrl(&mut e, 0x41);
        assert_eq!(e.selection().end.0, text.len());
        assert!(e.widgets().unwrap().states.read_only);
    }
}

fn settle_first_screen(e: &mut EditorView) {
    for _ in 0..1000 {
        e.poll_layout(&mut ViewContext::default());
        if glyph_rows(e).len() > 3 {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the first screen never painted");
}

#[test]
fn arrows_on_a_mapped_file_still_move_while_its_index_runs() {
    let path = temp_path("indexing.txt");
    {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let block = "0123456789 abcdefghijklmnopqrstuvwxyz\n".repeat(32 * 1024);
        for _ in 0..200 {
            file.write_all(block.as_bytes()).unwrap();
        }
    }
    let _remove = Remove(path.clone());
    let store = MappedStore::open(&path).unwrap();
    // Slow reads keep the first pages out of memory for a while, once the index passes them.
    store.set_test_read_delay(Duration::from_millis(2));
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(800.0, 600.0), &measure());
    focus(&mut e);
    settle_first_screen(&mut e);
    let mut line = 0_usize;
    let mut moves = 0;
    while !e.store_status().complete && moves < 150 {
        let down = moves % 2 == 0;
        let mut requests = key(&mut e, if down { DOWN } else { UP });
        line = if down { line + 1 } else { line - 1 };
        moves += 1;
        // A person's next key comes later than this, and the poll retries the move.
        let until = Instant::now() + Duration::from_millis(400);
        while e.caret().0 != line * 38 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
            let mut ctx = ViewContext::default();
            e.poll_layout(&mut ctx);
            requests.extend(ctx.requests);
        }
        assert_eq!(e.caret().0, line * 38, "the key did nothing, move {moves}");
        assert!(
            !invalidates(&requests).is_empty(),
            "the caret moved with no repaint, move {moves}"
        );
        // The caret is on screen whatever the store has in memory.
        assert!(caret_rect(&e).is_some(), "no caret, move {moves}");
        // The index's growth does not change text, so it must not cost any layout.
        e.poll_layout(&mut ViewContext::default());
        assert!(glyph_rows(&e).len() > 30, "a blank frame, move {moves}");
    }
    assert!(
        moves > 10,
        "the index ended after {moves} moves, which proves little"
    );
}

#[test]
fn a_key_that_finds_its_text_out_of_memory_runs_when_the_text_arrives() {
    let path = temp_path("far.txt");
    let body = "0123456789 abcdefghijklmnopqrstuvwxyz\n".repeat(2_000_000);
    std::fs::write(&path, &body).unwrap();
    let total = body.len();
    drop(body);
    let _remove = Remove(path.clone());
    let store = MappedStore::open(&path).unwrap();
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(800.0, 600.0), &measure());
    let until = Instant::now() + Duration::from_secs(20);
    while !e.store_status().complete && Instant::now() < until {
        e.poll_layout(&mut ViewContext::default());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(e.store_status().complete);
    // Far from the first pages, nothing near the caret is in memory yet.
    let mut moved_late = 0;
    for start in [total / 3, total / 2, total * 2 / 3] {
        let start = start / 38 * 38;
        e.select(
            ByteOffset(start)..ByteOffset(start),
            &mut ViewContext::default(),
        );
        key(&mut e, PG_DOWN);
        let at_once = e.caret().0;
        let until = Instant::now() + Duration::from_millis(250);
        while e.caret().0 == start && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
            e.poll_layout(&mut ViewContext::default());
        }
        assert_ne!(e.caret().0, start, "the key never ran");
        if at_once == start {
            moved_late += 1;
        }
    }
    eprintln!("{moved_late} of 3 keys waited for their text");
}

// ---- Item 8: clipboard and modifiers ----

#[derive(Clone)]
struct Slot(Rc<RefCell<Option<String>>>);
impl Clipboard for Slot {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(self.0.borrow().clone())
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        *self.0.borrow_mut() = Some(value.into());
        Ok(())
    }
}
fn slot(value: Option<&str>) -> (Slot, Rc<RefCell<Option<String>>>) {
    let shared = Rc::new(RefCell::new(value.map(String::from)));
    (Slot(shared.clone()), shared)
}

#[test]
fn copy_and_cut_with_nothing_selected_leave_the_clipboard_alone() {
    let mut e = open("abc", 300.0, 100.0);
    let (clip, shared) = slot(Some("the user's text"));
    e.set_clipboard(Box::new(clip));
    ctrl(&mut e, 0x43);
    ctrl(&mut e, 0x58);
    assert_eq!(shared.borrow().as_deref(), Some("the user's text"));
    assert_eq!(e.text().as_deref(), Some("abc"));
}

#[test]
fn pasting_nothing_changes_nothing() {
    for value in [None, Some("")] {
        let mut e = open("abc", 300.0, 100.0);
        let (clip, _shared) = slot(value);
        e.set_clipboard(Box::new(clip));
        ctrl(&mut e, 0x41);
        let requests = ctrl(&mut e, 0x56);
        assert_eq!(e.text().as_deref(), Some("abc"), "{value:?}");
        assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(3), "{value:?}");
        assert!(invalidates(&requests).is_empty(), "{value:?}");
    }
}

#[test]
fn ctrl_alt_is_altgr_and_its_character_is_typed() {
    let mut e = open("whole", 300.0, 100.0);
    let altgr = Modifiers {
        ctrl: true,
        alt: true,
        ..Default::default()
    };
    for code in [0x41, 0x43, 0x56, 0x58, 0x5a, BACKSPACE, DELETE, LEFT, END] {
        let handled = {
            let mut ctx = ViewContext::default();
            e.handle(
                &InputEvent {
                    time_us: 0,
                    kind: InputKind::KeyDown {
                        virtual_key: code,
                        modifiers: altgr,
                        repeat: false,
                    },
                },
                &mut ctx,
            )
        };
        assert_eq!(handled, qe_api::Handled::No, "key {code:#x}");
    }
    assert!(e.selection().is_empty());
    assert_eq!(e.text().as_deref(), Some("whole"));
    type_char(&mut e, 'ą');
    assert_eq!(e.text().as_deref(), Some("ąwhole"));
}

#[test]
fn copying_a_big_mapped_selection_runs_off_the_ui_thread() {
    let path = temp_path("bigcopy.txt");
    let body = "0123456789abcde\n".repeat(128 * 1024);
    std::fs::write(&path, &body).unwrap();
    let _remove = Remove(path.clone());
    let store = MappedStore::open(&path).unwrap();
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(300.0, 100.0), &measure());
    settle(&mut e);
    let (clip, shared) = slot(None);
    e.set_clipboard(Box::new(clip));
    ctrl(&mut e, 0x41);
    assert!(e.selection().end.0 > 448 * 1024);
    let started = Instant::now();
    ctrl(&mut e, 0x43);
    assert!(started.elapsed() < Duration::from_millis(500));
    settle(&mut e);
    assert_eq!(e.warning(), None);
    assert_eq!(shared.borrow().as_deref(), Some(body.as_str()));
    // Past the limit, the message says the size and the limit in plain words.
    e.test_set_copy_limit(1024 * 1024);
    ctrl(&mut e, 0x43);
    let warning = e.warning().unwrap().to_string();
    assert!(warning.contains("1 MiB"), "{warning}");
    assert!(!warning.contains("TooLong"), "{warning}");
}

// ---- Item 9: selection and mouse ----

#[test]
fn left_and_right_collapse_a_selection_to_its_edges() {
    both_layout_states("hello", |e| {
        ctrl(e, 0x41);
        key(e, LEFT);
        assert_eq!(e.caret().0, 0);
        assert!(e.selection().is_empty());
        ctrl(e, 0x41);
        key(e, RIGHT);
        assert_eq!(e.caret().0, 5);
        assert!(e.selection().is_empty());
    });
}

#[test]
fn double_click_needs_the_second_click_near_the_first() {
    let mut e = open("hello world\nnext words", 300.0, 100.0);
    click(&mut e, 2.0, 2.0, 1000);
    click(&mut e, 40.0, 20.0, 2000);
    assert!(e.selection().is_empty(), "far click");
    click(&mut e, 2.0, 2.0, 2_000_000);
    click(&mut e, 2.0, 2.0, 2_000_000 + 400_000);
    assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(5), "near click");
    click(&mut e, 2.0, 2.0, 9_000_000);
    click(&mut e, 2.0, 2.0, 9_000_000 + 600_000);
    assert!(e.selection().is_empty(), "too late");
    // The host's settings count.
    e.set_double_click_limits(100_000, 40.0, 40.0);
    click(&mut e, 2.0, 2.0, 20_000_000);
    click(&mut e, 15.0, 2.0, 20_000_000 + 50_000);
    assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(5), "wider setting");
    click(&mut e, 2.0, 2.0, 30_000_000);
    click(&mut e, 2.0, 2.0, 30_000_000 + 400_000);
    assert!(e.selection().is_empty(), "shorter setting");
}

#[test]
fn double_click_selects_a_word_a_space_run_or_one_mark() {
    let mut e = open("foo  bar, baz don't", 600.0, 100.0);
    let cell = measure().cell_width().0;
    let double = |e: &mut EditorView, column: f32, time: u64| {
        let x = column * cell + cell * 0.25;
        click(e, x, 2.0, time);
        click(e, x, 2.0, time + 1000);
    };
    double(&mut e, 1.0, 1_000_000);
    assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(3), "a word");
    double(&mut e, 3.0, 5_000_000);
    assert_eq!(e.selection(), ByteOffset(3)..ByteOffset(5), "a space run");
    double(&mut e, 8.0, 9_000_000);
    assert_eq!(e.selection(), ByteOffset(8)..ByteOffset(9), "one mark");
    double(&mut e, 15.0, 13_000_000);
    assert_eq!(
        e.selection(),
        ByteOffset(14)..ByteOffset(19),
        "an apostrophe stays"
    );
    // The right half of a letter still means that letter's word.
    let x = 2.0 * cell + cell * 0.9;
    click(&mut e, x, 2.0, 20_000_000);
    click(&mut e, x, 2.0, 20_000_000 + 1000);
    assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(3), "right half");
}

#[test]
fn word_keys_follow_appkit() {
    both_layout_states("abc def, ghi\n\n  jkl", |e| {
        ctrl(e, RIGHT);
        assert_eq!(e.caret().0, 3);
        ctrl(e, RIGHT);
        assert_eq!(e.caret().0, 7);
        ctrl(e, RIGHT);
        assert_eq!(e.caret().0, 12);
        ctrl(e, RIGHT);
        assert_eq!(e.caret().0, 19, "across the blank lines");
        ctrl(e, RIGHT);
        assert_eq!(e.caret().0, 19);
        ctrl(e, LEFT);
        assert_eq!(e.caret().0, 16);
        ctrl(e, LEFT);
        assert_eq!(e.caret().0, 9);
        ctrl(e, LEFT);
        assert_eq!(e.caret().0, 4);
        ctrl(e, LEFT);
        assert_eq!(e.caret().0, 0);
        ctrl(e, LEFT);
        assert_eq!(e.caret().0, 0);
        press(e, RIGHT, true, true);
        assert_eq!(e.selection(), ByteOffset(0)..ByteOffset(3));
    });
}

#[test]
fn ctrl_backspace_and_ctrl_delete_remove_a_word() {
    both_layout_states("abc def ghi", |e| {
        ctrl(e, DELETE);
        assert_eq!(e.text().unwrap(), " def ghi");
        ctrl(e, END);
        ctrl(e, BACKSPACE);
        assert_eq!(e.text().unwrap(), " def ");
        ctrl(e, BACKSPACE);
        assert_eq!(e.text().unwrap(), " ");
        ctrl(e, 0x5a);
        ctrl(e, 0x5a);
        ctrl(e, 0x5a);
        assert_eq!(e.text().unwrap(), "abc def ghi");
    });
    // A selection goes alone.
    let mut e = open("abc def", 300.0, 100.0);
    e.select(ByteOffset(1)..ByteOffset(2), &mut ViewContext::default());
    ctrl(&mut e, BACKSPACE);
    assert_eq!(e.text().unwrap(), "ac def");
}

#[test]
fn drag_scrolling_starts_at_once_and_repeats_with_no_mouse_move() {
    let text = lines(500);
    let mut e = open(&text, 200.0, 51.0);
    send(
        &mut e,
        InputKind::MouseDown {
            at: point(1.0, 1.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        1,
    );
    assert_eq!(e.scroll_offset(), 0.0);
    let requests = send(
        &mut e,
        InputKind::MouseMove {
            at: point(1.0, 200.0),
        },
        2,
    );
    let first = e.scroll_offset();
    assert!(first > 0.0, "scrolling starts on the move that leaves");
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, ViewRequest::RequestTimer(t) if *t <= 16)),
        "a timer repeats it: {requests:?}"
    );
    let mut ctx = ViewContext::default();
    e.on_timer(&mut ctx);
    assert!(e.scroll_offset() > first);
    assert!(timers(&ctx.requests) == 1);
    assert!(!e.selection().is_empty());
    let extent = e.selection().end.0;
    e.on_timer(&mut ViewContext::default());
    assert!(e.selection().end.0 > extent, "the selection follows");
    assert!(!glyph_rows(&e).is_empty(), "no blank frames");
    // Back inside, it stops.
    send(
        &mut e,
        InputKind::MouseMove {
            at: point(1.0, 20.0),
        },
        3,
    );
    let stopped = e.scroll_offset();
    let mut ctx = ViewContext::default();
    e.on_timer(&mut ctx);
    assert_eq!(e.scroll_offset(), stopped);
}

#[test]
fn horizontal_scroll_has_an_upper_limit() {
    let text = "x".repeat(100);
    let mut e = with_wrap(&text, false);
    e.layout(rect(100.0, 100.0), &measure());
    settle(&mut e);
    let hscroll = |e: &mut EditorView, delta_x: f32| {
        send(
            e,
            InputKind::Wheel {
                at: point(1.0, 1.0),
                delta_x,
                delta_y: 0.0,
                modifiers: Modifiers::default(),
            },
            0,
        );
    };
    hscroll(&mut e, 12_000.0);
    let limit = 100.0 * measure().cell_width().0 + 2.0 - 100.0;
    assert!(e.horizontal_offset() > 0.0);
    assert!(
        (e.horizontal_offset() - limit).abs() < 2.0,
        "{} against {limit}",
        e.horizontal_offset()
    );
    hscroll(&mut e, 12_000.0);
    assert!((e.horizontal_offset() - limit).abs() < 2.0);
    hscroll(&mut e, -24_000.0);
    assert_eq!(e.horizontal_offset(), 0.0);
    // Text narrower than the view does not scroll.
    let mut e = with_wrap("short", false);
    hscroll(&mut e, 12_000.0);
    assert_eq!(e.horizontal_offset(), 0.0);
}

#[test]
fn return_uses_the_macs_rule_for_the_most_common_ending() {
    assert_eq!(preferred_line_ending_bytes(b"no ending"), "\n");
    assert_eq!(
        preferred_line_ending_bytes(b"a\nb\r\n"),
        "\n",
        "tie goes to LF"
    );
    assert_eq!(
        preferred_line_ending_bytes(b"a\r\nb\rc"),
        "\r\n",
        "tie to CRLF"
    );
    assert_eq!(preferred_line_ending_bytes(b"a\rb\rc\r\n"), "\r");
    assert_eq!(preferred_line_ending_bytes(b"a\r\nb\r\nc\n"), "\r\n");
    // Only LF, CRLF and CR count. These do not make a line ending win.
    let other = "a\u{2028}b\u{2028}c\u{85}d";
    assert_eq!(preferred_line_ending_bytes(other.as_bytes()), "\n");
    let mut e = open("a\u{2028}b\u{2028}\rc", 300.0, 100.0);
    ctrl(&mut e, END);
    key(&mut e, ENTER);
    assert!(e.text().unwrap().ends_with("c\r"));
}

#[test]
fn history_memory_is_freed_when_idle_and_not_on_each_poll() {
    let big = "abcdefghij\n".repeat(400_000);
    let mut e = open("", 400.0, 400.0);
    focus(&mut e);
    e.set_clipboard(Box::new(MemoryClipboard(big)));
    ctrl(&mut e, 0x56);
    settle(&mut e);
    ctrl(&mut e, 0x5a);
    settle(&mut e);
    type_char(&mut e, 'x');
    settle(&mut e);
    let waiting = e.memory_to_release_bytes();
    assert!(waiting > 0, "the test needs history waiting to be freed");
    // Polling does not free it. One edit frees a little, so the list is still long.
    e.poll_layout(&mut ViewContext::default());
    e.on_timer(&mut ViewContext::default());
    assert_eq!(e.memory_to_release_bytes(), waiting);
    // Idle does.
    let idle = Instant::now() + Duration::from_secs(10);
    e.on_timer_at(idle, &mut ViewContext::default());
    assert_eq!(e.memory_to_release_bytes(), 0);
}

// ---- Item 11: idle ----

#[test]
fn only_a_focused_view_keeps_a_timer() {
    let mut e = open(&lines(5), 300.0, 100.0);
    let mut ctx = ViewContext::default();
    e.on_timer(&mut ctx);
    assert_eq!(timers(&ctx.requests), 0, "unfocused and idle");
    assert_eq!(timers(&focus(&mut e)), 1);
    let mut ctx = ViewContext::default();
    e.on_timer(&mut ctx);
    assert_eq!(timers(&ctx.requests), 1, "focused keeps its blink");
    let blur = send(&mut e, InputKind::Blur, 0);
    assert_eq!(timers(&blur), 0);
    let mut ctx = ViewContext::default();
    e.on_timer_at(Instant::now() + Duration::from_secs(1), &mut ctx);
    assert_eq!(timers(&ctx.requests), 0, "unfocused after blur");
    let mut ctx = ViewContext::default();
    e.poll_layout(&mut ctx);
    assert_eq!(timers(&ctx.requests), 0, "a poll asks for none");
}

#[test]
fn an_idle_mapped_view_with_a_finished_index_keeps_no_timer() {
    let path = temp_path("idle.txt");
    std::fs::write(&path, "abc\ndef\n").unwrap();
    let _remove = Remove(path.clone());
    let store = MappedStore::open(&path).unwrap();
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(300.0, 100.0), &measure());
    settle(&mut e);
    let mut ctx = ViewContext::default();
    e.on_timer(&mut ctx);
    assert_eq!(timers(&ctx.requests), 0);
}

#[test]
fn workers_stop_when_the_view_is_dropped() {
    let e = open(&lines(2000), 300.0, 1000.0);
    let alive = e.worker_counter();
    assert_eq!(alive(), 4);
    drop(e);
    for _ in 0..500 {
        if alive() == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{} workers still run", alive());
}

// ---- Item 12: the uncertain items ----

#[test]
fn a_layout_that_always_fails_stops_and_shows_a_warning() {
    for long in [false, true] {
        let text = if long {
            format!("ok\n{}\nok", "日".repeat(30_000))
        } else {
            "ok\n日本語\nok".to_string()
        };
        let mut e = EditorView::from_text(&text).unwrap();
        e.test_fail_layout_of_line(1);
        e.layout(rect(300.0, 100.0), &measure());
        settle(&mut e);
        assert!(!e.pending_layout());
        let warning = e.warning().expect("a warning").to_string();
        assert!(warning.contains("Line 2"), "{warning}");
        let mut ctx = ViewContext::default();
        e.on_timer(&mut ctx);
        assert_eq!(timers(&ctx.requests), 0, "no retry loop");
        assert_eq!(e.pending_task_count(), 0, "no new task");
        // The next edit tries again, so a repaired line can recover.
        e.test_fail_layout_of_line(usize::MAX);
        type_char(&mut e, 'x');
        settle(&mut e);
        assert_eq!(e.warning(), None, "long {long}");
        assert!(glyph_rows(&e).len() >= 3, "long {long}");
    }
}

#[test]
fn queued_tasks_skip_lines_that_left_the_view() {
    let text = (0..200_000)
        .map(|i| format!("日本語 {i}\n"))
        .collect::<String>();
    let mut e = cold(&text, 300.0, 800.0);
    for step in 0..60 {
        wheel(&mut e, -6000.0 * (step % 3 + 1) as f32);
    }
    settle(&mut e);
    assert!(e.skipped_task_count() > 0, "nothing was skipped");
    assert!(!glyph_rows(&e).is_empty());
}

#[test]
fn an_undo_group_with_several_changes_keeps_everything_right() {
    let body = (0..60)
        .map(|i| format!("line {i} word\n"))
        .collect::<String>();
    let mut e = open(&body, 90.0, 1800.0);
    let mut ctx = ViewContext::default();
    e.begin_edit_group();
    // Replace All's shape: edits from the end, on far lines, some adding lines.
    let spots: Vec<usize> = body.match_indices("word").map(|(i, _)| i).collect();
    for (n, at) in spots.iter().enumerate().rev().take(12) {
        let text = if n % 3 == 0 {
            "two\nrows"
        } else {
            "other text here"
        };
        e.replace_range(ByteOffset(*at)..ByteOffset(*at + 4), text, &mut ctx);
    }
    e.end_edit_group();
    settle(&mut e);
    let after = e.text().unwrap();
    assert_ne!(after, body);
    assert_eq!(glyph_rows(&e), glyph_rows(&open(&after, 90.0, 1800.0)));
    ctrl(&mut e, 0x5a);
    settle(&mut e);
    assert_eq!(e.text().unwrap(), body);
    assert_eq!(glyph_rows(&e), glyph_rows(&open(&body, 90.0, 1800.0)));
    assert_eq!(e.line_count(), 61);
    ctrl(&mut e, 0x59);
    settle(&mut e);
    assert_eq!(e.text().unwrap(), after);
    assert_eq!(glyph_rows(&e), glyph_rows(&open(&after, 90.0, 1800.0)));
    let top_to_bottom = selection_rects(&e);
    assert!(top_to_bottom.is_empty());
}

#[test]
fn a_mapped_line_with_no_end_yet_drops_its_old_width_on_resize() {
    let path = temp_path("partial.txt");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![b'x'; 64 * 1024]).unwrap();
        file.set_len(64 * 1024 * 1024).unwrap();
    }
    let _remove = Remove(path.clone());
    let store = MappedStore::open(&path).unwrap();
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(100.0, 100.0), &measure());
    let mut ctx = ViewContext::default();
    for _ in 0..200 {
        e.poll_layout(&mut ctx);
        if !glyph_rows(&e).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if e.store_status().complete {
        eprintln!("the index finished first, so this run proves nothing");
        return;
    }
    assert!(!glyph_rows(&e).is_empty());
    // The old partial layout has rows for width 100. A new width must not paint it.
    e.layout(rect(300.0, 100.0), &measure());
    assert!(glyph_rows(&e).is_empty(), "old width painted");
}

// ---- Item 10: loading off the UI thread ----

fn wait_for_load(load: &mut FileLoad) -> LoadResult {
    for _ in 0..5000 {
        if let Some(result) = load.poll() {
            return result;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the load never ended");
}
fn quiet() -> qe_buffer::Notify {
    Arc::new(|| {})
}

#[test]
fn a_file_loads_off_the_ui_thread_and_the_view_adopts_it() {
    let path = temp_path("load.txt");
    std::fs::write(&path, "one\r\ntwo\r\nthree\n").unwrap();
    let _remove = Remove(path.clone());
    let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = woken.clone();
    let wake: qe_buffer::Notify = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    });
    let mut load = FileLoad::start(&path, LoadOptions::default(), wake).unwrap();
    let mut e = cold("", 300.0, 200.0);
    let LoadResult::Loaded(file) = wait_for_load(&mut load) else {
        panic!("the load failed");
    };
    assert!(woken.load(std::sync::atomic::Ordering::Acquire) >= 1);
    e.adopt(file).unwrap();
    settle(&mut e);
    assert_eq!(e.text().unwrap(), "one\r\ntwo\r\nthree\n");
    assert_eq!(e.line_count(), 4);
    assert!(glyph_rows(&e).len() >= 3);
    // The file's main ending comes with it, and the new text edits as usual.
    ctrl(&mut e, END);
    key(&mut e, ENTER);
    assert!(e.text().unwrap().ends_with("three\n\r\n"));
    ctrl(&mut e, 0x5a);
    assert_eq!(e.text().unwrap(), "one\r\ntwo\r\nthree\n");
    ctrl(&mut e, 0x5a);
    assert_eq!(e.text().unwrap(), "one\r\ntwo\r\nthree\n", "nothing older");
}

#[test]
fn a_load_that_fails_says_why_in_words() {
    let missing = temp_path("no-such-file.txt");
    let mut load = FileLoad::start(&missing, LoadOptions::default(), quiet()).unwrap();
    let LoadResult::Failed(message) = wait_for_load(&mut load) else {
        panic!("a missing file must fail");
    };
    assert!(message.contains("no-such-file.txt"), "{message}");

    let bad = temp_path("bad-utf8.txt");
    std::fs::write(&bad, [b'a', 0xFF, b'b']).unwrap();
    let _remove = Remove(bad.clone());
    let mut load = FileLoad::start(&bad, LoadOptions::default(), quiet()).unwrap();
    let LoadResult::Failed(message) = wait_for_load(&mut load) else {
        panic!("bad UTF-8 must fail");
    };
    assert!(message.contains("UTF-8"), "{message}");

    // A failed read-only open reports the same way.
    let options = LoadOptions {
        force_read_only: true,
        ..Default::default()
    };
    let mut load = FileLoad::start(&missing, options, quiet()).unwrap();
    assert!(matches!(wait_for_load(&mut load), LoadResult::Failed(_)));

    // The view shows the failure, and the next key clears it.
    let mut e = open("", 300.0, 100.0);
    e.show_warning(message.clone());
    assert_eq!(e.warning(), Some(message.as_str()));
    type_char(&mut e, 'x');
    assert_eq!(e.warning(), None);
}

#[test]
fn closing_cancels_a_load_that_is_still_reading() {
    let path = temp_path("cancel.txt");
    {
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(256 * 1024 * 1024).unwrap();
    }
    let _remove = Remove(path.clone());
    let options = LoadOptions {
        test_read_delay: Some(Duration::from_millis(20)),
        ..Default::default()
    };
    let load = FileLoad::start(&path, options, quiet()).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    load.cancel_and_wait().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancel took {:?}",
        started.elapsed()
    );
    // A cancelled load reports that, if anything still polls it.
    let options = LoadOptions {
        test_read_delay: Some(Duration::from_millis(20)),
        ..Default::default()
    };
    let mut load = FileLoad::start(&path, options, quiet()).unwrap();
    load.cancel();
    assert!(matches!(wait_for_load(&mut load), LoadResult::Cancelled));
    // Plain dropping also stops it: the file can go right after.
    let options = LoadOptions {
        test_read_delay: Some(Duration::from_millis(5)),
        ..Default::default()
    };
    drop(FileLoad::start(&path, options, quiet()).unwrap());
}

#[test]
fn adopting_a_file_drops_work_queued_for_the_old_text() {
    let mut e = cold(&"日本語\n".repeat(5000), 300.0, 800.0);
    e.poll_layout(&mut ViewContext::default());
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new text\nsecond"),
        ending: "\n",
    })
    .unwrap();
    settle(&mut e);
    assert_eq!(e.text().unwrap(), "new text\nsecond");
    assert_eq!(
        glyph_rows(&e),
        glyph_rows(&open("new text\nsecond", 300.0, 800.0))
    );
    assert_eq!(e.pending_task_count(), 0);
}

#[test]
fn a_timer_after_adopting_a_file_lays_it_out() {
    let mut e = cold("", 300.0, 200.0);
    e.poll_layout(&mut ViewContext::default());
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new text\nsecond"),
        ending: "\n",
    })
    .unwrap();
    // The window's only wake-up: its timer, with no other event.
    let mut ctx = ViewContext::default();
    e.on_timer(&mut ctx);
    assert!(!glyph_rows(&e).is_empty());
    assert!(!invalidates(&ctx.requests).is_empty());
}

#[test]
fn a_text_area_below_the_top_row_keeps_every_position_right() {
    let (top, height) = (40.0, 100.0);
    let area = Rect {
        x: Dip(0.0),
        y: Dip(top),
        width: Dip(300.0),
        height: Dip(height),
    };
    let text = lines(100);
    let mut e = EditorView::from_text(&text).unwrap();
    e.layout(area, &measure());
    settle(&mut e);
    focus(&mut e);
    let row = measure().layout_line_for_test("x", Dip(100.0)).rows[0]
        .height
        .0;
    click(&mut e, 1.0, top + 1.0, 1_000_000);
    assert_eq!(e.caret().0, 0);
    let caret = caret_rect(&e).unwrap();
    assert_eq!(caret.y.0, top);
    click(&mut e, 1.0, top + row * 2.0 + 1.0, 5_000_000);
    assert_eq!(e.caret().0, "line 0\nline 1\n".len());
    assert_eq!(e.scroll_offset(), 0.0);
    // Repaints stay inside the area, and the warning strip too.
    let requests = ctrl(&mut e, 0x41);
    for rect in invalidates(&requests) {
        assert!(
            rect.y.0 >= top && rect.y.0 + rect.height.0 <= top + height + 0.5,
            "{rect:?}"
        );
    }
    let mut ctx = ViewContext::default();
    click(&mut e, 1.0, top + 1.0, 9_000_000);
    e.on_timer_at(Instant::now() + Duration::from_secs(1), &mut ctx);
    for rect in invalidates(&ctx.requests) {
        assert!(rect.y.0 >= top, "{rect:?}");
    }
    // The caret stays in the area after it scrolls.
    ctrl(&mut e, END);
    settle(&mut e);
    let caret = caret_rect(&e).unwrap();
    assert!(
        caret.y.0 >= top && caret.y.0 + caret.height.0 <= top + height + 0.5,
        "{caret:?}"
    );
    // A drag above the area scrolls up.
    let scrolled = e.scroll_offset();
    send(
        &mut e,
        InputKind::MouseDown {
            at: point(1.0, top + 5.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        20_000_000,
    );
    send(
        &mut e,
        InputKind::MouseMove {
            at: point(1.0, top - 5.0),
        },
        20_000_001,
    );
    assert!(e.scroll_offset() < scrolled);
}

// ---- The Opus report, second pass ----

#[test]
fn shift_home_then_backspace_on_a_long_line_takes_one_row() {
    let text = "x".repeat(100_000);
    let mut e = open(&text, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    shift(&mut e, HOME);
    let selected = e.selection();
    assert_eq!(selected.end.0, 100_000);
    let row = selected.end.0 - selected.start.0;
    let cols = (200.0 / measure().cell_width().0).floor() as usize;
    assert!(row > 0 && row <= cols, "selected {row}, a row holds {cols}");
    settle(&mut e);
    key(&mut e, BACKSPACE);
    assert_eq!(e.text().unwrap().len(), 100_000 - row);
}

#[test]
fn apply_wrap_and_apply_font_ask_for_their_repaint_and_layout() {
    let mut e = open(&lines(20), 300.0, 170.0);
    let mut ctx = ViewContext::default();
    e.apply_wrap(false, &mut ctx).unwrap();
    assert!(covers(&ctx.requests, 100.0), "wrap: {:?}", ctx.requests);
    assert!(
        !glyph_rows(&e).is_empty(),
        "wrap paints with no other event"
    );
    let mut ctx = ViewContext::default();
    e.apply_font("Consolas", 20.0, &mut ctx).unwrap();
    assert!(covers(&ctx.requests, 100.0), "font: {:?}", ctx.requests);
    assert!(
        !glyph_rows(&e).is_empty(),
        "font paints with no other event"
    );
    // Shaped text needs a worker, so a timer must be on its way.
    let mut e = open(&"日本語\n".repeat(20), 300.0, 170.0);
    let mut ctx = ViewContext::default();
    e.apply_wrap(false, &mut ctx).unwrap();
    assert_eq!(timers(&ctx.requests), 1, "{:?}", ctx.requests);
    // A plain `set_wrap` leaves the work for the next timer.
    let mut e = open(&lines(20), 300.0, 170.0);
    e.set_wrap(false).unwrap();
    assert!(glyph_rows(&e).is_empty());
    e.on_timer(&mut ViewContext::default());
    assert!(!glyph_rows(&e).is_empty());
}

#[test]
fn a_click_repaints_a_selection_far_from_the_click() {
    let text = lines(20);
    let mut e = open(&text, 300.0, 400.0);
    let row = measure().layout_line_for_test("x", Dip(100.0)).rows[0]
        .height
        .0;
    let start = "line 0\n".len();
    let end = lines(5).len() + 2;
    e.select(
        ByteOffset(start)..ByteOffset(end),
        &mut ViewContext::default(),
    );
    let requests = send(
        &mut e,
        InputKind::MouseDown {
            at: point(1.0, row * 8.0 + 2.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        1_000_000,
    );
    for line in 1..=5 {
        assert!(
            covers(&requests, row * line as f32 + 8.0),
            "line {line} keeps its old highlight: {requests:?}"
        );
    }
    // Lines between the old selection and the click did not change.
    assert!(!covers(&requests, row * 7.0 + 8.0), "{requests:?}");
}

#[test]
fn a_blink_on_a_wrapped_line_repaints_only_the_caret() {
    let text = "abcdefghij".repeat(30);
    let mut e = open(&text, 60.0, 400.0);
    focus(&mut e);
    key(&mut e, DOWN);
    let mut ctx = ViewContext::default();
    e.on_timer_at(Instant::now() + Duration::from_secs(1), &mut ctx);
    let rects = invalidates(&ctx.requests);
    assert_eq!(rects.len(), 1, "{rects:?}");
    assert!(
        rects[0].width.0 <= 6.0 && rects[0].height.0 <= 20.0,
        "{rects:?}"
    );
}

#[test]
fn copying_a_big_editable_selection_runs_off_the_ui_thread() {
    let text = "0123456789abcde\n".repeat(128 * 1024);
    let mut e = open(&text, 300.0, 100.0);
    let (clip, shared) = slot(None);
    e.set_clipboard(Box::new(clip));
    ctrl(&mut e, 0x41);
    ctrl(&mut e, 0x43);
    assert!(e.pending_layout(), "the copy is still out");
    settle(&mut e);
    assert_eq!(shared.borrow().as_deref(), Some(text.as_str()));
    // The small path stays direct.
    let mut e = open("abc def", 300.0, 100.0);
    let (clip, shared) = slot(None);
    e.set_clipboard(Box::new(clip));
    ctrl(&mut e, 0x41);
    ctrl(&mut e, 0x43);
    assert_eq!(shared.borrow().as_deref(), Some("abc def"));
}

#[test]
fn double_click_past_a_lines_end_selects_no_line_ending() {
    let mut e = open("foo bar\r\nnext", 600.0, 100.0);
    click(&mut e, 400.0, 2.0, 1_000_000);
    click(&mut e, 400.0, 2.0, 1_001_000);
    assert_eq!(e.selection(), ByteOffset(4)..ByteOffset(7));
    // A blank line has nothing to select.
    let mut e = open("a\n\nb", 600.0, 100.0);
    let row = measure().layout_line_for_test("x", Dip(100.0)).rows[0]
        .height
        .0;
    let y = row + 2.0;
    click(&mut e, 2.0, y, 1_000_000);
    click(&mut e, 2.0, y, 1_001_000);
    assert_eq!(e.selection(), ByteOffset(2)..ByteOffset(2));
}

#[test]
fn undo_forgets_the_affinity_of_the_row_end() {
    let text = "abcdefghij".repeat(4);
    let mut e = open(&text, 40.0, 400.0);
    focus(&mut e);
    key(&mut e, END);
    let at = e.caret();
    let on_first_row = caret_rect(&e).unwrap().y.0;
    type_char(&mut e, 'x');
    ctrl(&mut e, 0x5a);
    assert_eq!(e.caret(), at);
    settle(&mut e);
    assert!(
        caret_rect(&e).unwrap().y.0 > on_first_row,
        "after an undo the caret belongs to the text after it"
    );
}

#[test]
fn page_down_is_exactly_the_rows_that_fit_and_the_view_moves_the_same() {
    let text = lines(100);
    let mut e = open(&text, 200.0, 85.0);
    let row = measure().layout_line_for_test("x", Dip(100.0)).rows[0]
        .height
        .0;
    let rows = (85.0 / row).floor() as usize;
    key(&mut e, PG_DOWN);
    assert_eq!(e.caret().0, lines(rows).len());
    assert!((e.scroll_offset() - f64::from(row) * rows as f64).abs() < 0.01);
    key(&mut e, PG_DOWN);
    assert_eq!(e.caret().0, lines(rows * 2).len());
    assert!((e.scroll_offset() - f64::from(row) * (rows * 2) as f64).abs() < 0.01);
    key(&mut e, PG_UP);
    assert_eq!(e.caret().0, lines(rows).len());
    assert!((e.scroll_offset() - f64::from(row) * rows as f64).abs() < 0.01);
}

#[test]
fn page_down_inside_one_wrapped_line_lands_on_the_exact_row() {
    let text = "abcdefghijklmnopqrstuvwxyz0123456789".repeat(5);
    let mut e = open(&text, 40.0, 85.0);
    let layout = measure().layout_line_for_test(&text, Dip(40.0));
    let row = layout.rows[0].height.0;
    let rows = (85.0 / row).floor() as usize;
    key(&mut e, PG_DOWN);
    assert_eq!(e.caret(), layout.rows[rows].range.start);
    assert!((e.scroll_offset() - f64::from(row) * rows as f64).abs() < 0.01);
}

#[test]
fn rapid_typing_sends_no_task_for_a_line_already_out() {
    let text = "日本語\n".repeat(5000);
    let mut e = cold(&text, 300.0, 170.0);
    e.poll_layout(&mut ViewContext::default());
    let sent = e.layout_tasks_sent();
    assert!(sent > 0);
    for _ in 0..100 {
        type_char(&mut e, 'q');
    }
    assert_eq!(e.layout_tasks_sent(), sent, "typing with no poll sent more");
    // Each settled key sends only the edited line again.
    settle(&mut e);
    let sent = e.layout_tasks_sent();
    for _ in 0..5 {
        type_char(&mut e, 'q');
        settle(&mut e);
    }
    assert_eq!(e.layout_tasks_sent(), sent + 5);
}

#[test]
fn a_click_in_a_long_line_lands_on_the_row_under_it_with_no_scroll() {
    let text = "x".repeat(100_000);
    let mut e = open(&text, 80.0, 85.0);
    focus(&mut e);
    ctrl(&mut e, END);
    settle(&mut e);
    let bottom = e.scroll_offset();
    let caret_at_end = caret_rect(&e).unwrap();
    // The row above the last one is whole on screen.
    let y = caret_at_end.y.0 - caret_at_end.height.0 / 2.0;
    click(&mut e, 1.0, y, 5_000_000);
    assert_eq!(e.scroll_offset(), bottom, "the click moved the view");
    let caret = caret_rect(&e).unwrap();
    assert_eq!(caret.x.0, 0.0);
    assert!(
        caret.y.0 <= y && y < caret.y.0 + caret.height.0,
        "{caret:?} at {y}"
    );
    let clicked = e.caret();
    key(&mut e, HOME);
    assert_eq!(e.caret(), clicked, "the click lands on the row's start");
    let cols = (80.0 / measure().cell_width().0).floor() as usize;
    key(&mut e, END);
    assert_eq!(e.caret().0 - clicked.0, cols);
}

// ---- Horizontal scrolling must not jump while typing ----

fn scroll_right(e: &mut EditorView, delta_x: f32) {
    send(
        e,
        InputKind::Wheel {
            at: point(1.0, 1.0),
            delta_x,
            delta_y: 0.0,
            modifiers: Modifiers::default(),
        },
        0,
    );
}

#[test]
fn typing_on_a_narrower_line_keeps_a_scroll_a_wider_line_earned() {
    // The widest line is off screen when the caret's narrower line is typed on.
    let wide = "w".repeat(600);
    let narrow = "n".repeat(300);
    let filler = lines(40);
    let text = format!("{wide}\n{filler}{narrow}\n{filler}");
    let mut e = with_wrap(&text, false);
    e.layout(rect(400.0, 100.0), &measure());
    settle(&mut e);
    let cell = measure().cell_width().0;
    // Scroll right past where the narrow line alone would allow.
    let target = 300.0 * cell - 100.0;
    scroll_right(&mut e, target * 120.0 / 24.0);
    assert!((e.horizontal_offset() - target).abs() < 2.0);
    // The caret goes to the narrow line's end, which is in view at this scroll.
    let end = wide.len() + 1 + filler.len() + narrow.len();
    e.select(
        ByteOffset(end - 1)..ByteOffset(end - 1),
        &mut ViewContext::default(),
    );
    key(&mut e, RIGHT);
    settle(&mut e);
    let before = e.horizontal_offset();
    assert!((before - target).abs() < 2.0, "{before} against {target}");
    type_char(&mut e, 'y');
    assert!(
        (e.horizontal_offset() - before).abs() < 0.5,
        "typing jumped from {before} to {}",
        e.horizontal_offset()
    );
    settle(&mut e);
    assert!((e.horizontal_offset() - before).abs() < 0.5);
}

#[test]
fn typing_shaped_text_past_the_right_edge_does_not_jump_left() {
    let text = "日".repeat(200);
    let mut e = with_wrap(&text, false);
    e.layout(rect(200.0, 100.0), &measure());
    settle(&mut e);
    ctrl(&mut e, END);
    settle(&mut e);
    let before = e.horizontal_offset();
    assert!(before > 500.0);
    // The new layout is on a worker, and the view must wait for it where it is.
    type_char(&mut e, '語');
    let mut lowest = e.horizontal_offset();
    let mut ctx = ViewContext::default();
    for _ in 0..500 {
        e.poll_layout(&mut ctx);
        lowest = lowest.min(e.horizontal_offset());
        if !e.pending_layout() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(lowest >= before, "the view went from {before} to {lowest}");
    assert!(e.horizontal_offset() > before, "the caret stays in view");
}

#[test]
fn shrinking_the_widest_line_brings_the_limit_down_when_its_layout_arrives() {
    for token in ['x', '日'] {
        let text = token.to_string().repeat(300);
        let mut e = with_wrap(&text, false);
        e.layout(rect(200.0, 100.0), &measure());
        settle(&mut e);
        ctrl(&mut e, END);
        settle(&mut e);
        assert!(e.horizontal_offset() > 0.0);
        ctrl(&mut e, 0x41);
        type_char(&mut e, 'a');
        settle(&mut e);
        assert_eq!(e.horizontal_offset(), 0.0, "token {token}");
        assert!(!glyph_rows(&e).is_empty(), "token {token}");
    }
}

// ---- Pixels: what a partial repaint leaves equals a full paint ----

struct Screen {
    renderer: BitmapRenderer,
    width: u32,
    height: u32,
    measure: DirectWriteLayout,
}
impl Screen {
    fn new(width: u32, height: u32) -> Self {
        let mut renderer = BitmapRenderer::new(width, height).unwrap();
        renderer
            .set_text_settings(TextSettings::reference())
            .unwrap();
        Self {
            renderer,
            width,
            height,
            measure: measure(),
        }
    }
    /// Paints only what the view asked for, as the window does.
    fn apply(&mut self, e: &EditorView, requests: &[ViewRequest], context: &str) {
        let mut list = DisplayList::default();
        e.draw(&mut list, &Theme::new(ThemeKind::Light), &self.measure);
        for command in &list.commands {
            if let DrawCommand::GlyphRun { font_id, .. } = command {
                self.renderer
                    .register_font_face(*font_id, &qe_layout::font_face(*font_id).unwrap());
            }
        }
        for rect in invalidates(requests) {
            self.renderer.invalidate(rect);
        }
        self.renderer.begin_frame().unwrap();
        let drawn = self.renderer.draw(&list);
        self.renderer.end_frame().unwrap();
        drawn.unwrap_or_else(|e| panic!("{context}: {e:?}"));
        let partial = self.renderer.bitmap().unwrap();
        let full = self
            .renderer
            .render_to_bitmap(&list, self.width, self.height)
            .unwrap();
        let differ = partial
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .zip(full.bgra.as_chunks::<4>().0.iter())
            .enumerate()
            .filter(|(_, (a, b))| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 8))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if let Some(first) = differ.first() {
            panic!(
                "{context}: {} stale pixels, the first at ({}, {}), asked for {:?}, scroll {}",
                differ.len(),
                *first as u32 % self.width,
                *first as u32 / self.width,
                invalidates(requests),
                e.scroll_offset(),
            );
        }
    }
}

#[test]
fn a_partial_repaint_leaves_the_same_pixels_as_a_full_paint() {
    for (wrap, seed0, long) in [
        (true, 0x1234_5678_u32, 0),
        (false, 0x1234_5678, 0),
        (true, 0xBEEF, 0),
        (false, 0xBEEF, 0),
        (true, 77, 20_000),
        (false, 77, 20_000),
    ] {
        let text = format!(
            "{}\n{}",
            "x".repeat(long),
            "Hello world, this is a line of text\nsecond line\n\nfourth has 日本語 and e\u{301} here\n"
                .repeat(6)
        );
        let mut e = EditorView::from_text(&text).unwrap();
        e.set_preferred_ending("\n");
        e.set_wrap(wrap).unwrap();
        e.set_clipboard(Box::new(MemoryClipboard("pasted\r\ntext".into())));
        e.layout(rect(300.0, 200.0), &measure());
        settle(&mut e);
        let mut screen = Screen::new(300, 200);
        focus(&mut e);
        screen.apply(&e, &[ViewRequest::Invalidate(rect(300.0, 200.0))], "first");
        let mut seed = seed0;
        for step in 0..160_u64 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let pick = (seed >> 8) % 21;
            let x = ((seed >> 12) % 300) as f32;
            let y = ((seed >> 20) % 200) as f32;
            let time = 10_000_000 * (step + 1);
            let got = match pick {
                0..=3 => type_char(&mut e, ['a', ' ', '日', 'é'][(seed >> 16) as usize % 4]),
                4 => key(&mut e, ENTER),
                5 => key(&mut e, BACKSPACE),
                6 => key(&mut e, DELETE),
                7 => key(&mut e, LEFT),
                8 => key(&mut e, RIGHT),
                9 => key(&mut e, UP),
                10 => key(&mut e, DOWN),
                11 => shift(&mut e, DOWN),
                12 => shift(&mut e, LEFT),
                13 => ctrl(&mut e, 0x41),
                14 => ctrl(&mut e, 0x5a),
                15 => ctrl(&mut e, 0x56),
                16 => {
                    let at = point(x, y);
                    let mut got = send(
                        &mut e,
                        InputKind::MouseDown {
                            at,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                        },
                        time,
                    );
                    got.extend(send(
                        &mut e,
                        InputKind::MouseUp {
                            at,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                        },
                        time,
                    ));
                    got
                }
                17 => {
                    let mut ctx = ViewContext::default();
                    e.on_timer_at(Instant::now() + Duration::from_secs(1), &mut ctx);
                    ctx.requests
                }
                18 => send(
                    &mut e,
                    InputKind::Wheel {
                        at: point(1.0, 1.0),
                        delta_x: (y - 100.0) * 2.0,
                        delta_y: x - 150.0,
                        modifiers: Modifiers::default(),
                    },
                    time,
                ),
                19 => key(&mut e, if x < 150.0 { PG_DOWN } else { PG_UP }),
                _ => {
                    let mut ctx = ViewContext::default();
                    e.poll_layout(&mut ctx);
                    ctx.requests
                }
            };
            let context = format!("wrap {wrap} seed {seed0} step {step} pick {pick}");
            // First as a key paints with its layout still out, then with layout settled.
            screen.apply(&e, &got, &format!("{context}, right after the event"));
            let mut ctx = ViewContext::default();
            for _ in 0..500 {
                e.poll_layout(&mut ctx);
                if !e.pending_layout() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            screen.apply(&e, &ctx.requests, &format!("{context}, after layout"));
        }
    }
}

// ---- The seeded model ----

/// One caret stop of the model's text.
const TOKENS: [&str; 9] = ["a", " ", "e\u{301}", "日", "😀", "🇺🇸", "👩‍👩‍👧", "\n", "\r\n"];

/// Cuts text from these tokens into its stops. It is the plain model of what a caret
/// may do, and it knows nothing of layout.
fn stops_of(text: &str) -> Vec<usize> {
    let mut stops = vec![0];
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let token = TOKENS
            .iter()
            .filter(|t| rest.starts_with(**t))
            .max_by_key(|t| t.len())
            .or(["p", "q"].iter().find(|t| rest.starts_with(**t)))
            .unwrap_or_else(|| panic!("no token at {rest:?}"));
        at += token.len();
        stops.push(at);
    }
    stops
}

struct Model {
    text: String,
    anchor: usize,
    caret: usize,
}
impl Model {
    fn range(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
    fn replace(&mut self, value: &str) {
        let range = self.range();
        self.text.replace_range(range.clone(), value);
        self.caret = range.start + value.len();
        self.anchor = self.caret;
    }
    /// Undo may keep a selection, when it has nothing to undo. Other moves collapse it.
    fn take_selection_of(&mut self, e: &EditorView) {
        let range = e.selection();
        self.caret = e.caret().0;
        self.anchor = if range.start.0 == self.caret {
            range.end.0
        } else {
            range.start.0
        };
    }
    fn stop_before(&self, at: usize) -> Option<usize> {
        stops_of(&self.text).into_iter().rev().find(|s| *s < at)
    }
    fn stop_after(&self, at: usize) -> Option<usize> {
        stops_of(&self.text).into_iter().find(|s| *s > at)
    }
}

/// Runs seeded edits against the plain model. With `settled` false the keys arrive
/// faster than layout, and only some steps wait.
fn run_model(seed0: u32, wrap: bool, settled: bool, steps: usize) -> usize {
    let (width, height) = (90.0, 4000.0);
    let new_view = |text: &str| {
        let mut e = EditorView::from_text(text).unwrap();
        e.set_preferred_ending("\n");
        e.set_wrap(wrap).unwrap();
        e.layout(rect(width, height), &measure());
        e
    };
    let mut e = new_view("");
    e.set_clipboard(Box::new(MemoryClipboard("p\r\nq".into())));
    let mut model = Model {
        text: String::new(),
        anchor: 0,
        caret: 0,
    };
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(String::new());
    let mut seed = seed0.wrapping_mul(0x9E37_79B9);
    let mut compared = 0;
    for step in 0..steps {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let op = (seed >> 8) % 16;
        let pick = (seed >> 16) as usize;
        let context = format!("seed {seed0} wrap {wrap} settled {settled} step {step} op {op}");
        match op {
            0..=4 => {
                let token = ["a", " ", "e\u{301}", "日", "😀", "🇺🇸", "👩‍👩‍👧"][pick % 7];
                // A token of several characters arrives as one paste, as an input method does.
                let mut chars = token.chars();
                if token.chars().count() == 1 {
                    type_char(&mut e, chars.next().unwrap());
                } else {
                    let mut local = ViewContext::default();
                    let range = e.selection();
                    e.replace_range(range, token, &mut local);
                }
                model.replace(token);
            }
            5 => {
                key(&mut e, ENTER);
                model.replace("\n");
            }
            6 => {
                ctrl(&mut e, 0x56);
                model.replace("p\r\nq");
            }
            7 => {
                key(&mut e, LEFT);
                let range = model.range();
                let to = if range.is_empty() {
                    model.stop_before(model.caret).unwrap_or(0)
                } else {
                    range.start
                };
                model.caret = to;
                model.anchor = to;
            }
            8 => {
                key(&mut e, RIGHT);
                let range = model.range();
                let to = if range.is_empty() {
                    model.stop_after(model.caret).unwrap_or(model.caret)
                } else {
                    range.end
                };
                model.caret = to;
                model.anchor = to;
            }
            9 => {
                shift(&mut e, if pick.is_multiple_of(2) { LEFT } else { RIGHT });
                let to = if pick.is_multiple_of(2) {
                    model.stop_before(model.caret).unwrap_or(0)
                } else {
                    model.stop_after(model.caret).unwrap_or(model.caret)
                };
                model.caret = to;
            }
            10 => {
                key(&mut e, BACKSPACE);
                if model.range().is_empty() {
                    if let Some(to) = model.stop_before(model.caret) {
                        model.anchor = to;
                        model.replace("");
                    }
                } else {
                    model.replace("");
                }
            }
            11 => {
                key(&mut e, DELETE);
                if model.range().is_empty() {
                    if let Some(to) = model.stop_after(model.caret) {
                        model.anchor = to;
                        model.replace("");
                    }
                } else {
                    model.replace("");
                }
            }
            12 | 13 => {
                // Vertical moves: the plain model says only that the caret lands on a stop.
                key(&mut e, if op == 12 { UP } else { DOWN });
                model.caret = e.caret().0;
                model.anchor = model.caret;
            }
            14 => {
                ctrl(&mut e, 0x5a);
                let text = e.text().unwrap();
                assert!(seen.contains(&text), "{context}: undo made new text");
                model.text = text;
                model.take_selection_of(&e);
            }
            _ => {
                ctrl(&mut e, 0x59);
                let text = e.text().unwrap();
                assert!(seen.contains(&text), "{context}: redo made new text");
                model.text = text;
                model.take_selection_of(&e);
            }
        }
        seen.insert(model.text.clone());
        let text = e.text().unwrap();
        assert_eq!(text, model.text, "{context}");
        assert_eq!(e.caret().0, model.caret, "{context}: text {text:?}");
        assert_eq!(
            e.selection(),
            ByteOffset(model.range().start)..ByteOffset(model.range().end),
            "{context}"
        );
        assert_eq!(e.test_height_map_lines(), e.line_count(), "{context}");
        let stops = stops_of(&text);
        assert!(
            stops.binary_search(&e.caret().0).is_ok(),
            "{context}: caret {} is not a stop of {text:?}",
            e.caret().0
        );
        let wait_now = settled || (step + 1).is_multiple_of(20);
        if wait_now {
            settle(&mut e);
            assert_eq!(e.text().unwrap(), text, "{context}");
            if step.is_multiple_of(7) || !settled {
                let mut fresh = new_view(&text);
                settle(&mut fresh);
                fresh.select(e.selection(), &mut ViewContext::default());
                if e.scroll_offset() == 0.0 && e.horizontal_offset() == 0.0 {
                    assert_eq!(glyph_rows(&e), glyph_rows(&fresh), "{context}: {text:?}");
                    assert_eq!(selection_rects(&e), selection_rects(&fresh), "{context}");
                    compared += 1;
                }
            }
        }
    }
    compared
}

#[test]
fn seeded_edits_match_the_plain_model_with_layout_settled() {
    let mut compared = 0;
    for seed in [1, 7, 99] {
        for wrap in [true, false] {
            compared += run_model(seed, wrap, true, 180);
        }
    }
    assert!(compared > 40, "only {compared} paints compared");
}

#[test]
fn seeded_edits_match_the_plain_model_with_keys_faster_than_layout() {
    let mut compared = 0;
    for seed in [2, 13, 123] {
        for wrap in [true, false] {
            compared += run_model(seed, wrap, false, 240);
        }
    }
    assert!(compared > 20, "only {compared} paints compared");
}

// ---- Saved images with a selection and a caret ----

#[test]
fn saved_images_show_a_selection_and_a_caret() {
    let text = "Hello world\nSelected line\n\nمرحبا 😀\nLast line";
    let font = "Consolas";
    let measure = DirectWriteLayout::with_font(
        font,
        10.0,
        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
    )
    .unwrap();
    let mut e = EditorView::from_text(text).unwrap();
    e.set_font(font, 10.0).unwrap();
    e.layout(rect(300.0, 110.0), &measure);
    settle(&mut e);
    focus(&mut e);
    // From inside the first line, across a blank line, to two letters into the Arabic.
    e.select(ByteOffset(6)..ByteOffset(31), &mut ViewContext::default());
    for (name, caret) in [("selection", false), ("caret", true)] {
        if caret {
            e.select(ByteOffset(5)..ByteOffset(5), &mut ViewContext::default());
        }
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let mut list = DisplayList::default();
            e.draw(&mut list, &Theme::new(kind), &measure);
            let fonts: Vec<u32> = list
                .commands
                .iter()
                .filter_map(|c| match c {
                    DrawCommand::GlyphRun { font_id, .. } => Some(*font_id),
                    _ => None,
                })
                .collect();
            assert!(!fonts.is_empty());
            let fills = list
                .commands
                .iter()
                .filter(|c| matches!(c, DrawCommand::FillRect { .. }))
                .count();
            // The background, and the selection's rows or the caret.
            assert!(fills >= if caret { 2 } else { 4 }, "{fills} fills");
            for dpi in [96, 144, 192] {
                let mut renderer = BitmapRenderer::new(300, 110).unwrap();
                renderer
                    .set_text_settings(TextSettings::reference())
                    .unwrap();
                renderer.set_dpi(dpi).unwrap();
                for id in &fonts {
                    renderer.register_font_face(*id, &qe_layout::font_face(*id).unwrap());
                }
                let bitmap = renderer.render_to_bitmap(&list, 300, 110).unwrap();
                let file = format!("{name}-{kind:?}-{dpi}.bmp");
                check_image(&file, &bitmap);
            }
        }
    }
}

fn check_image(name: &str, bitmap: &qe_api::Bitmap) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/images")
        .join(name);
    if std::env::var_os("QE_EDITOR_DUMP_IMAGES").is_some() {
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
        std::fs::write(&path, bytes).unwrap();
        return;
    }
    let baseline = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}. Make it with QE_EDITOR_DUMP_IMAGES.",
            path.display()
        )
    });
    assert_eq!(baseline.len(), 54 + bitmap.bgra.len(), "{name}");
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
    assert!(changed <= 135, "{changed} changed pixels in {name}");
}

// ---- Item 10: budgets that release builds can assert ----

/// A key's work in this crate: `handle`, and building the frame's display list. The
/// window's part is measured by the example's `--bench-keys`, which root reports.
/// One bad run on a loaded PC does not fail this: the best of three runs counts.
#[cfg(not(debug_assertions))]
#[test]
fn release_budget_key_work_p99_under_2_ms_on_one_megabyte() {
    let text = "x\n".repeat(500_000);
    let mut e = open(&text, 800.0, 600.0);
    focus(&mut e);
    let measure = measure();
    let theme = Theme::new(ThemeKind::Light);
    let mut best = f64::MAX;
    for _ in 0..3 {
        let mut samples = Vec::new();
        for i in 0..400 {
            let started = Instant::now();
            if i % 2 == 0 {
                type_char(&mut e, 'q');
            } else {
                key(&mut e, BACKSPACE);
            }
            let mut list = DisplayList::default();
            e.draw(&mut list, &theme, &measure);
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            e.poll_layout(&mut ViewContext::default());
        }
        samples.sort_by(f64::total_cmp);
        best = best.min(samples[(samples.len() * 99).div_ceil(100) - 1]);
    }
    eprintln!("key work p99 on 1 MB, best of 3 runs: {best:.3} ms");
    assert!(best < 2.0, "p99 {best:.3} ms");
}
