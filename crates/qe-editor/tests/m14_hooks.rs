//! The hooks M14 needs, through the real editor: typing and clipboard filters, caret stops,
//! and the two calls that scroll to an exact place.
use qe_api::{
    ByteOffset, Dip, InputEvent, InputKind, LineIndex, LookupError, Modifiers, MouseButton, Point,
    Rect, View, ViewContext,
};
use qe_editor::{Clipboard, ClipboardFilter, EditorCommand, EditorView, MemoryClipboard};
use qe_layout::DirectWriteLayout;
use std::{cell::RefCell, ops::Range, rc::Rc, time::Duration};

fn rect(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}
fn settle(e: &mut EditorView) {
    let mut context = ViewContext::default();
    e.poll_layout(&mut context);
    for _ in 0..2500 {
        if !e.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
        context.requests.clear();
        e.poll_layout(&mut context);
    }
    panic!("the view did not settle");
}
fn open(text: &str, width: f32, height: f32) -> EditorView {
    let mut e = EditorView::from_text(text).unwrap();
    e.layout(rect(width, height), &DirectWriteLayout::new().unwrap());
    settle(&mut e);
    e
}
fn send(e: &mut EditorView, time_us: u64, kind: InputKind) {
    e.handle(&InputEvent { time_us, kind }, &mut ViewContext::default());
}
fn type_text(e: &mut EditorView, text: &str) {
    for c in text.chars() {
        send(e, 0, InputKind::Char(c));
    }
}
fn ctrl_key(e: &mut EditorView, code: u16) {
    send(
        e,
        0,
        InputKind::KeyDown {
            virtual_key: code,
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
        },
    );
}
fn b(n: usize) -> ByteOffset {
    ByteOffset(n)
}
fn select(e: &mut EditorView, range: Range<usize>) {
    e.select(b(range.start)..b(range.end), &mut ViewContext::default());
}
fn run(e: &mut EditorView, command: EditorCommand) {
    e.run(command, &mut ViewContext::default());
}
/// Two presses of the left button at a point: a real double click, in order. Each comes
/// long after the one before, by a clock of its own, so that it is never a third click.
fn double_click(e: &mut EditorView, x: f32, y: f32) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CLOCK: AtomicU64 = AtomicU64::new(0);
    let start = CLOCK.fetch_add(100_000_000, Ordering::Relaxed) + 100_000_000;
    let at = Point {
        x: Dip(x),
        y: Dip(y),
    };
    for time in [start, start + 1_000] {
        send(
            e,
            time,
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        );
        send(
            e,
            time + 100,
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        );
    }
}

// ---- Typing ----

#[test]
fn a_typing_filter_sees_the_character_and_the_text_before_it() {
    let mut e = open("0123456789", 400.0, 300.0);
    select(&mut e, 10..10);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    e.set_typing_filter(Some(Box::new(move |typed, before| {
        log.borrow_mut()
            .push((typed.to_string(), before.to_string()));
        None
    })));
    type_text(&mut e, "x");
    assert_eq!(seen.borrow()[0], ("x".to_string(), "23456789".to_string()));
    assert_eq!(e.text().unwrap(), "0123456789x");
}

#[test]
fn text_before_the_caret_is_cut_back_to_a_character() {
    // Eight bytes back falls inside the first of these 3-byte characters.
    let text = "\u{4e2d}\u{6587}\u{5b57}";
    let mut e = open(text, 400.0, 300.0);
    select(&mut e, 9..9);
    let seen = Rc::new(RefCell::new(String::new()));
    let log = seen.clone();
    e.set_typing_filter(Some(Box::new(move |_, before| {
        *log.borrow_mut() = before.to_string();
        None
    })));
    type_text(&mut e, "x");
    assert_eq!(*seen.borrow(), "\u{6587}\u{5b57}");
}

#[test]
fn two_hyphens_become_one_dash_and_undo_gives_back_what_was_typed() {
    let mut e = open("", 400.0, 300.0);
    e.set_typing_filter(Some(Box::new(|typed, before| {
        (typed == "-" && before.ends_with('-')).then(|| (1, "\u{2014}".to_string()))
    })));
    type_text(&mut e, "a--b");
    assert_eq!(e.text().unwrap(), "a\u{2014}b");
    assert_eq!(e.caret(), b(5));
    // The dash starts an undo step that later typing continues, as typing over a selection
    // does. So an undo takes the dash and what followed it, and gives back the one hyphen.
    ctrl_key(&mut e, 0x5a);
    assert_eq!(e.text().unwrap(), "a-");
    ctrl_key(&mut e, 0x5a);
    assert_eq!(e.text().unwrap(), "");
}

