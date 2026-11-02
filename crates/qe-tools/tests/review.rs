//! Cases from the two reviews of M12. Each names its finding. `Window` follows `WIRING.md`
//! line for line, so that these also prove the wiring example works as written.
use qe_api::{
    ByteOffset, CommandId, Dip, Handled, InputEvent, InputKind, Modifiers, MouseButton, Point,
    Rect, View, ViewContext, Widget, testing::FixedTextMeasurer,
};
use qe_editor::{DecorationKind, EditorView};
use qe_tools::{
    BarStop, FindOptions, MatchKind, Matcher, PanelStop, ToolsFocus, ToolsHost, is_cluster_edge,
    transform_text,
};
use qe_ui::MemoryClipboard;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn measure() -> FixedTextMeasurer {
    FixedTextMeasurer {
        char_width: Dip(7.0),
        height: Dip(16.0),
        baseline: Dip(12.0),
    }
}
fn area() -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(800.0),
        height: Dip(600.0),
    }
}
const ENTER: u16 = 0x0d;
const ESC: u16 = 0x1b;
const TAB: u16 = 0x09;
const BACK: u16 = 0x08;

struct Window {
    view: EditorView,
    tools: ToolsHost,
    ctx: ViewContext,
}
impl Window {
    fn new(text: &str) -> Self {
        Self::with_view(EditorView::from_text(text).unwrap())
    }
    fn with_view(view: EditorView) -> Self {
        let mut w = Self {
            view,
            tools: ToolsHost::with_clipboard(MemoryClipboard::shared()),
            ctx: ViewContext::default(),
        };
        w.relayout();
        w.settle_view();
        w
    }
    fn relayout(&mut self) {
        let m = measure();
        let rest = self.tools.layout(area(), &m);
        self.view.layout(rest, &m);
    }
    /// One input event, as `WIRING.md` has it. Commands come from the test by `command`.
    fn event(&mut self, kind: InputKind) -> Handled {
        let m = measure();
        let event = InputEvent { time_us: 0, kind };
        let handled = self
            .tools
            .handle_input(&event, &mut self.view, &m, &mut self.ctx);
        if handled == Handled::No && self.tools.event_for_editor(&event) {
            self.view.handle(&event, &mut self.ctx);
        }
        self.after();
        handled
    }
    fn after(&mut self) {
        if self.tools.take_layout_changed() {
            self.relayout();
        }
    }
    fn command(&mut self, command: CommandId) {
        let m = measure();
        self.tools
            .run_command(command, &mut self.view, &m, &mut self.ctx);
        self.after();
    }
    fn poll(&mut self) {
        let m = measure();
        self.view.poll_layout(&mut self.ctx);
        self.tools.poll(&mut self.view, &m, &mut self.ctx);
        self.ctx.requests.clear();
        self.after();
    }
    fn settle_view(&mut self) {
        let start = Instant::now();
        self.view.poll_layout(&mut self.ctx);
        while self.view.pending_layout() {
            assert!(start.elapsed() < Duration::from_secs(60), "no layout");
            std::thread::sleep(Duration::from_millis(1));
            self.view.poll_layout(&mut self.ctx);
        }
    }
    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.poll();
            let quiet = if self.tools.is_bar_open() {
                self.tools.is_quiet()
                    && (self.tools.match_count().1 || self.tools.query().is_empty())
            } else {
                self.tools.is_idle()
            };
            if quiet && !self.view.pending_layout() {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "did not settle");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn key(&mut self, virtual_key: u16) -> Handled {
        self.event(InputKind::KeyDown {
            virtual_key,
            modifiers: Modifiers::default(),
            repeat: false,
        })
    }
    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.event(InputKind::Char(c));
        }
    }
    fn click_at(&mut self, at: Point) -> Handled {
        let handled = self.event(InputKind::MouseDown {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        self.event(InputKind::MouseUp {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        handled
    }
    fn click(&mut self, bounds: Rect) -> Handled {
        self.click_at(Point {
            x: Dip(bounds.x.0 + bounds.width.0 / 2.0),
            y: Dip(bounds.y.0 + bounds.height.0 / 2.0),
        })
    }
    fn click_text(&mut self) -> Handled {
        self.click_at(Point {
            x: Dip(400.0),
            y: Dip(500.0),
        })
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
    fn edit(&mut self, a: usize, b: usize, text: &str) {
        self.view
            .replace_range(ByteOffset(a)..ByteOffset(b), text, &mut self.ctx);
    }
    fn current(&self) -> Option<(usize, usize)> {
        self.tools.current_match().map(|r| (r.start.0, r.end.0))
    }
    fn find(&mut self, query: &str) {
        self.command(CommandId::Find);
        self.typed(query);
        self.settle();
    }
    /// The bar with Replace showing, the query found, and the replacement typed.
    fn replace_bar(&mut self, query: &str, with: &str) {
        self.command(CommandId::FindReplace);
        self.typed(with);
        self.command(CommandId::Find);
        self.typed(query);
        self.settle();
    }
    fn focused_widgets(&self) -> usize {
        fn count(w: &Widget) -> usize {
            usize::from(w.states.focused) + w.children.iter().map(count).sum::<usize>()
        }
        self.tools.widgets().iter().map(count).sum::<usize>()
    }
    fn view_has_focus(&self) -> bool {
        self.view.widgets().unwrap().states.focused
    }
}

// ---- Finding 1 and Astra 1: Replace All keeps its own request ----

#[test]
fn f1_a_changed_query_cancels_replace_all() {
    // With an edit landing too, and with none.
    for edit in [true, false] {
        let mut w = Window::new("foo bar foo bar");
        w.replace_bar("foo", "x");
        let all = w.tools.bar().replace_all.bounds;
        w.click(all);
        w.command(CommandId::Find);
        w.key(BACK);
        w.key(BACK);
        w.key(BACK);
        w.typed("bar");
        if edit {
            w.edit(0, 0, "Q");
        }
        w.settle();
        let want = if edit {
            "Qfoo bar foo bar"
        } else {
            "foo bar foo bar"
        };
        assert_eq!(w.text(), want, "nothing was replaced");
        assert_eq!(w.tools.last_replaced(), None);
    }
}

#[test]
fn f1_a_changed_option_cancels_replace_all() {
    let mut w = Window::new("foo FOO");
    w.replace_bar("foo", "x");
    let all = w.tools.bar().replace_all.bounds;
    w.click(all);
    let options = w.tools.bar().options.bounds;
    w.click(options);
    w.key(ENTER); // Ignore Case off
    w.settle();
    assert_eq!(w.text(), "foo FOO");
}

#[test]
fn f1_a_rerun_uses_what_was_asked_not_the_fields() {
    let mut w = Window::new("foo bar foo");
    w.replace_bar("foo", "x");
    let all = w.tools.bar().replace_all.bounds;
    w.click(all);
    // The replacement field changes, which does not end the request, and an edit lands.
    let field = w.tools.bar().replace.bounds;
    w.click(field);
    w.typed("yyy");
    w.edit(0, 0, "foo ");
    w.settle();
    assert_eq!(w.text(), "x x bar x");
    assert_eq!(w.tools.last_replaced(), Some(3));
}

// ---- Finding 4 and Astra 2: Replace uses only the current query's match ----

#[test]
fn f4_replace_after_the_query_changed_does_not_replace_the_old_match() {
    let mut w = Window::new("foo bar");
    w.replace_bar("foo", "REPL");
    assert_eq!(w.current(), Some((0, 3)));
    w.typed("zzz");
    assert_eq!(w.current(), None, "the old match is gone at once");
    assert!(
        w.view
            .decorations(DecorationKind::CurrentFindMatch)
            .is_empty()
    );
    let one = w.tools.bar().replace_one.bounds;
    w.click(one);
    assert_eq!(w.text(), "foo bar");

    // To a query that does match, before its search returns.
    let mut w = Window::new("cat dog");
    w.replace_bar("cat", "X");
    w.key(BACK);
    w.key(BACK);
    w.key(BACK);
    w.typed("dog");
    let one = w.tools.bar().replace_one.bounds;
    w.click(one);
    w.settle();
    assert!(w.text().contains("cat"), "{}", w.text());
}

// ---- Finding 2: closing the bar ----

#[test]
fn f2_closing_leaves_the_selection_where_the_user_put_it() {
    let mut w = Window::new("one needle two three four five");
    w.find("needle");
    assert_eq!(w.selection(), (4, 10));
    w.select(25, 25);
    w.settle();
    let done = w.tools.bar().done.bounds;
    w.click(done);
    w.settle();
    assert_eq!(w.selection(), (25, 25));
}

#[test]
fn f2_closing_keeps_a_match_selected() {
    let mut w = Window::new("1 owl 2 owl");
    w.find("owl");
    w.key(ENTER);
    w.settle();
    assert_eq!(w.selection(), (8, 11));
    w.key(ESC);
    w.settle();
    assert_eq!(w.selection(), (8, 11));
}

#[test]
fn f2_a_step_with_the_bar_closed_leaves_nothing_stale() {
    let mut w = Window::new("zero one two needle three four");
    w.find("needle");
    w.key(ESC);
    w.settle();
    w.select(0, 0);
    w.command(CommandId::FindNext);
    w.settle();
    assert_eq!(w.current(), None, "a closed bar stands on nothing");
    w.edit(0, 9, "");
    w.settle();
    w.select(0, 0);
    w.command(CommandId::Find);
    w.settle();
    w.key(ESC);
    w.settle();
    assert_eq!(w.selection(), (0, 0));
}

// ---- Finding 3: nothing runs at idle ----

fn assert_idle_and_marks_show(w: &mut Window) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        w.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(w.tools.is_quiet(), "a worker still runs");
    let (workers, marks) = (w.tools.workers_started(), w.tools.marks_set());
    for _ in 0..200 {
        w.poll();
    }
    assert_eq!(
        w.tools.workers_started(),
        workers,
        "workers started at idle"
    );
    assert_eq!(w.tools.marks_set(), marks, "marks set at idle");
    let visible = w.view.visible_range().unwrap();
    let on_screen = w
        .view
        .decorations(DecorationKind::FindMatch)
        .iter()
        .filter(|m| m.end > visible.start && m.start < visible.end)
        .count();
    assert!(on_screen > 0, "no mark on screen in {visible:?}");
}

#[test]
fn f3_a_dense_query_scrolled_down_shows_marks_and_rests() {
    for line in ["a".repeat(99), "0,".repeat(49), "a".to_string()] {
        let text = format!("{line}\n").repeat(400_000 / (line.len() + 1));
        let mut w = Window::new(&text);
        let query = &line[..1];
        w.find(query);
        let far = text.len() / 2;
        w.view.run_host(
            qe_editor::HostCommand::Select {
                range: ByteOffset(far)..ByteOffset(far),
                reveal: true,
            },
            &mut w.ctx,
        );
        w.settle_view();
        assert_idle_and_marks_show(&mut w);
    }
}

#[test]
fn f3_more_matches_on_screen_than_marks_rests_too() {
    // A very big view shows more matches than the limit, so a scan always stops inside it.
    let text = format!("{}\n", "a".repeat(400)).repeat(1000);
    let mut w = Window::with_view(EditorView::from_text(&text).unwrap());
    w.find("a");
    // After the bar has opened, as opening it lays the view out again.
    let m = measure();
    let big = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(6000.0),
        height: Dip(4000.0),
    };
    w.tools.layout(big, &m);
    w.view.layout(big, &m);
    w.settle_view();
    let visible = w.view.visible_range().unwrap();
    assert!(
        visible.end.0 - visible.start.0 > qe_tools::MARK_LIMIT,
        "the view holds only {visible:?}"
    );
    assert_idle_and_marks_show(&mut w);
    assert!(w.view.decorations(DecorationKind::FindMatch).len() <= qe_tools::MARK_LIMIT);
}

// ---- Finding 5: Select Line is not modal ----

#[test]
fn f5_ctrl_f_moves_the_keyboard_and_only_one_widget_says_focused() {
    let mut w = Window::new("one\ntwo\nthree\n");
    w.command(CommandId::SelectLine);
    assert_eq!(w.tools.focus(), ToolsFocus::SelectLine);
    w.command(CommandId::Find);
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find));
    assert!(w.tools.is_select_line_open(), "the panel stays");
    assert_eq!(w.focused_widgets(), 1);
    w.typed("two");
    w.settle();
    assert_eq!(w.tools.bar().find.text, "two");
    assert_eq!(w.tools.panel().field.text, "");
    assert_eq!(w.selection(), (4, 7));
}

