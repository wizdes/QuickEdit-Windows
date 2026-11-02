#[allow(unused_imports)]
use qe_api::Layout as _;
use qe_api::*;
use qe_editor::*;
use qe_layout::DirectWriteLayout;
use std::{cell::RefCell, rc::Rc};

fn r(w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(w),
        height: Dip(h),
    }
}
fn settle(e: &mut EditorView) {
    let mut c = ViewContext::default();
    e.poll_layout(&mut c);
    for _ in 0..2500 {
        if !e.pending_layout() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        e.poll_layout(&mut c);
    }
    assert!(!e.pending_layout(), "layout never settled");
}
fn open(t: &str, w: f32, h: f32) -> EditorView {
    let mut e = EditorView::from_text(t).unwrap();
    e.layout(r(w, h), &DirectWriteLayout::new().unwrap());
    settle(&mut e);
    e
}
fn ev(kind: InputKind, t: u64) -> InputEvent {
    InputEvent { time_us: t, kind }
}
fn keym(e: &mut EditorView, code: u16, m: Modifiers) -> Vec<ViewRequest> {
    let mut c = ViewContext::default();
    e.handle(
        &ev(
            InputKind::KeyDown {
                virtual_key: code,
                modifiers: m,
                repeat: false,
            },
            0,
        ),
        &mut c,
    );
    c.requests
}
fn key(e: &mut EditorView, code: u16) -> Vec<ViewRequest> {
    keym(e, code, Modifiers::default())
}
fn ctrl(e: &mut EditorView, code: u16) -> Vec<ViewRequest> {
    keym(
        e,
        code,
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    )
}
fn ch(e: &mut EditorView, c: char) {
    e.handle(&ev(InputKind::Char(c), 0), &mut ViewContext::default());
}
fn click(e: &mut EditorView, x: f32, y: f32, t: u64) {
    let at = Point {
        x: Dip(x),
        y: Dip(y),
    };
    let mut c = ViewContext::default();
    e.handle(
        &ev(
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            t,
        ),
        &mut c,
    );
    e.handle(
        &ev(
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            t,
        ),
        &mut c,
    );
}
fn wheel(e: &mut EditorView, dy: f32) {
    e.handle(
        &ev(
            InputKind::Wheel {
                at: Point {
                    x: Dip(1.0),
                    y: Dip(1.0),
                },
                delta_x: 0.0,
                delta_y: dy,
                modifiers: Modifiers::default(),
            },
            0,
        ),
        &mut ViewContext::default(),
    );
}
fn focus(e: &mut EditorView) {
    e.handle(&ev(InputKind::Focus, 0), &mut ViewContext::default());
}
fn list(e: &EditorView) -> DisplayList {
    let mut l = DisplayList::default();
    e.draw(
        &mut l,
        &Theme::new(ThemeKind::Light),
        &DirectWriteLayout::new().unwrap(),
    );
    l
}
fn runs(e: &EditorView) -> Vec<(i32, Vec<u16>)> {
    list(e)
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::GlyphRun { at, glyph_ids, .. } => {
                Some(((at.y.0 * 10.0) as i32, glyph_ids.clone()))
            }
            _ => None,
        })
        .collect()
}
fn caret_drawn(e: &EditorView) -> bool {
    let th = Theme::new(ThemeKind::Light);
    list(e)
        .commands
        .iter()
        .any(|c| matches!(c, DrawCommand::FillRect { color, .. } if *color == th.text))
}
fn covers(reqs: &[ViewRequest], y: f32) -> bool {
    reqs.iter()
        .any(|q| matches!(q, ViewRequest::Invalidate(b) if b.y.0 <= y && y < b.y.0 + b.height.0))
}
#[derive(Clone)]
struct Clip(Rc<RefCell<String>>);
impl Clipboard for Clip {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(Some(self.0.borrow().clone()))
    }
    fn write(&mut self, v: &str) -> Result<(), String> {
        *self.0.borrow_mut() = v.into();
        Ok(())
    }
}

const UP: u16 = 0x26;
const DOWN: u16 = 0x28;
const LEFT: u16 = 0x25;
const RIGHT: u16 = 0x27;
const HOME: u16 = 0x24;
const END: u16 = 0x23;
const PGUP: u16 = 0x21;
const BS: u16 = 0x08;
const DELK: u16 = 0x2e;
const ENTER: u16 = 0x0d;

