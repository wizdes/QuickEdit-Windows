//! The hooks in `HOOKS.md`, each with layout settled, with layout not settled, on a
//! mapped file whose index is still running, and after edits.
use qe_api::{
    Affinity, ByteOffset, Dip, DisplayList, DrawCommand, InputEvent, InputKind, LineIndex,
    LookupError, Modifiers, MouseButton, Point, Rect, Renderer, Theme, ThemeKind, Utf16Offset,
    View, ViewContext, ViewRequest,
};
use qe_buffer::{MappedStore, PieceStore};
use qe_editor::{
    ChangesSince, Clipboard, CompositionClause, CompositionEnd, DecorationKind, EditorCommand,
    EditorView, GeometryError, HostCommand, LoadedFile, PageWrap, TextEdit, prepare_edits,
};
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const ESC: u16 = 0x1b;
const END: u16 = 0x23;
const HOME: u16 = 0x24;

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
fn ctx() -> ViewContext {
    ViewContext::default()
}
fn settle(e: &mut EditorView) {
    let mut context = ctx();
    e.poll_layout(&mut context);
    for _ in 0..2500 {
        if !e.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
        e.poll_layout(&mut context);
        context.requests.clear();
    }
    panic!("the view did not settle");
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
fn send(e: &mut EditorView, kind: InputKind) -> Vec<ViewRequest> {
    let mut context = ctx();
    e.handle(&InputEvent { time_us: 0, kind }, &mut context);
    context.requests
}
fn press(e: &mut EditorView, code: u16, ctrl: bool) -> Vec<ViewRequest> {
    send(
        e,
        InputKind::KeyDown {
            virtual_key: code,
            modifiers: Modifiers {
                ctrl,
                ..Default::default()
            },
            repeat: false,
        },
    )
}
fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn b(n: usize) -> ByteOffset {
    ByteOffset(n)
}
fn draw(e: &EditorView, kind: ThemeKind) -> DisplayList {
    let mut list = DisplayList::default();
    e.draw(&mut list, &Theme::new(kind), &measure());
    list
}
fn wrapped_lines(count: usize) -> String {
    (0..count)
        .map(|i| format!("{i:04} abcd efgh ijkl mnop qrst uvwx\n"))
        .collect()
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
    }
}
/// A file whose index reads slowly, so the end of the file stays unknown during a test.
fn indexing_view(name: &str) -> (EditorView, Remove) {
    let path = std::env::temp_dir().join(format!("qe-hooks-{}-{name}", std::process::id()));
    let line = "0123456789 abcdefghijklmnopqrstuvwxyz\n";
    std::fs::write(&path, line.repeat(300_000)).unwrap();
    let store = MappedStore::open(&path).unwrap();
    store.set_test_read_delay(Duration::from_millis(40));
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(800.0, 170.0), &measure());
    (e, Remove(path))
}

// ---- Reading text ----

#[test]
fn a_worker_snapshot_keeps_its_text_and_works_on_another_thread() {
    let mut e = open("one\ntwo", 400.0, 200.0);
    let snapshot = e.worker_snapshot();
    let revision = e.revision();
    assert_eq!(snapshot.revision(), revision);
    send(&mut e, InputKind::Char('x'));
    assert_ne!(e.revision(), revision);
    let read = std::thread::spawn(move || {
        let mut text = String::new();
        assert!(snapshot.visit_chunks(b(0)..b(7), &mut |s| text.push_str(s)));
        text
    });
    assert_eq!(read.join().unwrap(), "one\ntwo");
}

#[test]
fn a_mapped_file_says_what_is_not_known_and_a_worker_can_wait_for_it() {
    let (mut e, _remove) = indexing_view("snapshot.txt");
    assert!(!e.store_status().complete, "the index must still run");
    let far = b(10_000_000);
    assert!(matches!(
        e.line_of_byte(far),
        Err(LookupError::NotYetKnown { .. })
    ));
    assert!(matches!(
        e.byte_to_utf16(far),
        Err(LookupError::NotYetKnown { .. })
    ));
    let revision = e.revision();
    assert_eq!(e.changes_since(revision), ChangesSince::Current);
    let snapshot = e.worker_snapshot();
    let answer = std::thread::spawn(move || snapshot.byte_to_utf16(ByteOffset(1_000)));
    assert_eq!(answer.join().unwrap(), Some(Utf16Offset(1_000)));
    settle_nothing(&mut e);
}
/// A view on a slow index need not settle for this test to end.
fn settle_nothing(e: &mut EditorView) {
    e.poll_layout(&mut ctx());
}

#[test]
fn changes_since_gives_bytes_and_utf16_for_edits_and_undo() {
    let text = "a\u{1f600}b";
    let mut e = open(text, 400.0, 200.0);
    let start = e.revision();
    assert_eq!(e.changes_since(start), ChangesSince::Current);
    let mut context = ctx();
    // After the emoji: byte 5, UTF-16 unit 3.
    e.replace_range(b(5)..b(6), "xy", &mut context);
    let ChangesSince::Changes(changes) = e.changes_since(start) else {
        panic!("no changes");
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change.replaced, b(5)..b(6));
    assert_eq!(changes[0].change.new_len, 2);
    assert_eq!(changes[0].change.revision, e.revision());
    assert_eq!(changes[0].utf16.start, Utf16Offset(3));
    assert_eq!(changes[0].utf16.old_end, Utf16Offset(4));
    assert_eq!(changes[0].utf16.new_end, Utf16Offset(5));
    let after_edit = e.revision();
    e.run(EditorCommand::Undo, &mut context);
    let ChangesSince::Changes(changes) = e.changes_since(after_edit) else {
        panic!("undo must be a change");
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(e.text().unwrap(), text);
    let ChangesSince::Changes(all) = e.changes_since(start) else {
        panic!("no changes");
    };
    assert_eq!(all.len(), 2);
}

#[test]
fn changes_since_says_rescan_past_its_log_and_after_a_new_document() {
    let mut e = open("", 400.0, 200.0);
    let start = e.revision();
    let mut context = ctx();
    for _ in 0..5_000 {
        e.replace_range(b(0)..b(0), "x", &mut context);
        context.requests.clear();
    }
    assert_eq!(e.changes_since(start), ChangesSince::Rescan);
    let recent = e.revision();
    e.replace_range(b(0)..b(0), "x", &mut context);
    assert!(matches!(e.changes_since(recent), ChangesSince::Changes(c) if c.len() == 1));

    let mut e = open("", 400.0, 200.0);
    send(&mut e, InputKind::Char('x'));
    let document = e.document_number();
    let revision = e.revision();
    press(&mut e, 0x5a, true);
    e.mark_save_point();
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    })
    .unwrap();
    assert_ne!(e.document_number(), document);
    assert_eq!(e.changes_since(revision + 100), ChangesSince::Rescan);
    // Only the new document's changes are in the log.
    let fresh = e.revision();
    send(&mut e, InputKind::Char('y'));
    assert!(
        matches!(e.changes_since(fresh), ChangesSince::Changes(c) if c.len() == 1),
        "the old document's changes must be gone"
    );
}

#[test]
fn the_widget_gives_its_selection_in_utf16() {
    let mut e = open("\u{1f600}abc", 400.0, 200.0);
    e.select(b(4)..b(6), &mut ctx());
    assert_eq!(e.widgets().unwrap().selection, Some(2..4));
}

// ---- Word and line queries ----

#[test]
fn word_and_line_queries_answer_and_say_not_yet_known() {
    let mut e = open("hello big world\nsecond line", 400.0, 200.0);
    assert_eq!(e.word_range_at(b(7)).unwrap(), b(6)..b(9));
    assert_eq!(e.line_range(LineIndex(1)).unwrap(), b(16)..b(27));
    assert_eq!(e.line_of_byte(b(20)).unwrap(), LineIndex(1));
    assert_eq!(e.line_range(LineIndex(5)), Err(LookupError::OutOfRange));
    assert_eq!(e.line_of_byte(b(500)), Err(LookupError::OutOfRange));
    let mut context = ctx();
    e.replace_range(b(0)..b(0), "new\n", &mut context);
    assert_eq!(e.line_of_byte(b(24)).unwrap(), LineIndex(2));

    let (mut e, _remove) = indexing_view("words.txt");
    assert!(matches!(
        e.word_range_at(b(10_000_000)),
        Err(LookupError::NotYetKnown { .. })
    ));
    assert!(matches!(
        e.line_range(LineIndex(250_000)),
        Err(LookupError::NotYetKnown { .. })
    ));
}

// ---- Geometry ----

#[test]
fn geometry_says_not_laid_out_and_no_call_shapes() {
    let mut e = cold(&"\u{4e2d}\u{6587}\n".repeat(50), 400.0, 300.0);
    e.test_pause_workers(true);
    e.poll_layout(&mut ctx());
    assert_eq!(e.caret_rect(), Err(GeometryError::NotLaidOut));
    assert_eq!(
        e.range_rects(b(0)..b(3)),
        Err(GeometryError::NotLaidOut),
        "only a worker can lay this out"
    );
    assert_eq!(
        e.byte_at_point(point(5.0, 5.0)),
        Err(GeometryError::NotLaidOut)
    );
    assert_eq!(e.visible_range(), Err(GeometryError::NotLaidOut));
    let rows = e.visible_rows();
    assert!(!rows.complete);
    assert!(rows.rows.is_empty());
    e.test_pause_workers(false);
    settle(&mut e);
    assert!(e.caret_rect().is_ok());
    assert!(e.visible_rows().complete);
}

#[test]
fn a_range_gives_one_rectangle_for_each_row() {
    let text = "abcdefghij".repeat(30);
    let mut e = open(&text, 200.0, 300.0);
    let rows = e.visible_rows();
    assert!(rows.complete);
    assert!(rows.rows.len() > 3);
    let rects = e.range_rects(b(0)..b(text.len())).unwrap();
    assert_eq!(rects.len(), rows.rows.len());
    for (rect, row) in rects.iter().zip(&rows.rows) {
        assert_eq!(rect.rect.y, row.y);
        assert_eq!(rect.rect.height, row.height);
        assert!(!rect.right_to_left);
    }
    // The part after an edit moves with the text.
    let before = e.range_rects(b(5)..b(6)).unwrap()[0].rect;
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    settle(&mut e);
    let after = e.range_rects(b(6)..b(7)).unwrap()[0].rect;
    assert!(
        after.x.0 > before.x.0 + 1.0,
        "one cell on: {before:?} {after:?}"
    );
    // An empty range is the place of a caret.
    let caret = e.range_rects(b(3)..b(3)).unwrap();
    assert_eq!(caret.len(), 1);
}

#[test]
fn a_row_with_two_directions_gives_a_rectangle_for_each_run() {
    let text = "abc \u{5d0}\u{5d1}\u{5d2} def";
    let e = open(text, 600.0, 200.0);
    let rects = e.range_rects(b(0)..b(text.len())).unwrap();
    assert!(rects.len() >= 3, "{rects:?}");
    assert!(rects.iter().any(|r| r.right_to_left));
    assert!(rects.iter().any(|r| !r.right_to_left));
}