#[test]
fn f5_a_click_in_the_text_moves_the_keyboard_and_the_panel_stays() {
    let mut w = Window::new("one two");
    w.command(CommandId::SelectLine);
    let handled = w.click_text();
    assert_eq!(handled, Handled::No, "the click goes on to the text");
    assert!(w.tools.is_select_line_open());
    assert_eq!(w.tools.focus(), ToolsFocus::Editor);
    assert_eq!(w.focused_widgets(), 0);
    // Typing goes to the text, as the wiring example sends it.
    w.event(InputKind::Char('Z'));
    assert!(w.text().contains('Z'));
    assert_eq!(w.tools.panel().field.text, "");
}

#[test]
fn f5_a_click_in_the_field_moves_the_keyboard_there() {
    let mut w = Window::new("one\ntwo");
    w.command(CommandId::SelectLine);
    w.click_text();
    let field = w.tools.panel().field.bounds;
    w.click(field);
    assert_eq!(w.tools.focus(), ToolsFocus::SelectLine);
    w.typed("2");
    assert_eq!(w.tools.panel().field.text, "2");

    // After Tab to the button too.
    w.key(TAB);
    assert_eq!(w.tools.panel().keyboard, Some(PanelStop::Select));
    let field = w.tools.panel().field.bounds;
    w.click(field);
    assert_eq!(w.tools.panel().keyboard, Some(PanelStop::Field));
    w.typed("7");
    assert_eq!(
        w.tools.panel().field.text.len(),
        2,
        "the 7 went to the field"
    );
    assert_eq!(w.focused_widgets(), 1);
}

