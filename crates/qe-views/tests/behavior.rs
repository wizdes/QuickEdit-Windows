//! `ViewFrame` against the real editor and DirectWrite, with a fake clock. The numbers in
//! these tests are the Mac's. Each is named beside its source in the crate's README.
use qe_api::{
    ByteOffset, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers,
    MouseButton, Point, Rect, Theme, ThemeKind, ViewContext, ViewRequest,
};
use qe_editor::EditorView;
use qe_layout::DirectWriteLayout;
use qe_views::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

const FAMILY: &str = "Cascadia Mono";

struct Rig {
    editor: EditorView,
    frame: ViewFrame,
    measure: DirectWriteLayout,
    clock: Rc<Cell<f64>>,
}

fn rect(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}
fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}

impl Rig {
    fn new(text: &str, width: f32, height: f32) -> Self {
        Self::with_paper(text, width, height, Paper::LETTER)
    }
    fn with_paper(text: &str, width: f32, height: f32, paper: Paper) -> Self {
        let measure =
            DirectWriteLayout::with_font(FAMILY, 10.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
        let mut editor = EditorView::from_text(text).unwrap();
        editor
            .apply_font(FAMILY, 10.0, &mut ViewContext::default())
            .unwrap();
        let mut frame = ViewFrame::new(paper, FAMILY, 10.0);
        frame.set_reduce_motion(false);
        let clock = Rc::new(Cell::new(100.0));
        let shared = clock.clone();
        frame.set_clock(Box::new(move || shared.get()));
        let mut rig = Self {
            editor,
            frame,
            measure,
            clock,
        };
        let mut ctx = ViewContext::default();
        rig.frame
            .layout(&mut rig.editor, rect(width, height), &rig.measure, &mut ctx);
        rig.settle();
        rig
    }
    /// Runs layout and the map's worker until both are done, with the clock past any wait.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.advance(0.2);
            let mut ctx = ViewContext::default();
            self.editor.poll_layout(&mut ctx);
            self.frame.sync(&mut self.editor, &self.measure, &mut ctx);
            if self.editor.visible_rows().complete && !self.frame.map_job_running() {
                let mut ctx = ViewContext::default();
                self.frame.sync(&mut self.editor, &self.measure, &mut ctx);
                if self.editor.visible_rows().complete && !self.frame.map_job_running() {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "layout never settled");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn advance(&self, seconds: f64) {
        self.clock.set(self.clock.get() + seconds);
    }
    fn tick(&mut self) -> ViewContext {
        let mut ctx = ViewContext::default();
        self.frame.sync(&mut self.editor, &self.measure, &mut ctx);
        ctx
    }
    fn send(&mut self, kind: InputKind) -> (Handled, ViewContext) {
        let mut ctx = ViewContext::default();
        let handled = self.frame.handle(
            &mut self.editor,
            &InputEvent { time_us: 0, kind },
            &self.measure,
            &mut ctx,
        );
        (handled, ctx)
    }
    fn hover(&mut self, x: f32, y: f32) -> ViewContext {
        self.send(InputKind::MouseMove { at: point(x, y) }).1
    }
    fn down(&mut self, x: f32, y: f32) -> ViewContext {
        self.send(InputKind::MouseDown {
            at: point(x, y),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        })
        .1
    }
    fn up(&mut self, x: f32, y: f32) -> ViewContext {
        self.send(InputKind::MouseUp {
            at: point(x, y),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        })
        .1
    }
    fn type_char(&mut self, c: char) -> ViewContext {
        self.send(InputKind::Char(c)).1
    }
    fn draw(&self, kind: ThemeKind) -> DisplayList {
        let mut list = DisplayList::default();
        self.frame
            .draw(&self.editor, &mut list, &Theme::new(kind), &self.measure);
        list
    }
    fn map_left(&self) -> f32 {
        self.frame.map_area().x.0
    }
}

fn timers(ctx: &ViewContext) -> Vec<u32> {
    ctx.requests
        .iter()
        .filter_map(|request| match request {
            ViewRequest::RequestTimer(ms) => Some(*ms),
            _ => None,
        })
        .collect()
}
fn repaints(ctx: &ViewContext) -> Vec<Rect> {
    ctx.requests
        .iter()
        .filter_map(|request| match request {
            ViewRequest::Invalidate(rect) => Some(*rect),
            _ => None,
        })
        .collect()
}
fn overlaps(a: Rect, b: Rect) -> bool {
    a.x.0 < b.x.0 + b.width.0
        && b.x.0 < a.x.0 + a.width.0
        && a.y.0 < b.y.0 + b.height.0
        && b.y.0 < a.y.0 + a.height.0
}
/// An edit at the document's end that leaves the caret, and so the view, where they are.
fn append_away_from_view(rig: &mut Rig, text: &str) {
    let snapshot = rig.editor.worker_snapshot();
    let end = snapshot.len_bytes();
    let edits = [qe_editor::TextEdit {
        range: end..end,
        text: text.into(),
    }];
    let prepared = qe_editor::prepare_edits(
        snapshot.as_ref(),
        &edits,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let selection = rig.editor.selection();
    let mut ctx = ViewContext::default();
    rig.editor
        .apply_edits(&prepared, selection, &mut ctx)
        .unwrap();
}
fn lines(count: usize) -> String {
    (0..count).map(|n| format!("line number {n}\n")).collect()
}

// ---- The box's fade ----

#[test]
fn the_box_fades_in_over_0_15_s_and_out_over_0_25_s() {
    let mut rig = Rig::new("short\n", 800.0, 400.0);
    assert_eq!(rig.frame.fade().alpha(), 0.0);
    let inside = rig.map_left() + 10.0;
    let ctx = rig.hover(inside, 10.0);
    assert_eq!(timers(&ctx), vec![16]);
    assert!(rig.frame.fade().fading());

    rig.advance(0.075);
    let ctx = rig.tick();
    assert!((rig.frame.fade().alpha() - 0.5).abs() < 0.001);
    assert_eq!(timers(&ctx), vec![16]);
    assert!(!repaints(&ctx).is_empty());

    rig.advance(0.075);
    let ctx = rig.tick();
    assert_eq!(rig.frame.fade().alpha(), 1.0);
    assert!(!rig.frame.fade().fading());
    assert!(timers(&ctx).is_empty(), "no timer once it stops");

    // Out, from the map into the text.
    let ctx = rig.hover(100.0, 10.0);
    assert_eq!(timers(&ctx), vec![16]);
    rig.advance(0.125);
    rig.tick();
    assert!((rig.frame.fade().alpha() - 0.5).abs() < 0.001);
    rig.advance(0.125);
    let ctx = rig.tick();
    assert_eq!(rig.frame.fade().alpha(), 0.0);
    assert!(timers(&ctx).is_empty());
}

#[test]
fn a_fade_that_starts_part_way_takes_its_share_of_the_time() {
    let mut fade = Fade::default();
    fade.aim(true, 0.0, false);
    fade.step(0.075);
    assert!((fade.alpha() - 0.5).abs() < 0.001);
    // Turning back at one half takes half of the 0.25 s.
    fade.aim(false, 0.075, false);
    assert!((fade.alpha() - 0.5).abs() < 0.001);
    fade.step(0.075 + 0.0625);
    assert!((fade.alpha() - 0.25).abs() < 0.001);
    fade.step(0.075 + 0.125);
    assert_eq!(fade.alpha(), 0.0);
    assert!(!fade.fading());
}

#[test]
fn reduce_motion_jumps_and_needs_no_timer() {
    let mut rig = Rig::new("short\n", 800.0, 400.0);
    rig.frame.set_reduce_motion(true);
    let inside = rig.map_left() + 10.0;
    let ctx = rig.hover(inside, 10.0);
    assert_eq!(rig.frame.fade().alpha(), 1.0);
    assert!(timers(&ctx).is_empty());
    assert!(!repaints(&ctx).is_empty());
    let ctx = rig.hover(100.0, 10.0);
    assert_eq!(rig.frame.fade().alpha(), 0.0);
    assert!(timers(&ctx).is_empty());
}

#[test]
fn the_box_shows_within_8_dips_of_the_map_and_stays_during_a_drag() {
    let mut rig = Rig::new(&lines(2000), 800.0, 400.0);
    let left = rig.map_left();
    rig.hover(left - 8.0, 10.0);
    assert!(rig.frame.fade().fading(), "8 dips left is near");
    rig.advance(1.0);
    rig.tick();
    rig.hover(left - 8.01, 10.0);
    rig.advance(1.0);
    rig.tick();
    assert_eq!(rig.frame.fade().alpha(), 0.0, "past 8 dips is not");

    // A drag keeps it, even with the pointer far away.
    rig.hover(left + 5.0, 10.0);
    rig.advance(1.0);
    rig.tick();
    rig.down(left + 5.0, 10.0);
    rig.hover(10.0, 10.0);
    rig.advance(1.0);
    rig.tick();
    assert_eq!(rig.frame.fade().alpha(), 1.0);
    // The drag ends away from the map, so the box fades from there, with no flash.
    rig.up(10.0, 10.0);
    assert!(rig.frame.fade().fading());
    rig.advance(1.0);
    rig.tick();
    assert_eq!(rig.frame.fade().alpha(), 0.0);
}

#[test]
fn nothing_runs_while_idle() {
    let mut rig = Rig::new(&lines(500), 800.0, 400.0);
    let jobs = rig.frame.counts().map_jobs;
    for _ in 0..5 {
        rig.advance(1.0);
        let ctx = rig.tick();
        assert!(timers(&ctx).is_empty());
        assert!(repaints(&ctx).is_empty());
    }
    assert_eq!(rig.frame.counts().map_jobs, jobs);
    assert!(!rig.frame.map_job_running());
}

// ---- The box's place and height ----

/// The Mac's `mapTopLine`, with its whole-line `first`.
fn mac_top(first: usize, last: usize, count: usize, rows: usize) -> f64 {
    if count <= rows {
        return 0.0;
    }
    let scrollable = (count - (last - first + 1)).max(1);
    (first as f64 / scrollable as f64).min(1.0) * (count - rows) as f64
}

#[test]
fn the_box_matches_the_macs_formula_on_rows_that_do_not_wrap() {
    let count = 5000;
    let mut rig = Rig::new(&lines(count), 800.0, 400.0);
    let rows = 200;
    assert_eq!(rig.frame.geometry().unwrap().rows, rows);
    for target in [0.0, 0.1, 0.25, 0.5, 0.77, 1.0] {
        let y = target * rig.editor.max_scroll_position();
        let mut ctx = ViewContext::default();
        rig.editor.set_scroll_position(y, &mut ctx);
        rig.settle();
        let visible = rig.editor.visible_rows();
        let first = visible.rows.first().unwrap().line.0;
        let last = visible.rows.last().unwrap().line.0;
        let box_rect = rig.frame.map_box().unwrap().rect;
        // The same height as the Mac's, exactly.
        assert_eq!(box_rect.height.0, (last - first + 1) as f32 * 2.0);
        // The same top, to within one map row, which is the Mac's own step.
        let top = mac_top(first, last, count, rows);
        let mac = (first as f64 - top) * 2.0;
        assert!(
            (f64::from(box_rect.y.0) - mac).abs() <= 2.0,
            "at {target}: {} and {mac}",
            box_rect.y.0
        );
        // It is the full width of the map.
        assert_eq!(box_rect.x.0, rig.map_left());
        assert_eq!(box_rect.width.0, 100.0);
    }
}

#[test]
fn a_document_that_fits_the_map_does_not_scroll_it() {
    let mut rig = Rig::new(&lines(100), 800.0, 400.0);
    let geometry = rig.frame.geometry().unwrap();
    assert_eq!(geometry.top_line, 0.0);
    // The box walks down the map row by row.
    let mut ctx = ViewContext::default();
    rig.editor
        .set_scroll_position(rig.editor.max_scroll_position(), &mut ctx);
    rig.settle();
    let geometry = rig.frame.geometry().unwrap();
    assert_eq!(geometry.top_line, 0.0);
    let bottom = geometry.box_top() + geometry.box_height();
    assert!((bottom - 200.0).abs() <= 2.0, "{bottom}");
}

#[test]
fn map_formulas_invert_each_other() {
    for (count, on_screen, rows) in [(5000usize, 30usize, 200usize), (150, 30, 200), (40, 30, 10)] {
        for step in 0..=20 {
            let fraction = f64::from(step) / 20.0;
            let scrollable = count - on_screen;
            let first = fraction * scrollable as f64;
            let geometry = MapGeometry::new(first, on_screen, count, rows);
            if rows > on_screen || count <= rows {
                let back =
                    first_line_for_box_top(geometry.box_top(), count, on_screen, rows, 400.0);
                assert!(
                    (back - first).abs() < 1e-6,
                    "{count} {step}: {back} {first}"
                );
            }
        }
    }
    assert_eq!(first_line_for_box_top(10.0, 5, 30, 200, 400.0), 0.0);
    assert_eq!(first_line_for_box_top(-50.0, 5000, 30, 200, 400.0), 0.0);
    assert_eq!(first_line_for_box_top(1e9, 5000, 30, 200, 400.0), 4970.0);
}

// ---- Clicking and dragging ----

#[test]
fn dragging_moves_the_box_exactly_with_the_pointer() {
    let mut rig = Rig::new(&lines(5000), 800.0, 400.0);
    let x = rig.map_left() + 30.0;
    rig.settle();
    let start = rig.frame.map_box().unwrap().rect;
    rig.down(x, start.y.0 + 7.0);
    assert_eq!(
        rig.editor.scroll_offset(),
        0.0,
        "a press on the box moves nothing"
    );
    for delta in [10.0f32, 25.5, 90.0, 150.0, 40.0, 3.0] {
        let y = start.y.0 + 7.0 + delta;
        rig.hover(x, y);
        let box_now = rig.frame.map_box().unwrap().rect;
        let error = (f64::from(box_now.y.0) - f64::from(start.y.0 + delta)).abs();
        assert!(
            error < 0.02,
            "moved {delta}: box at {} ({error})",
            box_now.y.0
        );
    }
    rig.up(x, 0.0);
}

#[test]
fn a_drag_past_the_ends_stops_at_the_top_and_the_bottom() {
    let mut rig = Rig::new(&lines(5000), 800.0, 400.0);
    let x = rig.map_left() + 30.0;
    let start = rig.frame.map_box().unwrap().rect;
    rig.down(x, start.y.0 + 1.0);
    rig.hover(x, 100_000.0);
    assert_eq!(rig.editor.scroll_offset(), rig.editor.max_scroll_position());
    rig.hover(x, -100_000.0);
    assert_eq!(rig.editor.scroll_offset(), 0.0);
    rig.up(x, 0.0);
}

#[test]
fn a_click_off_the_box_centers_it_on_the_pointer() {
    let mut rig = Rig::new(&lines(5000), 800.0, 400.0);
    let x = rig.map_left() + 30.0;
    let height = rig.frame.map_box().unwrap().rect.height.0;
    rig.down(x, 300.0);
    let box_now = rig.frame.map_box().unwrap().rect;
    let middle = box_now.y.0 + height / 2.0;
    assert!((middle - 300.0).abs() < 0.02, "middle at {middle}");
    rig.up(x, 300.0);
    assert!(rig.editor.scroll_offset() > 0.0);
    // The click asked for the mouse, and gave it back.
    let (_, ctx) = rig.send(InputKind::MouseDown {
        at: point(x, 100.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    assert!(ctx.requests.contains(&ViewRequest::CaptureMouse(true)));
    let ctx = rig.up(x, 100.0);
    assert!(ctx.requests.contains(&ViewRequest::CaptureMouse(false)));
}

#[test]
fn a_drag_is_right_on_wrapped_lines_too() {
    let text: String = (0..3000)
        .map(|n| "word ".repeat(if n % 7 == 0 { 80 } else { 3 }) + "\n")
        .collect();
    let mut rig = Rig::new(&text, 500.0, 400.0);
    let x = rig.map_left() + 30.0;
    let start = rig.frame.map_box().unwrap().rect;
    rig.down(x, start.y.0 + 2.0);
    rig.hover(x, start.y.0 + 2.0 + 120.0);
    let scroll = rig.editor.scroll_offset();
    assert!(scroll > 0.0);
    assert!(scroll < rig.editor.max_scroll_position());
    let box_now = rig.frame.map_box().unwrap().rect;
    assert!(
        (box_now.y.0 - (start.y.0 + 120.0)).abs() < 0.02,
        "{}",
        box_now.y.0
    );
    rig.up(x, 0.0);
}

#[test]
fn the_map_takes_only_left_clicks() {
    let mut rig = Rig::new(&lines(5000), 800.0, 400.0);
    let x = rig.map_left() + 30.0;
    let (handled, _) = rig.send(InputKind::MouseDown {
        at: point(x, 300.0),
        button: MouseButton::Right,
        modifiers: Modifiers::default(),
    });
    assert_eq!(handled, Handled::Yes);
    assert_eq!(rig.editor.scroll_offset(), 0.0);
}

#[test]
fn a_lost_mouse_ends_the_drag() {
    let mut rig = Rig::new(&lines(5000), 800.0, 400.0);
    let x = rig.map_left() + 30.0;
    rig.down(x, 1.0);
    let mut ctx = ViewContext::default();
    rig.frame.capture_lost(&mut ctx);
    let scroll = rig.editor.scroll_offset();
    rig.hover(x, 300.0);
    assert_eq!(rig.editor.scroll_offset(), scroll);
}

// ---- The map's rows ----

#[test]
fn row_runs_copy_the_macs_rules() {
    assert_eq!(row_runs("ab  cd", 98, 8), vec![(0, 2), (4, 2)]);
    // A tab goes to the next stop, and no-break spaces are blanks.
    assert_eq!(row_runs("a\tb", 98, 8), vec![(0, 1), (8, 1)]);
    assert_eq!(row_runs("a\tb", 98, 4), vec![(0, 1), (4, 1)]);
    assert_eq!(row_runs("a\u{a0}b", 98, 8), vec![(0, 1), (2, 1)]);
    // Line endings do not count as characters.
    assert_eq!(row_runs("ab\r\n", 98, 8), vec![(0, 2)]);
    assert_eq!(row_runs("", 98, 8), Vec::<(u16, u16)>::new());
    // A long line stops at the map's width.
    assert_eq!(row_runs(&"x".repeat(500), 98, 8), vec![(0, 98)]);
    // UTF-16 units count, as on the Mac: a mark and an emoji's two units.
    assert_eq!(row_runs("e\u{301}x", 98, 8), vec![(0, 3)]);
    assert_eq!(row_runs("\u{1f600}x", 98, 8), vec![(0, 3)]);
    // A tab cannot run past the end.
    assert_eq!(
        row_runs(&format!("{}\tx", "a".repeat(97)), 98, 8),
        vec![(0, 97)]
    );
}

fn map_rects(list: &DisplayList) -> Vec<Rect> {
    list.commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRects { rects, .. } => Some(rects.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn the_map_draws_blocks_of_the_macs_size_at_the_macs_place() {
    let rig = Rig::new("ab cd\n\n  x\n", 800.0, 400.0);
    let rects = map_rects(&rig.draw(ThemeKind::Light));
    let left = rig.map_left();
    let want = [
        (left + 2.0, 0.0, 2.0),
        (left + 5.0, 0.0, 2.0),
        (left + 4.0, 4.0, 1.0),
    ];
    assert_eq!(rects.len(), want.len(), "{rects:?}");
    for (rect, (x, y, width)) in rects.iter().zip(want) {
        assert_eq!((rect.x.0, rect.y.0, rect.width.0), (x, y, width));
        assert_eq!(rect.height.0, 1.4);
    }
}

#[test]
fn the_map_uses_the_themes_colors_and_only_shows_the_box_when_near() {
    let mut rig = Rig::new("hello\n", 800.0, 400.0);
    let theme = Theme::new(ThemeKind::Dark);
    let list = rig.draw(ThemeKind::Dark);
    assert!(list.commands.iter().any(|command| matches!(
        command,
        DrawCommand::FillRects { color, .. } if *color == theme.map_text
    )));
    assert!(
        !list
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::StrokeRect { .. }))
    );
    let left = rig.map_left();
    rig.hover(left + 1.0, 1.0);
    rig.advance(1.0);
    rig.tick();
    let list = rig.draw(ThemeKind::Dark);
    assert!(list.commands.iter().any(|command| matches!(
        command,
        DrawCommand::StrokeRect { color, width, .. } if *color == theme.map_box_outline && width.0 == 1.0
    )));
    assert!(list.commands.iter().any(|command| matches!(
        command,
        DrawCommand::FillRect { color, .. } if *color == theme.map_box_fill
    )));
    // No separator, as of the Mac's 1.2.1: the map's background is the text's.
    assert!(list.commands.iter().any(|command| matches!(
        command,
        DrawCommand::FillRect { rect, color } if *rect == rig.frame.map_area() && *color == theme.text_background
    )));
}

#[test]
fn a_key_starts_only_the_wait_and_the_rows_follow_on_the_worker() {
    let mut rig = Rig::new("first\nsecond\n", 800.0, 400.0);
    let jobs = rig.frame.counts().map_jobs;
    let ctx = rig.type_char('x');
    // The map's area does not repaint, and no job starts, on the key.
    assert_eq!(rig.frame.counts().map_jobs, jobs);
    let map = rig.frame.map_area();
    assert!(
        repaints(&ctx).iter().all(|rect| !overlaps(*rect, map)),
        "{:?}",
        repaints(&ctx)
    );
    assert!(timers(&ctx).iter().any(|ms| *ms <= 100));
    // More keys inside the wait add no work.
    rig.advance(0.05);
    rig.type_char('y');
    assert_eq!(rig.frame.counts().map_jobs, jobs);
    // After the wait, one job runs, and only the row that changed repaints.
    rig.advance(0.06);
    let mut painted = repaints(&rig.tick());
    assert_eq!(rig.frame.counts().map_jobs, jobs + 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.frame.map_job_running() {
        painted.extend(repaints(&rig.tick()));
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    painted.extend(repaints(&rig.tick()));
    let in_map: Vec<_> = painted
        .iter()
        .filter(|rect| overlaps(**rect, map))
        .collect();
    assert!(!in_map.is_empty());
    assert!(in_map.iter().all(|rect| rect.y.0 < 6.0), "{in_map:?}");
    let rects = map_rects(&rig.draw(ThemeKind::Light));
    assert_eq!(rects[0].width.0, 7.0, "the row shows the two new letters");
}

#[test]
fn an_edit_past_the_rows_the_map_holds_starts_no_job() {
    let mut rig = Rig::new(&lines(5000), 800.0, 200.0);
    let jobs = rig.frame.counts().map_jobs;
    append_away_from_view(&mut rig, "tail");
    rig.settle();
    assert_eq!(rig.frame.counts().map_jobs, jobs);
}

#[test]
fn the_work_of_a_job_stays_the_same_as_the_document_grows() {
    // The rows read are the map's own, plus a fixed margin, for any document size.
    let small = Rig::new(&lines(3000), 800.0, 400.0);
    let big = Rig::new(&lines(30_000), 800.0, 400.0);
    let count = |rig: &Rig| map_rects(&rig.draw(ThemeKind::Light)).len();
    assert_eq!(count(&small), count(&big));
    assert!(count(&small) < 3 * 202, "{}", count(&small));
}

// ---- The gutter ----

#[test]
fn line_numbers_start_off_and_the_map_starts_on() {
    let rig = Rig::new("x\n", 800.0, 400.0);
    assert!(!rig.frame.line_numbers());
    assert!(rig.frame.document_map());
    assert_eq!(rig.frame.gutter_area().width.0, 0.0);
    assert_eq!(rig.frame.map_area().width.0, 100.0);
    assert_eq!(rig.editor.text_area().width.0, 700.0);
}

#[test]
fn gutter_sizes_follow_the_mac() {
    assert_eq!(gutter_digits(0), 3);
    assert_eq!(gutter_digits(999), 3);
    assert_eq!(gutter_digits(1000), 4);
    assert_eq!(gutter_digits(1_000_000), 7);
    assert_eq!(gutter_width(3, 7.0), 35.0);
    assert_eq!(gutter_width(3, 7.1), 36.0);
    assert_eq!(gutter_font_points(10.0), 9.0);
    assert_eq!(gutter_font_points(13.0), 11.0);
    assert_eq!(gutter_font_points(5.0), 9.0);
    assert_eq!(gutter_font_points(160.0), 136.0);
    assert_eq!(GUTTER_LEFT_PADDING, 6.0);
    assert_eq!(GUTTER_RIGHT_PADDING, 8.0);
}

fn numbers_on(rig: &mut Rig) {
    let mut ctx = ViewContext::default();
    rig.frame
        .set_line_numbers(true, &mut rig.editor, &rig.measure, &mut ctx);
    rig.settle();
}

#[test]
fn numbers_show_on_a_lines_first_row_only_right_aligned_and_centered() {
    let mut rig = Rig::new(
        &format!("{}\nshort\n", "wrap me ".repeat(100)),
        500.0,
        400.0,
    );
    numbers_on(&mut rig);
    let visible = rig.editor.visible_rows();
    assert!(visible.rows.iter().filter(|row| row.continuation).count() > 3);
    let numbers: Vec<usize> = rig
        .frame
        .labels()
        .iter()
        .map(|label| label.number)
        .collect();
    assert_eq!(numbers, vec![1, 2, 3]);
    let list = rig.draw(ThemeKind::Light);
    let area = rig.frame.gutter_area();
    let right = area.x.0 + area.width.0 - 8.0;
    let measured = rig
        .measure
        .measure_with_font("1", "Segoe UI 9")
        .unwrap()
        .metrics;
    let runs: Vec<_> = list
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::TextRun {
                at,
                text,
                font,
                font_size,
                color,
            } => Some((*at, text.clone(), font.clone(), *font_size, *color)),
            _ => None,
        })
        .collect();
    assert_eq!(runs.len(), 3);
    for (at, text, font, size, color) in &runs {
        assert_eq!(font, GUTTER_FONT);
        // 85% of 10 pt, rounded, at least 9.
        assert!((size.0 - 12.0).abs() < 0.01);
        assert_eq!(*color, Theme::new(ThemeKind::Light).gutter_text);
        let width = text.len() as f32 * measured.width.0;
        assert!((at.x.0 + width - right).abs() < 0.01, "{}", at.x.0);
    }
    // Centered on the first row.
    let first = &visible.rows[0];
    let want = first.y.0 + first.height.0 / 2.0 - measured.height.0 / 2.0 + measured.baseline.0;
    assert!((runs[0].0.y.0 - want).abs() < 0.01);
    // A 1-dip separator at the right edge.
    let theme = Theme::new(ThemeKind::Light);
    assert!(list.commands.iter().any(|command| matches!(
        command,
        DrawCommand::FillRect { rect, color } if *color == theme.gutter_separator
            && rect.width.0 == 1.0 && rect.x.0 == area.x.0 + area.width.0 - 1.0
    )));
}

#[test]
fn the_gutter_grows_with_the_line_count_and_the_text_stays() {
    let mut rig = Rig::new(&lines(998), 800.0, 200.0);
    numbers_on(&mut rig);
    let narrow = rig.frame.gutter_area().width.0;
    assert_eq!(rig.editor.text_area().x.0, narrow);
    let mut ctx = ViewContext::default();
    rig.editor.set_scroll_position(1000.0, &mut ctx);
    rig.settle();
    let top = rig.editor.visible_rows().rows[0].first_byte;
    // The caret stands in the view, so that the edit below does not scroll to it.
    let mut ctx = ViewContext::default();
    rig.editor.run_host(
        qe_editor::HostCommand::Select {
            range: top..top,
            reveal: false,
        },
        &mut ctx,
    );
    // A new line at the end takes the count to 1000.
    append_away_from_view(&mut rig, "\nlast\n");
    rig.settle();
    let wide = rig.frame.gutter_area().width.0;
    assert!(wide > narrow);
    assert_eq!(rig.editor.text_area().x.0, wide);
    assert_eq!(rig.editor.text_area().width.0, 800.0 - wide - 100.0);
    assert_eq!(rig.editor.visible_rows().rows[0].first_byte, top);
}

#[test]
fn typing_inside_a_line_redraws_the_numbers_after_0_1_s_and_a_new_line_at_once() {
    let mut rig = Rig::new(&lines(50), 800.0, 400.0);
    numbers_on(&mut rig);
    let reads = rig.frame.counts().gutter_reads;
    let row_reads = rig.frame.counts().row_reads;
    let ctx = rig.type_char('x');
    assert_eq!(rig.frame.counts().gutter_reads, reads, "no read on the key");
    assert_eq!(
        rig.frame.counts().row_reads,
        row_reads,
        "no row read on the key"
    );
    let gutter = rig.frame.gutter_area();
    assert!(repaints(&ctx).iter().all(|rect| !overlaps(*rect, gutter)));
    assert!(timers(&ctx).iter().any(|ms| *ms <= 100));
    rig.advance(0.05);
    rig.tick();
    assert_eq!(rig.frame.counts().gutter_reads, reads, "still waiting");
    rig.advance(0.051);
    rig.tick();
    assert_eq!(rig.frame.counts().gutter_reads, reads + 1, "the wait ended");

    rig.settle();
    let reads = rig.frame.counts().gutter_reads;
    rig.send(InputKind::KeyDown {
        virtual_key: 0x0d,
        modifiers: Modifiers::default(),
        repeat: false,
    });
    assert!(
        rig.frame.counts().gutter_reads > reads,
        "a new line reads at once"
    );
}

#[test]
fn only_the_numbers_that_move_repaint() {
    let mut rig = Rig::new(&lines(50), 800.0, 400.0);
    numbers_on(&mut rig);
    // A new line on the last line on screen moves only what is below it.
    append_away_from_view(&mut rig, "\n");
    let ctx = rig.tick();
    let gutter = rig.frame.gutter_area();
    assert!(
        repaints(&ctx).iter().all(|rect| !overlaps(*rect, gutter)),
        "{:?}",
        repaints(&ctx)
    );
}

#[test]
fn clicks_on_the_gutter_and_the_map_do_not_move_the_caret_and_a_text_drag_carries_on() {
    let mut rig = Rig::new(&lines(50), 800.0, 400.0);
    numbers_on(&mut rig);
    let before = rig.editor.selection();
    rig.down(2.0, 50.0);
    rig.up(2.0, 50.0);
    assert_eq!(rig.editor.selection(), before);
    let left = rig.map_left();
    rig.down(left + 2.0, 50.0);
    rig.up(left + 2.0, 50.0);
    assert_eq!(rig.editor.selection(), before);

    // A press in the text selects, and a drag over the map still belongs to the text.
    let text_x = rig.editor.text_area().x.0 + 40.0;
    rig.down(text_x, 3.0);
    let anchor = rig.editor.selection().start;
    rig.hover(left + 50.0, 100.0);
    let selection = rig.editor.selection();
    assert_eq!(selection.start, anchor);
    assert!(selection.end > anchor);
    rig.up(left + 50.0, 100.0);
}

#[test]
fn the_views_work_on_a_read_only_view_and_show_unicode_lines() {
    let text = "e\u{301}\u{1f600}\r\n\u{628}\u{628}\r\n\u{4e2d}\r\n";
    let mut rig = Rig::new(text, 800.0, 400.0);
    numbers_on(&mut rig);
    let mut ctx = ViewContext::default();
    rig.editor
        .run_host(qe_editor::HostCommand::PreventEditing(true), &mut ctx);
    let numbers: Vec<usize> = rig
        .frame
        .labels()
        .iter()
        .map(|label| label.number)
        .collect();
    assert_eq!(numbers, vec![1, 2, 3, 4]);
    let ctx = rig.type_char('x');
    assert!(timers(&ctx).iter().all(|ms| *ms < 1000));
    assert_eq!(rig.editor.selection(), ByteOffset(0)..ByteOffset(0));
}

// ---- Over 50 MB ----

#[test]
fn over_50_mib_the_gutter_and_the_map_turn_off_and_at_50_mib_they_stay() {
    let mut rig = Rig::new(&lines(50), 800.0, 400.0);
    numbers_on(&mut rig);
    rig.frame.set_file_bytes(Some(LARGE_FILE_BYTES));
    let ctx = rig.tick();
    assert!(!rig.frame.is_large());
    assert!(repaints(&ctx).is_empty());
    assert_eq!(rig.frame.map_area().width.0, 100.0);
    assert!(rig.frame.gutter_area().width.0 > 0.0);

    rig.frame.set_file_bytes(Some(LARGE_FILE_BYTES + 1));
    let ctx = rig.tick();
    assert!(rig.frame.is_large());
    assert_eq!(rig.frame.map_area().width.0, 0.0);
    assert_eq!(rig.frame.gutter_area().width.0, 0.0);
    assert_eq!(rig.editor.text_area().width.0, 800.0);
    assert_eq!(rig.editor.text_area().x.0, 0.0);
    assert!(!repaints(&ctx).is_empty());
    assert!(rig.frame.map_box().is_none());
    assert!(rig.frame.labels().is_empty());

    // The settings stay, and only the views go. No work happens, and nothing draws.
    assert!(rig.frame.line_numbers() && rig.frame.document_map());
    let jobs = rig.frame.counts().map_jobs;
    let reads = rig.frame.counts().gutter_reads;
    rig.type_char('x');
    rig.settle();
    assert_eq!(rig.frame.counts().map_jobs, jobs);
    assert_eq!(rig.frame.counts().gutter_reads, reads);
    let list = rig.draw(ThemeKind::Light);
    assert!(map_rects(&list).is_empty());
    assert!(
        !list
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::TextRun { .. }))
    );
    // Moving over where the map was does not show a box.
    rig.hover(790.0, 10.0);
    assert_eq!(rig.frame.fade().alpha(), 0.0);
    assert!(!rig.frame.fade().fading());
    // A smaller document brings them back.
    rig.frame.set_file_bytes(Some(10));
    rig.settle();
    assert_eq!(rig.frame.map_area().width.0, 100.0);
    assert!(rig.frame.gutter_area().width.0 > 0.0);
    assert_eq!(
        LARGE_FILE_TITLE_NOTE,
        "Large file: spelling and substitutions off"
    );
}

// ---- Wrap to Page ----

#[test]
fn paper_comes_from_the_region_as_row_i26_says() {
    assert_eq!(Paper::for_region("US"), Paper::LETTER);
    assert_eq!(Paper::for_region("ca"), Paper::LETTER);
    assert_eq!(Paper::for_region("GB"), Paper::A4);
    assert_eq!(Paper::for_region("JP"), Paper::A4);
    assert_eq!(Paper::for_region(""), Paper::A4);
    assert_eq!(Paper::from_setting(Some("Legal"), "US"), Paper::LEGAL);
    assert_eq!(Paper::from_setting(Some("junk"), "CA"), Paper::LETTER);
    assert_eq!(Paper::from_setting(None, "DE"), Paper::A4);
    assert_eq!((Paper::LETTER.width, Paper::LETTER.height), (612.0, 792.0));
    assert_eq!((Paper::A4.width, Paper::A4.height), (595.0, 842.0));
    assert_eq!((Paper::LEGAL.width, Paper::LEGAL.height), (612.0, 1008.0));
    assert_eq!(Paper::LETTER.text_width_pt(), 468.0);
    assert_eq!(Paper::LETTER.text_width_dips(), 624.0);
    assert_eq!(Paper::LETTER.margin_dips(), 96.0);
    assert_eq!(PAGE_MARGIN_PT, 72.0);
    // The paper Windows' own region gives is one of the two.
    let system = system_paper();
    assert!(system == Paper::LETTER || system == Paper::A4);
    let _ = system_reduce_motion();
}

#[test]
fn wrap_to_page_hides_the_side_views_and_draws_white_paper_in_a_dark_window() {
    let mut rig = Rig::new(&"word ".repeat(400), 1000.0, 400.0);
    numbers_on(&mut rig);
    assert!(rig.editor.text_area().x.0 > 0.0);
    let mut ctx = ViewContext::default();
    rig.frame
        .set_page_wrap(true, &mut rig.editor, &rig.measure, &mut ctx)
        .unwrap();
    rig.settle();
    assert!(rig.frame.page_wrap());
    assert_eq!(rig.frame.gutter_area().width.0, 0.0);
    assert_eq!(rig.frame.map_area().width.0, 0.0);
    assert_eq!(rig.editor.text_area().width.0, 1000.0);

    let dark = Theme::new(ThemeKind::Dark);
    let list = rig.draw(ThemeKind::Dark);
    let fills: Vec<(Rect, qe_api::Color)> = list
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRect { rect, color } => Some((*rect, *color)),
            _ => None,
        })
        .collect();
    assert_eq!(fills[0].1, dark.under_page);
    assert_eq!(fills[1].1, qe_api::Color::rgb(255, 255, 255));
    assert_eq!(fills[1].0.width.0, 816.0);
    // The Mac's frame around the text's sides, in light gray, in both themes.
    assert_eq!(fills[2].0.x.0, 95.0);
    assert_eq!(fills[3].0.x.0, 96.0 + 624.0);
    assert_eq!(fills[2].1, qe_api::Color::rgb(170, 170, 170));
    // The editor paints no background over the paper, and its text is dark.
    let glyphs: Vec<qe_api::Color> = list
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::GlyphRun { color, .. } => Some(*color),
            _ => None,
        })
        .collect();
    assert!(!glyphs.is_empty());
    assert!(
        glyphs
            .iter()
            .all(|color| *color == Theme::new(ThemeKind::Light).text)
    );
    assert!(
        !fills
            .iter()
            .any(|(_, color)| *color == dark.text_background)
    );
    // Rows hold what fits in 624 dips, whatever the window's width.
    let visible = rig.editor.visible_rows();
    let row_bytes = visible.rows[1].first_byte.0 - visible.rows[0].first_byte.0;
    assert_eq!(row_bytes, 80, "{visible:?}");

