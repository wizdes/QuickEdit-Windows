//! Cases from the reviews of M12. Each names its finding. `Window` follows the event path, the
//! layout, the key pairs and `Resize` in `WIRING.md`. It has no key table or menu, so it calls
//! `run_command` directly.
use qe_api::{
    ByteOffset, CommandId, Dip, Handled, InputEvent, InputKind, Modifiers, MouseButton, Point,
    Rect, View, ViewContext, Widget, testing::FixedTextMeasurer,
};
use qe_editor::{DecorationKind, EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
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
    size: Rect,
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
            size: area(),
        };
        w.relayout();
        w.settle_view();
        w
    }
    fn relayout(&mut self) {
        let m = measure();
        let rest = self.tools.layout(self.size, &m);
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
    /// A window resize, as `WIRING.md` has it: through the event path, then a new layout.
    fn resize(&mut self, width: f32, height: f32) {
        self.size.width = Dip(width);
        self.size.height = Dip(height);
        let handled = self.event(InputKind::Resize {
            width: Dip(width),
            height: Dip(height),
        });
        assert_eq!(handled, Handled::No, "Resize is for the host");
        self.relayout();
    }
    /// A key as Windows sends it: `KeyDown`, then its `Char`, then `KeyUp`.
    fn pair(&mut self, virtual_key: u16, c: char) {
        self.key(virtual_key);
        self.event(InputKind::Char(c));
        self.event(InputKind::KeyUp {
            virtual_key,
            modifiers: Modifiers::default(),
        });
    }
    /// Polls until no job runs, for a file whose index has stopped.
    fn settle_stopped(&mut self) {
        let start = Instant::now();
        while !self.tools.is_quiet() {
            self.poll();
            assert!(start.elapsed() < Duration::from_secs(20), "it still waits");
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
        let field = self.tools.bar().find.bounds;
        self.click(field);
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

// ---- Finding 1 and second review 1: Replace All keeps its own request ----

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

// ---- Finding 4 and second review 2: Replace uses only the current query's match ----

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
    while start.elapsed() < Duration::from_millis(300) || !w.tools.is_quiet() {
        w.poll();
        // Time for the work on a busy PC. A scan that started again and again never gets quiet.
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "a worker still runs"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
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

// ---- Second review 3: an edit beside a match ----

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

// ---- Second review 7: a result for the text as it stands ----

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

// ---- Second review 11 ----

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

// ---- Tests that passed with the bug: the reviews' lists ----

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

// ---- Second review, finding 2: the Complete list with real key pairs ----

const SPACE: u16 = 0x20;
const LEFT: u16 = 0x25;

fn with_complete_list() -> Window {
    let mut w = Window::new("foobar fo");
    w.select(9, 9);
    w.command(CommandId::Complete);
    w.settle();
    assert_eq!(w.tools.complete_words().unwrap(), ["foobar"]);
    w
}

#[test]
fn r2_f2_tab_and_enter_take_the_word_and_type_nothing() {
    for (key, c) in [(TAB, '\t'), (ENTER, '\r')] {
        let mut w = with_complete_list();
        w.pair(key, c);
        w.settle();
        assert_eq!(w.text(), "foobar foobar", "{key:#x}");
        assert_eq!(w.tools.focus(), ToolsFocus::Editor);
        // The next key is a plain key again.
        w.pair(TAB, '\t');
        assert_eq!(w.text(), "foobar foobar\t", "{key:#x}");
    }
}

#[test]
fn r2_f2_space_closes_the_list_and_types_a_space() {
    let mut w = with_complete_list();
    w.pair(SPACE, ' ');
    w.settle();
    assert_eq!(w.text(), "foobar fo ");
    assert!(w.tools.complete_words().is_none());
}

#[test]
fn r2_f2_backspace_closes_the_list_and_deletes_one_character() {
    let mut w = with_complete_list();
    w.pair(BACK, '\u{8}');
    w.settle();
    assert_eq!(w.text(), "foobar f");
    assert!(w.tools.complete_words().is_none());
}

#[test]
fn r2_f2_no_other_key_is_lost() {
    // An arrow that is not for the list moves the caret.
    let mut w = with_complete_list();
    w.key(LEFT);
    w.settle();
    assert!(w.tools.complete_words().is_none());
    assert_eq!(w.selection(), (8, 8));
    // A function key and a Ctrl chord close it and go on to the host's key table.
    for (key, ctrl) in [(0x72, false), (0x53, true)] {
        let mut w = with_complete_list();
        let modifiers = Modifiers {
            ctrl,
            ..Modifiers::default()
        };
        let handled = w.event(InputKind::KeyDown {
            virtual_key: key,
            modifiers,
            repeat: false,
        });
        assert_eq!(handled, Handled::No, "{key:#x}");
        assert!(w.tools.complete_words().is_none(), "{key:#x}");
    }
    // A letter goes in, and the list looks again.
    let mut w = with_complete_list();
    w.pair(0x4f, 'o');
    w.settle();
    assert_eq!(w.text(), "foobar foo");
    assert_eq!(w.tools.complete_words().unwrap(), ["foobar"]);
    // Esc closes it and changes nothing.
    let mut w = with_complete_list();
    w.pair(ESC, '\u{1b}');
    w.settle();
    assert_eq!(w.text(), "foobar fo");
    assert!(w.tools.complete_words().is_none());
}

#[test]
fn r2_f2_a_key_that_closes_a_panel_or_list_does_not_type_its_character_elsewhere() {
    // Space on the panel's close button: the text stays as it is.
    let mut w = Window::new("one two");
    w.command(CommandId::SelectLine);
    w.key(TAB);
    w.key(TAB);
    assert_eq!(w.tools.panel().keyboard, Some(PanelStop::Close));
    w.pair(SPACE, ' ');
    assert!(!w.tools.is_select_line_open());
    assert_eq!(w.text(), "one two");
    // A Space on the options list chooses a row, and no space goes in the field.
    let mut w = Window::new("one two");
    w.find("one");
    let options = w.tools.bar().options.bounds;
    w.click(options);
    assert_eq!(w.tools.focus(), ToolsFocus::List);
    w.key(0x28);
    w.pair(SPACE, ' ');
    assert_eq!(w.tools.bar().find.text, "one");
    assert_eq!(w.text(), "one two");
    // A real space in a field still goes in.
    let mut w = Window::new("one two");
    w.command(CommandId::Find);
    w.pair(0x4f, 'o');
    w.pair(SPACE, ' ');
    w.pair(0x54, 't');
    assert_eq!(w.tools.bar().find.text, "o t");
}

// ---- Second review, finding 3: an index that stops ----

fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("qe-tools-review-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}
/// A mapped file with a bad byte at 2,400,000, with its index stopped there.
fn view_with_a_stopped_index(name: &str) -> EditorView {
    let mut bytes = b"needle line\n".repeat(400_000);
    bytes[2_400_000] = 0xff;
    view_with_a_stopped_index_of(name, &bytes)
}
fn view_with_a_stopped_index_of(name: &str, bytes: &[u8]) -> EditorView {
    let path = temp_file(name, bytes);
    let options = LoadOptions {
        force_read_only: true,
        ..Default::default()
    };
    let quiet: qe_tools::Wake = std::sync::Arc::new(|| {});
    let mut load = FileLoad::start(&path, options, quiet).unwrap();
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
    let start = Instant::now();
    while view.store_status().failure.is_none() {
        assert!(start.elapsed() < Duration::from_secs(60), "no failure came");
        std::thread::sleep(Duration::from_millis(1));
    }
    view
}

#[test]
fn r2_f3_a_search_ends_with_a_final_count_when_the_index_stops() {
    let mut w = Window::with_view(view_with_a_stopped_index("count.txt"));
    let known = w.view.store_status().known_bytes.0;
    assert!(known < 4_800_000, "the index stopped at {known}");
    w.command(CommandId::Find);
    w.typed("needle");
    let start = Instant::now();
    while !w.tools.match_count().1 {
        w.poll();
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the search never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let (count, done) = w.tools.match_count();
    assert!(done);
    assert_eq!(count, (known / 12) as u64, "all that could be read");
    let status = w.tools.status().unwrap_or("");
    assert!(status.contains("could not be read"), "{status:?}");
    assert!(
        w.tools.bar().count.text.contains("found"),
        "{}",
        w.tools.bar().count.text
    );
    w.settle_stopped();
}

#[test]
fn r2_f3_a_match_just_before_the_stopped_place_is_found() {
    let mut bytes = b"plain line\n".repeat(440_000);
    bytes[2_399_990..2_399_996].copy_from_slice(b"needle");
    bytes[2_400_000] = 0xff;
    let mut w = Window::with_view(view_with_a_stopped_index_of("edge.txt", &bytes));
    w.command(CommandId::Find);
    w.typed("needle");
    let start = Instant::now();
    while !(w.tools.is_quiet() && w.tools.match_count().1) {
        w.poll();
        assert!(start.elapsed() < Duration::from_secs(20), "it never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(w.tools.match_count(), (1, true));
    assert_eq!(w.selection(), (2_399_990, 2_399_996));
}

#[test]
fn r2_f3_a_step_stops_waiting_and_says_why() {
    let mut w = Window::with_view(view_with_a_stopped_index("step.txt"));
    w.command(CommandId::Find);
    w.typed("no such text");
    let start = Instant::now();
    while !w.tools.is_quiet() {
        w.poll();
        assert!(start.elapsed() < Duration::from_secs(20), "it still waits");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!w.tools.is_step_waiting_for_test());
    let status = w.tools.status().unwrap_or("");
    assert!(status.contains("could not be read"), "{status:?}");
    // F3 afterwards says so too, with no wait.
    w.command(CommandId::FindNext);
    let start = Instant::now();
    while !w.tools.is_quiet() {
        w.poll();
        assert!(start.elapsed() < Duration::from_secs(20), "F3 still waits");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(w.tools.take_beep());
}

#[test]
fn r2_f3_select_line_past_a_stopped_index_says_why() {
    let mut w = Window::with_view(view_with_a_stopped_index("lines.txt"));
    w.command(CommandId::SelectLine);
    w.typed("399999");
    w.key(ENTER);
    let start = Instant::now();
    while !w.tools.is_idle() {
        w.poll();
        assert!(start.elapsed() < Duration::from_secs(20), "it still waits");
        std::thread::sleep(Duration::from_millis(1));
    }
    let warning = w.view.warning().unwrap_or("").to_string();
    assert!(warning.contains("could not be read"), "{warning:?}");
}

// ---- Second review, finding 4: Resize and DpiChange ----

#[test]
fn r2_f4_resize_leaves_the_bar_room_and_reaches_a_panel_with_the_keys() {
    // The bar open, the panel with the keys, the alert, a list, and all closed.
    for state in 0..5 {
        let mut w = Window::new("one\ntwo\nthree\n");
        match state {
            0 => {}
            1 => w.command(CommandId::Find),
            2 => w.command(CommandId::SelectLine),
            3 => {
                w.command(CommandId::SelectLine);
                w.typed("abc");
                w.key(ENTER);
                assert_eq!(w.tools.focus(), ToolsFocus::Alert);
            }
            _ => {
                w.find("one");
                let options = w.tools.bar().options.bounds;
                w.click(options);
                assert_eq!(w.tools.focus(), ToolsFocus::List);
            }
        }
        let bar = if state == 1 || state == 4 { 32.0 } else { 0.0 };
        assert_eq!(w.view.text_area().y.0, bar, "state {state}");
        let resize = InputEvent {
            time_us: 0,
            kind: InputKind::Resize {
                width: Dip(1000.0),
                height: Dip(700.0),
            },
        };
        assert!(!w.tools.event_for_editor(&resize), "state {state}");
        w.resize(1000.0, 700.0);
        let inner = w.view.text_area();
        assert_eq!(inner.y.0, bar, "state {state}");
        assert_eq!(inner.height.0, 700.0 - bar, "state {state}");
        assert_eq!(inner.width.0, 1000.0, "state {state}");
        assert_ne!(
            w.tools.focus(),
            ToolsFocus::List,
            "a list does not outlive its place"
        );
        let shrink = w.event(InputKind::DpiChange { dpi: 144 });
        assert_eq!(shrink, Handled::No, "state {state}");
    }
}

#[test]
fn r2_f4_the_panel_moves_with_the_window() {
    let mut w = Window::new("one");
    w.command(CommandId::SelectLine);
    let before = w.tools.panel().frame;
    w.resize(1000.0, 700.0);
    let after = w.tools.panel().frame;
    assert!(
        after.x.0 > before.x.0,
        "centered again: {before:?} {after:?}"
    );
}

// ---- Second review, finding 5: one focused widget, and lists close on Blur ----

#[test]
fn r2_f5_only_one_widget_reports_focus_under_a_list_or_alert() {
    let mut w = Window::new("one two");
    w.find("one");
    let options = w.tools.bar().options.bounds;
    w.click(options);
    assert_eq!(w.tools.focus(), ToolsFocus::List);
    assert_eq!(w.focused_widgets(), 1, "the options list");
    w.key(ESC);
    assert_eq!(w.focused_widgets(), 1, "back in the bar");

    let mut w = Window::new("one two");
    w.command(CommandId::SelectLine);
    w.typed("abc");
    w.key(ENTER);
    assert_eq!(w.tools.focus(), ToolsFocus::Alert);
    assert_eq!(w.focused_widgets(), 1, "the alert");

    let mut w = with_complete_list();
    assert_eq!(w.focused_widgets(), 1, "the Complete list");
    w.key(ESC);
    assert_eq!(w.focused_widgets(), 0, "the text has it");
}

#[test]
fn r2_f5_lists_close_when_the_window_loses_the_keyboard() {
    let mut w = Window::new("one two");
    w.find("one");
    let options = w.tools.bar().options.bounds;
    w.click(options);
    w.event(InputKind::Blur);
    assert_ne!(w.tools.focus(), ToolsFocus::List);
    assert_eq!(w.focused_widgets(), 0, "the field heard the window's Blur");
    w.event(InputKind::Focus);
    assert_eq!(w.focused_widgets(), 1);

    let mut w = with_complete_list();
    w.event(InputKind::Blur);
    assert!(w.tools.complete_words().is_none());
    assert!(!w.view_has_focus());
    w.event(InputKind::Focus);
    assert!(w.view_has_focus());
}

// ---- Second review, finding 6: where the panel sits ----

#[test]
fn r2_f6_the_panel_and_its_alert_sit_below_the_bars_rows() {
    let mut w = Window::new("one");
    w.command(CommandId::FindReplace);
    w.command(CommandId::SelectLine);
    let bar = w.tools.bar().bounds;
    let bottom = bar.y.0 + bar.height.0;
    assert_eq!(bar.height.0, 64.0);
    assert!(
        w.tools.panel().frame.y.0 >= bottom,
        "{:?}",
        w.tools.panel().frame
    );
    w.typed("abc");
    w.key(ENTER);
    let alert = w.tools.widgets().pop().unwrap().bounds;
    assert!(alert.y.0 >= bottom, "{alert:?}");
    // It moves when the bar opens under it, and back when the bar closes.
    let mut w = Window::new("one");
    w.command(CommandId::SelectLine);
    let alone = w.tools.panel().frame.y.0;
    w.command(CommandId::Find);
    assert!(w.tools.panel().frame.y.0 >= 32.0 + alone - 0.5);
    w.key(ESC);
    assert!(!w.tools.is_bar_open());
    assert_eq!(w.tools.panel().frame.y.0, alone);
}

#[test]
fn r2_f6_a_revealed_selection_is_not_under_the_panel() {
    // Short enough not to wrap, and wide enough to reach under the panel.
    let line = "x".repeat(60);
    let text = (0..300)
        .map(|i| format!("{i:03} {line}\n"))
        .collect::<String>();
    let mut w = Window::new(&text);
    w.command(CommandId::SelectLine);
    let frame = w.tools.panel().frame;
    // Scroll so that line 101 is under the panel, then select it with the Select button.
    let start = ByteOffset(100 * (line.len() + 5));
    w.view.run_host(
        qe_editor::HostCommand::Select {
            range: start..start,
            reveal: true,
        },
        &mut w.ctx,
    );
    w.settle_view();
    let rect = w.view.range_rects(start..start).unwrap()[0].rect;
    let scroll = w.view.scroll_offset();
    w.view.run_host(
        qe_editor::HostCommand::ScrollTo(scroll + f64::from(rect.y.0 - frame.y.0 - 8.0)),
        &mut w.ctx,
    );
    w.settle_view();
    let rect = w.view.range_rects(start..start).unwrap()[0].rect;
    assert!(
        rect.y.0 < frame.y.0 + frame.height.0 && rect.y.0 >= frame.y.0,
        "the setup: {rect:?} is under {frame:?}"
    );
    w.typed("101");
    let select = w.tools.panel().select.bounds;
    w.click(select);
    w.settle();
    assert!(
        w.tools.is_select_line_open(),
        "the Select button leaves it open"
    );
    let selection = w.view.selection();
    assert!(selection.start < selection.end);
    let rects = w.view.range_rects(selection).unwrap();
    assert!(!rects.is_empty());
    for found in rects {
        let (top, bottom) = (found.rect.y.0, found.rect.y.0 + found.rect.height.0);
        assert!(
            found.rect.x.0 + found.rect.width.0 > frame.x.0,
            "the setup: it reaches across"
        );
        assert!(
            bottom <= frame.y.0 || top >= frame.y.0 + frame.height.0,
            "{:?} is under {frame:?}",
            found.rect
        );
    }
}

#[test]
fn r2_f6_a_step_also_keeps_its_match_out_from_under_the_panel() {
    let line = "x".repeat(150);
    let text = (0..300)
        .map(|i| {
            let word = if i == 101 { "needle" } else { "plain" };
            format!("{} {word} {line}\n", "y".repeat(50))
        })
        .collect::<String>();
    let mut w = Window::new(&text);
    w.find("needle");
    w.command(CommandId::SelectLine);
    let frame = w.tools.panel().frame;
    let at = w.view.selection().start;
    let scroll = w.view.scroll_offset();
    let rect = w.view.range_rects(at..at).unwrap()[0].rect;
    w.view.run_host(
        qe_editor::HostCommand::ScrollTo(scroll + f64::from(rect.y.0 - frame.y.0 - 8.0)),
        &mut w.ctx,
    );
    w.settle_view();
    w.command(CommandId::FindPrevious);
    w.settle();
    let selection = w.view.selection();
    let found = w.view.range_rects(selection).unwrap()[0].rect;
    assert!(
        found.y.0 >= frame.y.0 + frame.height.0,
        "{found:?} under {frame:?}"
    );
}

#[test]
fn r2_f6_esc_in_the_panel_closes_it() {
    let mut w = Window::new("one");
    w.command(CommandId::SelectLine);
    w.pair(ESC, '\u{1b}');
    assert!(!w.tools.is_select_line_open());
    assert_eq!(w.tools.focus(), ToolsFocus::Editor);
    assert_eq!(w.text(), "one");
}

// ---- Second review, finding 7: rows ----

#[test]
fn r2_f7_find_shows_one_row_and_replace_shows_two() {
    let mut w = Window::new("one");
    w.command(CommandId::FindReplace);
    assert!(w.tools.bar().replacing);
    assert_eq!(w.tools.bar().bounds.height.0, 64.0);
    assert_eq!(w.view.text_area().y.0, 64.0);
    w.key(ESC);
    w.command(CommandId::Find);
    assert!(!w.tools.bar().replacing, "after closing");
    assert_eq!(w.tools.bar().bounds.height.0, 32.0);
    assert_eq!(w.view.text_area().y.0, 32.0);
    w.command(CommandId::FindReplace);
    assert_eq!(w.view.text_area().y.0, 64.0);
    w.command(CommandId::Find);
    assert!(!w.tools.bar().replacing, "with the bar open");
    assert_eq!(w.view.text_area().y.0, 32.0);
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find));
}

// ---- Second review, finding 8: Capitalize ----

#[test]
fn r2_f8_capitalize_uses_title_forms() {
    let cases = [
        // Georgian has no title form, so it stays.
        ("\u{10d2}\u{10d0}\u{10db}", "\u{10d2}\u{10d0}\u{10db}"),
        // Greek with a small iota has its own title letter.
        ("\u{1f80}b", "\u{1f88}b"),
        ("\u{1f97}b", "\u{1f9f}b"),
        ("\u{1fa3}b", "\u{1fab}b"),
        ("\u{1fb3}b", "\u{1fbc}b"),
        ("\u{1fc3}b", "\u{1fcc}b"),
        ("\u{1ff3}b", "\u{1ffc}b"),
        // These keep the small iota as a mark.
        ("\u{1fb2}b", "\u{1fba}\u{345}b"),
        ("\u{1fb4}b", "\u{386}\u{345}b"),
        ("\u{1fb7}b", "\u{391}\u{342}\u{345}b"),
        ("\u{1fc7}b", "\u{397}\u{342}\u{345}b"),
        ("\u{1ff7}b", "\u{3a9}\u{342}\u{345}b"),
        // A letter that starts with an apostrophe capitalizes the letter after it.
        ("\u{149}a", "\u{2bc}Na"),
        // What worked already.
        ("\u{df}a", "Ssa"),
        ("\u{fb01}a", "Fia"),
        ("\u{1c6}a", "\u{1c5}a"),
        ("\u{587}a", "\u{535}\u{582}a"),
        ("hello WORLD", "Hello World"),
    ];
    for (text, want) in cases {
        let got = transform_text(text, qe_tools::Transform::Capitalize).unwrap();
        assert_eq!(got, want, "{text:?}");
    }
}

// ---- Second review, finding 9: menu commands while a field has the keys ----

#[test]
fn r2_f9_edit_commands_from_the_menu_reach_the_field_that_has_the_keys() {
    let mut w = Window::new("document");
    let m = measure();
    // With the text in charge, they are the host's.
    for command in [
        CommandId::Undo,
        CommandId::Cut,
        CommandId::Copy,
        CommandId::Paste,
    ] {
        let got = w.tools.run_command(command, &mut w.view, &m, &mut w.ctx);
        assert_eq!(got, Handled::No, "{command:?}");
    }
    w.command(CommandId::Find);
    w.typed("hello");
    let run = |w: &mut Window, command| {
        let got = w.tools.run_command(command, &mut w.view, &m, &mut w.ctx);
        assert_eq!(got, Handled::Yes, "{command:?}");
    };
    run(&mut w, CommandId::SelectAll);
    run(&mut w, CommandId::Copy);
    run(&mut w, CommandId::Cut);
    assert_eq!(w.tools.bar().find.text, "");
    assert_eq!(w.tools.query(), "", "Cut changes the search too");
    run(&mut w, CommandId::Paste);
    run(&mut w, CommandId::Paste);
    assert_eq!(w.tools.bar().find.text, "hellohello");
    assert_eq!(w.tools.query(), "hellohello");
    run(&mut w, CommandId::Undo);
    assert_ne!(w.tools.bar().find.text, "hellohello");
    assert_eq!(w.text(), "document", "the document took none of them");
    assert_eq!(
        w.tools.command_state(CommandId::Copy, &w.view),
        Some((true, false))
    );

    // A button has nothing to cut, and the document is not changed behind the user's back.
    w.key(TAB);
    assert!(!matches!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find)));
    run(&mut w, CommandId::Cut);
    assert_eq!(w.text(), "document");
    assert_eq!(
        w.tools.command_state(CommandId::Cut, &w.view),
        Some((false, false))
    );

    // The panel's field too.
    let mut w = Window::new("document");
    w.command(CommandId::SelectLine);
    w.typed("12");
    let run = |w: &mut Window, command| {
        w.tools.run_command(command, &mut w.view, &m, &mut w.ctx);
    };
    run(&mut w, CommandId::SelectAll);
    run(&mut w, CommandId::Cut);
    assert_eq!(w.tools.panel().field.text, "");
    assert_eq!(w.text(), "document");
}

// ---- Second review: its uncertain concerns 1, 3 and 4 ----

#[test]
fn r2_c1_a_found_match_waits_behind_keys_that_would_move_it() {
    let text = format!("{}\nabc", "\u{4e2d}".repeat(400));
    let at = text.len() - 3;
    let mut w = Window::new("");
    let m = measure();
    let narrow = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(300.0),
        height: Dip(600.0),
    };
    let mut cold = EditorView::from_text(&text).unwrap();
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
    let letter = InputEvent {
        time_us: 0,
        kind: InputKind::Char('x'),
    };
    cold.handle(&letter, &mut w.ctx);
    assert_eq!(cold.queued_input_count(), 2);
    w.tools
        .view_switched(Some(&mut w.view), &mut cold, &mut w.ctx);
    w.tools
        .run_command(CommandId::Find, &mut cold, &m, &mut w.ctx);
    w.tools.set_query_for_test("abc", &mut cold, &m, &mut w.ctx);
    // The step ends while the keys wait, and nothing selected yet.
    std::thread::sleep(Duration::from_millis(100));
    for _ in 0..20 {
        w.tools.poll(&mut cold, &m, &mut w.ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(cold.queued_input_count(), 2);
    cold.test_pause_workers(false);
    let start = Instant::now();
    while !w.tools.is_quiet() || cold.queued_input_count() > 0 || cold.pending_layout() {
        cold.poll_layout(&mut w.ctx);
        w.tools.poll(&mut cold, &m, &mut w.ctx);
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "it stayed waiting"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let result = cold.text().unwrap();
    let selection = cold.selection();
    assert_eq!(result.get(selection.start.0..selection.end.0), Some("abc"));
    assert_eq!(selection.start.0, at + 1, "one letter went in before it");
}

#[test]
fn r2_c3_a_complete_result_opens_no_list_once_the_keyboard_has_moved() {
    let mut w = Window::new("foobar fo");
    w.select(9, 9);
    w.command(CommandId::Complete);
    w.command(CommandId::Find);
    w.settle();
    assert!(w.tools.complete_words().is_none());
    assert_eq!(w.tools.focus(), ToolsFocus::Bar(BarStop::Find));
    // Or the window itself lost it.
    let mut w = Window::new("foobar fo");
    w.select(9, 9);
    w.command(CommandId::Complete);
    w.event(InputKind::Blur);
    w.settle();
    assert!(w.tools.complete_words().is_none());
}

#[test]
fn r2_c4_an_entry_waiting_for_the_index_ends_when_the_field_changes() {
    let mut w = Window::new("one\ntwo\nthree\n");
    w.command(CommandId::SelectLine);
    w.typed("3");
    w.tools.set_pending_lines_for_test("2", true);
    w.settle();
    assert_eq!(w.selection(), (0, 0), "the old entry selects nothing");
    assert!(w.tools.is_select_line_open());
    // The same entry still goes on.
    w.tools.set_pending_lines_for_test("3", true);
    w.settle();
    assert_eq!(w.selection(), (8, 14));
    assert!(!w.tools.is_select_line_open());
}

// ---- Second review: what survived its break run ----

#[test]
fn r2_a_mark_scan_for_old_text_is_not_used() {
    // A transform and a mark scan end together, so one poll sees both.
    let text = format!(
        "{}\nx x x\n{}",
        "\u{131}".repeat(20),
        "\u{131}".repeat(40_000)
    );
    let mut w = Window::new(&text);
    let m = measure();
    w.command(CommandId::Find);
    w.select(0, text.len());
    w.tools
        .run_command(CommandId::UpperCase, &mut w.view, &m, &mut w.ctx);
    w.tools.set_query_for_test("x", &mut w.view, &m, &mut w.ctx);
    let start = Instant::now();
    while !w.tools.transform_and_marks_done_for_test() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the workers did not end"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = w.view.revision();
    w.view.poll_layout(&mut w.ctx);
    w.tools.poll(&mut w.view, &m, &mut w.ctx);
    assert_ne!(
        w.view.revision(),
        before,
        "the transform went in during this poll"
    );
    let now = w.text();
    for mark in w.view.decorations(DecorationKind::FindMatch) {
        let got = now.get(mark.start.0..mark.end.0);
        assert_eq!(
            got.map(str::to_lowercase),
            Some("x".to_string()),
            "{mark:?}"
        );
    }
}

#[test]
fn r2_a_search_start_inside_an_edit_goes_to_the_edits_start() {
    let mut w = Window::new("hello world cat");
    w.select(5, 5);
    w.command(CommandId::Find);
    w.edit(2, 8, "cat");
    w.poll();
    w.typed("cat");
    w.settle();
    assert_eq!(w.selection(), (2, 5));
}

#[test]
fn r2_typing_after_a_step_searches_from_that_step() {
    let mut w = Window::new("cat dog cat dog");
    w.find("cat");
    assert_eq!(w.selection(), (0, 3));
    w.command(CommandId::FindNext);
    w.settle();
    assert_eq!(w.selection(), (8, 11));
    w.typed(" d");
    w.settle();
    assert_eq!(w.selection(), (8, 13));
}

#[test]
fn r2_a_waiting_step_looks_once_more_when_the_index_stops_and_says_why() {
    let mut w = Window::with_view(view_with_a_stopped_index("wait.txt"));
    w.command(CommandId::Find);
    w.typed("no such text");
    w.settle_stopped();
    w.tools
        .set_step_waiting_for_test(qe_tools::Direction::Forward, ByteOffset(0)..ByteOffset(0));
    w.settle_stopped();
    assert!(!w.tools.is_step_waiting_for_test());
    let status = w.tools.status().unwrap_or("");
    assert!(status.contains("could not be read"), "{status:?}");
}

#[test]
fn r2_replace_all_leaves_the_caret_after_the_last_replacement() {
    let mut w = Window::new("cat x cat y");
    w.replace_bar("cat", "elephant");
    let all = w.tools.bar().replace_all.bounds;
    w.click(all);
    w.settle();
    assert_eq!(w.text(), "elephant x elephant y");
    assert_eq!(w.selection(), (19, 19));
}