#[test]
fn a_long_line_answers_from_its_segments() {
    let text = "word ".repeat(40_000);
    let mut e = open(&text, 400.0, 300.0);
    let rows = e.visible_rows();
    assert!(rows.complete);
    assert!(!rows.rows.is_empty());
    assert!(rows.rows[1..].iter().all(|r| r.continuation));
    let range = e.visible_range().unwrap();
    assert_eq!(range.start, b(0));
    assert!(range.end.0 > 100);
    let hit = e.byte_at_point(point(1.0, 1.0)).unwrap();
    assert_eq!(hit.byte, b(0));
    assert!(e.range_rects(b(10)..b(20)).unwrap().len() == 1);
    assert_eq!(
        e.range_rects(b(190_000)..b(190_010)),
        Err(GeometryError::NotLaidOut),
        "its layout is far off screen"
    );
    e.set_scroll_position(20_000.0, &mut ctx());
    settle(&mut e);
    let rows = e.visible_rows();
    assert!(rows.complete);
    assert!(rows.rows.iter().all(|r| r.continuation));
    let top = rows.rows[0].first_byte;
    let hit = e.byte_at_point(point(1.0, rows.rows[0].y.0 + 1.0)).unwrap();
    assert_eq!(hit.byte, top);
    assert!(e.caret_rect().is_ok() || e.caret_rect() == Err(GeometryError::NotLaidOut));
}

#[test]
fn the_caret_and_the_byte_at_a_point_agree_in_window_coordinates() {
    let mut e = EditorView::from_text(&wrapped_lines(100)).unwrap();
    e.layout(
        Rect {
            x: Dip(30.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(300.0),
        },
        &measure(),
    );
    settle(&mut e);
    e.set_scroll_position(200.0, &mut ctx());
    settle(&mut e);
    let row = e.visible_rows().rows[2].clone();
    e.select(
        ByteOffset(row.first_byte.0 + 3)..ByteOffset(row.first_byte.0 + 3),
        &mut ctx(),
    );
    let caret = e.caret_rect().unwrap();
    assert_eq!(caret.y, row.y);
    assert!(caret.x.0 > 30.0, "{caret:?}");
    let hit = e
        .byte_at_point(point(caret.x.0 + 0.1, caret.y.0 + 1.0))
        .unwrap();
    assert_eq!(hit.byte.0, row.first_byte.0 + 3);
    assert_eq!(hit.affinity, Affinity::After);
    // Past the end of the text is the end of the text.
    let end = e.byte_at_point(point(40.0, 100_000.0)).unwrap();
    assert_eq!(end.byte, e.store_status().known_bytes);
}

#[test]
fn visible_rows_cover_the_view_and_follow_the_scroll() {
    let mut e = open(&wrapped_lines(200), 120.0, 200.0);
    let rows = e.visible_rows();
    assert!(rows.complete);
    assert!(rows.rows.windows(2).all(|p| p[0].y.0 < p[1].y.0));
    assert_eq!(rows.rows[0].line, LineIndex(0));
    assert!(!rows.rows[0].continuation);
    assert!(rows.rows.iter().any(|r| r.continuation));
    let range = e.visible_range().unwrap();
    assert_eq!(range.start, b(0));
    e.set_scroll_position(1_000.0, &mut ctx());
    settle(&mut e);
    let later = e.visible_rows();
    assert!(later.rows[0].line.0 > 0);
    assert!(later.rows.iter().all(|r| r.y.0 > -100.0 && r.y.0 < 200.0));
    assert!(e.visible_range().unwrap().start.0 > 0);
}

#[test]
fn geometry_on_a_mapped_file_says_not_yet_known() {
    let (mut e, _remove) = indexing_view("geometry.txt");
    assert!(matches!(
        e.range_rects(b(10_000_000)..b(10_000_001)),
        Err(GeometryError::Lookup(LookupError::NotYetKnown { .. }))
    ));
    let start = Instant::now();
    while !e.visible_rows().complete && start.elapsed() < Duration::from_secs(20) {
        e.poll_layout(&mut ctx());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(e.visible_rows().complete);
    assert_eq!(e.visible_rows().rows[0].first_byte, b(0));
    assert_eq!(e.range_rects(b(0)..b(5)).map(|r| r.len()), Ok(1));
}

// ---- Decorations ----

fn fills_of(list: &DisplayList, color: qe_api::Color) -> Vec<Rect> {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::FillRect { rect, color: c } if *c == color => Some(*rect),
            _ => None,
        })
        .collect()
}

#[test]
fn an_edit_moves_marks_and_drops_the_ones_it_touches() {
    let mut e = open("0123456789 0123456789 0123456789", 600.0, 200.0);
    let kind = DecorationKind::SpellingUnderline;
    e.set_decorations(
        kind,
        vec![b(22)..b(26), b(0)..b(3), b(11)..b(15)],
        &mut ctx(),
    );
    assert_eq!(
        e.decorations(kind),
        [b(0)..b(3), b(11)..b(15), b(22)..b(26)]
    );
    // Before all of the second and third marks, and just after the first.
    e.replace_range(b(5)..b(5), "xx", &mut ctx());
    assert_eq!(
        e.decorations(kind),
        [b(0)..b(3), b(13)..b(17), b(24)..b(28)]
    );
    // An edit inside a mark drops it, and only it.
    e.replace_range(b(14)..b(15), "y", &mut ctx());
    assert_eq!(e.decorations(kind), [b(0)..b(3), b(24)..b(28)]);
    // An insertion at a mark's edges leaves its text alone.
    e.replace_range(b(24)..b(24), "z", &mut ctx());
    assert_eq!(e.decorations(kind), [b(0)..b(3), b(25)..b(29)]);
    e.replace_range(b(29)..b(29), "z", &mut ctx());
    assert_eq!(e.decorations(kind), [b(0)..b(3), b(25)..b(29)]);
    // A deletion that covers a mark drops it, and one before a mark moves it back.
    e.replace_range(b(1)..b(10), "", &mut ctx());
    assert_eq!(e.decorations(kind), [b(16)..b(20)]);
    // An edit far before and then far after walks over the list both ways.
    e.replace_range(b(30)..b(30), "q", &mut ctx());
    assert_eq!(e.decorations(kind), [b(16)..b(20)]);
    e.replace_range(b(0)..b(0), "q", &mut ctx());
    assert_eq!(e.decorations(kind), [b(17)..b(21)]);
    // A new list replaces the old one, and overlaps join.
    e.set_decorations(kind, vec![b(1)..b(4), b(3)..b(6), b(8)..b(8)], &mut ctx());
    assert_eq!(e.decorations(kind), [b(1)..b(6)]);
}

#[test]
fn undo_and_the_other_kinds_follow_the_same_rule() {
    let mut e = open("abc def", 600.0, 200.0);
    e.set_decorations(DecorationKind::FindMatch, vec![b(4)..b(7)], &mut ctx());
    e.set_decorations(
        DecorationKind::CurrentFindMatch,
        vec![b(0)..b(3)],
        &mut ctx(),
    );
    send(&mut e, InputKind::Char('x'));
    assert_eq!(e.decorations(DecorationKind::FindMatch), [b(5)..b(8)]);
    assert_eq!(
        e.decorations(DecorationKind::CurrentFindMatch),
        [b(1)..b(4)]
    );
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "abc def");
    assert_eq!(e.decorations(DecorationKind::FindMatch), [b(4)..b(7)]);
    assert_eq!(
        e.decorations(DecorationKind::CurrentFindMatch),
        [b(0)..b(3)]
    );
}

#[test]
fn only_marks_on_screen_paint_and_fills_go_behind_the_text() {
    let text = wrapped_lines(5_000);
    let mut e = open(&text, 600.0, 200.0);
    let ranges: Vec<_> = (0..5_000).map(|i| b(i * 35)..b(i * 35 + 4)).collect();
    let theme = Theme::new(ThemeKind::Light);
    e.set_decorations(DecorationKind::FindMatch, ranges, &mut ctx());
    let list = draw(&e, ThemeKind::Light);
    let fills = fills_of(&list, theme.find_match);
    let rows = e.visible_rows().rows.len();
    assert!(!fills.is_empty() && fills.len() <= rows, "{}", fills.len());
    let first_fill = list
        .commands
        .iter()
        .position(
            |c| matches!(c, DrawCommand::FillRect { color, .. } if *color == theme.find_match),
        )
        .unwrap();
    let first_glyph = list
        .commands
        .iter()
        .position(|c| matches!(c, DrawCommand::GlyphRun { .. }))
        .unwrap();
    assert!(first_fill < first_glyph, "a fill would hide the text");
    // The fill sits on the mark.
    let expected = e.range_rects(b(35)..b(39)).unwrap();
    assert_eq!(fills[1].x, expected[0].rect.x);
    assert_eq!(fills[1].y, expected[0].rect.y);
    assert_eq!(fills[1].width, expected[0].rect.width);
}

#[test]
fn underlines_go_over_the_text_and_marks_follow_the_margins() {
    let mut e = open("hello there", 600.0, 200.0);
    let mut context = ctx();
    e.set_decorations(
        DecorationKind::SpellingUnderline,
        vec![b(0)..b(5)],
        &mut context,
    );
    e.set_decorations(
        DecorationKind::CompositionUnderline,
        vec![b(6)..b(11)],
        &mut context,
    );
    let list = draw(&e, ThemeKind::Dark);
    let glyph = list
        .commands
        .iter()
        .position(|c| matches!(c, DrawCommand::GlyphRun { .. }))
        .unwrap();
    let wavy = list
        .commands
        .iter()
        .position(|c| matches!(c, DrawCommand::WavyUnderline { .. }))
        .expect("no wavy line");
    let line = list
        .commands
        .iter()
        .position(|c| matches!(c, DrawCommand::Line { .. }))
        .expect("no straight line");
    assert!(wavy > glyph && line > glyph);
    e.set_margins(Dip(50.0), Dip(0.0), &mut ctx());
    settle(&mut e);
    let list = draw(&e, ThemeKind::Dark);
    let from_x = list
        .commands
        .iter()
        .find_map(|c| match c {
            DrawCommand::WavyUnderline { from, .. } => Some(from.x.0),
            _ => None,
        })
        .unwrap();
    assert!(from_x >= 50.0, "{from_x}");
}

#[test]
fn setting_marks_repaints_only_the_lines_with_marks_on_screen() {
    let mut e = open(&wrapped_lines(500), 600.0, 200.0);
    let invalidated = |context: &ViewContext| {
        context
            .requests
            .iter()
            .filter(|r| matches!(r, ViewRequest::Invalidate(_)))
            .count()
    };
    let mut context = ctx();
    let far = 35 * 400;
    e.set_decorations(
        DecorationKind::FindMatch,
        vec![b(far)..b(far + 4)],
        &mut context,
    );
    assert_eq!(invalidated(&context), 0, "the mark is off screen");
    let mut context = ctx();
    e.set_decorations(DecorationKind::FindMatch, vec![b(35)..b(39)], &mut context);
    assert_eq!(invalidated(&context), 1, "one line, with its new mark");
    let mut context = ctx();
    e.set_decorations(DecorationKind::FindMatch, vec![], &mut context);
    assert_eq!(invalidated(&context), 1, "one line, with its old mark");
}