    // And back.
    let mut ctx = ViewContext::default();
    rig.frame
        .set_page_wrap(false, &mut rig.editor, &rig.measure, &mut ctx)
        .unwrap();
    rig.settle();
    assert!(rig.frame.gutter_area().width.0 > 0.0);
    assert_eq!(rig.frame.map_area().width.0, 100.0);
    assert_eq!(wrap_menu_title(false), "Wrap to Page");
    assert_eq!(wrap_menu_title(true), "Wrap to Window");
}

#[test]
fn the_page_keeps_its_line_breaks_when_the_window_shrinks_and_paper_scrolls_sideways() {
    let mut rig = Rig::new(&"word ".repeat(400), 1000.0, 400.0);
    let mut ctx = ViewContext::default();
    rig.frame
        .set_page_wrap(true, &mut rig.editor, &rig.measure, &mut ctx)
        .unwrap();
    rig.settle();
    let wide: Vec<_> = rig
        .editor
        .visible_rows()
        .rows
        .iter()
        .map(|r| r.first_byte)
        .collect();
    let mut ctx = ViewContext::default();
    rig.frame
        .layout(&mut rig.editor, rect(400.0, 400.0), &rig.measure, &mut ctx);
    rig.settle();
    let narrow: Vec<_> = rig
        .editor
        .visible_rows()
        .rows
        .iter()
        .map(|r| r.first_byte)
        .collect();
    assert_eq!(wide, narrow);
}