#[test]
fn a_filtered_character_replaces_the_selection() {
    let mut e = open("say hello", 400.0, 300.0);
    select(&mut e, 4..9);
    e.set_typing_filter(Some(Box::new(|typed, before| {
        assert_eq!(before, "say ");
        (typed == "\"").then(|| (0, "\u{201c}".to_string()))
    })));
    type_text(&mut e, "\"");
    assert_eq!(e.text().unwrap(), "say \u{201c}");
}

#[test]
fn a_filter_that_asks_for_too_much_types_the_character_and_says_so() {
    let mut e = open("\u{4e2d}", 400.0, 300.0);
    select(&mut e, 3..3);
    e.set_typing_filter(Some(Box::new(|_, _| Some((1, "x".to_string())))));
    type_text(&mut e, "y");
    assert_eq!(e.text().unwrap(), "\u{4e2d}y", "no text is cut in half");
    assert!(e.warning().is_some_and(|w| w.contains("substitution")));
    e.set_typing_filter(Some(Box::new(|_, _| Some((99, "x".to_string())))));
    type_text(&mut e, "z");
    assert_eq!(e.text().unwrap(), "\u{4e2d}yz");
}

#[test]
fn a_read_only_view_never_asks_the_filter() {
    let mut e = open("abc", 400.0, 300.0);
    e.set_prevent_editing(true);
    e.set_typing_filter(Some(Box::new(|_, _| panic!("no edit, so no filter"))));
    type_text(&mut e, "x");
    assert_eq!(e.text().unwrap(), "abc");
    assert_eq!(e.refused_edits(), 1);
}

#[test]
fn the_filter_comes_off_with_none() {
    let mut e = open("", 400.0, 300.0);
    e.set_typing_filter(Some(Box::new(|_, _| Some((0, "?".to_string())))));
    type_text(&mut e, "a");
    e.set_typing_filter(None);
    type_text(&mut e, "b");
    assert_eq!(e.text().unwrap(), "?b");
}

#[test]
fn a_filter_sees_composition_text_that_a_key_commits_first() {
    let mut e = open("", 400.0, 300.0);
    let seen = Rc::new(RefCell::new(String::new()));
    let log = seen.clone();
    e.set_typing_filter(Some(Box::new(move |_, before| {
        *log.borrow_mut() = before.to_string();
        None
    })));
    let mut context = ViewContext::default();
    e.ime_update("\u{4e2d}", 1, &mut context);
    send(&mut e, 0, InputKind::Char('x'));
    assert_eq!(*seen.borrow(), "\u{4e2d}");
    assert_eq!(e.text().unwrap(), "\u{4e2d}x");
}

// ---- Word selection ----

#[test]
fn a_double_click_selects_by_word_and_any_other_selection_does_not() {
    let mut e = open("hello big world", 400.0, 300.0);
    assert!(!e.selection_is_word());
    double_click(&mut e, 2.0, 5.0);
    assert_eq!(e.selection(), b(0)..b(5));
    assert!(e.selection_is_word());
    // The same bytes selected another way are not a word selection.
    select(&mut e, 0..5);
    assert!(!e.selection_is_word());
    double_click(&mut e, 2.0, 5.0);
    assert!(e.selection_is_word());
    type_text(&mut e, "x");
    assert!(!e.selection_is_word(), "an edit ends it");
    run(&mut e, EditorCommand::SelectAll);
    assert!(!e.selection_is_word());
}

// ---- Clipboard ----