/// A million marks. A key after the first must cost what a key with no marks costs.
#[cfg(not(debug_assertions))]
#[test]
fn release_budget_a_long_list_of_marks_does_not_slow_a_key() {
    let line = "abcd ".repeat(16) + "\n";
    let lines = 125_000;
    let text = line.repeat(lines);
    let mut e = open(&text, 800.0, 600.0);
    send(&mut e, InputKind::Focus);
    let middle = text.len() / 2;
    e.select(b(middle)..b(middle), &mut ctx());
    settle(&mut e);
    let time_keys = |e: &mut EditorView| {
        let theme = Theme::new(ThemeKind::Light);
        let measure = measure();
        let mut samples = Vec::new();
        for i in 0..400 {
            let started = Instant::now();
            if i % 2 == 0 {
                send(e, InputKind::Char('q'));
            } else {
                press(e, 0x08, false);
            }
            let mut list = DisplayList::default();
            e.draw(&mut list, &theme, &measure);
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            e.poll_layout(&mut ctx());
        }
        samples.sort_by(f64::total_cmp);
        samples[(samples.len() * 99).div_ceil(100) - 1]
    };
    let plain = (0..3).map(|_| time_keys(&mut e)).fold(f64::MAX, f64::min);
    let width = line.len();
    let ranges: Vec<_> = (0..lines)
        .flat_map(|l| (0..16).map(move |k| b(l * width + k * 5)..b(l * width + k * 5 + 4)))
        .collect();
    let started = Instant::now();
    e.set_decorations(DecorationKind::FindMatch, ranges, &mut ctx());
    eprintln!("setting 2,000,000 marks: {:?}", started.elapsed());
    let marked = (0..3).map(|_| time_keys(&mut e)).fold(f64::MAX, f64::min);
    eprintln!("key p99 with no marks {plain:.3} ms, with 2,000,000 marks {marked:.3} ms");
    assert!(marked < 2.0, "p99 {marked:.3} ms");
    assert!(
        marked < plain * 2.0 + 0.5,
        "{plain:.3} became {marked:.3} ms"
    );
}

// ---- Input method ----

fn ime(e: &mut EditorView, text: &str, cursor: usize) -> Vec<ViewRequest> {
    send(
        e,
        InputKind::ImeUpdate {
            text: text.to_string(),
            cursor_utf16: cursor,
        },
    )
}
fn glyph_starts(list: &DisplayList) -> Vec<f32> {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::GlyphRun { at, .. } => Some(at.x.0),
            _ => None,
        })
        .collect()
}
fn glyph_count(list: &DisplayList) -> usize {
    list.commands
        .iter()
        .map(|c| match c {
            DrawCommand::GlyphRun { glyph_ids, .. } => glyph_ids.len(),
            _ => 0,
        })
        .sum()
}

#[test]
fn composition_shows_inline_with_the_store_unchanged() {
    let mut e = open("abcdef", 600.0, 200.0);
    send(&mut e, InputKind::Focus);
    e.select(b(3)..b(3), &mut ctx());
    let revision = e.revision();
    let plain_glyphs = glyph_count(&draw(&e, ThemeKind::Light));
    send(&mut e, InputKind::ImeStart);
    assert!(e.composition().is_some());
    ime(&mut e, "\u{4e2d}\u{6587}", 1);
    settle(&mut e);
    let state = e.composition().unwrap();
    assert!(state.showing);
    assert_eq!(state.at, b(3));
    assert_eq!(state.text, "\u{4e2d}\u{6587}");
    assert_eq!(e.text().unwrap(), "abcdef", "the store did not change");
    assert_eq!(e.revision(), revision);
    assert_eq!(e.selection(), b(3)..b(3));
    let list = draw(&e, ThemeKind::Light);
    assert!(glyph_count(&list) > plain_glyphs, "the text shows");
    assert!(
        list.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::Line { .. })),
        "the composition is underlined"
    );
    // Its cursor stands after the first character, and the rectangles cover it.
    let rects = e.composition_rects().unwrap();
    assert_eq!(rects.len(), 1);
    let caret = e.caret_rect().unwrap();
    assert!(caret.x.0 > rects[0].rect.x.0 + 1.0);
    assert!(caret.x.0 < rects[0].rect.x.0 + rects[0].rect.width.0 - 1.0);
}

#[test]
fn composition_moves_the_text_after_it() {
    let mut e = open("abcdef", 600.0, 200.0);
    e.select(b(3)..b(3), &mut ctx());
    let right_of_plain = last_glyph_end(&draw(&e, ThemeKind::Light));
    ime(&mut e, "\u{4e2d}\u{6587}", 2);
    settle(&mut e);
    let right_of_composed = last_glyph_end(&draw(&e, ThemeKind::Light));
    assert!(right_of_composed > right_of_plain + 10.0);
    let starts = glyph_starts(&draw(&e, ThemeKind::Light));
    assert!(starts.len() >= 2, "{starts:?}");
}
fn last_glyph_end(list: &DisplayList) -> f32 {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::GlyphRun { at, advances, .. } => {
                Some(at.x.0 + advances.iter().map(|a| a.0).sum::<f32>())
            }
            _ => None,
        })
        .fold(0.0, f32::max)
}

#[test]
fn composition_never_lays_out_on_the_ui_thread() {
    let mut e = open("abcdef", 600.0, 200.0);
    e.test_pause_workers(true);
    ime(&mut e, "\u{4e2d}", 1);
    e.poll_layout(&mut ctx());
    assert!(!e.composition().unwrap().showing);
    assert_eq!(e.composition_rects(), Err(GeometryError::NotLaidOut));
    assert!(e.pending_layout(), "a timer waits for the worker");
    e.test_pause_workers(false);
    settle(&mut e);
    assert!(e.composition().unwrap().showing);
}

#[test]
fn a_commit_is_one_edit_in_place_of_the_selection() {
    let mut e = open("abcdef", 600.0, 200.0);
    e.select(b(1)..b(3), &mut ctx());
    send(&mut e, InputKind::ImeStart);
    ime(&mut e, "\u{4e2d}", 1);
    ime(&mut e, "\u{6587}", 1);
    settle(&mut e);
    assert_eq!(e.selection(), b(1)..b(3), "the selection waits");
    let revision = e.revision();
    send(
        &mut e,
        InputKind::ImeEnd {
            text: "\u{6587}\u{5b57}".into(),
        },
    );
    assert!(e.composition().is_none());
    assert_eq!(e.text().unwrap(), "a\u{6587}\u{5b57}def");
    assert_eq!(e.caret(), b(7));
    assert!(matches!(e.changes_since(revision), ChangesSince::Changes(c) if c.len() == 1));
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "abcdef", "one undo takes it all back");
}

#[test]
fn a_cancel_leaves_the_store_and_the_selection_as_they_were() {
    let mut e = open("abcdef\nsecond", 600.0, 200.0);
    let height = e.total_height();
    e.select(b(1)..b(3), &mut ctx());
    let revision = e.revision();
    send(&mut e, InputKind::ImeStart);
    ime(&mut e, "\u{4e2d}", 1);
    settle(&mut e);
    send(
        &mut e,
        InputKind::ImeEnd {
            text: String::new(),
        },
    );
    assert!(e.composition().is_none());
    assert_eq!(e.revision(), revision);
    assert_eq!(e.text().unwrap(), "abcdef\nsecond");
    assert_eq!(e.selection(), b(1)..b(3));
    assert_eq!(e.total_height(), height);
    settle(&mut e);
    assert!(
        !draw(&e, ThemeKind::Light)
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::Line { .. })),
        "no underline is left"
    );
}

#[test]
fn a_composition_that_wraps_gives_its_room_back() {
    let mut e = open("abcd\nsecond line", 120.0, 300.0);
    let height = e.total_height();
    e.select(b(2)..b(2), &mut ctx());
    ime(&mut e, &"\u{4e2d}".repeat(40), 40);
    settle(&mut e);
    assert!(e.total_height() > height, "the lines below move down");
    e.ime_cancel(&mut ctx());
    settle(&mut e);
    assert_eq!(e.total_height(), height);
}

#[test]
fn esc_asks_the_host_to_cancel_and_ends_it_here() {
    let mut e = open("abc", 600.0, 200.0);
    ime(&mut e, "\u{4e2d}", 1);
    let requests = press(&mut e, ESC, false);
    assert!(requests.contains(&ViewRequest::CancelComposition));
    assert!(e.composition().is_none());
    assert!(
        !press(&mut e, ESC, false).contains(&ViewRequest::CancelComposition),
        "with no composition, Esc is what it was"
    );
}

#[test]
fn an_edit_or_a_moved_selection_commits_a_composition_and_tells_the_host() {
    let mut e = open("abc", 600.0, 200.0);
    ime(&mut e, "\u{4e2d}", 1);
    let mut context = ctx();
    e.replace_range(b(0)..b(0), "x", &mut context);
    assert!(context.requests.contains(&ViewRequest::CancelComposition));
    assert!(e.composition().is_none());
    assert_eq!(e.text().unwrap(), "x\u{4e2d}abc", "its text went in");

    ime(&mut e, "\u{6587}", 1);
    let requests = press(&mut e, 0x27, false);
    assert!(requests.contains(&ViewRequest::CancelComposition));
    assert!(e.composition().is_none());
    assert!(e.text().unwrap().contains('\u{6587}'), "its text went in");
}

#[test]
fn a_read_only_view_shows_no_composition() {
    let mut e = open("abc", 600.0, 200.0);
    e.set_prevent_editing(true);
    let refused = e.refused_edits();
    ime(&mut e, "\u{4e2d}", 1);
    assert!(e.composition().is_none());
    assert!(e.refused_edits() > refused);
    send(&mut e, InputKind::ImeEnd { text: "x".into() });
    assert_eq!(e.text().unwrap(), "abc");
}

#[test]
fn a_cursor_inside_a_surrogate_pair_steps_back() {
    let mut e = open("abc", 600.0, 200.0);
    send(&mut e, InputKind::Focus);
    ime(&mut e, "\u{1f600}x", 1);
    settle(&mut e);
    assert!(e.composition().unwrap().showing);
    let rects = e.composition_rects().unwrap();
    assert_eq!(e.caret_rect().unwrap().x, rects[0].rect.x);
    ime(&mut e, "\u{1f600}x", 99);
    settle(&mut e);
    let rects = e.composition_rects().unwrap();
    assert!(e.caret_rect().unwrap().x.0 >= rects[0].rect.x.0 + rects[0].rect.width.0 - 1.0);
}

#[test]
fn a_composition_on_a_long_line_shows_inside_its_segment() {
    let text = "word ".repeat(40_000);
    let mut e = open(&text, 400.0, 300.0);
    send(&mut e, InputKind::Focus);
    e.select(b(100)..b(100), &mut ctx());
    settle(&mut e);
    ime(&mut e, "\u{4e2d}\u{6587}", 2);
    settle(&mut e);
    assert!(e.composition().unwrap().showing);
    assert!(e.composition_rects().is_ok());
    assert!(e.caret_rect().is_ok());
    send(&mut e, InputKind::ImeEnd { text: "x".into() });
    assert_eq!(e.text().unwrap().len(), text.len() + 1);
}

#[test]
fn input_method_events_keep_their_place_behind_a_waiting_key() {
    let mut e = cold(&"\u{4e2d}".repeat(400), 300.0, 600.0);
    e.test_pause_workers(true);
    press(&mut e, END, false);
    assert_eq!(e.queued_input_count(), 1);
    send(&mut e, InputKind::ImeStart);
    ime(&mut e, "x", 1);
    send(&mut e, InputKind::ImeEnd { text: "x".into() });
    assert_eq!(e.queued_input_count(), 4, "all wait behind End");
    assert!(!e.text().unwrap().contains('x'));
    e.test_pause_workers(false);
    settle(&mut e);
    assert_eq!(e.queued_input_count(), 0);
    let text = e.text().unwrap();
    assert!(text.find('x').unwrap() > 0, "the commit landed after End");
}