#[test]
fn f5_the_panel_has_a_close_button_and_the_macs_content_size() {
    let mut w = Window::new("one");
    w.command(CommandId::SelectLine);
    assert_eq!(w.tools.panel().bounds.width.0, 253.0);
    assert_eq!(w.tools.panel().bounds.height.0, 62.0);
    assert!(
        w.tools.panel().frame.height.0 > 62.0,
        "a title row sits above"
    );
    let close = w.tools.panel().close.bounds;
    w.click(close);
    assert!(!w.tools.is_select_line_open());
    assert_eq!(w.tools.focus(), ToolsFocus::Editor);
    assert!(w.tools.widgets().is_empty());
}

#[test]
fn f5_a_click_in_the_bar_moves_the_keyboard_from_the_panel_and_back() {
    let mut w = Window::new("one");
    w.command(CommandId::Find);
    w.command(CommandId::SelectLine);
    assert_eq!(w.tools.focus(), ToolsFocus::SelectLine);
    assert_eq!(w.focused_widgets(), 1);
    let find = w.tools.bar().find.bounds;
    w.click(find);
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find));
    assert_eq!(w.focused_widgets(), 1);
    let field = w.tools.panel().field.bounds;
    w.click(field);
    assert_eq!(w.tools.focus(), ToolsFocus::SelectLine);
    assert_eq!(w.focused_widgets(), 1);
}