#[derive(Clone, Debug, PartialEq)]
enum Call {
    Cut(Range<ByteOffset>, Option<char>, Option<char>, bool),
    Paste(String, Option<char>, Option<char>, bool),
}
#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<Call>>>);
impl ClipboardFilter for Recorder {
    fn cut_range(
        &mut self,
        selected: Range<ByteOffset>,
        before: Option<char>,
        after: Option<char>,
        was_word: bool,
    ) -> Range<ByteOffset> {
        self.0
            .borrow_mut()
            .push(Call::Cut(selected.clone(), before, after, was_word));
        // Takes the space after a word, as Smart Copy does.
        if was_word && after == Some(' ') {
            selected.start..ByteOffset(selected.end.0 + 1)
        } else {
            selected
        }
    }
    fn paste_text(
        &mut self,
        text: &str,
        before: Option<char>,
        after: Option<char>,
        was_word: bool,
    ) -> String {
        self.0
            .borrow_mut()
            .push(Call::Paste(text.to_string(), before, after, was_word));
        if was_word {
            format!("[{text}]")
        } else {
            text.to_string()
        }
    }
}
struct Shared(Rc<RefCell<String>>);
impl Clipboard for Shared {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(Some(self.0.borrow().clone()))
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        *self.0.borrow_mut() = value.to_string();
        Ok(())
    }
}
fn with_filter(text: &str) -> (EditorView, Recorder, Rc<RefCell<String>>) {
    let mut e = open(text, 400.0, 300.0);
    let recorder = Recorder::default();
    e.set_clipboard_filter(Some(Box::new(recorder.clone())));
    let clipboard = Rc::new(RefCell::new(String::new()));
    e.set_clipboard(Box::new(Shared(clipboard.clone())));
    (e, recorder, clipboard)
}

#[test]
fn cut_of_a_word_removes_what_the_filter_says_and_copies_only_the_word() {
    let (mut e, recorder, clipboard) = with_filter("hello big world");
    double_click(&mut e, 2.0, 5.0);
    run(&mut e, EditorCommand::Cut);
    assert_eq!(*clipboard.borrow(), "hello");
    assert_eq!(e.text().unwrap(), "big world");
    assert_eq!(
        recorder.0.borrow()[0],
        Call::Cut(b(0)..b(5), None, Some(' '), true)
    );
    // The whole Cut is one undo step.
    ctrl_key(&mut e, 0x5a);
    assert_eq!(e.text().unwrap(), "hello big world");
}

#[test]
fn delete_uses_the_filter_and_a_plain_selection_says_it_was_not_a_word() {
    for command in [
        EditorCommand::DeleteBackward,
        EditorCommand::DeleteForward,
        EditorCommand::DeleteWordBackward,
        EditorCommand::DeleteWordForward,
    ] {
        let (mut e, _, _) = with_filter("a\u{4e2d}b c");
        double_click(&mut e, 2.0, 5.0);
        assert!(e.selection_is_word(), "{command:?}");
        run(&mut e, command);
        assert_eq!(e.text().unwrap(), "c", "{command:?}");
        let (mut e, recorder_2, _) = with_filter("a\u{4e2d}b c");
        select(&mut e, 1..4);
        run(&mut e, command);
        assert_eq!(e.text().unwrap(), "ab c", "{command:?}");
        assert_eq!(
            recorder_2.0.borrow()[0],
            Call::Cut(b(1)..b(4), Some('a'), Some('b'), false),
            "characters next to the range, whole"
        );
    }
}

#[test]
fn a_delete_with_no_selection_never_asks_the_filter() {
    let (mut e, recorder, _) = with_filter("abc");
    select(&mut e, 3..3);
    run(&mut e, EditorCommand::DeleteBackward);
    assert_eq!(e.text().unwrap(), "ab");
    assert!(recorder.0.borrow().is_empty());
}

#[test]
fn paste_after_a_word_copy_says_it_was_a_word() {
    let (mut e, recorder, _) = with_filter("hello big");
    double_click(&mut e, 2.0, 5.0);
    run(&mut e, EditorCommand::Copy);
    select(&mut e, 5..5);
    run(&mut e, EditorCommand::Paste);
    assert_eq!(e.text().unwrap(), "hello[hello] big");
    assert_eq!(
        recorder.0.borrow()[0],
        Call::Paste("hello".to_string(), Some('o'), Some(' '), true)
    );
}

#[test]
fn paste_of_text_from_elsewhere_or_a_plain_copy_is_not_a_word() {
    let (mut e, recorder, clipboard) = with_filter("hello big");
    double_click(&mut e, 2.0, 5.0);
    run(&mut e, EditorCommand::Copy);
    // Another app put other text on the clipboard.
    *clipboard.borrow_mut() = "other".to_string();
    select(&mut e, 0..0);
    run(&mut e, EditorCommand::Paste);
    assert_eq!(e.text().unwrap(), "otherhello big");
    assert_eq!(
        recorder.0.borrow()[0],
        Call::Paste("other".to_string(), None, Some('h'), false)
    );
    // A later Copy that is not of a word ends the memory of the word.
    select(&mut e, 0..5);
    run(&mut e, EditorCommand::Copy);
    select(&mut e, 0..0);
    run(&mut e, EditorCommand::Paste);
    assert_eq!(
        recorder.0.borrow()[1],
        Call::Paste("other".to_string(), None, Some('o'), false)
    );
}