// ---- Space and scrolling ----

#[test]
fn margins_move_the_text_area_and_hit_tests_and_keep_the_top_text() {
    let mut e = open(&wrapped_lines(400), 600.0, 300.0);
    e.set_scroll_position(1_500.0, &mut ctx());
    settle(&mut e);
    let top = e.test_byte_at_top();
    e.set_margins(Dip(40.0), Dip(100.0), &mut ctx());
    assert_eq!(
        e.text_area(),
        Rect {
            x: Dip(40.0),
            y: Dip(0.0),
            width: Dip(460.0),
            height: Dip(300.0),
        }
    );
    settle(&mut e);
    assert_eq!(e.test_byte_at_top(), top, "the same text stays at the top");
    let row = e.visible_rows().rows[0].clone();
    assert_eq!(
        e.byte_at_point(point(40.0, row.y.0 + 1.0)).unwrap().byte,
        row.first_byte,
        "the text starts at the margin"
    );
    let starts = glyph_starts(&draw(&e, ThemeKind::Light));
    assert!(starts.iter().all(|x| *x >= 40.0), "{starts:?}");
    // A later resize and a click use the same space.
    send(
        &mut e,
        InputKind::Resize {
            width: Dip(700.0),
            height: Dip(300.0),
        },
    );
    assert_eq!(e.text_area().width, Dip(560.0));
    settle(&mut e);
    send(
        &mut e,
        InputKind::MouseDown {
            at: point(40.0, row.y.0 + 1.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(e.caret(), e.visible_rows().rows[0].first_byte);
    // Equal margins change nothing, so a gutter that stays does not restart the layout.
    let sent = e.layout_tasks_sent();
    settle(&mut e);
    assert_eq!(e.layout_tasks_sent(), sent);
}

#[test]
fn a_scroll_position_is_exact_and_stays_inside_the_limits() {
    let mut e = open(&wrapped_lines(400), 600.0, 300.0);
    let mut context = ctx();
    e.set_scroll_position(123.5, &mut context);
    assert_eq!(e.scroll_offset(), 123.5);
    assert!(
        context
            .requests
            .iter()
            .any(|r| matches!(r, ViewRequest::Invalidate(_)))
    );
    e.set_scroll_position(-5.0, &mut ctx());
    assert_eq!(e.scroll_offset(), 0.0);
    e.set_scroll_position(1e12, &mut ctx());
    assert_eq!(e.scroll_offset(), e.max_scroll_position());
    assert_eq!(e.max_scroll_position(), e.total_height() - 300.0);
    // It ends a pending view change, as a wheel does.
    e.set_scroll_position(800.0, &mut ctx());
    settle(&mut e);
    let old_top = e.test_byte_at_top();
    send(
        &mut e,
        InputKind::Resize {
            width: Dip(200.0),
            height: Dip(300.0),
        },
    );
    e.set_scroll_position(100.0, &mut ctx());
    settle(&mut e);
    assert!(e.test_byte_at_top() < old_top, "the view did not go back");
}

#[test]
fn a_page_wrap_ignores_the_view_width_and_its_margin_moves_the_text() {
    let text = wrapped_lines(100);
    let mut e = open(&text, 800.0, 300.0);
    send(&mut e, InputKind::Focus);
    e.set_page_wrap(
        Some(PageWrap {
            text_width: Dip(150.0),
            margin: Dip(30.0),
        }),
        &mut ctx(),
    );
    settle(&mut e);
    let height = e.total_height();
    let rows = e.visible_rows();
    assert!(rows.rows.iter().any(|r| r.continuation), "it wraps at 150");
    assert_eq!(
        e.byte_at_point(point(30.0, rows.rows[0].y.0 + 1.0))
            .unwrap()
            .byte,
        b(0)
    );
    assert!(
        glyph_starts(&draw(&e, ThemeKind::Light))
            .iter()
            .all(|x| *x >= 30.0)
    );
    // The view narrower than the page: the lines stay, and it scrolls sideways.
    send(
        &mut e,
        InputKind::Resize {
            width: Dip(100.0),
            height: Dip(300.0),
        },
    );
    settle(&mut e);
    assert_eq!(e.total_height(), height, "the view's width changes nothing");
    press(&mut e, END, false);
    settle(&mut e);
    assert!(e.horizontal_offset() > 0.0, "the caret stays in view");
    // Back to the view's width, with the same text at the top.
    e.set_scroll_position(600.0, &mut ctx());
    settle(&mut e);
    let top = e.test_byte_at_top();
    e.set_page_wrap(None, &mut ctx());
    settle(&mut e);
    assert_eq!(e.test_byte_at_top(), top);
}

#[test]
fn zoom_through_the_font_keeps_the_view_steady() {
    let mut e = open(&wrapped_lines(400), 600.0, 300.0);
    e.set_scroll_position(1_500.0, &mut ctx());
    settle(&mut e);
    let top = e.test_byte_at_top();
    for points in [20.0, 5.0, 40.0, 10.0] {
        e.apply_font("Consolas", points, &mut ctx()).unwrap();
        settle(&mut e);
        assert_eq!(e.test_byte_at_top(), top, "{points} pt");
    }
}

// ---- Edits from tools ----

fn edit(start: usize, end: usize, text: &str) -> TextEdit {
    TextEdit {
        range: b(start)..b(end),
        text: text.to_string(),
    }
}
static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

#[test]
fn many_replacements_are_one_edit_and_one_undo_step_with_the_callers_selection() {
    let mut e = open("aa bb aa cc aa", 600.0, 200.0);
    send(&mut e, InputKind::Char('z'));
    let edits = [edit(1, 3, "XXX"), edit(7, 9, ""), edit(13, 15, "Y")];
    let snapshot = e.worker_snapshot();
    let revision = e.revision();
    let prepared = std::thread::spawn(move || {
        prepare_edits(snapshot.as_ref(), &edits, &NOT_CANCELLED).unwrap()
    })
    .join()
    .unwrap();
    assert_eq!(prepared.base_revision(), revision);
    assert_eq!(prepared.edit_count(), 3);
    assert_eq!(prepared.replaced_range(), b(1)..b(15));
    let sent = e.layout_tasks_sent();
    e.apply_edits(&prepared, b(2)..b(4), &mut ctx()).unwrap();
    assert_eq!(e.text().unwrap(), "zXXX bb  cc Y");
    assert_eq!(e.selection(), b(2)..b(4));
    assert!(e.layout_tasks_sent() >= sent);
    let ChangesSince::Changes(changes) = e.changes_since(revision) else {
        panic!("no change");
    };
    assert_eq!(changes.len(), 1, "one edit, not one for each replacement");
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "zaa bb aa cc aa", "one undo step");
    press(&mut e, 0x5a, true);
    assert_eq!(
        e.text().unwrap(),
        "aa bb aa cc aa",
        "typing was its own step"
    );
}

#[test]
fn prepared_edits_are_refused_when_they_would_not_fit() {
    let mut e = open("aa bb", 600.0, 200.0);
    let snapshot = e.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &[edit(0, 2, "x")], &NOT_CANCELLED).unwrap();
    send(&mut e, InputKind::Char('q'));
    let error = e
        .apply_edits(&prepared, b(0)..b(0), &mut ctx())
        .unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(e.text().unwrap(), "qaa bb");

    let snapshot = e.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &[edit(0, 2, "x")], &NOT_CANCELLED).unwrap();
    e.set_prevent_editing(true);
    let refused = e.refused_edits();
    assert!(e.apply_edits(&prepared, b(0)..b(0), &mut ctx()).is_err());
    assert_eq!(e.refused_edits(), refused + 1);
    assert_eq!(e.text().unwrap(), "qaa bb");
    e.set_prevent_editing(false);

    let snapshot = e.worker_snapshot();
    let bad = [edit(3, 4, "x"), edit(0, 1, "y")];
    assert!(prepare_edits(snapshot.as_ref(), &bad, &NOT_CANCELLED).is_err());
    assert!(prepare_edits(snapshot.as_ref(), &[], &NOT_CANCELLED).is_err());
    let cancelled = AtomicBool::new(true);
    assert!(prepare_edits(snapshot.as_ref(), &[edit(0, 1, "y")], &cancelled).is_err());
    // A range inside a character cannot be read, so it does not go through.
    let e = open("\u{4e2d}abc", 600.0, 200.0);
    let snapshot = e.worker_snapshot();
    assert!(
        prepare_edits(
            snapshot.as_ref(),
            &[edit(1, 1, ""), edit(1, 4, "")],
            &NOT_CANCELLED
        )
        .is_err()
    );
}

#[test]
fn many_lines_replaced_at_once_keep_heights_and_marks_right() {
    let text = wrapped_lines(300);
    let mut e = open(&text, 600.0, 300.0);
    e.set_decorations(
        DecorationKind::FindMatch,
        vec![b(0)..b(4), b(35 * 200)..b(35 * 200 + 4)],
        &mut ctx(),
    );
    let edits: Vec<_> = (0..100)
        .map(|i| edit(i * 35, i * 35 + 34, "x\ny"))
        .collect();
    let snapshot = e.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &edits, &NOT_CANCELLED).unwrap();
    e.apply_edits(&prepared, b(0)..b(0), &mut ctx()).unwrap();
    settle(&mut e);
    let fresh = open(&e.text().unwrap(), 600.0, 300.0);
    assert_eq!(e.line_count(), fresh.line_count());
    assert_eq!(e.total_height(), fresh.total_height());
    // The first mark's text went, and the later one moved with its text.
    let moved = e.decorations(DecorationKind::FindMatch);
    assert_eq!(moved.len(), 1);
    let new_at = moved[0].start.0;
    assert_eq!(&e.text().unwrap()[new_at..new_at + 4], "0200");
}

/// Prints the time of a Replace All. Run it by hand:
/// `cargo test -p qe-editor --release --locked --test hooks measure_replace_all -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_replace_all_on_100_000_matches_in_100_mb() {
    let line =
        "the quick brown fox jumps over the lazy dog 0123456789 and then some more words go here\n";
    let lines = 100_000_000 / line.len();
    let text = line.repeat(lines);
    let mut e = open(&text, 800.0, 600.0);
    let step = lines / 100_000;
    let edits: Vec<_> = (0..100_000)
        .map(|i| {
            let at = i * step * line.len() + 4;
            edit(at, at + 5, "slow")
        })
        .collect();
    let snapshot = e.worker_snapshot();
    let started = Instant::now();
    let prepared = prepare_edits(snapshot.as_ref(), &edits, &NOT_CANCELLED).unwrap();
    println!(
        "prepare, on a worker: {:?}, {} replacements, {} MB in one edit",
        started.elapsed(),
        prepared.edit_count(),
        prepared.new_len() / 1_000_000
    );
    drop(snapshot);
    let mut context = ctx();
    let started = Instant::now();
    e.apply_edits(&prepared, b(0)..b(0), &mut context).unwrap();
    println!("apply, on the UI thread: {:?}", started.elapsed());
    let started = Instant::now();
    press(&mut e, 0x5a, true);
    println!("undo, on the UI thread: {:?}", started.elapsed());
}