// ---- Finding 7: the wiring example, followed exactly ----

#[test]
fn f7_closing_gives_the_room_back() {
    let mut w = Window::new("hello");
    w.command(CommandId::Find);
    assert_eq!(w.view.text_area().y.0, 32.0);
    w.key(ESC);
    assert_eq!(w.view.text_area().y.0, 0.0, "Esc lays the text out again");
    w.command(CommandId::FindReplace);
    assert_eq!(w.view.text_area().y.0, 64.0);
    let done = w.tools.bar().done.bounds;
    w.click(done);
    assert_eq!(w.view.text_area().y.0, 0.0, "Done too");
}

#[test]
fn f7_the_wheel_reaches_the_text_while_the_bar_has_the_keys() {
    let text = "line\n".repeat(2000);
    let mut w = Window::new(&text);
    w.command(CommandId::Find);
    let before = w.view.scroll_offset();
    w.event(InputKind::Wheel {
        at: Point {
            x: Dip(300.0),
            y: Dip(300.0),
        },
        delta_x: 0.0,
        delta_y: -120.0,
        modifiers: Modifiers::default(),
    });
    w.settle_view();
    assert!(w.view.scroll_offset() > before, "the text scrolled");
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find));
}

#[test]
fn f7_focus_and_blur_reach_the_view() {
    let mut w = Window::new("hello");
    w.view.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Focus,
        },
        &mut w.ctx,
    );
    assert!(w.view_has_focus());
    w.command(CommandId::Find);
    assert!(!w.view_has_focus(), "only the field's caret blinks");
    w.key(ESC);
    assert!(w.view_has_focus());
    w.command(CommandId::SelectLine);
    assert!(!w.view_has_focus());
    w.click_text();
    assert!(w.view_has_focus());
}