#[test]
fn a_filter_that_gives_a_bad_range_changes_no_text_and_says_so() {
    struct Bad;
    impl ClipboardFilter for Bad {
        fn cut_range(
            &mut self,
            _: Range<ByteOffset>,
            _: Option<char>,
            _: Option<char>,
            _: bool,
        ) -> Range<ByteOffset> {
            b(1)..b(99)
        }
        fn paste_text(&mut self, text: &str, _: Option<char>, _: Option<char>, _: bool) -> String {
            text.to_string()
        }
    }
    let mut e = open("abc", 400.0, 300.0);
    e.set_clipboard_filter(Some(Box::new(Bad)));
    select(&mut e, 0..1);
    run(&mut e, EditorCommand::DeleteForward);
    assert_eq!(e.text().unwrap(), "abc");
    assert!(e.warning().is_some_and(|w| w.contains("edit failed")));
}

#[test]
fn no_filter_cuts_and_pastes_as_before() {
    let mut e = open("hello big", 400.0, 300.0);
    e.set_clipboard(Box::new(MemoryClipboard(String::new())));
    double_click(&mut e, 2.0, 5.0);
    run(&mut e, EditorCommand::Cut);
    assert_eq!(e.text().unwrap(), " big");
    run(&mut e, EditorCommand::Paste);
    assert_eq!(e.text().unwrap(), "hello big");
}

// ---- Caret stops ----

#[test]
fn caret_stops_step_over_whole_clusters_and_line_endings() {
    // A flag, a family joined by ZWJ, CRLF, and a combining mark.
    let flag = "\u{1f1fa}\u{1f1f8}";
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    let mark = "e\u{301}";
    let text = format!("{flag}{family}\r\n{mark}x");
    let mut e = open(&text, 400.0, 300.0);
    let a = flag.len();
    let c = a + family.len();
    let d = c + 2;
    let f = d + mark.len();
    let g = f + 1;
    let stops = [0, a, c, d, f, g];
    for pair in stops.windows(2) {
        assert_eq!(e.caret_stop_after(b(pair[0])), Ok(Some(b(pair[1]))));
        assert_eq!(e.caret_stop_before(b(pair[1])), Ok(Some(b(pair[0]))));
    }
    assert_eq!(e.caret_stop_after(b(g)), Ok(None));
    assert_eq!(e.caret_stop_before(b(0)), Ok(None));
    // They agree with the keys, which walk the same stops.
    select(&mut e, 0..0);
    for pair in stops.windows(2) {
        send(
            &mut e,
            0,
            InputKind::KeyDown {
                virtual_key: 0x27,
                modifiers: Modifiers::default(),
                repeat: false,
            },
        );
        assert_eq!(e.caret(), b(pair[1]));
    }
}

#[test]
fn a_caret_stop_inside_a_character_or_past_the_end_is_an_error() {
    let mut e = open("\u{4e2d}", 400.0, 300.0);
    assert_eq!(e.caret_stop_after(b(1)), Err(LookupError::OutOfRange));
    assert_eq!(e.caret_stop_before(b(1)), Err(LookupError::OutOfRange));
    assert_eq!(e.caret_stop_after(b(99)), Err(LookupError::OutOfRange));
}

// ---- Scrolling ----

/// Lines that wrap to many rows up to `tall`, and to one row after it.
fn uneven_lines(tall: usize, short: usize) -> String {
    let mut text = String::new();
    for i in 0..tall {
        text += &format!("{i:04} {}\n", "word ".repeat(60));
    }
    for i in 0..short {
        text += &format!("{:04} short\n", i + tall);
    }
    text
}
fn top_row(e: &EditorView) -> (usize, bool, f32) {
    let rows = e.visible_rows();
    let first = rows.rows.first().expect("a row shows");
    (first.line.0, first.continuation, first.y.0)
}