// ---- Commands in order ----

#[test]
fn host_commands_wait_behind_a_waiting_key() {
    let mut e = cold(&"\u{4e2d}".repeat(400), 300.0, 600.0);
    e.test_pause_workers(true);
    press(&mut e, END, false);
    assert_eq!(e.queued_input_count(), 1);
    let mut context = ctx();
    e.run_host(HostCommand::PreventEditing(true), &mut context);
    e.run_host(
        HostCommand::Select {
            range: b(3)..b(3),
            reveal: true,
        },
        &mut context,
    );
    assert_eq!(e.queued_input_count(), 3);
    assert!(e.can_edit(), "it waits");
    assert_eq!(e.selection(), b(0)..b(0));
    e.test_pause_workers(false);
    settle(&mut e);
    assert_eq!(e.queued_input_count(), 0);
    assert!(!e.can_edit());
    assert_eq!(e.selection(), b(3)..b(3));
}

#[test]
fn each_host_command_does_its_job() {
    let text = wrapped_lines(100);
    let mut e = open(&text, 200.0, 300.0);
    let rows = e.total_height();
    let mut context = ctx();
    e.run_host(HostCommand::Wrap(false), &mut context);
    settle(&mut e);
    assert!(e.total_height() < rows, "wrap off");
    e.run_host(HostCommand::Wrap(true), &mut context);
    settle(&mut e);
    e.run_host(
        HostCommand::Font {
            family: "Consolas".into(),
            points: 24.0,
        },
        &mut context,
    );
    settle(&mut e);
    assert!(e.total_height() > rows, "a bigger font");
    e.run_host(
        HostCommand::Margins {
            left: Dip(10.0),
            right: Dip(0.0),
        },
        &mut context,
    );
    assert_eq!(e.text_area().x, Dip(10.0));
    e.run_host(HostCommand::ScrollTo(50.0), &mut context);
    assert_eq!(e.scroll_offset(), 50.0);
    e.run_host(
        HostCommand::PageWrap(Some(PageWrap {
            text_width: Dip(300.0),
            margin: Dip(0.0),
        })),
        &mut context,
    );
    settle(&mut e);
    let snapshot = e.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &[edit(0, 4, "abcd")], &NOT_CANCELLED).unwrap();
    e.run_host(
        HostCommand::ApplyEdits {
            edits: Arc::new(prepared.clone()),
            selection: b(1)..b(2),
        },
        &mut context,
    );
    assert_eq!(e.selection(), b(1)..b(2));
    // A failed command shows why, because no one is there to take a return value.
    e.run_host(
        HostCommand::ApplyEdits {
            edits: Arc::new(prepared),
            selection: b(0)..b(0),
        },
        &mut context,
    );
    assert!(e.warning().unwrap_or_default().contains("changed"));
    e.run_host(HostCommand::PreventEditing(true), &mut context);
    assert!(!e.can_edit());
}

#[test]
fn a_right_click_asks_for_the_context_menu_at_its_point() {
    let mut e = open("abc", 600.0, 200.0);
    let at = point(12.0, 5.0);
    let requests = send(
        &mut e,
        InputKind::MouseDown {
            at,
            button: MouseButton::Right,
            modifiers: Modifiers::default(),
        },
    );
    assert!(requests.contains(&ViewRequest::ContextMenu(at)));
    assert_eq!(e.selection(), b(0)..b(0), "it does not move the caret");
}

// ---- Save ----

#[test]
fn flush_input_puts_waiting_typing_in_the_text() {
    let mut e = cold(&"\u{4e2d}".repeat(400), 300.0, 600.0);
    e.test_pause_workers(true);
    press(&mut e, HOME, false);
    press(&mut e, END, false);
    send(&mut e, InputKind::Char('a'));
    assert!(e.queued_input_count() >= 2);
    assert!(!e.text().unwrap().contains('a'));
    e.flush_input(&mut ctx());
    assert_eq!(e.queued_input_count(), 0);
    let snapshot = e.worker_snapshot();
    let mut text = String::new();
    assert!(snapshot.visit_chunks(b(0)..snapshot.len_bytes(), &mut |s| text.push_str(s)));
    assert!(text.contains('a'), "the snapshot holds what was typed");
    e.test_pause_workers(false);
    // A key after a flush waits again, with its own wait.
    let mut e = cold(&"\u{4e2d}".repeat(400), 300.0, 600.0);
    e.test_pause_workers(true);
    press(&mut e, END, false);
    e.flush_input(&mut ctx());
    press(&mut e, END, false);
    assert_eq!(e.queued_input_count(), 1);
    e.poll_layout(&mut ctx());
    assert_eq!(
        e.queued_input_count(),
        1,
        "a flush left no old clock behind"
    );
    e.test_pause_workers(false);
}

#[test]
fn a_save_point_can_be_marked_and_an_edit_during_a_write_is_not_saved() {
    let mut e = open("abc", 600.0, 200.0);
    assert!(e.is_at_save_point());
    send(&mut e, InputKind::Char('x'));
    assert!(!e.is_at_save_point());
    e.mark_save_point();
    assert!(e.is_at_save_point());
    press(&mut e, 0x5a, true);
    assert!(!e.is_at_save_point(), "undo leaves the saved text");
    press(&mut e, 0x59, true);
    assert!(e.is_at_save_point(), "redo comes back to it");
    // The write took a snapshot of this revision, and a key came during the write.
    let written = e.revision();
    send(&mut e, InputKind::Char('y'));
    assert!(!e.mark_save_point_at(written));
    assert!(!e.is_at_save_point());
    let written = e.revision();
    assert!(e.mark_save_point_at(written));
    assert!(e.is_at_save_point());
}

struct NoClipboard;
impl Clipboard for NoClipboard {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn write(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Cut, Save, then Open quickly must not lose the cut text.
#[test]
fn adopt_waits_for_a_running_cut() {
    let text = "abc\n".repeat(1024 * 1024);
    let mut e = open(&text, 300.0, 100.0);
    e.set_clipboard(Box::new(NoClipboard));
    press(&mut e, 0x41, true);
    press(&mut e, 0x58, true);
    assert!(e.cut_is_running());
    e.mark_save_point();
    let error = e
        .adopt(LoadedFile::Editable {
            store: PieceStore::new("new"),
            ending: "\n",
        })
        .unwrap_err();
    assert!(error.contains("Cut"), "{error}");
    settle(&mut e);
    assert!(!e.cut_is_running());
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    })
    .unwrap();
}

// ---- The second review's findings ----

/// Finding 1. The written text is not the text at the old save point.
#[test]
fn save_completion_after_undo_invalidates_previous_saved_state() {
    let mut e = open("a", 600.0, 200.0);
    assert!(e.is_at_save_point());
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    let written = e.revision();
    assert!(!e.is_at_save_point());
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "a");
    assert!(
        e.is_at_save_point(),
        "before the write ends this is still true"
    );
    // The write of "xa" ends, so the disk no longer holds "a".
    assert!(!e.mark_save_point_at(written));
    assert!(!e.is_at_save_point(), "the disk holds other text");
    press(&mut e, 0x59, true);
    press(&mut e, 0x5a, true);
    assert!(
        !e.is_at_save_point(),
        "undo back to the old point is not saved"
    );
    let error = e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    });
    assert!(error.is_err(), "a file must not replace unsaved text");
    // A later full save is good again, and so is the new document.
    e.mark_save_point();
    assert!(e.is_at_save_point());
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    })
    .unwrap();
    assert!(e.is_at_save_point());
}

#[test]
fn a_later_edit_after_a_save_completion_then_undo_is_not_saved() {
    let mut e = open("a", 600.0, 200.0);
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    e.mark_save_point();
    let written = e.revision();
    e.replace_range(b(0)..b(0), "y", &mut ctx());
    assert!(!e.mark_save_point_at(written));
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "xa");
    // This text is what was written. The check errs on the side of asking to save.
    assert!(!e.is_at_save_point());
}

/// Finding 2.
#[test]
fn stale_edits_after_adopt_are_rejected() {
    let mut e = open("old text", 600.0, 200.0);
    let snapshot = e.worker_snapshot();
    let prepared =
        prepare_edits(snapshot.as_ref(), &[edit(0, 3, "REPLACED")], &NOT_CANCELLED).unwrap();
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new text"),
        ending: "\n",
    })
    .unwrap();
    let error = e
        .apply_edits(&prepared, b(0)..b(0), &mut ctx())
        .unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(e.text().unwrap(), "new text");
}

#[test]
fn edits_prepared_in_another_view_are_rejected() {
    let first = open("same text", 600.0, 200.0);
    let mut second = open("same text", 600.0, 200.0);
    let snapshot = first.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &[edit(0, 4, "X")], &NOT_CANCELLED).unwrap();
    assert!(
        second
            .apply_edits(&prepared, b(0)..b(0), &mut ctx())
            .is_err()
    );
    assert_eq!(second.text().unwrap(), "same text");
    // A snapshot from outside the view has no identity at all.
    let plain = PieceStore::new("same text");
    let prepared = prepare_edits(&plain, &[edit(0, 4, "X")], &NOT_CANCELLED).unwrap();
    assert!(
        second
            .apply_edits(&prepared, b(0)..b(0), &mut ctx())
            .is_err()
    );
}

#[test]
fn old_save_completion_does_not_save_a_new_document() {
    let mut e = open("old", 600.0, 200.0);
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    let old_revision = e.revision();
    e.mark_save_point();
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    })
    .unwrap();
    e.replace_range(b(0)..b(0), "y", &mut ctx());
    assert!(!e.mark_save_point_at(old_revision), "another document");
    assert!(!e.is_at_save_point());
    // And it did not spoil a true state in the new document.
    let mut e = open("old", 600.0, 200.0);
    let old_revision = e.revision();
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    })
    .unwrap();
    assert!(!e.mark_save_point_at(old_revision));
    assert!(e.is_at_save_point(), "the new document's own state stands");
    // Another view's number is refused too.
    let other = open("old", 600.0, 200.0);
    let mut e = open("old", 600.0, 200.0);
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    assert!(!e.mark_save_point_at(other.revision()));
    assert!(!e.is_at_save_point());
}

#[test]
fn old_change_cursor_requires_rescan_after_adopt() {
    let mut e = open("old", 600.0, 200.0);
    let cursor = e.revision();
    e.adopt(LoadedFile::Editable {
        store: PieceStore::new("new"),
        ending: "\n",
    })
    .unwrap();
    assert_eq!(e.changes_since(cursor), ChangesSince::Rescan);
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    assert_eq!(e.changes_since(cursor), ChangesSince::Rescan);
    let other = open("old", 600.0, 200.0);
    assert_eq!(e.changes_since(other.revision()), ChangesSince::Rescan);
}

#[test]
fn a_snapshot_carries_the_revision_of_its_view() {
    let mut e = open("abc", 600.0, 200.0);
    let snapshot = e.worker_snapshot();
    assert_eq!(snapshot.revision(), e.revision());
    assert_eq!(snapshot.len_bytes(), b(3));
    assert_eq!(snapshot.byte_to_utf16(b(2)), Some(Utf16Offset(2)));
    assert_eq!(snapshot.try_line_count(), Ok(1));
    let before = e.revision();
    e.replace_range(b(0)..b(0), "x", &mut ctx());
    assert_eq!(snapshot.revision(), before, "a snapshot does not change");
    assert_ne!(e.worker_snapshot().revision(), before);
    assert!(e.revision() > before, "numbers go up within a document");
}