// ---- Zoom ----

#[test]
fn zoom_steps_are_the_macs() {
    assert_eq!(zoom_in_scale(1.0), 2.0);
    assert_eq!(zoom_in_scale(2.0), 4.0);
    assert_eq!(zoom_in_scale(8.0), 16.0);
    assert_eq!(zoom_in_scale(16.0), 16.0);
    assert_eq!(zoom_in_scale(0.25), 0.5);
    assert_eq!(zoom_in_scale(0.5), 1.0);
    assert_eq!(zoom_in_scale(0.45), 1.0);
    assert_eq!(zoom_out_scale(1.0), 0.5);
    assert_eq!(zoom_out_scale(0.5), 0.25);
    assert_eq!(zoom_out_scale(0.25), 0.25);
    assert_eq!(zoom_out_scale(4.0), 2.0);
    assert_eq!(zoom_out_scale(2.0), 1.0);
    assert_eq!(zoom_out_scale(1.9), 1.0);
    assert_eq!(zoom_out_scale(16.0), 8.0);
    assert_eq!((MIN_ZOOM, MAX_ZOOM), (0.25, 16.0));
}

#[test]
fn zoom_runs_through_apply_font_and_keeps_the_top_text() {
    let mut rig = Rig::new(&lines(1000), 800.0, 400.0);
    numbers_on(&mut rig);
    let mut ctx = ViewContext::default();
    rig.editor.set_scroll_position(4000.0, &mut ctx);
    rig.settle();
    let top = rig.editor.visible_rows().rows[0].first_byte;
    let height = rig.editor.visible_rows().rows[0].height.0;
    let gutter = rig.frame.gutter_area().width.0;

    let mut ctx = ViewContext::default();
    rig.frame
        .zoom_in(&mut rig.editor, &rig.measure, &mut ctx)
        .unwrap();
    assert!(repaints(&ctx).contains(&rect(800.0, 400.0)));
    rig.settle();
    assert_eq!(rig.frame.zoom(), 2.0);
    let rows = rig.editor.visible_rows();
    assert!((rows.rows[0].height.0 / height - 2.0).abs() < 0.2);
    assert!(
        rows.rows.iter().take(2).any(|row| row.first_byte == top),
        "{top:?} {rows:?}"
    );
    assert!(
        rig.frame.gutter_area().width.0 > gutter,
        "the gutter follows"
    );

    // The map keeps its size, and shows the lines on screen.
    assert_eq!(rig.frame.map_area().width.0, 100.0);
    let on_screen = rig.frame.geometry().unwrap().on_screen;
    assert_eq!(
        on_screen,
        rows.rows.last().unwrap().line.0 - rows.rows[0].line.0 + 1
    );

    let mut ctx = ViewContext::default();
    rig.frame
        .actual_size(&mut rig.editor, &rig.measure, &mut ctx)
        .unwrap();
    rig.settle();
    assert_eq!(rig.frame.zoom(), 1.0);
    assert_eq!(rig.frame.gutter_area().width.0, gutter);
    assert!((rig.editor.visible_rows().rows[0].height.0 - height).abs() < 0.01);

    // Out of range says so, and changes nothing.
    let mut ctx = ViewContext::default();
    let wrong = rig
        .frame
        .set_zoom(32.0, &mut rig.editor, &rig.measure, &mut ctx);
    assert!(wrong.unwrap_err().contains("1600"));
    assert_eq!(rig.frame.zoom(), 1.0);
}