#[test]
fn t01_up_arrow_leaves_the_line() {
    let mut e = open("one\ntwo\nthree", 300.0, 200.0);
    ctrl(&mut e, END);
    assert_eq!(e.caret().0, 13);
    key(&mut e, UP);
    eprintln!("caret after Up = {}", e.caret().0);
    assert!(
        e.caret().0 < 8,
        "caret stayed on the last line at {}",
        e.caret().0
    );
}
#[test]
fn t02_page_up_moves() {
    let t = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
    let mut e = open(&t, 200.0, 85.0);
    ctrl(&mut e, END);
    settle(&mut e);
    key(&mut e, LEFT);
    settle(&mut e);
    let at = e.caret().0;
    key(&mut e, PGUP);
    eprintln!("before {at} after {}", e.caret().0);
    assert!(e.caret().0 + 20 < at, "page up moved to {}", e.caret().0);
}
#[test]
fn t03_enter_at_bottom_reveals_caret() {
    let mut e = open("", 200.0, 51.0);
    for _ in 0..10 {
        key(&mut e, ENTER);
        settle(&mut e);
    }
    eprintln!("scroll {}", e.scroll_offset());
    assert!(
        e.scroll_offset() > 0.0,
        "caret is on line 11 and the view did not move"
    );
}
#[test]
fn t04_typing_after_wheel_reveals_caret() {
    let t = "x\n".repeat(1000);
    let mut e = open(&t, 200.0, 51.0);
    wheel(&mut e, -12000.0);
    settle(&mut e);
    let s = e.scroll_offset();
    assert!(s > 0.0);
    ch(&mut e, 'q');
    assert!(
        e.scroll_offset() < s,
        "typed at line 0 while view stays at {s}"
    );
}
#[test]
fn t05_undo_keeps_wrapped_rows_apart() {
    let t = "abcd efgh ijkl mnop qrst uvwx yz01 2345 6789\n".repeat(6);
    let mut e = open(&t, 60.0, 600.0);
    let before = runs(&e);
    ch(&mut e, 'x');
    settle(&mut e);
    ctrl(&mut e, 0x5a);
    settle(&mut e);
    let after = runs(&e);
    let ys = |v: &Vec<(i32, Vec<u16>)>| {
        let mut y: Vec<i32> = v.iter().map(|a| a.0).collect();
        y.sort();
        y.dedup();
        y
    };
    eprintln!("rows before {:?}\nrows after {:?}", ys(&before), ys(&after));
    assert_eq!(ys(&before), ys(&after));
}
#[test]
fn t06_backspace_twice_long_line() {
    let t = format!("head\n{}", "x".repeat(100_000));
    let mut e = open(&t, 400.0, 200.0);
    ctrl(&mut e, END);
    settle(&mut e);
    key(&mut e, BS);
    key(&mut e, BS);
    let n = e.text().unwrap().len();
    eprintln!("len {} -> {n}", t.len());
    assert_eq!(n, t.len() - 2);
}
#[test]
fn t07_backspace_twice_10k() {
    let t = format!("head\n{}", "x".repeat(10_000));
    let mut e = open(&t, 400.0, 200.0);
    ctrl(&mut e, END);
    settle(&mut e);
    key(&mut e, BS);
    key(&mut e, BS);
    let n = e.text().unwrap().len();
    eprintln!("len {} -> {n}", t.len());
    assert_eq!(n, t.len() - 2);
}
#[test]
fn t07b_forward_delete_twice_10k() {
    let t = format!("{}\ntail", "x".repeat(10_000));
    let mut e = open(&t, 400.0, 200.0);
    key(&mut e, DELK);
    key(&mut e, DELK);
    let n = e.text().unwrap().len();
    eprintln!("len {} -> {n}", t.len());
    assert_eq!(n, t.len() - 2);
}
#[test]
fn t07c_first_key_before_layout() {
    let t = format!("head\n{}", "x".repeat(10_000));
    let mut e = EditorView::from_text(&t).unwrap();
    ctrl(&mut e, END);
    key(&mut e, BS);
    let n = e.text().unwrap().len();
    eprintln!("len {} -> {n}", t.len());
    assert_eq!(n, t.len() - 1);
}
#[test]
fn t07d_right_arrow_before_layout_10k() {
    let t = format!("{}\ntail", "x".repeat(10_000));
    let mut e = EditorView::from_text(&t).unwrap();
    key(&mut e, RIGHT);
    eprintln!("caret {}", e.caret().0);
    assert_eq!(e.caret().0, 1);
}
#[test]
fn t08_prevent_editing_blocks_undo() {
    let mut e = open("abc", 300.0, 100.0);
    ch(&mut e, 'x');
    e.set_prevent_editing(true);
    ctrl(&mut e, 0x5a);
    assert_eq!(e.text().as_deref(), Some("xabc"));
}
#[test]
fn t09_prevent_backspace_no_selection() {
    let mut e = open("abc", 300.0, 100.0);
    ctrl(&mut e, END);
    e.set_prevent_editing(true);
    key(&mut e, BS);
    assert!(e.selection().is_empty(), "selection {:?}", e.selection());
}
#[test]
fn t10_copy_empty_keeps_clipboard() {
    let mut e = open("abc", 300.0, 100.0);
    let s = Rc::new(RefCell::new("user text".to_string()));
    e.set_clipboard(Box::new(Clip(s.clone())));
    ctrl(&mut e, 0x43);
    assert_eq!(s.borrow().as_str(), "user text");
}
#[test]
fn t11_paste_empty_keeps_selection() {
    let mut e = open("abc", 300.0, 100.0);
    let s = Rc::new(RefCell::new(String::new()));
    e.set_clipboard(Box::new(Clip(s)));
    ctrl(&mut e, 0x41);
    ctrl(&mut e, 0x56);
    assert_eq!(e.text().as_deref(), Some("abc"));
}
#[test]
fn t12_altgr_a_does_not_select_all() {
    let mut e = open("whole document", 300.0, 100.0);
    keym(
        &mut e,
        0x41,
        Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        },
    );
    ch(&mut e, '\u{105}');
    eprintln!("{:?}", e.text());
    assert!(e.text().unwrap().contains("whole document"));
}
#[test]
fn t13_select_all_repaints_middle_lines() {
    let t = (0..10).map(|i| format!("line {i}\n")).collect::<String>();
    let mut e = open(&t, 300.0, 400.0);
    let q = ctrl(&mut e, 0x41);
    eprintln!("{q:?}");
    assert!(covers(&q, 17.0 * 5.0 + 1.0), "line 5 gets no repaint");
}
#[test]
fn t13b_click_clears_old_selection_pixels() {
    let t = (0..10).map(|i| format!("line {i}\n")).collect::<String>();
    let mut e = open(&t, 300.0, 400.0);
    ctrl(&mut e, 0x41);
    let at = Point {
        x: Dip(1.0),
        y: Dip(1.0),
    };
    let mut c = ViewContext::default();
    e.handle(
        &ev(
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            1_000_000,
        ),
        &mut c,
    );
    assert!(
        covers(&c.requests, 17.0 * 5.0 + 1.0),
        "line 5 keeps stale highlight: {:?}",
        c.requests
    );
}
#[test]
fn t14_select_all_keeps_scroll() {
    let t = "x\n".repeat(1000);
    let mut e = open(&t, 200.0, 51.0);
    ctrl(&mut e, 0x41);
    assert_eq!(e.scroll_offset(), 0.0);
}
#[test]
fn t15a_backspace_cannot_leave_caret_in_crlf() {
    let mut e = open("a\rx\nb", 300.0, 100.0);
    key(&mut e, RIGHT);
    key(&mut e, RIGHT);
    key(&mut e, RIGHT);
    assert_eq!(e.caret().0, 3);
    key(&mut e, BS);
    assert_eq!(e.text().as_deref(), Some("a\r\nb"));
    assert_ne!(e.caret().0, 2, "caret between CR and LF");
}
#[test]
fn t15b_paste_cr_before_lf() {
    let mut e = open("a\nb", 300.0, 100.0);
    key(&mut e, RIGHT);
    e.set_clipboard(Box::new(MemoryClipboard("\r".into())));
    ctrl(&mut e, 0x56);
    assert_eq!(e.text().as_deref(), Some("a\r\nb"));
    assert_ne!(e.caret().0, 2, "caret between CR and LF");
}
#[test]
fn t15c_down_into_crlf() {
    let mut e = open("abcdef\nab\r\nzz", 300.0, 100.0);
    key(&mut e, RIGHT);
    key(&mut e, RIGHT);
    key(&mut e, RIGHT);
    key(&mut e, DOWN);
    eprintln!("caret {}", e.caret().0);
    assert_ne!(e.caret().0, 10, "caret between CR and LF");
    assert_eq!(e.caret().0, 9);
}
#[test]
fn t15d_down_into_cluster() {
    let mut e = open("abcdef\ne\u{301}\u{301}zz", 300.0, 100.0);
    key(&mut e, RIGHT);
    key(&mut e, RIGHT);
    key(&mut e, DOWN);
    eprintln!("caret {}", e.caret().0);
    // Two cells across: past the one-cell cluster at bytes 7 to 12, and past one z.
    assert_eq!(e.caret().0, 13, "caret inside cluster at {}", e.caret().0);
}
#[test]
fn t16_set_wrap_keeps_font() {
    let mut e = open("abc", 300.0, 100.0);
    e.set_font("Consolas", 30.0).unwrap();
    settle(&mut e);
    let em = |e: &EditorView| {
        list(e).commands.iter().find_map(|c| match c {
            DrawCommand::GlyphRun { font_em_size, .. } => Some(font_em_size.0),
            _ => None,
        })
    };
    let a = em(&e);
    e.set_wrap(false).unwrap();
    settle(&mut e);
    let b = em(&e);
    eprintln!("{a:?} {b:?}");
    assert_eq!(a, b);
}
#[test]
fn t16b_small_font_fills_window() {
    let t = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
    let mut e = open(&t, 300.0, 170.0);
    let big = runs(&e).len();
    e.set_font("Consolas", 6.0).unwrap();
    settle(&mut e);
    let l = list(&e);
    let maxy = l
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::GlyphRun { at, .. } => Some(at.y.0),
            _ => None,
        })
        .fold(0.0, f32::max);
    eprintln!(
        "runs at default {big}, at 6pt {}, lowest row y {maxy} of 170",
        runs(&e).len()
    );
    assert!(
        maxy > 150.0,
        "bottom of the window stays empty, lowest row y {maxy}"
    );
}
#[test]
fn t17_resize_keeps_text_visible() {
    let t = "abcd efgh ijkl mnop qrst uvwx yz01 2345 6789\n".repeat(60);
    let mut e = open(&t, 60.0, 200.0);
    ctrl(&mut e, END);
    settle(&mut e);
    key(&mut e, LEFT);
    settle(&mut e);
    let s = e.scroll_offset();
    e.layout(r(800.0, 200.0), &DirectWriteLayout::new().unwrap());
    let mut c = ViewContext::default();
    e.handle(
        &ev(
            InputKind::Resize {
                width: Dip(800.0),
                height: Dip(200.0),
            },
            0,
        ),
        &mut c,
    );
    settle(&mut e);
    let n = runs(&e).len();
    eprintln!("scroll before {s} after {} runs {n}", e.scroll_offset());
    assert!(n > 0, "blank window");
}
#[test]
fn t18_view_stable_when_layout_above_arrives() {
    let t = (0..400)
        .map(|i| format!("{i:04} abcd efgh ijkl mnop qrst uvwx\n"))
        .collect::<String>();
    let mut e = open(&t, 60.0, 170.0);
    wheel(&mut e, -60000.0);
    settle(&mut e);
    let s = e.scroll_offset();
    wheel(&mut e, 4000.0);
    let s1 = e.scroll_offset();
    click(&mut e, 1.0, 1.0, 10_000_000);
    let a = e.caret().0;
    settle(&mut e);
    let sc = e.scroll_offset();
    click(&mut e, 1.0, 1.0, 20_000_000);
    let b = e.caret().0;
    eprintln!(
        "scroll {s} {s1} {sc}; first byte {a} then {b}; line {} {}",
        a / 34,
        b / 34
    );
    assert_eq!(
        a / 34,
        b / 34,
        "top line changed when layouts above arrived"
    );
}
#[test]
fn t19_drag_autoscroll_no_blank() {
    let t = (0..2000).map(|i| format!("line {i}\n")).collect::<String>();
    let mut e = open(&t, 200.0, 51.0);
    let mut c = ViewContext::default();
    let at = Point {
        x: Dip(1.0),
        y: Dip(1.0),
    };
    e.handle(
        &ev(
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            1,
        ),
        &mut c,
    );
    e.handle(
        &ev(
            InputKind::MouseMove {
                at: Point {
                    x: Dip(1.0),
                    y: Dip(200.0),
                },
            },
            2,
        ),
        &mut c,
    );
    for _ in 0..200 {
        e.on_timer(&mut c);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let n = runs(&e).len();
    eprintln!(
        "scroll {} caret {} runs {n}",
        e.scroll_offset(),
        e.caret().0
    );
    assert!(n > 0, "autoscroll shows a blank window");
}
#[test]
fn t20_caret_visible_after_typing() {
    let mut e = open("abc\n", 300.0, 100.0);
    focus(&mut e);
    settle(&mut e);
    assert!(caret_drawn(&e));
    ch(&mut e, 'x');
    let mut c = ViewContext::default();
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(16));
        e.on_timer(&mut c);
        if !e.pending_layout() {
            break;
        }
    }
    assert!(caret_drawn(&e), "caret hidden right after a key");
}
#[test]
fn t20b_caret_visible_when_typing_in_hidden_phase() {
    let mut e = open("abc\n", 300.0, 100.0);
    focus(&mut e);
    settle(&mut e);
    e.on_timer_at(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        &mut ViewContext::default(),
    );
    assert!(!caret_drawn(&e));
    ch(&mut e, 'x');
    settle(&mut e);
    assert!(caret_drawn(&e), "caret stays hidden after a key");
}
#[test]
fn t20c_line_is_blank_between_key_and_worker() {
    let mut e = open("hello world\nsecond", 300.0, 100.0);
    let before = runs(&e).len();
    ch(&mut e, 'x');
    let now = runs(&e).len();
    eprintln!("runs before {before}, right after the key {now}");
    assert_eq!(
        before, now,
        "edited line paints blank until a worker result"
    );
}
#[test]
fn t20d_key_paint_rect_after_worker() {
    let mut e = open("hello world\nsecond", 300.0, 400.0);
    ch(&mut e, 'x');
    let mut c = ViewContext::default();
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        e.poll_layout(&mut c);
        if !e.pending_layout() {
            break;
        }
    }
    let whole = c
        .requests
        .iter()
        .any(|q| matches!(q, ViewRequest::Invalidate(b) if b.height.0 >= 400.0));
    eprintln!("{:?}", c.requests);
    assert!(!whole, "one typed key repaints the whole text area");
}
#[test]
fn t21_end_stops_before_ending() {
    for (t, want) in [("abc\r\ndef", 3), ("abc\ndef", 3), ("abc\u{2028}def", 3)] {
        let mut e = open(t, 300.0, 100.0);
        key(&mut e, END);
        assert_eq!(e.caret().0, want, "{t:?}");
    }
}
#[test]
fn t22_down_goes_to_next_visual_row() {
    let line = "abcdefghij".repeat(4);
    let t = format!("{line}\n{line}");
    let mut e = open(&t, 40.0, 400.0);
    for _ in 0..30 {
        let before = e.caret().0;
        key(&mut e, DOWN);
        settle(&mut e);
        if e.caret().0 > 40 || before == e.caret().0 {
            break;
        }
    }
    let l = DirectWriteLayout::new()
        .unwrap()
        .layout_line(&qe_buffer::PieceStore::new(&line), LineIndex(0), Dip(40.0))
        .unwrap();
    let row0 = l.rows[0].range.end.0;
    eprintln!(
        "caret {} rows {} first row ends {row0}",
        e.caret().0,
        l.rows.len()
    );
    assert!(
        e.caret().0 - 41 <= row0,
        "Down skipped rows, col {} first row ends {row0}",
        e.caret().0 - 41
    );
}
#[test]
fn t23_goal_column() {
    let mut e = open("abcdefgh\nab\nabcdefgh", 300.0, 100.0);
    for _ in 0..6 {
        key(&mut e, RIGHT);
    }
    key(&mut e, DOWN);
    key(&mut e, DOWN);
    assert_eq!(e.caret().0, 12 + 6);
}
#[test]
fn t24_nothing_to_undo_keeps_selection() {
    let mut e = open("abc", 300.0, 100.0);
    ctrl(&mut e, 0x41);
    let q = ctrl(&mut e, 0x5a);
    eprintln!("{q:?}");
    assert_eq!(e.selection().end.0, 3);
    assert_eq!(e.selection().start.0, 0);
}
#[test]
fn t25_long_line_made_short() {
    let t = "x".repeat(100_000);
    for wrap in [true, false] {
        let mut e = EditorView::from_text(&t).unwrap();
        e.set_wrap(wrap).unwrap();
        e.layout(r(200.0, 170.0), &DirectWriteLayout::new().unwrap());
        settle(&mut e);
        ctrl(&mut e, END);
        settle(&mut e);
        ctrl(&mut e, 0x41);
        ch(&mut e, 'a');
        ch(&mut e, 'b');
        ch(&mut e, 'c');
        settle(&mut e);
        assert_eq!(e.text().as_deref(), Some("abc"));
        focus(&mut e);
        assert!(
            !runs(&e).is_empty(),
            "short line after long line not painted wrap={wrap}"
        );
        assert!(caret_drawn(&e), "no caret wrap={wrap}");
        key(&mut e, BS);
        settle(&mut e);
        assert_eq!(e.text().as_deref(), Some("ab"));
        key(&mut e, LEFT);
        assert_eq!(e.caret().0, 1, "wrap={wrap}");
    }
}
#[test]
fn t26_up_down_inside_long_wrapped_line() {
    let t = "x".repeat(100_000);
    let mut e = open(&t, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    let at = e.caret().0;
    key(&mut e, UP);
    eprintln!("caret {at} -> {}", e.caret().0);
    assert!(
        e.caret().0 < at,
        "Up does nothing inside a long wrapped line"
    );
}
#[test]
fn t27_left_in_long_wrapped_keeps_view() {
    let t = "x".repeat(100_000);
    let mut e = open(&t, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    let s = e.scroll_offset();
    assert!(s > 1000.0);
    key(&mut e, LEFT);
    eprintln!("scroll {s} -> {}", e.scroll_offset());
    assert!(
        (e.scroll_offset() - s).abs() < 100.0,
        "view jumped from {s} to {}",
        e.scroll_offset()
    );
}
#[test]
fn t27b_click_in_long_wrapped_keeps_view() {
    let t = "x".repeat(100_000);
    let mut e = open(&t, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    let s = e.scroll_offset();
    click(&mut e, 5.0, 60.0, 5_000_000);
    eprintln!("scroll {s} -> {} caret {}", e.scroll_offset(), e.caret().0);
    assert!(
        (e.scroll_offset() - s).abs() < 100.0,
        "view jumped from {s} to {}",
        e.scroll_offset()
    );
}
#[test]
fn t27c_home_on_long_wrapped_line_stays_on_row() {
    let t = "x".repeat(100_000);
    let mut e = open(&t, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    key(&mut e, HOME);
    eprintln!(
        "caret after Home {} scroll {}",
        e.caret().0,
        e.scroll_offset()
    );
    assert!(e.caret().0 > 90_000, "Home jumped to byte {}", e.caret().0);
}
#[test]
fn t28_two_far_clicks_are_not_a_double_click() {
    let mut e = open("hello world\nnext words", 300.0, 100.0);
    click(&mut e, 2.0, 2.0, 1000);
    click(&mut e, 40.0, 20.0, 2000);
    assert!(
        e.selection().is_empty(),
        "far click selected a word {:?}",
        e.selection()
    );
}
#[test]
fn t29_fuzz_view_matches_fresh_view() {
    for seed0 in [1u32, 7, 99] {
        let mut e = open("", 90.0, 2400.0);
        let mut model = String::new();
        let mut seed = seed0.wrapping_mul(0x9E3779B9);
        e.set_clipboard(Box::new(MemoryClipboard("p\r\nq".into())));
        for step in 0..260 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let op = (seed >> 8) % 14;
            let sel = e.selection();
            let edit = |m: &mut String, v: &str| {
                m.replace_range(sel.start.0..sel.end.0, v);
            };
            match op {
                0..=3 => {
                    let c = ['a', ' ', '\u{e9}', '\u{1F600}'][(seed >> 16) as usize % 4];
                    ch(&mut e, c);
                    edit(&mut model, &c.to_string());
                }
                4 => {
                    key(&mut e, ENTER);
                    edit(&mut model, "\n");
                }
                5 => {
                    ctrl(&mut e, 0x56);
                    edit(&mut model, "p\r\nq");
                }
                6 => {
                    key(&mut e, LEFT);
                }
                7 => {
                    key(&mut e, RIGHT);
                }
                8 => {
                    key(&mut e, HOME);
                }
                9 => {
                    key(&mut e, END);
                }
                10 => {
                    keym(
                        &mut e,
                        RIGHT,
                        Modifiers {
                            shift: true,
                            ..Default::default()
                        },
                    );
                }
                11 => {
                    key(&mut e, BS);
                    model = e.text().unwrap();
                }
                12 => {
                    key(&mut e, DELK);
                    model = e.text().unwrap();
                }
                _ => {
                    ctrl(&mut e, 0x5a);
                    model = e.text().unwrap();
                }
            }
            settle(&mut e);
            let text = e.text().unwrap();
            assert_eq!(text, model, "seed {seed0} step {step} op {op}");
            assert!(
                text.is_char_boundary(e.caret().0),
                "caret off char boundary"
            );
            let b = text.as_bytes();
            let c = e.caret().0;
            assert!(
                !(c > 0 && c < b.len() && b[c - 1] == b'\r' && b[c] == b'\n'),
                "seed {seed0} step {step} op {op} caret between CR and LF"
            );
            if step % 13 == 0 {
                let fresh = open(&text, 90.0, 2400.0);
                assert_eq!(
                    runs(&e),
                    runs(&fresh),
                    "seed {seed0} step {step} op {op}: paint differs from a fresh view of the same text"
                );
            }
        }
    }
}
#[test]
#[ignore = "it only prints timings"]
fn measure_only_t30_timing_big() {
    let time = |e: &mut EditorView, label: &str| {
        let mut v = vec![];
        for i in 0..200 {
            let s = std::time::Instant::now();
            if i % 2 == 0 {
                ch(e, 'q');
            } else {
                key(e, BS);
            }
            v.push(s.elapsed().as_secs_f64() * 1000.0);
            if i % 8 == 0 {
                let mut c = ViewContext::default();
                e.poll_layout(&mut c);
            }
        }
        v.sort_by(f64::total_cmp);
        eprintln!(
            "{label}: p50 {:.3} p99 {:.3} max {:.3} ms (debug build)",
            v[100], v[197], v[199]
        );
    };
    let one = "x".repeat(20_000_000);
    let mut e = open(&one, 400.0, 400.0);
    time(&mut e, "20 MB one line, caret at start");
    ctrl(&mut e, END);
    settle(&mut e);
    time(&mut e, "20 MB one line, caret at end");
    let many = "x\n".repeat(10_000_000);
    let mut e = open(&many, 400.0, 400.0);
    time(&mut e, "20 MB 10M lines");
    let s = std::time::Instant::now();
    key(&mut e, ENTER);
    eprintln!("enter {:.3} ms", s.elapsed().as_secs_f64() * 1000.0);
}
#[test]
#[ignore = "it only prints timings"]
fn measure_only_t31_poll_frees_whole_history_on_ui() {
    let big = "abcdefghij\n".repeat(9_000_000);
    let mut e = open("", 400.0, 400.0);
    e.set_clipboard(Box::new(MemoryClipboard(big)));
    ctrl(&mut e, 0x56);
    settle(&mut e);
    ctrl(&mut e, 0x5a);
    settle(&mut e);
    let s = std::time::Instant::now();
    ch(&mut e, 'x');
    let k = s.elapsed();
    let s = std::time::Instant::now();
    e.poll_layout(&mut ViewContext::default());
    let p = s.elapsed();
    eprintln!("key {:?} then first poll {:?} (debug build)", k, p);
}
#[test]
fn u01_tall_window_is_filled() {
    let t = (0..500).map(|i| format!("line {i}\n")).collect::<String>();
    for h in [600.0f32, 1000.0, 1400.0] {
        let e = open(&t, 800.0, h);
        let maxy = list(&e)
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::GlyphRun { at, .. } => Some(at.y.0),
                _ => None,
            })
            .fold(0.0, f32::max);
        eprintln!(
            "window height {h}: lowest row baseline y {maxy}, blank below {}",
            h - maxy
        );
        assert!(h - maxy < 20.0, "window {h} is blank below {maxy}");
    }
}
#[test]
fn u02_layout_above_moves_view() {
    let t = (0..400)
        .map(|i| format!("{i:04} abcd efgh ijkl mnop qrst uvwx\n"))
        .collect::<String>();
    let mut e = open(&t, 60.0, 170.0);
    wheel(&mut e, -60000.0);
    settle(&mut e);
    wheel(&mut e, 2118.0);
    click(&mut e, 1.0, 1.0, 10_000_000);
    let a = e.caret().0;
    let s1 = e.scroll_offset();
    settle(&mut e);
    let s2 = e.scroll_offset();
    click(&mut e, 1.0, 1.0, 20_000_000);
    let b = e.caret().0;
    eprintln!(
        "scroll {s1} -> {s2}; byte under the top left {a} -> {b}; line {} -> {}",
        a / 35,
        b / 35
    );
    assert_eq!(
        a / 35,
        b / 35,
        "the top line moved when layouts above arrived"
    );
}
#[test]
fn u03_short_after_long() {
    let t = "x".repeat(100_000);
    let mut e = open(&t, 200.0, 170.0);
    let n0 = runs(&e).len();
    ctrl(&mut e, 0x41);
    ch(&mut e, 'a');
    settle(&mut e);
    eprintln!(
        "runs while long {n0}, after replacing all with one letter {}; text {:?}",
        runs(&e).len(),
        e.text()
    );
    assert_eq!(e.text().as_deref(), Some("a"));
    assert!(!runs(&e).is_empty(), "the letter must paint");
    assert!(n0 > 1);
    let mut e = open(&t, 200.0, 170.0);
    ctrl(&mut e, END);
    settle(&mut e);
    keym(
        &mut e,
        HOME,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    eprintln!("shift home selection {:?}", e.selection());
    let row = e.selection().end.0 - e.selection().start.0;
    assert!(row > 0 && row < 100, "Shift+Home selected {row} bytes");
    settle(&mut e);
    key(&mut e, BS);
    settle(&mut e);
    eprintln!(
        "after shift+home + backspace, len {}",
        e.text().unwrap().len()
    );
    assert_eq!(e.text().unwrap().len(), 100_000 - row);
}
#[test]
fn u04_fuzz_no_undo() {
    for seed0 in [1u32, 7, 99, 123] {
        let mut e = open("", 90.0, 2400.0);
        let mut seed = seed0.wrapping_mul(0x9E3779B9);
        e.set_clipboard(Box::new(MemoryClipboard("p\r\nq".into())));
        for step in 0..260 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let op = (seed >> 8) % 13;
            match op {
                0..=3 => {
                    ch(
                        &mut e,
                        ['a', ' ', '\u{e9}', '\u{1F600}'][(seed >> 16) as usize % 4],
                    );
                }
                4 => {
                    key(&mut e, ENTER);
                }
                5 => {
                    ctrl(&mut e, 0x56);
                }
                6 => {
                    key(&mut e, LEFT);
                }
                7 => {
                    key(&mut e, RIGHT);
                }
                8 => {
                    key(&mut e, HOME);
                }
                9 => {
                    key(&mut e, END);
                }
                10 => {
                    keym(
                        &mut e,
                        RIGHT,
                        Modifiers {
                            shift: true,
                            ..Default::default()
                        },
                    );
                }
                11 => {
                    key(&mut e, BS);
                }
                _ => {
                    key(&mut e, DELK);
                }
            }
            settle(&mut e);
            let text = e.text().unwrap();
            assert!(
                text.is_char_boundary(e.caret().0),
                "seed {seed0} step {step}"
            );
            let fresh = open(&text, 90.0, 2400.0);
            if runs(&e) != runs(&fresh) {
                panic!(
                    "seed {seed0} step {step} op {op} text {text:?}\n  got {:?}\n  new {:?}",
                    runs(&e).iter().map(|a| a.0).collect::<Vec<_>>(),
                    runs(&fresh).iter().map(|a| a.0).collect::<Vec<_>>()
                );
            }
        }
    }
}
#[test]
fn u05_mapped_notify_toggles_caret() {
    let path = std::env::temp_dir().join(format!("qe-probe-{}.txt", std::process::id()));
    std::fs::write(&path, "abc\ndef\n").unwrap();
    let mut e = EditorView::from_mapped(qe_buffer::MappedStore::open(&path).unwrap()).unwrap();
    settle(&mut e);
    focus(&mut e);
    settle(&mut e);
    let mut seen = vec![];
    for _ in 0..4 {
        e.on_timer(&mut ViewContext::default());
        seen.push(caret_drawn(&e));
    }
    assert_eq!(
        seen, [true; 4],
        "a wake-up from the store must not blink the caret"
    );
    drop(e);
    let _ = std::fs::remove_file(&path);
}