/// Finding 3.
#[test]
fn preparation_rejects_end_inside_utf8_character() {
    let e = open("\u{1f600}abc", 600.0, 200.0);
    let snapshot = e.worker_snapshot();
    assert!(prepare_edits(snapshot.as_ref(), &[edit(0, 1, "x")], &NOT_CANCELLED).is_err());
    assert!(prepare_edits(snapshot.as_ref(), &[edit(1, 4, "x")], &NOT_CANCELLED).is_err());
    assert!(prepare_edits(snapshot.as_ref(), &[edit(5, 99, "x")], &NOT_CANCELLED).is_err());
    assert!(prepare_edits(snapshot.as_ref(), &[edit(0, 4, "x")], &NOT_CANCELLED).is_ok());
    assert!(prepare_edits(snapshot.as_ref(), &[edit(7, 7, "x")], &NOT_CANCELLED).is_ok());
}

#[test]
fn applying_a_failed_edit_reports_failure_and_leaves_the_selection() {
    let mut e = open("abc", 600.0, 200.0);
    e.select(b(1)..b(1), &mut ctx());
    // The history refuses this edit, because a group is open on the store.
    let snapshot = e.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &[edit(0, 1, "x")], &NOT_CANCELLED).unwrap();
    assert!(
        e.test_apply_failing(&prepared, b(2)..b(2), &mut ctx())
            .is_err()
    );
    assert_eq!(e.text().unwrap(), "abc");
    assert_eq!(e.selection(), b(1)..b(1));
    assert!(e.warning().is_some());
}

/// Finding 4.
#[test]
fn flushing_a_dropped_end_and_typing_keeps_the_warning() {
    let mut e = cold(&"\u{4e2d}".repeat(400), 300.0, 600.0);
    e.test_pause_workers(true);
    press(&mut e, END, false);
    send(&mut e, InputKind::Char('a'));
    e.flush_input(&mut ctx());
    assert!(e.text().unwrap().starts_with('a'), "End was dropped");
    let warning = e.warning().unwrap_or_default().to_string();
    assert!(warning.contains("Home or End"), "{warning:?}");
    e.test_pause_workers(false);
}

#[test]
fn a_timed_out_end_and_typing_keeps_the_warning_without_a_flush_too() {
    let mut e = cold(&"\u{4e2d}".repeat(400), 300.0, 600.0);
    e.test_pause_workers(true);
    press(&mut e, END, false);
    send(&mut e, InputKind::Char('a'));
    e.on_timer_at(Instant::now() + Duration::from_secs(3), &mut ctx());
    assert!(e.text().unwrap().starts_with('a'));
    let warning = e.warning().unwrap_or_default().to_string();
    assert!(warning.contains("Home or End"), "{warning:?}");
    e.test_pause_workers(false);
}

/// Finding 5.
#[test]
fn marks_survive_insertions_moving_back_toward_start() {
    let mut e = open("aa--------bb", 600.0, 200.0);
    let kind = DecorationKind::FindMatch;
    e.set_decorations(kind, vec![b(0)..b(2), b(10)..b(12)], &mut ctx());
    let hundred = "z".repeat(100);
    e.replace_range(b(5)..b(5), &hundred, &mut ctx());
    assert_eq!(e.decorations(kind), [b(0)..b(2), b(110)..b(112)]);
    e.replace_range(b(0)..b(0), "q", &mut ctx());
    assert_eq!(e.decorations(kind), [b(1)..b(3), b(111)..b(113)]);
}

#[test]
fn undo_keeps_unaffected_marks_after_reversing_edit_order() {
    let mut e = open("aa--------bb", 600.0, 200.0);
    let kind = DecorationKind::FindMatch;
    e.set_decorations(kind, vec![b(0)..b(2), b(10)..b(12)], &mut ctx());
    e.replace_range(b(5)..b(5), &"z".repeat(100), &mut ctx());
    e.replace_range(b(0)..b(0), "q", &mut ctx());
    press(&mut e, 0x5a, true);
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "aa--------bb");
    assert_eq!(e.decorations(kind), [b(0)..b(2), b(10)..b(12)]);
    press(&mut e, 0x59, true);
    assert_eq!(e.decorations(kind), [b(0)..b(2), b(110)..b(112)]);
}

/// A model of the list against many edits, in both directions and with both signs.
#[test]
fn marks_match_a_plain_model_through_seeded_edits() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    let mut e = open(&"m".repeat(2_000), 600.0, 200.0);
    let kind = DecorationKind::SpellingUnderline;
    let start: Vec<_> = (0..40).map(|i| i * 50..i * 50 + 10).collect();
    e.set_decorations(
        kind,
        start.iter().map(|r| b(r.start)..b(r.end)).collect(),
        &mut ctx(),
    );
    let mut model = start;
    let mut len = 2_000usize;
    for _ in 0..400 {
        let from = next() % (len + 1);
        let to = (from + next() % 30).min(len);
        let added = next() % 200;
        e.replace_range(b(from)..b(to), &"n".repeat(added), &mut ctx());
        model.retain(|r| {
            r.end <= from || r.start >= to || (from == to && !(r.start < from && from < r.end))
        });
        for r in &mut model {
            if r.start >= to {
                r.start = r.start + added - (to - from);
                r.end = r.end + added - (to - from);
            }
        }
        len = len + added - (to - from);
        let want: Vec<_> = model.iter().map(|r| b(r.start)..b(r.end)).collect();
        assert_eq!(e.decorations(kind), want);
    }
}

/// Finding 6: the word query uses one bounded window, and plain text uses none.
#[test]
fn a_word_query_on_plain_text_shapes_nothing() {
    let mut e = open("hello big world", 600.0, 200.0);
    let before = qe_layout::thread_format_count();
    assert_eq!(e.word_range_at(b(7)).unwrap(), b(6)..b(9));
    assert_eq!(qe_layout::thread_format_count(), before);
}

/// Prints and limits what the shaped window costs. It holds at most 2 KiB of text.
#[cfg(not(debug_assertions))]
#[test]
fn release_budget_a_word_query_on_shaped_text_has_a_bounded_cost() {
    for (name, text) in [
        ("Chinese", "\u{4e2d}\u{6587}".repeat(3_000)),
        ("marks", format!("a{}", "\u{301}".repeat(6_000))),
        ("flags", "\u{1f1fa}\u{1f1f8}".repeat(1_000)),
        ("emoji", "\u{1f600}".repeat(2_000)),
    ] {
        let mut e = open(&text, 600.0, 200.0);
        let mut samples = Vec::new();
        for i in 0..200 {
            let at = (text.len() / 200 * i..text.len())
                .find(|n| text.is_char_boundary(*n))
                .unwrap_or(0);
            let started = Instant::now();
            e.word_range_at(b(at)).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        let (p99, max) = (samples[197], samples[199]);
        eprintln!("word query, {name}: p99 {p99:.3} ms, max {max:.3} ms");
        assert!(p99 < 2.0, "{name}: p99 {p99:.3} ms");
    }
}

/// Finding 7.
#[test]
fn composition_after_ascii_to_shaped_threshold_stays_visible() {
    let mut e = open(&"a".repeat(10_000), 400.0, 300.0);
    send(&mut e, InputKind::Focus);
    e.select(b(9_000)..b(9_000), &mut ctx());
    settle(&mut e);
    ime(&mut e, "\u{4e2d}", 1);
    settle(&mut e);
    assert!(e.composition().unwrap().showing);
    let rects = e.composition_rects().unwrap();
    assert_eq!(rects.len(), 1, "{rects:?}");
    assert!(rects[0].rect.width.0 > 1.0);
    let caret = e.caret_rect().unwrap();
    assert!((0.0..300.0).contains(&caret.y.0), "{caret:?}");
    let list = draw(&e, ThemeKind::Light);
    assert!(glyph_count(&list) > 0);
    e.ime_commit("\u{4e2d}", &mut ctx());
    assert_eq!(e.text().unwrap().len(), 10_003);
}

/// Finding 8.
#[test]
fn composition_reflows_when_view_width_changes() {
    let mut e = open("abc", 600.0, 300.0);
    send(&mut e, InputKind::Focus);
    ime(&mut e, &"w".repeat(60), 60);
    settle(&mut e);
    assert!(e.caret_rect().unwrap().x.0 > 200.0);
    send(
        &mut e,
        InputKind::Resize {
            width: Dip(100.0),
            height: Dip(300.0),
        },
    );
    settle(&mut e);
    let caret = e.caret_rect().unwrap();
    assert!(caret.x.0 <= 100.0, "{caret:?}");
    assert!(
        e.composition_rects().unwrap().len() > 1,
        "it wraps at the new width"
    );
    // Its height follows, and goes back to the true height at its end.
    let tall = e.total_height();
    e.ime_cancel(&mut ctx());
    settle(&mut e);
    assert!(e.total_height() < tall);
    assert_eq!(e.total_height(), open("abc", 100.0, 300.0).total_height());
}

#[test]
fn a_font_change_reflows_a_composition_and_an_old_result_is_refused() {
    let mut e = open("abc", 600.0, 300.0);
    send(&mut e, InputKind::Focus);
    e.test_pause_workers(true);
    ime(&mut e, "wwww", 4);
    e.apply_font("Consolas", 24.0, &mut ctx()).unwrap();
    e.test_pause_workers(false);
    settle(&mut e);
    let rects = e.composition_rects().unwrap();
    assert!(rects[0].rect.width.0 > 40.0, "{rects:?}");
}

/// Finding 9.
#[test]
fn geometry_does_not_report_old_positions_during_composition() {
    let mut e = open("abc\nnext", 600.0, 300.0);
    let plain = e.range_rects(b(1)..b(2)).unwrap()[0].rect;
    ime(&mut e, "long prefix ", 0);
    settle(&mut e);
    let moved = e.range_rects(b(1)..b(2)).unwrap();
    assert_eq!(moved.len(), 1);
    assert!(moved[0].rect.x.0 > plain.x.0 + 50.0, "{moved:?}");
    assert_eq!(
        moved[0].range,
        b(1)..b(2),
        "it speaks of the document's bytes"
    );
    // A range over the composition gives both sides of it, and not the text in it.
    let across = e.range_rects(b(0)..b(3)).unwrap();
    assert_eq!(
        across.iter().map(|r| r.range.clone()).collect::<Vec<_>>(),
        [b(0)..b(3)]
    );
    // The byte under a point follows what is painted.
    let hit = e
        .byte_at_point(point(moved[0].rect.x.0 + 0.1, moved[0].rect.y.0 + 1.0))
        .unwrap();
    assert_eq!(hit.byte, b(1));
    let first = e.composition_rects().unwrap()[0].rect;
    let hit = e
        .byte_at_point(point(first.x.0 + 20.0, first.y.0 + 1.0))
        .unwrap();
    assert_eq!(
        hit.byte,
        b(0),
        "inside the composition is its place in the text"
    );
    // Rows give the document's bytes, and the next line stays where it is.
    let rows = e.visible_rows();
    assert_eq!(rows.rows[0].first_byte, b(0));
    assert_eq!(rows.rows[1].first_byte, b(4));
    assert_eq!(e.visible_range().unwrap(), b(0)..b(8));
}

#[test]
fn rows_of_a_composed_line_give_document_bytes() {
    let mut e = open("first\nsecond\nthird", 600.0, 300.0);
    e.select(b(9)..b(9), &mut ctx());
    ime(&mut e, "wwww", 4);
    settle(&mut e);
    let rows = e.visible_rows();
    let firsts: Vec<_> = rows.rows.iter().map(|r| r.first_byte).collect();
    assert_eq!(firsts, [b(0), b(6), b(13)]);
    assert_eq!(e.visible_range().unwrap(), b(0)..b(18));
    let hit = e.byte_at_point(point(0.1, rows.rows[1].y.0 + 1.0)).unwrap();
    assert_eq!(hit.byte, b(6));
}

#[test]
fn geometry_past_a_selection_that_gives_way_follows_the_composition() {
    let mut e = open("abcdef", 600.0, 300.0);
    e.select(b(1)..b(3), &mut ctx());
    let plain = e.range_rects(b(4)..b(5)).unwrap()[0].rect;
    ime(&mut e, "\u{4e2d}\u{6587}\u{5b57}", 0);
    settle(&mut e);
    let moved = e.range_rects(b(4)..b(5)).unwrap()[0].rect;
    assert!(moved.x.0 > plain.x.0, "{plain:?} {moved:?}");
    // The hidden selected text has no rectangle of its own.
    let hidden = e.range_rects(b(1)..b(3)).unwrap();
    assert!(hidden.iter().all(|r| r.range.is_empty()), "{hidden:?}");
}

/// Finding 10.
#[test]
fn page_wrap_reveals_composition_caret() {
    let mut e = open("abc", 100.0, 300.0);
    send(&mut e, InputKind::Focus);
    e.set_page_wrap(
        Some(PageWrap {
            text_width: Dip(600.0),
            margin: Dip(20.0),
        }),
        &mut ctx(),
    );
    settle(&mut e);
    ime(&mut e, &"w".repeat(60), 60);
    settle(&mut e);
    assert!(e.horizontal_offset() > 0.0);
    let caret = e.caret_rect().unwrap();
    assert!((0.0..=100.0).contains(&caret.x.0), "{caret:?}");
}

/// Finding 11.
#[test]
fn margins_clip_horizontally_scrolled_text() {
    let mut e = open(&"m".repeat(200), 600.0, 200.0);
    e.apply_wrap(false, &mut ctx()).unwrap();
    e.set_margins(Dip(40.0), Dip(100.0), &mut ctx());
    settle(&mut e);
    press(&mut e, END, false);
    settle(&mut e);
    assert!(e.horizontal_offset() > 0.0);
    let list = draw(&e, ThemeKind::Light);
    let area = e.text_area();
    let clip = list
        .commands
        .iter()
        .position(|c| *c == DrawCommand::PushClip(area));
    let glyph = list
        .commands
        .iter()
        .position(|c| matches!(c, DrawCommand::GlyphRun { .. }))
        .unwrap();
    let pop = list
        .commands
        .iter()
        .rposition(|c| *c == DrawCommand::PopClip);
    assert!(clip.is_some_and(|c| c < glyph), "no clip before the text");
    assert!(pop.is_some_and(|p| p > glyph), "the clip must end");
    // And in pixels: nothing paints in the reserved columns.
    let mut renderer = BitmapRenderer::new(600, 200).unwrap();
    renderer.set_dpi(96).unwrap();
    for command in &list.commands {
        if let DrawCommand::GlyphRun { font_id, .. } = command {
            renderer.register_font_face(*font_id, &qe_layout::font_face(*font_id).unwrap());
        }
    }
    let bitmap = renderer.render_to_bitmap(&list, 600, 200).unwrap();
    let pixel = |x: usize, y: usize| &bitmap.bgra[(y * 600 + x) * 4..(y * 600 + x) * 4 + 4];
    let outside = pixel(5, 5).to_vec();
    for y in 0..200 {
        for x in (0..40).chain(500..600) {
            assert_eq!(pixel(x, y), outside.as_slice(), "paint at {x}, {y}");
        }
    }
}

#[test]
fn with_no_margins_there_is_no_extra_clip() {
    let e = open("abc", 600.0, 200.0);
    let list = draw(&e, ThemeKind::Light);
    assert!(
        !list
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::PushClip(_)))
    );
}