#[test]
fn f7_the_window_losing_the_keyboard_reaches_the_control_that_has_it() {
    let mut w = Window::new("hello");
    w.command(CommandId::Find);
    assert!(w.tools.bar().find.focused);
    w.event(InputKind::Blur);
    assert!(!w.tools.bar().find.focused);
    w.event(InputKind::Focus);
    assert!(w.tools.bar().find.focused);
    assert!(!w.view_has_focus());
}

// ---- Finding 10: no change, no undo step ----

#[test]
fn f10_a_transform_with_no_change_is_no_edit() {
    for (text, at) in [("hello world", 5), ("ABC", 1), ("hello  ", 6)] {
        let mut w = Window::new(text);
        w.select(at, at);
        let revision = w.view.revision();
        w.command(CommandId::UpperCase);
        w.settle();
        assert_eq!(w.text(), text);
        assert_eq!(w.view.revision(), revision, "{text:?}: no edit");
        assert_eq!(w.selection(), (at, at), "{text:?}: the caret stays");
    }
    // With a selection too.
    let mut w = Window::new("ABC");
    w.select(0, 3);
    let revision = w.view.revision();
    w.command(CommandId::UpperCase);
    assert_eq!(w.view.revision(), revision);
    assert_eq!(w.selection(), (0, 3));
}

#[test]
fn f10_a_big_selection_with_no_change_is_no_edit() {
    let text = "ABC\n".repeat(50_000);
    let mut w = Window::new(&text);
    w.select(0, text.len());
    let revision = w.view.revision();
    w.command(CommandId::UpperCase);
    w.settle();
    assert_eq!(w.view.revision(), revision);
    assert_eq!(w.selection(), (0, text.len()));
}

// ---- Finding 11 ----

#[test]
fn f11_prevent_editing_moves_the_keyboard_off_replace() {
    let mut w = Window::new("aa");
    w.command(CommandId::FindReplace);
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Replace));
    w.view.set_prevent_editing(true);
    w.settle();
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find));
}

// ---- Finding 12 ----

#[test]
fn f12_complete_with_a_field_focused_does_nothing() {
    let mut w = Window::new("hello he");
    w.select(8, 8);
    w.command(CommandId::Find);
    let workers = w.tools.workers_started();
    w.command(CommandId::Complete);
    assert_eq!(w.tools.workers_started(), workers);
    assert!(w.tools.complete_words().is_none());
}

#[test]
fn f12_a_letter_the_editor_did_not_take_leaves_nothing_waiting() {
    let mut w = Window::new("hello he");
    w.select(8, 8);
    w.command(CommandId::Complete);
    w.settle();
    assert!(w.tools.complete_words().is_some());
    let event = InputEvent {
        time_us: 0,
        kind: InputKind::Char('l'),
    };
    let m = measure();
    // The host drops this letter, so no edit comes.
    w.tools.handle_input(&event, &mut w.view, &m, &mut w.ctx);
    assert!(!w.tools.is_idle());
    w.poll();
    assert!(w.tools.is_idle(), "nothing keeps polling");
}

// ---- Finding 13 ----

#[test]
fn f13_the_search_start_follows_edits() {
    let mut w = Window::new("cat cat");
    w.select(4, 4);
    w.command(CommandId::Find);
    w.edit(0, 0, "cat ");
    w.poll();
    w.typed("cat");
    w.settle();
    assert_eq!(
        w.selection(),
        (8, 11),
        "from the same text, which has moved"
    );
    // A delete before it moves it back.
    let mut w = Window::new("xxxxx cat cat");
    w.select(10, 10);
    w.command(CommandId::Find);
    w.edit(0, 6, "");
    w.poll();
    w.typed("cat");
    w.settle();
    assert_eq!(w.selection(), (4, 7));
}

// ---- Finding 14 ----

#[test]
fn f14_a_new_step_replaces_one_that_waits_for_the_index() {
    let mut w = Window::new("cat dog");
    w.find("cat");
    w.tools
        .set_step_waiting_for_test(qe_tools::Direction::Forward, ByteOffset(0)..ByteOffset(0));
    assert!(w.tools.is_step_waiting_for_test());
    w.command(CommandId::FindNext);
    assert!(!w.tools.is_step_waiting_for_test());
}