#[test]
fn a_line_goes_to_its_exact_place_however_lines_above_it_wrap() {
    let text = uneven_lines(400, 2_000);
    let mut e = open(&text, 300.0, 200.0);
    for line in [1_500usize, 700, 2_100, 3, 405] {
        e.set_top_line(line as f64, &mut ViewContext::default());
        settle(&mut e);
        let (top, continuation, y) = top_row(&e);
        assert_eq!(top, line);
        assert!(!continuation);
        assert_eq!(y, 0.0, "line {line} starts at the top of the view");
    }
    // The old way, a share of the height, lands elsewhere on this text. This is why the call
    // exists: the height the lines above take is not their share of the lines.
    let share = 1_500.0 / 2_400.0;
    e.set_scroll_position(share * e.max_scroll_position(), &mut ViewContext::default());
    settle(&mut e);
    assert_ne!(top_row(&e).0, 1_500);
}

#[test]
fn a_line_stays_at_the_top_as_layouts_above_it_arrive() {
    let text = uneven_lines(400, 2_000);
    let mut e = open(&text, 300.0, 200.0);
    e.set_top_line(1_000.0, &mut ViewContext::default());
    settle(&mut e);
    // Wheel up into the tall lines, which changes heights above, and come back by line.
    send(
        &mut e,
        0,
        InputKind::Wheel {
            at: Point {
                x: Dip(5.0),
                y: Dip(5.0),
            },
            delta_x: 0.0,
            delta_y: 3.0,
            modifiers: Modifiers::default(),
        },
    );
    e.set_top_line(399.0, &mut ViewContext::default());
    settle(&mut e);
    e.set_top_line(1_000.0, &mut ViewContext::default());
    settle(&mut e);
    assert_eq!(top_row(&e).0, 1_000);
}

#[test]
fn a_fraction_of_a_line_scrolls_inside_it_and_bad_values_stay_in_range() {
    let text = uneven_lines(5, 200);
    let mut e = open(&text, 300.0, 200.0);
    e.set_top_line(2.5, &mut ViewContext::default());
    settle(&mut e);
    let (top, continuation, _) = top_row(&e);
    assert_eq!(top, 2);
    assert!(continuation, "half way down a line of many rows");
    e.set_top_line(f64::NAN, &mut ViewContext::default());
    assert_eq!(e.scroll_offset(), 0.0);
    e.set_top_line(-4.0, &mut ViewContext::default());
    assert_eq!(e.scroll_offset(), 0.0);
    e.set_top_line(1e12, &mut ViewContext::default());
    assert_eq!(e.scroll_offset(), e.max_scroll_position());
}

#[test]
fn reveal_range_puts_the_start_at_the_top_and_the_selection_stays() {
    let text = uneven_lines(100, 1_000);
    let mut e = open(&text, 300.0, 200.0);
    select(&mut e, 2..4);
    let line = e.line_range(LineIndex(600)).unwrap();
    e.reveal_range(line.clone(), true, &mut ViewContext::default())
        .unwrap();
    settle(&mut e);
    assert_eq!(top_row(&e).0, 600);
    assert_eq!(top_row(&e).2, 0.0);
    assert_eq!(e.selection(), b(2)..b(4));
}

#[test]
fn reveal_range_with_no_layout_yet_lands_exactly_once_the_layout_arrives() {
    // The lines above the target are tall and were never laid out, so the first place is
    // an estimate. Each new layout refines it.
    let text = uneven_lines(600, 200);
    let mut e = open(&text, 300.0, 200.0);
    let line = e.line_range(LineIndex(500)).unwrap();
    e.reveal_range(line, true, &mut ViewContext::default())
        .unwrap();
    settle(&mut e);
    let (top, continuation, y) = top_row(&e);
    assert_eq!((top, continuation, y), (500, false, 0.0));
}

#[test]
fn a_place_inside_a_line_with_no_layout_lands_once_the_layout_arrives() {
    // Line 500 wraps to many rows and has no layout, so only its top is known at first.
    let text = uneven_lines(600, 200);
    let mut e = open(&text, 300.0, 200.0);
    let line = e.line_range(LineIndex(500)).unwrap();
    let inside = ByteOffset(line.start.0 + 200);
    e.reveal_range(inside..inside, true, &mut ViewContext::default())
        .unwrap();
    settle(&mut e);
    let rows = e.visible_rows();
    // The row above can show as a sliver a rounding error high, so look for the byte's row.
    let at = rows.rows.partition_point(|row| row.first_byte <= inside) - 1;
    let row = &rows.rows[at];
    assert_eq!(row.line.0, 500);
    assert!(row.continuation, "a row far down the line");
    assert_eq!(row.y.0, 0.0);
}