/// Finding 12.
#[test]
fn a_failed_composition_layout_ends_its_wait_and_says_why() {
    let mut e = open("abc", 600.0, 300.0);
    ime(&mut e, "good", 4);
    settle(&mut e);
    assert!(e.composition().unwrap().showing);
    e.test_fail_composition_layout(true);
    ime(&mut e, "bad", 3);
    settle(&mut e);
    assert!(!e.pending_layout(), "nothing more will come");
    let warning = e.warning().unwrap_or_default().to_string();
    assert!(warning.contains("composition"), "{warning:?}");
    assert!(
        e.composition().unwrap().showing,
        "the last good layout stays"
    );
    e.test_fail_composition_layout(false);
    ime(&mut e, "again", 5);
    settle(&mut e);
    assert_eq!(e.composition().unwrap().text, "again");
}

/// Finding 13.
#[test]
fn mapped_revision_stays_fixed_while_index_grows_as_documented() {
    let path = std::env::temp_dir().join(format!("qe-hooks-{}-rev.txt", std::process::id()));
    std::fs::write(&path, "0123456789\n".repeat(400_000)).unwrap();
    let _remove = Remove(path.clone());
    let store = MappedStore::open(&path).unwrap();
    store.set_test_read_delay(Duration::from_millis(2));
    let mut e = EditorView::from_mapped(store).unwrap();
    e.layout(rect(800.0, 170.0), &measure());
    let first = e.revision();
    assert!(!e.store_status().complete);
    let until = Instant::now() + Duration::from_secs(60);
    while !e.store_status().complete && Instant::now() < until {
        e.poll_layout(&mut ctx());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(e.store_status().complete);
    assert_eq!(e.revision(), first);
    assert_eq!(e.changes_since(first), ChangesSince::Current);
    assert_eq!(e.worker_snapshot().revision(), first);
}

/// Finding 14: what HOOKS.md now says about groups.
#[test]
fn an_edit_group_is_one_change_at_each_edit_and_undo_makes_one() {
    let mut e = open("aa--------bb", 600.0, 200.0);
    let kind = DecorationKind::FindMatch;
    e.set_decorations(kind, vec![b(3)..b(5)], &mut ctx());
    let start = e.revision();
    e.begin_edit_group();
    e.replace_range(b(0)..b(2), "x", &mut ctx());
    e.replace_range(b(9)..b(11), "y", &mut ctx());
    e.end_edit_group();
    let ChangesSince::Changes(changes) = e.changes_since(start) else {
        panic!("no changes");
    };
    assert_eq!(changes.len(), 2, "each edit in the group is a change");
    assert_eq!(
        e.decorations(kind),
        [b(2)..b(4)],
        "a mark between them moves"
    );
    let done = e.revision();
    press(&mut e, 0x5a, true);
    let ChangesSince::Changes(changes) = e.changes_since(done) else {
        panic!("no changes");
    };
    assert_eq!(changes.len(), 1, "an undo of the group is one change");
    assert_eq!(
        e.decorations(kind),
        Vec::<std::ops::Range<ByteOffset>>::new()
    );
}

/// The uncertain concern on long lines: a line whose map has not arrived costs little.
#[test]
fn a_composition_before_the_long_map_copies_only_its_segment() {
    let text = "word ".repeat(4_000_000);
    let mut e = cold(&text, 400.0, 300.0);
    e.test_pause_workers(true);
    // The fastest of ten, so that a loaded PC does not fail this.
    let each = (0..10)
        .map(|_| {
            let started = Instant::now();
            ime(&mut e, "\u{4e2d}", 1);
            started.elapsed()
        })
        .min()
        .unwrap();
    assert!(
        each < Duration::from_millis(10),
        "{each:?} for a 20 MB line"
    );
    e.test_pause_workers(false);
}

// ---- Saved pictures ----

#[test]
fn saved_images_of_marks_and_composition() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images");
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        for (name, composed) in [("marks", false), ("composition", true)] {
            let mut e = open(
                "The quick brown fox jumps\nover the lazy dog again\nand speling is not right",
                420.0,
                90.0,
            );
            let mut context = ctx();
            send(&mut e, InputKind::Focus);
            e.set_decorations(
                DecorationKind::FindMatch,
                vec![b(4)..b(9), b(32)..b(35)],
                &mut context,
            );
            e.set_decorations(
                DecorationKind::CurrentFindMatch,
                vec![b(16)..b(19)],
                &mut context,
            );
            e.set_decorations(
                DecorationKind::SpellingUnderline,
                vec![b(54)..b(61)],
                &mut context,
            );
            e.set_decorations(
                DecorationKind::CompositionUnderline,
                vec![b(40)..b(44)],
                &mut context,
            );
            if composed {
                e.select(b(36)..b(36), &mut ctx());
                ime(&mut e, "\u{4e2d}\u{6587}", 1);
            }
            settle(&mut e);
            let list = draw(&e, kind);
            let mut renderer = BitmapRenderer::new(420, 90).unwrap();
            renderer
                .set_text_settings(TextSettings::reference())
                .unwrap();
            renderer.set_dpi(96).unwrap();
            for command in &list.commands {
                if let DrawCommand::GlyphRun { font_id, .. } = command {
                    renderer.register_font_face(*font_id, &qe_layout::font_face(*font_id).unwrap());
                }
            }
            let bitmap = renderer.render_to_bitmap(&list, 420, 90).unwrap();
            let file = path.join(format!("hooks-{name}-{kind:?}-96.bmp"));
            if std::env::var_os("QE_EDITOR_DUMP_IMAGES").is_some() {
                let mut bmp = Vec::with_capacity(54 + bitmap.bgra.len());
                bmp.extend_from_slice(b"BM");
                bmp.extend_from_slice(&((54 + bitmap.bgra.len()) as u32).to_le_bytes());
                bmp.extend_from_slice(&[0; 4]);
                bmp.extend_from_slice(&54u32.to_le_bytes());
                bmp.extend_from_slice(&40u32.to_le_bytes());
                bmp.extend_from_slice(&420i32.to_le_bytes());
                bmp.extend_from_slice(&(-90i32).to_le_bytes());
                bmp.extend_from_slice(&1u16.to_le_bytes());
                bmp.extend_from_slice(&32u16.to_le_bytes());
                bmp.extend_from_slice(&[0; 24]);
                bmp.extend_from_slice(&bitmap.bgra);
                std::fs::write(&file, bmp).unwrap();
            } else {
                let baseline = std::fs::read(&file).unwrap();
                assert_eq!(baseline.len(), 54 + bitmap.bgra.len(), "{name} {kind:?}");
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
                    "{changed} changed pixels in {name} {kind:?}"
                );
            }
        }
    }
}

// ---- A composition's text is never dropped on its own ----

#[test]
fn a_caret_move_commits_the_composition_and_lands_on_the_same_text() {
    let mut e = open("abcd", 600.0, 200.0);
    ime(&mut e, "xyz", 3);
    settle(&mut e);
    // Selects "cd" as the text stood: the commit moves it along by three bytes.
    let mut context = ctx();
    e.select(b(2)..b(4), &mut context);
    assert_eq!(e.text().unwrap(), "xyzabcd");
    assert!(e.composition().is_none());
    assert_eq!(e.selection(), b(5)..b(7), "still on cd");
    assert!(context.requests.contains(&ViewRequest::CancelComposition));
    assert_eq!(
        e.take_composition_end(),
        Some(CompositionEnd::Committed {
            revision: e.revision()
        })
    );
}