// ---- Astra 3: an edit beside a match ----

#[test]
fn a3_an_edit_beside_a_full_word_match_ends_it() {
    let mut w = Window::new("cat dog");
    w.replace_bar("cat", "X");
    let options = w.tools.bar().options.bounds;
    w.click(options);
    for _ in 0..4 {
        w.key(0x28);
    }
    w.key(ENTER); // Full Word
    w.settle();
    assert_eq!(w.tools.find_options().kind, MatchKind::FullWord);
    assert_eq!(w.current(), Some((0, 3)));
    w.edit(3, 3, "s");
    w.settle();
    assert_eq!(w.current(), None);
    assert!(
        w.view
            .decorations(DecorationKind::CurrentFindMatch)
            .is_empty()
    );
    w.select(0, 3);
    let one = w.tools.bar().replace_one.bounds;
    w.click(one);
    w.settle();
    assert_eq!(w.text(), "cats dog", "Replace does not touch it");
}

#[test]
fn a3_replace_checks_the_match_even_before_a_poll() {
    let mut w = Window::new("e dog");
    w.replace_bar("e", "X");
    assert_eq!(w.current(), Some((0, 1)));
    // A mark joins the match, and no poll has seen the edit.
    w.edit(1, 1, "\u{301}");
    w.select(0, 1);
    let one = w.tools.bar().replace_one.bounds;
    w.click(one);
    w.settle();
    assert_eq!(w.text(), "e\u{301} dog");
}

// ---- Astra 7: a result for the text as it stands ----

#[test]
fn a7_replace_moves_the_next_match_only_once() {
    let mut w = Window::new("cat cat");
    w.replace_bar("cat", "elephant");
    assert_eq!(w.current(), Some((0, 3)));
    let one = w.tools.bar().replace_one.bounds;
    w.click(one);
    // The next match's worker ends before the host looks at the edit.
    std::thread::sleep(Duration::from_millis(50));
    w.settle();
    assert_eq!(w.text(), "elephant cat");
    assert_eq!(w.selection(), (9, 12));
    assert_eq!(w.current(), Some((9, 12)));
}

// ---- Astra 11 ----

#[test]
fn a11_capitalize_keeps_the_final_sigma() {
    let cases = [
        ("\u{39f}\u{3a3}", "\u{39f}\u{3c2}"),
        ("\u{39f}\u{3a3} x", "\u{39f}\u{3c2} X"),
        ("\u{3a3}\u{39f}\u{3a3}", "\u{3a3}\u{3bf}\u{3c2}"),
        ("\u{3a3}", "\u{3a3}"),
        ("\u{3a3}\u{39f}", "\u{3a3}\u{3bf}"),
    ];
    for (text, want) in cases {
        let got = transform_text(text, qe_tools::Transform::Capitalize).unwrap();
        assert_eq!(got, want, "{text:?}");
    }
}

// ---- Tests that passed with the bug: Astra's section and Opus's table ----

#[test]
fn survivor_the_current_match_moves_with_edits_before_it() {
    let mut w = Window::new("aaa cat bbb");
    w.find("cat");
    assert_eq!(w.current(), Some((4, 7)));
    w.edit(0, 0, "x");
    w.settle();
    assert_eq!(w.current(), Some((5, 8)), "a longer text before it");
    w.edit(0, 3, "");
    w.settle();
    assert_eq!(w.current(), Some((2, 5)), "a shorter text before it");
    assert_eq!(
        w.view.decorations(DecorationKind::CurrentFindMatch),
        vec![ByteOffset(2)..ByteOffset(5)]
    );
}

#[test]
fn survivor_an_insertion_at_the_end_of_a_match_keeps_it() {
    let mut w = Window::new("cat dog");
    w.find("cat");
    w.edit(3, 3, "!");
    w.settle();
    assert_eq!(w.current(), Some((0, 3)));
    w.edit(0, 0, "!");
    w.settle();
    assert_eq!(w.current(), Some((1, 4)));
}

