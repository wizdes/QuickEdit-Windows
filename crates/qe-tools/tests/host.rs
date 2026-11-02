//! `ToolsHost` against a real `EditorView`, by commands and input, as a window uses it.
use qe_api::{
    ByteOffset, CommandId, Dip, InputEvent, InputKind, Modifiers, MouseButton, Point, Rect, View,
    ViewContext, testing::FixedTextMeasurer,
};
use qe_editor::{
    DecorationKind, EditorCommand, EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile,
};
use qe_tools::{BarStop, FindOptions, Substitutions, ToolsFocus, ToolsHost};
use qe_ui::MemoryClipboard;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn measure() -> FixedTextMeasurer {
    FixedTextMeasurer {
        char_width: Dip(7.0),
        height: Dip(16.0),
        baseline: Dip(12.0),
    }
}
fn area(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}
fn ctx() -> ViewContext {
    ViewContext::default()
}

/// A view that has a layout, and tools.
struct Setup {
    view: EditorView,
    tools: ToolsHost,
    ctx: ViewContext,
}
impl Setup {
    fn new(text: &str) -> Self {
        let view = EditorView::from_text(text).unwrap();
        Self::with_view(view)
    }
    fn with_view(mut view: EditorView) -> Self {
        let mut tools = ToolsHost::with_clipboard(MemoryClipboard::shared());
        let rest = tools.layout(area(800.0, 600.0), &measure());
        view.layout(rest, &measure());
        let mut setup = Self {
            view,
            tools,
            ctx: ctx(),
        };
        setup.settle_view();
        setup
    }
    fn settle_view(&mut self) {
        let start = Instant::now();
        self.view.poll_layout(&mut self.ctx);
        while self.view.pending_layout() {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the view did not settle"
            );
            std::thread::sleep(Duration::from_millis(1));
            self.view.poll_layout(&mut self.ctx);
        }
    }
    fn command(&mut self, command: CommandId) {
        let m = measure();
        self.tools
            .run_command(command, &mut self.view, &m, &mut self.ctx);
        self.relayout();
    }
    fn relayout(&mut self) {
        let m = measure();
        let rest = self.tools.layout(area(800.0, 600.0), &m);
        self.view.layout(rest, &m);
    }
    fn send(&mut self, kind: InputKind) -> qe_api::Handled {
        let m = measure();
        let event = InputEvent { time_us: 0, kind };
        self.tools
            .handle_input(&event, &mut self.view, &m, &mut self.ctx)
    }
    fn key(&mut self, virtual_key: u16) -> qe_api::Handled {
        self.send(InputKind::KeyDown {
            virtual_key,
            modifiers: Modifiers::default(),
            repeat: false,
        })
    }
    fn shift_key(&mut self, virtual_key: u16) -> qe_api::Handled {
        self.send(InputKind::KeyDown {
            virtual_key,
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
            repeat: false,
        })
    }
    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.send(InputKind::Char(c));
        }
    }
    /// Polls until the workers have finished and their results are in.
    fn settle(&mut self) {
        let m = measure();
        let start = Instant::now();
        loop {
            self.view.poll_layout(&mut self.ctx);
            self.tools.poll(&mut self.view, &m, &mut self.ctx);
            self.ctx.requests.clear();
            let (_, counted) = self.tools.match_count();
            let open = self.tools.is_bar_open();
            let quiet = if open {
                self.tools.is_quiet() && (counted || self.tools.query().is_empty())
            } else {
                self.tools.is_idle()
            };
            if quiet && !self.view.pending_layout() {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the tools did not settle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn text(&self) -> String {
        self.view.text().unwrap()
    }
    fn selection(&self) -> (usize, usize) {
        let s = self.view.selection();
        (s.start.0, s.end.0)
    }
    fn select(&mut self, a: usize, b: usize) {
        self.view
            .select(ByteOffset(a)..ByteOffset(b), &mut self.ctx);
    }
    fn marks(&self, kind: DecorationKind) -> Vec<(usize, usize)> {
        self.view
            .decorations(kind)
            .iter()
            .map(|r| (r.start.0, r.end.0))
            .collect()
    }
    fn find(&mut self, query: &str) {
        self.command(CommandId::Find);
        self.typed(query);
        self.settle();
    }
}
const ENTER: u16 = 0x0d;
const ESC: u16 = 0x1b;
const TAB: u16 = 0x09;

#[test]
fn closed_tools_cost_nothing() {
    let mut s = Setup::new("hello hello hello");
    let before = s.tools.layout(area(800.0, 600.0), &measure());
    assert_eq!(before, area(800.0, 600.0), "a closed bar takes no room");
    assert!(s.tools.is_idle());
    assert_eq!(s.tools.focus(), ToolsFocus::Editor);
    let m = measure();
    s.ctx.requests.clear();
    for _ in 0..50 {
        s.view
            .replace_range(ByteOffset(0)..ByteOffset(0), "x", &mut s.ctx);
        s.ctx.requests.clear();
        s.tools.poll(&mut s.view, &m, &mut s.ctx);
        assert!(s.ctx.requests.is_empty(), "poll asked for nothing");
    }
    assert_eq!(s.tools.workers_started(), 0, "no thread ran");
    assert_eq!(s.tools.marks_set(), 0);
    assert!(s.marks(DecorationKind::FindMatch).is_empty());
    let mut list = qe_api::DisplayList::default();
    let theme = qe_api::Theme::new(qe_api::ThemeKind::Light);
    s.tools.draw(&mut list, &theme, &m);
    assert_eq!(list, qe_api::DisplayList::default(), "it paints nothing");
    assert!(s.tools.widgets().is_empty());
    // Keys and clicks pass through it.
    assert_eq!(s.key(ENTER), qe_api::Handled::No);
    assert_eq!(s.send(InputKind::Char('a')), qe_api::Handled::No);
}

#[test]
fn ctrl_f_opens_typing_finds_and_selects_as_on_the_mac() {
    let mut s = Setup::new("1 owl 2 owl");
    s.command(CommandId::Find);
    assert!(s.tools.is_bar_open());
    assert_eq!(s.tools.focus(), ToolsFocus::Bar(BarStop::Find));
    s.typed("owl");
    s.settle();
    assert_eq!(s.tools.match_count(), (2, true));
    assert_eq!(s.tools.bar().count.text, "2 found");
    assert_eq!(
        s.selection(),
        (2, 5),
        "the first match, from the search's start"
    );
    assert_eq!(s.marks(DecorationKind::CurrentFindMatch), vec![(2, 5)]);
    assert_eq!(s.marks(DecorationKind::FindMatch), vec![(8, 11)]);
    // Return moves on. Esc closes, keeps the match selected, and gives the keys back.
    assert_eq!(s.key(ENTER), qe_api::Handled::Yes);
    s.settle();
    assert_eq!(s.selection(), (8, 11));
    assert_eq!(s.key(ESC), qe_api::Handled::Yes);
    s.settle();
    assert!(!s.tools.is_bar_open());
    assert_eq!(s.tools.focus(), ToolsFocus::Editor);
    assert_eq!(s.selection(), (8, 11));
    assert!(s.marks(DecorationKind::FindMatch).is_empty());
    assert!(s.marks(DecorationKind::CurrentFindMatch).is_empty());
    assert_eq!(s.text(), "1 owl 2 owl", "the text is the same");
}

#[test]
fn typing_starts_again_from_where_the_search_began() {
    let mut s = Setup::new("ab abc abcd");
    s.command(CommandId::Find);
    s.typed("ab");
    s.settle();
    assert_eq!(s.selection(), (0, 2));
    s.typed("c");
    s.settle();
    assert_eq!(s.selection(), (3, 6));
    s.typed("d");
    s.settle();
    assert_eq!(s.selection(), (7, 11));
    assert_eq!(s.tools.match_count(), (1, true));
    // Backspace goes back, from the same start.
    s.key(0x08);
    s.settle();
    assert_eq!(s.selection(), (3, 6));
    // No match says so, and keeps the selection.
    s.typed("zz");
    s.settle();
    assert_eq!(s.tools.bar().count.text, "Not found");
    assert!(s.tools.current_match().is_none());
    assert!(s.marks(DecorationKind::FindMatch).is_empty());
}

#[test]
fn f3_and_shift_f3_with_the_bar_closed_wrap_as_on_the_mac() {
    // The Mac's testUseSelectionThenFindNextAndWrap and testFindPreviousAndWrap.
    let mut s = Setup::new("1 dog 2 dog 3 dog");
    s.select(2, 5);
    s.command(CommandId::UseSelectionForFind);
    assert_eq!(s.tools.query(), "dog");
    assert!(!s.tools.is_bar_open());
    s.command(CommandId::FindNext);
    s.settle();
    assert_eq!(s.selection(), (8, 11));
    s.command(CommandId::FindNext);
    s.settle();
    assert_eq!(s.selection(), (14, 17));
    s.command(CommandId::FindNext);
    s.settle();
    assert_eq!(s.selection(), (2, 5), "it wraps to the start");
    s.command(CommandId::FindPrevious);
    s.settle();
    assert_eq!(s.selection(), (14, 17), "it wraps to the end");
    s.command(CommandId::FindPrevious);
    s.settle();
    assert_eq!(s.selection(), (8, 11));
    assert!(
        s.marks(DecorationKind::FindMatch).is_empty(),
        "no marks while closed"
    );
    assert!(s.tools.is_idle());
}

#[test]
fn f3_steps_queue_in_order() {
    let mut s = Setup::new("a a a a a a");
    s.select(0, 1);
    s.command(CommandId::UseSelectionForFind);
    for _ in 0..3 {
        s.command(CommandId::FindNext);
    }
    s.settle();
    assert_eq!(s.selection(), (6, 7), "three steps from the first");
}

#[test]
fn wrap_around_off_stops_and_beeps() {
    let mut s = Setup::new("x dog y");
    s.command(CommandId::Find);
    s.typed("dog");
    s.settle();
    s.tools.set_find_options_for_test(FindOptions {
        wrap: false,
        ..FindOptions::default()
    });
    assert!(!s.tools.take_beep());
    s.command(CommandId::FindNext);
    s.settle();
    assert!(s.tools.take_beep());
    assert_eq!(s.selection(), (2, 5), "the selection stays");
}

#[test]
fn f3_with_no_query_beeps_and_changes_nothing() {
    let mut s = Setup::new("abc");
    s.command(CommandId::FindNext);
    assert!(s.tools.take_beep());
    assert_eq!(s.tools.workers_started(), 0);
}

#[test]
fn marks_follow_edits_through_changes_since() {
    let mut s = Setup::new("cat dog cat");
    s.find("cat");
    assert_eq!(s.tools.match_count(), (2, true));
    assert_eq!(s.marks(DecorationKind::CurrentFindMatch), vec![(0, 3)]);
    assert_eq!(s.marks(DecorationKind::FindMatch), vec![(8, 11)]);
    // Typing a new match before the others moves them and adds it.
    s.view
        .replace_range(ByteOffset(4)..ByteOffset(4), "cat ", &mut s.ctx);
    s.settle();
    assert_eq!(s.tools.match_count(), (3, true));
    assert_eq!(s.marks(DecorationKind::FindMatch), vec![(4, 7), (12, 15)]);
    // Breaking the current match drops it.
    s.view
        .replace_range(ByteOffset(1)..ByteOffset(2), "o", &mut s.ctx);
    s.settle();
    assert!(
        s.tools.current_match().is_none(),
        "current {:?} marks {:?} text {:?}",
        s.tools.current_match(),
        s.marks(DecorationKind::CurrentFindMatch),
        s.text()
    );
    assert_eq!(s.tools.match_count(), (2, true));
    assert!(s.marks(DecorationKind::CurrentFindMatch).is_empty());
    // Undo brings it back as a match.
    s.view.run(EditorCommand::Undo, &mut s.ctx);
    s.settle();
    assert_eq!(s.tools.match_count(), (3, true));
}

#[test]
fn only_text_near_the_view_gets_marks_and_the_count_is_all() {
    let line = "needle in the line\n";
    let total = 200_000;
    let text = line.repeat(total);
    let mut s = Setup::new(&text);
    s.find("needle");
    assert_eq!(s.tools.match_count(), (total as u64, true));
    let marks = s.marks(DecorationKind::FindMatch);
    assert!(!marks.is_empty());
    assert!(
        marks.len() <= qe_tools::MARK_LIMIT,
        "{} marks of {total} matches",
        marks.len()
    );
    let last = marks.last().unwrap().1;
    assert!(last < 200_000, "marks stop near the view, at {last}");
    // Moving far away finds marks there.
    let far = line.len() * 150_000;
    s.view.select(ByteOffset(far)..ByteOffset(far), &mut s.ctx);
    s.view.run_host(
        qe_editor::HostCommand::Select {
            range: ByteOffset(far)..ByteOffset(far),
            reveal: true,
        },
        &mut s.ctx,
    );
    let start = Instant::now();
    loop {
        s.settle();
        let marks = s.marks(DecorationKind::FindMatch);
        if s.view.visible_range().is_ok()
            && marks
                .iter()
                .any(|m| m.0 >= far - 20_000 && m.0 <= far + 20_000)
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no marks near the new place"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn layout_not_settled_is_not_an_error() {
    let mut view = EditorView::from_text("cat cat").unwrap();
    let mut tools = ToolsHost::with_clipboard(MemoryClipboard::shared());
    let m = measure();
    let mut c = ctx();
    let rest = tools.layout(area(800.0, 600.0), &m);
    // The view has been given no layout at all yet.
    tools.run_command(CommandId::Find, &mut view, &m, &mut c);
    let event = |ch| InputEvent {
        time_us: 0,
        kind: InputKind::Char(ch),
    };
    for ch in "cat".chars() {
        tools.handle_input(&event(ch), &mut view, &m, &mut c);
    }
    let start = Instant::now();
    while !tools.match_count().1 {
        tools.poll(&mut view, &m, &mut c);
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(tools.match_count().0, 2);
    assert!(
        view.decorations(DecorationKind::FindMatch).is_empty(),
        "no marks yet"
    );
    view.layout(rest, &m);
    while view.pending_layout() || view.decorations(DecorationKind::FindMatch).is_empty() {
        view.poll_layout(&mut c);
        tools.poll(&mut view, &m, &mut c);
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the marks did not come"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn replace_and_replace_all_through_the_bar() {
    // The Mac's testReplaceAllIgnoringCase.
    let mut s = Setup::new("1 Fox 2 fox 3 FOX");
    s.command(CommandId::FindReplace);
    assert_eq!(
        s.tools.focus(),
        ToolsFocus::Bar(BarStop::Replace),
        "the keys go to the replace field, as on the Mac"
    );
    s.typed("cat");
    // Shift+Tab goes back through the row to Done and on. Click the find field's place.
    let find = s.tools.bar().find.bounds;
    s.send(InputKind::MouseDown {
        at: Point {
            x: Dip(find.x.0 + 5.0),
            y: Dip(find.y.0 + 5.0),
        },
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    assert_eq!(s.tools.focus(), ToolsFocus::Bar(BarStop::Find));
    s.typed("fox");
    s.settle();
    assert_eq!(s.selection(), (2, 5));
    click(&mut s, |b| b.replace_one.bounds);
    s.settle();
    assert_eq!(s.text(), "1 cat 2 fox 3 FOX");
    assert_eq!(s.selection(), (8, 11), "Replace moves to the next match");
    click(&mut s, |b| b.replace_all.bounds);
    s.settle();
    assert_eq!(s.text(), "1 cat 2 cat 3 cat");
    assert_eq!(s.tools.last_replaced(), Some(2));
    assert_eq!(s.tools.status(), Some("2 replaced"));
    assert_eq!(s.tools.match_count(), (0, true));
    // Undo takes Replace All back as one step.
    s.view.run(EditorCommand::Undo, &mut s.ctx);
    s.settle();
    assert_eq!(s.text(), "1 cat 2 fox 3 FOX");
    assert_eq!(s.tools.match_count(), (2, true));
}

fn click(s: &mut Setup, place: impl Fn(&qe_tools::FindBar) -> Rect) {
    let bounds = place(s.tools.bar());
    let at = Point {
        x: Dip(bounds.x.0 + bounds.width.0 / 2.0),
        y: Dip(bounds.y.0 + bounds.height.0 / 2.0),
    };
    s.send(InputKind::MouseDown {
        at,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    s.send(InputKind::MouseUp {
        at,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
}

#[test]
fn a_stale_replace_all_is_refused_and_run_again() {
    let mut s = Setup::new("aa bb aa");
    s.command(CommandId::FindReplace);
    s.typed("x");
    let m = measure();
    s.tools
        .set_query_for_test("aa", &mut s.view, &m, &mut s.ctx);
    s.settle();
    click(&mut s, |b| b.replace_all.bounds);
    // The worker's result will be for the old text: the user types before it is applied.
    s.view
        .replace_range(ByteOffset(0)..ByteOffset(0), "aa ", &mut s.ctx);
    s.settle();
    assert_eq!(
        s.text(),
        "x x bb x",
        "the new match is replaced too, and nothing moved"
    );
    assert_eq!(s.tools.last_replaced(), Some(3));
    let mut c = ctx();
    s.view.run(EditorCommand::Undo, &mut c);
    assert_eq!(
        s.text(),
        "aa aa bb aa",
        "one undo step takes back only Replace All"
    );
}

#[test]
fn a_read_only_view_refuses_replace_as_the_mac_does() {
    let mut s = Setup::new("aa bb");
    s.view.set_prevent_editing(true);
    s.command(CommandId::FindReplace);
    assert_eq!(
        s.tools.focus(),
        ToolsFocus::Bar(BarStop::Find),
        "the replace field cannot take the keys"
    );
    assert!(!s.tools.bar().replace.enabled);
    assert!(!s.tools.bar().replace_all.enabled);
    s.typed("aa");
    s.settle();
    assert_eq!(s.tools.match_count(), (1, true), "find works");
    click(&mut s, |b| b.replace_all.bounds);
    s.settle();
    assert_eq!(s.text(), "aa bb");
    let (enabled, _) = s
        .tools
        .command_state(CommandId::FindReplace, &s.view)
        .unwrap();
    assert!(!enabled);
    // Transformations and Complete refuse too.
    s.select(0, 2);
    s.key(ESC);
    s.settle();
    s.select(0, 2);
    s.command(CommandId::UpperCase);
    s.settle();
    assert_eq!(s.text(), "aa bb");
    assert!(s.tools.take_beep());
    s.command(CommandId::Complete);
    assert!(s.tools.take_beep());
}

#[test]
fn keys_in_the_bar_stay_out_of_the_document() {
    let mut s = Setup::new("hello");
    s.command(CommandId::Find);
    // A plain key that nothing uses stops, and Ctrl and F keys go on to the host's table.
    assert_eq!(s.key(0x72 + 8), qe_api::Handled::No);
    assert_eq!(s.send(InputKind::Char('z')), qe_api::Handled::Yes);
    assert_eq!(s.tools.query(), "z");
    // Tab walks the bar. In a button a typed letter goes nowhere.
    s.key(TAB);
    assert_eq!(s.tools.focus(), ToolsFocus::Bar(BarStop::Previous));
    assert_eq!(s.send(InputKind::Char('q')), qe_api::Handled::Yes);
    s.key(TAB);
    s.key(TAB);
    s.key(TAB);
    assert_eq!(s.tools.focus(), ToolsFocus::Bar(BarStop::Done));
    assert_eq!(s.tools.query(), "z");
    s.shift_key(TAB);
    assert_eq!(s.tools.focus(), ToolsFocus::Bar(BarStop::Options));
    // Without Replace, Tab skips its row.
    s.key(TAB);
    s.key(TAB);
    assert_eq!(s.tools.focus(), ToolsFocus::Bar(BarStop::Find));
    assert_eq!(s.text(), "hello");
}

#[test]
fn a_click_in_the_text_gives_the_keys_back_and_leaves_the_bar_open() {
    let mut s = Setup::new("hello");
    s.command(CommandId::Find);
    let handled = s.send(InputKind::MouseDown {
        at: Point {
            x: Dip(100.0),
            y: Dip(300.0),
        },
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    assert_eq!(
        handled,
        qe_api::Handled::No,
        "the click goes on to the editor"
    );
    assert!(s.tools.is_bar_open());
    assert_eq!(s.tools.focus(), ToolsFocus::Editor);
}

#[test]
fn the_options_list_changes_the_search() {
    let mut s = Setup::new("Cat cat catalog");
    s.find("cat");
    assert_eq!(s.tools.match_count(), (3, true));
    let pick = |s: &mut Setup, downs: usize| {
        click(s, |b| b.options.bounds);
        assert_eq!(s.tools.focus(), ToolsFocus::List);
        for _ in 0..downs {
            s.key(0x28);
        }
        s.key(ENTER);
        s.settle();
    };
    pick(&mut s, 0); // Ignore Case off
    assert!(!s.tools.find_options().ignore_case);
    assert_eq!(s.tools.match_count(), (2, true));
    pick(&mut s, 3); // Starts With is two rows past Contains
    let kind = s.tools.find_options().kind;
    assert_eq!(kind, qe_tools::MatchKind::StartsWith, "{kind:?}");
    assert_eq!(s.tools.match_count(), (2, true));
    pick(&mut s, 4); // Full Word
    assert_eq!(s.tools.find_options().kind, qe_tools::MatchKind::FullWord);
    assert_eq!(s.tools.match_count(), (1, true));
    assert_ne!(s.tools.focus(), ToolsFocus::List);
}

#[test]
fn select_line_return_closes_the_button_stays_as_on_the_mac() {
    let text = (1..=20)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut s = Setup::new(&text);
    s.command(CommandId::SelectLine);
    assert!(s.tools.is_select_line_open());
    assert_eq!(s.tools.focus(), ToolsFocus::SelectLine);
    assert_eq!(s.tools.panel().bounds.width.0, 253.0);
    assert_eq!(s.tools.panel().bounds.height.0, 62.0);
    assert_eq!(s.tools.panel().field.bounds.width.0, 138.0);
    assert_eq!(s.tools.panel().field.placeholder, "Line number");
    s.typed("20");
    s.key(ENTER);
    assert!(
        !s.tools.is_select_line_open(),
        "Return closes it after a good entry"
    );
    assert_eq!(&s.text()[s.selection().0..s.selection().1], "L20");
    assert_eq!(s.tools.focus(), ToolsFocus::Editor);

    // A range by the button: it stays open.
    s.command(CommandId::SelectLine);
    assert_eq!(s.tools.panel().field.text, "20", "the old entry stays");
    s.typed("3-4");
    let bounds = s.tools.panel().select.bounds;
    let at = Point {
        x: Dip(bounds.x.0 + 5.0),
        y: Dip(bounds.y.0 + 5.0),
    };
    let press = |s: &mut Setup, down| {
        s.send(if down {
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            }
        } else {
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            }
        })
    };
    press(&mut s, true);
    press(&mut s, false);
    assert!(s.tools.is_select_line_open(), "the button leaves it open");
    assert_eq!(&s.text()[s.selection().0..s.selection().1], "L3\nL4\n");
    // An empty entry does nothing, and Esc closes.
    s.key(0x08);
    s.key(0x08);
    s.key(0x08);
    assert_eq!(s.tools.panel().field.text, "");
    s.key(ENTER);
    assert!(s.tools.is_select_line_open());
    assert!(s.tools.alert_text().is_none());
    s.key(ESC);
    assert!(!s.tools.is_select_line_open());
}

#[test]
fn select_line_error_beeps_shows_the_macs_words_and_stays_open() {
    let mut s = Setup::new("a\nb");
    s.command(CommandId::SelectLine);
    s.typed("99");
    s.key(ENTER);
    assert!(s.tools.take_beep());
    assert_eq!(
        s.tools.alert_text(),
        Some("Invalid line specification \u{201c}99\u{201d}.")
    );
    assert_eq!(s.tools.focus(), ToolsFocus::Alert);
    assert!(s.tools.is_select_line_open());
    assert_eq!(s.selection(), (0, 0), "nothing moved");
    // Return is OK, and the panel stays.
    s.send(InputKind::KeyUp {
        virtual_key: ENTER,
        modifiers: Modifiers::default(),
    });
    s.key(ENTER);
    s.send(InputKind::KeyUp {
        virtual_key: ENTER,
        modifiers: Modifiers::default(),
    });
    assert!(s.tools.alert_text().is_none());
    assert!(s.tools.is_select_line_open());
    // A bad form gives the same words.
    s.typed("x");
    s.key(ENTER);
    assert!(s.tools.alert_text().is_some());
}

#[test]
fn select_line_with_find_focused_acts_on_the_document() {
    // The Mac's testSelectLineWithTheFindFieldFocused.
    let text = (1..=20)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut s = Setup::new(&text);
    s.find("L1");
    s.command(CommandId::SelectLine);
    s.typed("20");
    s.key(ENTER);
    assert_eq!(&s.text()[s.selection().0..s.selection().1], "L20");
    assert_eq!(s.tools.query(), "L1", "the find field is the same");
    assert!(s.tools.is_bar_open());
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("qe-tools-host-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}
fn open_mapped(path: &std::path::Path, wake: qe_tools::Wake) -> EditorView {
    let options = LoadOptions {
        force_read_only: true,
        ..Default::default()
    };
    let mut load = FileLoad::start(path, options, wake).unwrap();
    let start = Instant::now();
    let loaded = loop {
        if let Some(result) = load.poll() {
            break result;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "the load hung");
        std::thread::sleep(Duration::from_millis(1));
    };
    let LoadResult::Loaded(LoadedFile::ReadOnly(store)) = loaded else {
        panic!("the file must open read-only");
    };
    let mut view = EditorView::from_text("").unwrap();
    view.adopt(LoadedFile::ReadOnly(store)).unwrap();
    view
}

#[test]
fn a_mapped_file_finds_while_indexing_and_select_line_answers_honestly() {
    let path = temp_path("indexing.txt");
    let lines = 2_000_000;
    let text = "the quick brown fox jumps\r\n".repeat(lines);
    std::fs::write(&path, &text).unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let view = open_mapped(
        &path,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    );
    let was_complete_at_start = view.store_status().complete;
    let mut s = Setup::with_view(view);
    s.command(CommandId::SelectLine);
    // The last line, while the index may not have reached it.
    s.typed(&lines.to_string());
    s.key(ENTER);
    let answered_at_once = !s.tools.is_select_line_open();
    if !answered_at_once {
        assert!(
            s.view.warning().unwrap_or("").contains("still being read"),
            "it says why: {:?}",
            s.view.warning()
        );
        assert!(s.tools.alert_text().is_none(), "this is not a bad entry");
    }
    s.settle_select_line();
    assert!(
        !s.tools.is_select_line_open(),
        "it selected once the line was known"
    );
    let line_len = text.len() / lines;
    assert_eq!(s.selection(), (text.len() - line_len, text.len()));
    // Find on the same file, and its current match, and the count.
    s.find("FOX");
    assert_eq!(s.tools.match_count(), (lines as u64, true));
    // A read-only file refuses Replace All.
    assert!(!s.tools.bar().replace_all.enabled);
    println!("index complete at the start: {was_complete_at_start}");
    drop(s);
    std::fs::remove_file(&path).unwrap();
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

impl Setup {
    fn settle_select_line(&mut self) {
        let m = measure();
        let start = Instant::now();
        while self.tools.is_select_line_open() {
            self.view.poll_layout(&mut self.ctx);
            self.tools.poll(&mut self.view, &m, &mut self.ctx);
            self.ctx.requests.clear();
            assert!(start.elapsed() < Duration::from_secs(120), "no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[test]
fn transformations_keep_the_selection_and_are_one_undo_step() {
    let mut s = Setup::new("hello wORLD");
    s.select(0, 11);
    s.command(CommandId::UpperCase);
    assert_eq!(s.text(), "HELLO WORLD");
    assert_eq!(s.selection(), (0, 11));
    s.command(CommandId::LowerCase);
    assert_eq!(s.text(), "hello world");
    s.command(CommandId::Capitalize);
    assert_eq!(s.text(), "Hello World");
    assert_eq!(s.selection(), (0, 11));
    s.view.run(EditorCommand::Undo, &mut s.ctx);
    assert_eq!(s.text(), "hello world");
}

#[test]
fn a_transform_that_changes_length_selects_the_new_text() {
    let mut s = Setup::new("xstra\u{df}ey");
    s.select(1, 8);
    s.command(CommandId::UpperCase);
    assert_eq!(s.text(), "xSTRASSEy");
    assert_eq!(s.selection(), (1, 8));
    // With no selection, the word at the caret changes and the caret moves past it.
    let mut s = Setup::new("one two");
    s.select(5, 5);
    s.command(CommandId::UpperCase);
    assert_eq!(s.text(), "one TWO");
    assert_eq!(s.selection(), (7, 7));
}

#[test]
fn a_big_selection_transforms_on_a_worker_and_a_stale_one_is_refused() {
    let big = "abc\r\n".repeat(50_000);
    let mut s = Setup::new(&big);
    s.select(0, big.len());
    let before = s.tools.workers_started();
    s.command(CommandId::UpperCase);
    assert_eq!(s.tools.workers_started(), before + 1, "it went to a worker");
    assert!(!s.tools.is_idle());
    s.settle();
    assert_eq!(s.text(), big.to_uppercase());
    assert_eq!(s.selection(), (0, big.len()));
    s.view.run(EditorCommand::Undo, &mut s.ctx);
    assert_eq!(s.text(), big);

    // The user types while the worker runs: the result is refused, in words.
    s.select(0, big.len());
    s.command(CommandId::UpperCase);
    s.select(0, 0);
    s.view
        .replace_range(ByteOffset(0)..ByteOffset(0), "Z", &mut s.ctx);
    s.settle();
    assert_eq!(
        s.text(),
        format!("Z{big}"),
        "nothing was applied to moved text"
    );
    assert!(s.view.warning().unwrap_or("").contains("text changed"));
}

#[test]
fn complete_opens_with_the_words_and_takes_the_choice() {
    let mut s = Setup::new("hello help helmet say hel");
    let end = s.text().len();
    s.select(end, end);
    s.command(CommandId::Complete);
    s.settle();
    assert_eq!(
        s.tools.complete_words(),
        Some(["hello", "helmet", "help"].map(String::from).to_vec())
    );
    assert_eq!(s.tools.focus(), ToolsFocus::List);
    s.key(0x28);
    s.key(0x28); // Down twice
    s.key(ENTER);
    s.settle();
    assert_eq!(s.text(), "hello help helmet say help");
    assert_eq!(s.selection(), (26, 26));
    assert_eq!(s.tools.focus(), ToolsFocus::Editor);
    s.view.run(EditorCommand::Undo, &mut s.ctx);
    assert_eq!(s.text(), "hello help helmet say hel", "one undo step");
}

#[test]
fn complete_esc_keeps_the_text_and_no_word_beeps() {
    let mut s = Setup::new("hello say hel");
    s.select(13, 13);
    s.command(CommandId::Complete);
    s.settle();
    assert!(s.tools.complete_words().is_some());
    s.key(ESC);
    assert_eq!(s.text(), "hello say hel");
    assert!(s.tools.complete_words().is_none());

    // No word before the caret, as after a space.
    let mut s = Setup::new("hello ");
    s.select(6, 6);
    s.command(CommandId::Complete);
    s.settle();
    assert!(s.tools.take_beep());
    assert!(s.tools.complete_words().is_none());

    // No other word starts this way.
    let mut s = Setup::new("zeb");
    s.select(3, 3);
    s.command(CommandId::Complete);
    s.settle();
    assert!(s.tools.take_beep());
    assert_eq!(s.text(), "zeb");

    // In an empty document.
    let mut s = Setup::new("");
    s.command(CommandId::Complete);
    s.settle();
    assert!(s.tools.take_beep());
}

#[test]
fn complete_with_unicode_replaces_the_whole_prefix() {
    let mut s = Setup::new("\u{e9}cole e\u{301}x \u{e9}c");
    let end = s.text().len();
    s.select(end, end);
    s.command(CommandId::Complete);
    s.settle();
    assert_eq!(
        s.tools.complete_words(),
        Some(vec!["\u{e9}cole".to_string()])
    );
    s.key(ENTER);
    s.settle();
    assert_eq!(s.text(), "\u{e9}cole e\u{301}x \u{e9}cole");
}

#[test]
fn typing_a_letter_narrows_the_list_and_other_keys_close_it() {
    let mut s = Setup::new("hello help he");
    s.select(13, 13);
    s.command(CommandId::Complete);
    s.settle();
    assert_eq!(s.tools.complete_words().unwrap().len(), 2);
    // The letter goes on to the editor, then the list comes back narrower.
    assert_eq!(s.send(InputKind::Char('l')), qe_api::Handled::No);
    s.view.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Char('l'),
        },
        &mut s.ctx,
    );
    s.settle();
    s.typed("");
    assert_eq!(s.text(), "hello help hel");
    assert_eq!(s.tools.complete_words().unwrap().len(), 2);
    assert_eq!(s.send(InputKind::Char(' ')), qe_api::Handled::No);
    assert!(s.tools.complete_words().is_none());
}

#[test]
fn a_list_for_old_text_does_not_open() {
    let mut s = Setup::new("hello he");
    s.select(8, 8);
    s.command(CommandId::Complete);
    s.view
        .replace_range(ByteOffset(0)..ByteOffset(0), "x", &mut s.ctx);
    s.settle();
    assert!(s.tools.complete_words().is_none());
}

#[test]
fn the_toggles_flip_and_the_menu_reads_them() {
    let mut s = Setup::new("x");
    assert_eq!(s.tools.substitutions(), Substitutions::default());
    assert_eq!(
        s.tools.command_state(CommandId::SmartCopyPaste, &s.view),
        Some((true, true))
    );
    assert_eq!(
        s.tools.command_state(CommandId::SmartQuotes, &s.view),
        Some((true, false))
    );
    s.command(CommandId::SmartQuotes);
    s.command(CommandId::SmartDashes);
    s.command(CommandId::SmartCopyPaste);
    let now = s.tools.substitutions();
    assert!(now.quotes && now.dashes && !now.smart_copy);
    assert_eq!(s.tools.command_state(CommandId::Save, &s.view), None);
    assert!(!ToolsHost::takes_command(CommandId::Save));
    assert_eq!(
        s.tools
            .run_command(CommandId::Save, &mut s.view, &measure(), &mut s.ctx),
        qe_api::Handled::No
    );
    for command in qe_api::CommandId::ALL {
        assert_eq!(
            ToolsHost::takes_command(*command),
            s.tools.command_state(*command, &s.view).is_some(),
            "{command:?}"
        );
    }
}

#[test]
fn switching_documents_moves_the_search() {
    let mut s = Setup::new("cat cat");
    s.find("cat");
    assert_eq!(s.tools.match_count(), (2, true));
    let m = measure();
    let mut other = EditorView::from_text("cat dog").unwrap();
    other.layout(area(800.0, 568.0), &m);
    let mut c = ctx();
    s.tools.view_switched(Some(&mut s.view), &mut other, &mut c);
    assert!(s.marks(DecorationKind::FindMatch).is_empty());
    assert!(s.marks(DecorationKind::CurrentFindMatch).is_empty());
    let start = Instant::now();
    while !s.tools.match_count().1 || other.decorations(DecorationKind::FindMatch).is_empty() {
        other.poll_layout(&mut c);
        s.tools.poll(&mut other, &m, &mut c);
        assert!(start.elapsed() < Duration::from_secs(30), "no result");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(s.tools.match_count(), (1, true));
}

#[test]
fn the_wake_runs_when_a_worker_has_results() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let mut s = Setup::new("cat cat");
    s.tools.set_wake(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    s.find("cat");
    assert!(wakes.load(Ordering::SeqCst) > 0);
}

#[test]
fn jump_to_selection_centers_it() {
    let text = "line\n".repeat(2000);
    let mut s = Setup::new(&text);
    let at = 5 * 1000;
    s.view.run_host(
        qe_editor::HostCommand::Select {
            range: ByteOffset(at)..ByteOffset(at + 4),
            reveal: true,
        },
        &mut s.ctx,
    );
    s.settle_view();
    s.command(CommandId::JumpToSelection);
    s.settle();
    s.settle_view();
    let rects = s.view.range_rects(ByteOffset(at)..ByteOffset(at)).unwrap();
    let y = rects[0].rect.y.0;
    let middle = s.view.text_area().y.0 + s.view.text_area().height.0 / 2.0;
    assert!((y - middle).abs() < 30.0, "at {y}, the middle is {middle}");
}

#[test]
fn widgets_name_the_bar_and_the_panel() {
    let mut s = Setup::new("x");
    s.command(CommandId::FindReplace);
    s.command(CommandId::SelectLine);
    let widgets = s.tools.widgets();
    assert_eq!(widgets.len(), 2);
    assert_eq!(widgets[0].name, "Find");
    let names: Vec<&str> = widgets[0]
        .children
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    for want in [
        "Find",
        "Previous match",
        "Next match",
        "Options",
        "Done",
        "Replace",
        "Replace All",
    ] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    assert_eq!(widgets[1].name, "Select Line");
    assert!(
        widgets[1]
            .description
            .as_deref()
            .unwrap()
            .contains("(e.g., 10-20)")
    );
}

#[test]
fn the_count_keeps_coming_while_the_search_runs() {
    // Each poll reads the job, which lets the worker wake the host again.
    let text = "needle ".repeat(3_000_000);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let mut s = Setup::new(&text);
    s.tools.set_wake(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    s.command(CommandId::Find);
    s.typed("needle");
    let m = measure();
    let mut seen_part_way = false;
    let start = Instant::now();
    while !s.tools.match_count().1 {
        s.tools.poll(&mut s.view, &m, &mut s.ctx);
        let (count, done) = s.tools.match_count();
        seen_part_way |= !done && count > 0;
        assert!(start.elapsed() < Duration::from_secs(120));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(s.tools.match_count(), (3_000_000, true));
    assert!(seen_part_way, "a count must show before the end");
    assert!(
        wakes.load(Ordering::SeqCst) >= 3,
        "the first match, more, and the end"
    );
}

#[test]
fn prevent_editing_while_the_bar_is_open_disables_replace() {
    let mut s = Setup::new("aa");
    s.command(CommandId::FindReplace);
    assert!(s.tools.bar().replace_all.enabled);
    let m = measure();
    s.tools
        .set_query_for_test("aa", &mut s.view, &m, &mut s.ctx);
    s.view.set_prevent_editing(true);
    s.settle();
    assert!(!s.tools.bar().replace_all.enabled);
}