#[test]
fn an_edit_commits_the_composition_first_and_lands_on_the_same_text() {
    let mut e = open("abcd", 600.0, 200.0);
    let mut context = ctx();
    e.select(b(1)..b(1), &mut context);
    ime(&mut e, "\u{4e2d}", 1);
    // Replaces "cd" as the text stood.
    e.replace_range(b(2)..b(4), "Z", &mut context);
    assert_eq!(e.text().unwrap(), "a\u{4e2d}bZ");
    assert!(e.composition().is_none());
    // Each step comes back on its own: the edit, then the commit.
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "a\u{4e2d}bcd");
    press(&mut e, 0x5a, true);
    assert_eq!(e.text().unwrap(), "abcd");
}

#[test]
fn undo_during_a_composition_commits_it_first_and_takes_back_that_commit() {
    let mut e = open("ab", 600.0, 200.0);
    let mut context = ctx();
    e.replace_range(b(2)..b(2), "c", &mut context);
    e.select(b(0)..b(0), &mut context);
    ime(&mut e, "x", 1);
    press(&mut e, 0x5a, true);
    assert!(e.composition().is_none());
    assert_eq!(e.text().unwrap(), "abc", "the undo took back the commit");
    press(&mut e, 0x5a, true);
    assert_eq!(
        e.text().unwrap(),
        "ab",
        "and the next one the edit before it"
    );
}

#[test]
fn only_esc_or_an_explicit_cancel_throws_the_text_away() {
    let mut e = open("ab", 600.0, 200.0);
    ime(&mut e, "x", 1);
    press(&mut e, ESC, false);
    assert_eq!(e.text().unwrap(), "ab");
    assert_eq!(e.take_composition_end(), Some(CompositionEnd::Cancelled));
    assert_eq!(e.take_composition_end(), None, "it is told once");
    ime(&mut e, "y", 1);
    e.ime_cancel(&mut ctx());
    assert_eq!(e.text().unwrap(), "ab");
    assert_eq!(e.take_composition_end(), Some(CompositionEnd::Cancelled));
}

#[test]
fn a_click_commits_the_composition_even_at_the_same_place() {
    let mut e = open("abcd", 600.0, 200.0);
    ime(&mut e, "xy", 2);
    settle(&mut e);
    send(
        &mut e,
        InputKind::MouseDown {
            at: Point {
                x: Dip(0.0),
                y: Dip(1.0),
            },
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    );
    assert!(e.composition().is_none());
    assert_eq!(e.text().unwrap(), "xyabcd");
}

#[test]
fn prevent_editing_commits_a_composition_before_it_refuses_edits() {
    let mut e = open("ab", 600.0, 200.0);
    ime(&mut e, "x", 1);
    let mut context = ctx();
    e.run_host(HostCommand::PreventEditing(true), &mut context);
    assert_eq!(e.text().unwrap(), "xab");
    assert!(e.composition().is_none());
    assert!(!e.can_edit());
}

#[test]
fn a_replace_all_prepared_before_a_composition_is_refused_and_the_text_is_kept() {
    let mut e = open("abab", 600.0, 200.0);
    let edits = [TextEdit {
        range: b(0)..b(1),
        text: "Q".into(),
    }];
    let snapshot = e.worker_snapshot();
    let prepared = prepare_edits(snapshot.as_ref(), &edits, &AtomicBool::new(false)).unwrap();
    ime(&mut e, "x", 1);
    let answer = e.apply_edits(&prepared, b(0)..b(0), &mut ctx());
    assert!(answer.is_err());
    assert_eq!(
        e.text().unwrap(),
        "xabab",
        "the composition went in, and the old edits did not"
    );
}

// ---- Places inside a composition ----

fn composed(text: &str) -> EditorView {
    let mut e = open("first\nsecond", 600.0, 200.0);
    e.select(b(6)..b(6), &mut ctx());
    ime(&mut e, text, text.encode_utf16().count());
    settle(&mut e);
    e
}

#[test]
fn part_of_a_composition_has_its_own_rectangle() {
    let mut e = composed("\u{304b}\u{306a}\u{6f22}");
    let whole = e.composition_rects().unwrap()[0].rect;
    let first = e.composition_range_rects(0..1).unwrap()[0].rect;
    let last = e.composition_range_rects(2..3).unwrap()[0].rect;
    assert!(
        first.x.0 >= whole.x.0 && first.width.0 < whole.width.0,
        "{first:?} {whole:?}"
    );
    assert!(
        last.x.0 > first.x.0 + first.width.0 - 1.0,
        "{last:?} {first:?}"
    );
    let between = e.composition_range_rects(1..1).unwrap()[0].rect;
    assert!(
        (between.x.0 - (first.x.0 + first.width.0)).abs() < 1.0,
        "{between:?} {first:?}"
    );
    // Newer text waits for its layout, so the old rectangles are no answer.
    e.test_pause_workers(true);
    ime(&mut e, "\u{304b}", 1);
    assert_eq!(
        e.composition_range_rects(0..1),
        Err(GeometryError::NotLaidOut)
    );
    assert!(!e.composition().unwrap().current);
    e.test_pause_workers(false);
    settle(&mut e);
    assert!(e.composition().unwrap().current);
}

#[test]
fn part_of_a_composition_counts_surrogate_pairs_and_line_breaks_in_utf16() {
    let e = composed("\u{1f600}\r\nab");
    let emoji = e.composition_range_rects(0..2).unwrap()[0].rect;
    let a = e.composition_range_rects(4..5).unwrap()[0].rect;
    assert!(a.x.0 >= emoji.x.0 + emoji.width.0, "{a:?} after {emoji:?}");
}

#[test]
fn a_point_on_a_composition_answers_from_what_shows() {
    let e = composed("\u{304b}\u{306a}");
    let rect = e.composition_range_rects(1..2).unwrap()[0].rect;
    let on_second = Point {
        x: Dip(rect.x.0 + 1.0),
        y: Dip(rect.y.0 + rect.height.0 / 2.0),
    };
    assert_eq!(e.composition_place_at_point(on_second), Some(1));
    assert_eq!(
        e.byte_at_point(on_second).unwrap().byte,
        b(6),
        "where it would go"
    );
    // Past the composition's end, on its line: the store's text after it.
    let past = Point {
        x: Dip(rect.x.0 + rect.width.0 + 200.0),
        y: on_second.y,
    };
    assert_eq!(e.byte_at_point(past).unwrap().byte, b(12));
    // On the line above, nothing about the composition applies.
    let above = Point {
        x: Dip(1.0),
        y: Dip(rect.y.0 - 5.0),
    };
    assert_eq!(e.composition_place_at_point(above), None);
    assert_eq!(e.byte_at_point(above).unwrap().byte, b(0));
}

fn composition_lines(e: &EditorView) -> Vec<(f32, f32, f32)> {
    let theme = Theme::new(ThemeKind::Light);
    let mut list = DisplayList::default();
    e.draw(&mut list, &theme, &measure());
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Line {
                from, to, color, ..
            } if *color == theme.text => Some((from.x.0, to.x.0, from.y.0)),
            _ => None,
        })
        .collect()
}

#[test]
fn clauses_get_their_own_underlines_and_the_target_a_heavier_one() {
    let mut e = composed("\u{304b}\u{306a}\u{6f22}\u{5b57}");
    assert_eq!(
        composition_lines(&e).len(),
        1,
        "one line for all of it, with no clauses"
    );
    let clauses = vec![
        CompositionClause {
            range_utf16: 0..2,
            target: false,
        },
        CompositionClause {
            range_utf16: 2..4,
            target: true,
        },
    ];
    let mut context = ctx();
    e.ime_set_clauses(clauses.clone(), &mut context);
    assert_eq!(e.composition_clauses(), clauses);
    assert!(!context.requests.is_empty(), "the line repaints");
    let lines = composition_lines(&e);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[0].1 <= lines[1].0,
        "a gap between the clauses: {lines:?}"
    );
    assert_eq!(
        lines[1].0, lines[2].0,
        "the target's two lines are over each other"
    );
    assert!((lines[1].2 - lines[2].2).abs() == 1.0, "{lines:?}");
    // The same clauses again cost nothing.
    let mut context = ctx();
    e.ime_set_clauses(clauses, &mut context);
    assert!(context.requests.is_empty());
}

#[test]
fn select_directed_keeps_the_caret_at_the_end_asked_for() {
    let mut e = open("hello", 600.0, 200.0);
    e.select_directed(b(4), b(1), &mut ctx());
    assert_eq!(e.selection(), b(1)..b(4));
    assert_eq!(e.caret(), b(1));
}

#[test]
fn a_point_on_a_later_row_of_a_composition_that_wraps_is_on_the_composition() {
    let mut e = open("ab\ncdef", 120.0, 400.0);
    e.run_host(HostCommand::Wrap(true), &mut ctx());
    settle(&mut e);
    ime(&mut e, &"w".repeat(60), 60);
    settle(&mut e);
    let rows = e.composition_range_rects(0..60).unwrap();
    assert!(rows.len() >= 3, "the test needs rows: {}", rows.len());
    let last = rows[rows.len() - 1].rect;
    let hit = e
        .byte_at_point(Point {
            x: Dip(last.x.0 + 1.0),
            y: Dip(last.y.0 + 2.0),
        })
        .unwrap();
    assert!(
        hit.byte <= ByteOffset(2),
        "on the composition's line: {hit:?}"
    );
}

#[test]
fn a_point_on_a_composition_in_a_long_line_answers_from_what_shows() {
    let mut e = open(&"a".repeat(9_000), 800.0, 200.0);
    ime(&mut e, "wwwwwwww", 8);
    settle(&mut e);
    let rows = e.composition_range_rects(0..8).unwrap();
    let last = rows[rows.len() - 1].rect;
    let hit = e
        .byte_at_point(Point {
            x: Dip(last.x.0 + last.width.0 + 1.0),
            y: Dip(last.y.0 + 2.0),
        })
        .unwrap();
    assert!(
        hit.byte <= ByteOffset(1),
        "just past the composition: {hit:?}"
    );
}

#[test]
fn adopt_during_a_composition_commits_the_text_and_then_refuses() {
    let mut e = open("abc", 600.0, 200.0);
    ime(&mut e, "typed", 5);
    let answer = e.adopt(LoadedFile::Editable {
        store: qe_buffer::PieceStore::new("other doc"),
        ending: "\n",
    });
    assert!(answer.is_err());
    assert_eq!(e.text().unwrap(), "typedabc");
    assert!(e.composition().is_none());
    assert!(matches!(
        e.take_composition_end(),
        Some(qe_editor::CompositionEnd::Committed { .. })
    ));
}

#[test]
fn adopt_during_an_empty_composition_goes_ahead() {
    let mut e = open("abc", 600.0, 200.0);
    ime(&mut e, "", 0);
    e.adopt(LoadedFile::Editable {
        store: qe_buffer::PieceStore::new("other doc"),
        ending: "\n",
    })
    .unwrap();
    assert_eq!(e.text().unwrap(), "other doc");
}