#[test]
fn the_end_of_a_line_with_no_layout_lands_at_the_bottom_once_the_layout_arrives() {
    let text = uneven_lines(600, 200);
    let mut e = open(&text, 300.0, 200.0);
    let line = e.line_range(LineIndex(500)).unwrap();
    let end = ByteOffset(line.end.0 - 1);
    e.reveal_range(line.start..end, false, &mut ViewContext::default())
        .unwrap();
    settle(&mut e);
    let rows = e.visible_rows();
    let last = rows.rows.last().unwrap();
    assert_eq!(last.line.0, 500);
    assert!(last.first_byte < end);
    assert!((last.y.0 + last.height.0 - 200.0).abs() < 0.5);
}

#[test]
fn reveal_range_puts_the_end_at_the_bottom() {
    let text = uneven_lines(0, 1_000);
    let mut e = open(&text, 300.0, 200.0);
    let line = e.line_range(LineIndex(500)).unwrap();
    let end = ByteOffset(line.end.0 - 1);
    e.reveal_range(line.start..end, false, &mut ViewContext::default())
        .unwrap();
    settle(&mut e);
    let rows = e.visible_rows();
    let last = rows.rows.last().unwrap();
    assert_eq!(last.line.0, 500);
    let bottom = last.y.0 + last.height.0;
    assert!((bottom - 200.0).abs() < 0.5, "bottom {bottom}");
}

#[test]
fn reveal_range_refuses_a_place_that_is_not_in_the_text() {
    let mut e = open("\u{4e2d}abc", 300.0, 200.0);
    let scroll = e.scroll_offset();
    assert!(
        e.reveal_range(b(1)..b(3), true, &mut ViewContext::default())
            .is_err()
    );
    assert!(
        e.reveal_range(b(3)..b(99), false, &mut ViewContext::default())
            .is_err()
    );
    assert_eq!(e.scroll_offset(), scroll);
}

#[test]
fn a_wheel_turn_ends_the_wait_of_a_reveal() {
    let text = uneven_lines(600, 200);
    let mut e = open(&text, 300.0, 200.0);
    let line = e.line_range(LineIndex(500)).unwrap();
    e.test_pause_workers(true);
    e.reveal_range(line, true, &mut ViewContext::default())
        .unwrap();
    send(
        &mut e,
        0,
        InputKind::Wheel {
            at: Point {
                x: Dip(5.0),
                y: Dip(5.0),
            },
            delta_x: 0.0,
            delta_y: 10.0,
            modifiers: Modifiers::default(),
        },
    );
    let after_wheel = e.scroll_offset();
    e.test_pause_workers(false);
    settle(&mut e);
    // The arrival of layouts moves the place only by what they change above it, and it does
    // not go back to line 500.
    assert_ne!(top_row(&e).0, 500);
    assert!(after_wheel > 0.0);
}

#[test]
fn a_hold_refuses_every_edit_and_lifting_it_gives_edits_back() {
    let mut e = EditorView::from_text("abc").unwrap();
    let mut ctx = ViewContext::default();
    e.set_hold_edits(true);
    assert!(!e.can_edit());
    type_text(&mut e, "x");
    e.run(EditorCommand::DeleteBackward, &mut ctx);
    e.replace_range(ByteOffset(0)..ByteOffset(1), "z", &mut ctx);
    assert!(e.refused_edits() >= 2);
    assert_eq!(e.worker_snapshot().len_bytes().0, 3);
    e.set_hold_edits(false);
    assert!(e.can_edit());
    e.replace_range(ByteOffset(0)..ByteOffset(1), "z", &mut ctx);
    assert_eq!(e.worker_snapshot().len_bytes().0, 3);
}

#[test]
fn a_host_that_runs_command_keys_leaves_the_view_only_the_moving_and_editing_keys() {
    let mut e = EditorView::from_text("abc").unwrap();
    let mut ctx = ViewContext::default();
    e.run(EditorCommand::SelectAll, &mut ctx);
    e.run(EditorCommand::DeleteForward, &mut ctx);
    assert_eq!(e.worker_snapshot().len_bytes().0, 0);
    e.set_host_runs_command_keys(true);
    ctrl_key(&mut e, 0x5a); // Z
    assert_eq!(e.worker_snapshot().len_bytes().0, 0, "the view's own Undo");
    e.set_host_runs_command_keys(false);
    ctrl_key(&mut e, 0x5a);
    assert_eq!(
        e.worker_snapshot().len_bytes().0,
        3,
        "a view on its own keeps them"
    );
}