#[test]
fn survivor_a_step_result_for_old_text_is_not_used() {
    let mut w = Window::new("xxxx dog");
    w.select(0, 4);
    w.command(CommandId::UseSelectionForFind);
    w.command(CommandId::Find);
    w.key(BACK);
    w.typed("dog");
    w.settle();
    w.key(ESC);
    w.settle();
    w.select(0, 0);
    w.command(CommandId::FindNext);
    // The edit lands before the host takes the worker's result, for offsets 5..8.
    w.edit(0, 0, "yy");
    w.settle();
    assert_eq!(w.selection(), (7, 10), "the match in the text as it stands");
}

#[test]
fn survivor_replace_all_waits_behind_keys_that_wait() {
    let mut w = Window::with_view(EditorView::from_text(&"\u{4e2d}".repeat(400)).unwrap());
    let m = measure();
    let narrow = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(300.0),
        height: Dip(600.0),
    };
    w.tools.layout(narrow, &m);
    w.view.layout(narrow, &m);
    w.settle_view();
    w.replace_bar("\u{4e2d}", "x");
    // A cold view: End waits for a layout that a paused worker does not give.
    let mut cold = EditorView::from_text(&"\u{4e2d}".repeat(400)).unwrap();
    cold.layout(narrow, &m);
    cold.test_pause_workers(true);
    let end = InputEvent {
        time_us: 0,
        kind: InputKind::KeyDown {
            virtual_key: 0x23,
            modifiers: Modifiers::default(),
            repeat: false,
        },
    };
    cold.handle(&end, &mut w.ctx);
    assert_eq!(cold.queued_input_count(), 1);
    w.tools
        .view_switched(Some(&mut w.view), &mut cold, &mut w.ctx);
    w.tools.press_replace_all_for_test(&mut cold);
    let start = Instant::now();
    // Until the job has finished, as a look any sooner shows nothing about waiting.
    while w.tools.replace_is_ready_for_test() && !w.tools.replace_waits_for_test() {
        w.tools.poll(&mut cold, &m, &mut w.ctx);
        assert!(start.elapsed() < Duration::from_secs(30), "no result");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        w.tools.replace_waits_for_test(),
        "it was applied with keys waiting"
    );
    assert_eq!(cold.queued_input_count(), 1);
    assert!(cold.text().unwrap().starts_with('\u{4e2d}'), "it waits");
    cold.test_pause_workers(false);
    let start = Instant::now();
    while w.tools.replace_is_ready_for_test() {
        cold.poll_layout(&mut w.ctx);
        w.tools.poll(&mut cold, &m, &mut w.ctx);
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "it stayed waiting"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(cold.text().unwrap(), "x".repeat(400));
}

// ---- Independent checks of the search rule ----

#[test]
fn cluster_table_keeps_marks_of_more_scripts() {
    let any = FindOptions::default();
    for (base, mark) in [
        ('\u{b15}', '\u{b3e}'),   // Oriya
        ('\u{b95}', '\u{bbe}'),   // Tamil
        ('\u{c15}', '\u{c3e}'),   // Telugu
        ('\u{c95}', '\u{cbe}'),   // Kannada
        ('\u{d15}', '\u{d3e}'),   // Malayalam
        ('\u{f40}', '\u{f71}'),   // Tibetan
        ('\u{1780}', '\u{17b6}'), // Khmer
        ('\u{1000}', '\u{102d}'), // Myanmar
        ('\u{915}', '\u{93e}'),   // Devanagari
    ] {
        let text = format!("{base}{mark}");
        let query = base.to_string();
        assert!(qe_tools::find_text(&text, &query, any).is_empty(), "{base}");
        assert!(!is_cluster_edge(&text, base.len_utf8()));
    }
}

#[test]
fn still_matches_reads_the_neighbors() {
    let view = EditorView::from_text("cats cat e\u{301}").unwrap();
    let snapshot = view.worker_snapshot();
    let never = AtomicBool::new(false);
    let word = FindOptions {
        kind: MatchKind::FullWord,
        ..FindOptions::default()
    };
    let cat = Matcher::new("cat", word).unwrap();
    let r = |a: usize, b: usize| ByteOffset(a)..ByteOffset(b);
    assert!(!cat.still_matches(snapshot.as_ref(), r(0, 3), &never));
    assert!(cat.still_matches(snapshot.as_ref(), r(5, 8), &never));
    let e = Matcher::new("e", FindOptions::default()).unwrap();
    assert!(!e.still_matches(snapshot.as_ref(), r(9, 10), &never));
}