#[test]
fn the_zoom_stops_at_each_end() {
    let mut rig = Rig::new("x\n", 800.0, 400.0);
    let mut ctx = ViewContext::default();
    for _ in 0..8 {
        rig.frame
            .zoom_in(&mut rig.editor, &rig.measure, &mut ctx)
            .unwrap();
    }
    assert_eq!(rig.frame.zoom(), 16.0);
    assert!(!rig.frame.can_zoom_in());
    assert!(rig.frame.can_zoom_out());
    for _ in 0..10 {
        rig.frame
            .zoom_out(&mut rig.editor, &rig.measure, &mut ctx)
            .unwrap();
    }
    assert_eq!(rig.frame.zoom(), 0.25);
    assert!(!rig.frame.can_zoom_out());
    assert!(rig.frame.can_zoom_in());
    rig.settle();
}

#[test]
fn a_page_keeps_the_same_breaks_at_every_zoom() {
    let text = "the quick brown fox jumps over the lazy dog. ".repeat(60);
    let mut rig = Rig::new(&text, 3000.0, 2000.0);
    let mut ctx = ViewContext::default();
    rig.frame
        .set_page_wrap(true, &mut rig.editor, &rig.measure, &mut ctx)
        .unwrap();
    rig.settle();
    let at_100: Vec<_> = rig
        .editor
        .visible_rows()
        .rows
        .iter()
        .map(|r| r.first_byte)
        .collect();
    assert!(at_100.len() > 10);
    for scale in [0.5f32, 2.0] {
        let mut ctx = ViewContext::default();
        rig.frame
            .set_zoom(scale, &mut rig.editor, &rig.measure, &mut ctx)
            .unwrap();
        rig.settle();
        let rows: Vec<_> = rig
            .editor
            .visible_rows()
            .rows
            .iter()
            .map(|r| r.first_byte)
            .collect();
        let shared = rows.len().min(at_100.len());
        assert_eq!(rows[..shared], at_100[..shared], "at {scale}");
        assert!(shared > 5);
    }
}

#[test]
fn the_frame_asks_for_the_soonest_timer_when_the_editor_asked_for_one_too() {
    let mut rig = Rig::new("x\n", 800.0, 400.0);
    let mut ctx = ViewContext::default();
    ctx.request_timer(5);
    let left = rig.map_left();
    rig.frame.handle(
        &mut rig.editor,
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseMove {
                at: point(left + 1.0, 1.0),
            },
        },
        &rig.measure,
        &mut ctx,
    );
    assert_eq!(timers(&ctx), vec![5], "the sooner request stands");

    let mut ctx = ViewContext::default();
    ctx.request_timer(500);
    rig.advance(0.01);
    rig.frame.sync(&mut rig.editor, &rig.measure, &mut ctx);
    assert_eq!(
        timers(&ctx).last(),
        Some(&16),
        "the last request wins in the shell"
    );
}
