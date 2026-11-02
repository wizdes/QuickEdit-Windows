//! `ViewFrame`: the gutter, the map, the paper and the zoom around one `EditorView`.
//!
//! All calls run on the UI thread. `draw` paints from what the frame holds and never from
//! the live document, so a repaint for another reason cannot show a number or a map row
//! early, and a partial repaint always matches a full one. `sync` is what moves that held
//! state forward, and it asks for a repaint of exactly the parts that changed.
use crate::fade::Fade;
use crate::gutter::{
    GUTTER_FONT, GUTTER_REDRAW_SECONDS, GUTTER_RIGHT_PADDING, GutterLabel, gutter_digits,
    gutter_font_points, gutter_labels, gutter_width,
};
use crate::map::{
    Job, MAP_BLOCK_HEIGHT, MAP_COLUMN_WIDTH, MAP_HOVER_MARGIN, MAP_LEFT, MAP_MOST_ROWS,
    MAP_REDRAW_SECONDS, MAP_ROW_HEIGHT, MAP_SLACK_ROWS, MAP_WIDTH, MapGeometry, MapRows, MapWorker,
    Wake, first_line_for_box_top,
};
use crate::page::{MAX_ZOOM, MIN_ZOOM, Paper, zoom_in_scale, zoom_out_scale};
use crate::system::system_reduce_motion;
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, MouseCursor,
    Point, Rect, StoreStatus, TextMeasurer, Theme, ThemeKind, View, ViewContext, ViewRequest,
};
use qe_editor::{ChangesSince, EditorView, PageWrap, VisibleRows};
use std::sync::Arc;
use std::time::Instant;

/// Over this many bytes the gutter and the map turn off (`largeFileThreshold` on the Mac).
pub const LARGE_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// The Mac says nothing about the gutter or the map going. It only has this title note,
/// which the window's title adds after the file's name and a dash.
pub const LARGE_FILE_TITLE_NOTE: &str = "Large file: spelling and substitutions off";

const FADE_FRAME_MS: u32 = 16;
/// How tall a digit's ink is, as a share of the font size. Segoe UI's digits are 0.7 em.
const DIGIT_INK_EM: f32 = 0.72;
/// `NSColor.lightGray`, which the Mac's page frame uses in both themes.
const PAGE_FRAME: Color = Color {
    r: 170,
    g: 170,
    b: 170,
    a: 255,
};

/// The map's box as it shows now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapBox {
    pub rect: Rect,
    pub alpha: f32,
}

/// Counts that let a test or a measurement see the work done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCounts {
    /// Jobs sent to the map's worker.
    pub map_jobs: u64,
    /// Times the gutter's numbers were worked out again.
    pub gutter_reads: u64,
    /// Calls to `visible_rows`.
    pub row_reads: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NumberMetrics {
    points: f32,
    digit_width: f32,
    height: f32,
    baseline: f32,
}

/// Who owns a pressed button until its release. The map and the text own a drag. `Side`
/// is a press that does nothing: in the gutter, as the Mac's ruler takes no clicks, or a
/// second button during a drag in the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PressOwner {
    None,
    Text,
    Map,
    Side,
}

/// One owner for each mouse button, because two buttons can be down at once.
#[derive(Clone, Copy, Debug)]
struct Presses([PressOwner; 5]);
impl Presses {
    fn slot(button: MouseButton) -> usize {
        match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::X1 => 3,
            MouseButton::X2 => 4,
        }
    }
    fn set(&mut self, button: MouseButton, owner: PressOwner) {
        self.0[Self::slot(button)] = owner;
    }
    fn take(&mut self, button: MouseButton) -> PressOwner {
        std::mem::replace(&mut self.0[Self::slot(button)], PressOwner::None)
    }
    fn any(&self, owner: PressOwner) -> bool {
        self.0.contains(&owner)
    }
    /// Who a move goes to. A drag in the map holds the pointer, so it comes first.
    fn move_owner(&self) -> PressOwner {
        [PressOwner::Map, PressOwner::Text, PressOwner::Side]
            .into_iter()
            .find(|owner| self.any(*owner))
            .unwrap_or(PressOwner::None)
    }
}

/// See the crate's README for the calls a host makes.
pub struct ViewFrame {
    line_numbers: bool,
    document_map: bool,
    page_wrap: bool,
    scale: f32,
    paper: Paper,
    family: String,
    base_points: f32,
    tab_width: usize,
    file_bytes: Option<u64>,

    bounds: Rect,
    large: bool,
    gutter_w: f32,
    map_w: f32,
    metrics: Option<NumberMetrics>,
    digits: usize,
    labels: Vec<GutterLabel>,
    gutter_due: Option<f64>,

    geometry: Option<MapGeometry>,
    rows: MapRows,
    map_due: Option<f64>,
    map_stale: bool,
    worker: Option<MapWorker>,
    job_number: u64,
    job_done: u64,
    wake: Option<Wake>,
    problem: Option<String>,
    /// The lines the last job read, and how many lines the index knew then. A job that
    /// could not reach its lines is sent again only when the index knows more.
    last_job: Option<(std::ops::Range<usize>, usize)>,

    fade: Fade,
    near: bool,
    drag_offset: Option<f64>,
    /// How far down its travel the pointer holds the box, during a drag. Heights learned
    /// from new layouts move the scroll limit, and the box must stay under the pointer.
    drag_fraction: Option<f64>,
    press: Presses,

    seen_revision: u64,
    seen_document: u64,
    seen_lines: usize,
    seen_scroll: f64,
    force_read: bool,

    start: Instant,
    clock: Option<Box<dyn Fn() -> f64>>,
    reduce_motion: bool,
    counts: FrameCounts,
}

impl ViewFrame {
    /// `family` and `points` are the editor's font at 100%. Line numbers start off and the
    /// map starts on, as the Mac's defaults do.
    pub fn new(paper: Paper, family: &str, points: f32) -> Self {
        Self {
            line_numbers: false,
            document_map: true,
            page_wrap: false,
            scale: 1.0,
            paper,
            family: family.into(),
            base_points: points,
            tab_width: 8,
            file_bytes: None,
            bounds: Rect::default(),
            large: false,
            gutter_w: 0.0,
            map_w: 0.0,
            metrics: None,
            digits: 0,
            labels: Vec::new(),
            gutter_due: None,
            geometry: None,
            rows: MapRows::default(),
            map_due: None,
            map_stale: true,
            worker: None,
            job_number: 0,
            job_done: 0,
            wake: None,
            problem: None,
            last_job: None,
            fade: Fade::default(),
            near: false,
            drag_offset: None,
            drag_fraction: None,
            press: Presses([PressOwner::None; 5]),
            seen_revision: u64::MAX,
            seen_document: u64::MAX,
            seen_lines: usize::MAX,
            seen_scroll: f64::NAN,
            force_read: true,
            start: Instant::now(),
            clock: None,
            reduce_motion: system_reduce_motion(),
            counts: FrameCounts::default(),
        }
    }

    // ---- What the host sets ----

    /// How the map's worker tells the host that its rows are ready. Without it, the frame
    /// asks for a 16 ms timer while a job runs. Set it before the first `sync`.
    pub fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }
    /// A test's clock, in seconds.
    pub fn set_clock(&mut self, clock: Box<dyn Fn() -> f64>) {
        self.clock = Some(clock);
    }
    /// Call this when `system_reduce_motion` changes, such as on `WM_SETTINGCHANGE`.
    pub fn set_reduce_motion(&mut self, reduce: bool) {
        self.reduce_motion = reduce;
    }
    /// The size of the file as read, as the Mac measures it. Without it, the frame uses
    /// the text it knows of, so typing past 50 MiB also turns the views off.
    pub fn set_file_bytes(&mut self, bytes: Option<u64>) {
        self.file_bytes = bytes;
    }
    pub fn set_tab_width(&mut self, width: usize) {
        self.map_stale |= width != self.tab_width;
        self.tab_width = width.max(1);
    }
    /// A new paper size. With Wrap to Page on, the text wraps again at the new width at once.
    pub fn set_paper(
        &mut self,
        paper: Paper,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        self.paper = paper;
        if self.page_wrap {
            editor.set_page_wrap(self.page_hook(), ctx);
            self.force_read = true;
            ctx.invalidate(self.bounds);
            self.sync(editor, measure, ctx);
        }
    }
    pub fn set_line_numbers(
        &mut self,
        on: bool,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        self.line_numbers = on;
        self.force_read = true;
        self.sync(editor, measure, ctx);
    }
    pub fn set_document_map(
        &mut self,
        on: bool,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        self.document_map = on;
        self.force_read = true;
        self.sync(editor, measure, ctx);
    }
    /// Wrap to Page on or off. The gutter and the map hide while it is on, as on the Mac.
    pub fn set_page_wrap(
        &mut self,
        on: bool,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        if on {
            editor.set_wrap(true)?;
        }
        self.page_wrap = on;
        editor.set_page_wrap(self.page_hook(), ctx);
        editor.set_paints_background(!on, ctx);
        self.force_read = true;
        ctx.invalidate(self.bounds);
        self.sync(editor, measure, ctx);
        Ok(())
    }
    /// Sets the zoom through `apply_font`. A page keeps its line breaks, because its width
    /// grows with the font. Out of range is an error.
    pub fn set_zoom(
        &mut self,
        scale: f32,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        if !(MIN_ZOOM..=MAX_ZOOM).contains(&scale) {
            return Err(format!(
                "The zoom must be from {}% to {}%.",
                MIN_ZOOM * 100.0,
                MAX_ZOOM * 100.0
            ));
        }
        editor.apply_font(&self.family, self.base_points * scale, ctx)?;
        self.scale = scale;
        editor.set_page_wrap(self.page_hook(), ctx);
        self.force_read = true;
        ctx.invalidate(self.bounds);
        self.sync(editor, measure, ctx);
        Ok(())
    }
    pub fn zoom_in(
        &mut self,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.set_zoom(zoom_in_scale(self.scale), editor, measure, ctx)
    }
    pub fn zoom_out(
        &mut self,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.set_zoom(zoom_out_scale(self.scale), editor, measure, ctx)
    }
    pub fn actual_size(
        &mut self,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.set_zoom(1.0, editor, measure, ctx)
    }
    /// Sets the font at 100%, keeping the zoom.
    pub fn set_base_font(
        &mut self,
        family: &str,
        points: f32,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        editor.apply_font(family, points * self.scale, ctx)?;
        self.family = family.into();
        self.base_points = points;
        self.force_read = true;
        self.sync(editor, measure, ctx);
        Ok(())
    }

    // ---- What the host reads ----

    pub fn zoom(&self) -> f32 {
        self.scale
    }
    pub fn can_zoom_in(&self) -> bool {
        self.scale < MAX_ZOOM - 0.0001
    }
    pub fn can_zoom_out(&self) -> bool {
        self.scale > MIN_ZOOM + 0.0001
    }
    pub fn page_wrap(&self) -> bool {
        self.page_wrap
    }
    pub fn paper(&self) -> Paper {
        self.paper
    }
    pub fn line_numbers(&self) -> bool {
        self.line_numbers
    }
    pub fn document_map(&self) -> bool {
        self.document_map
    }
    /// True when the document is over 50 MiB.
    pub fn is_large(&self) -> bool {
        self.large
    }
    pub fn gutter_area(&self) -> Rect {
        Rect {
            width: Dip(self.gutter_w),
            ..self.bounds
        }
    }
    pub fn map_area(&self) -> Rect {
        Rect {
            x: Dip(self.bounds.x.0 + self.bounds.width.0 - self.map_w),
            width: Dip(self.map_w),
            ..self.bounds
        }
    }
    pub fn labels(&self) -> &[GutterLabel] {
        &self.labels
    }
    pub fn fade(&self) -> &Fade {
        &self.fade
    }
    pub fn counts(&self) -> FrameCounts {
        self.counts
    }
    pub fn geometry(&self) -> Option<MapGeometry> {
        self.geometry
    }
    /// True while the worker owes rows. It is false when the frame is idle.
    pub fn map_job_running(&self) -> bool {
        self.job_done != self.job_number
    }
    /// The job the map's worker may work on, so a test can see that it was told to stop.
    pub fn map_worker_newest_job(&self) -> u64 {
        self.worker.as_ref().map_or(0, MapWorker::newest_job)
    }
    /// A problem that stopped the map, in plain words.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }
    /// Where the box is, or `None` when the map is off or knows no geometry yet.
    pub fn map_box(&self) -> Option<MapBox> {
        if self.map_w <= 0.0 {
            return None;
        }
        let geometry = self.geometry?;
        Some(MapBox {
            rect: self.box_rect(&geometry),
            alpha: self.fade.alpha(),
        })
    }

    // ---- Calls that run the frame ----

    /// At a resize. `bounds` is the whole content area, which the editor's margins split.
    pub fn layout(
        &mut self,
        editor: &mut EditorView,
        bounds: Rect,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        if bounds != self.bounds {
            self.bounds = bounds;
            self.force_read = true;
            ctx.invalidate(bounds);
        }
        editor.layout(bounds, measure);
        self.sync(editor, measure, ctx);
    }

    /// Input for the content area. It replaces the host's `editor.handle`.
    pub fn handle(
        &mut self,
        editor: &mut EditorView,
        event: &InputEvent,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Handled {
        let handled = self.route(editor, event, ctx);
        self.sync(editor, measure, ctx);
        handled
    }

    /// The pointer left the window. A host calls this on `WM_MOUSELEAVE`.
    pub fn pointer_left(&mut self, ctx: &mut ViewContext) {
        self.set_near(false, ctx);
    }
    /// Another window took the mouse during a drag, in the map or in the text.
    pub fn capture_lost(&mut self, editor: &mut EditorView, ctx: &mut ViewContext) {
        if self.press.any(PressOwner::Text) {
            editor.cancel_drag();
        }
        if self.press.any(PressOwner::Map) {
            self.drag_offset = None;
            self.drag_fraction = None;
            self.near = false;
            self.aim_fade(ctx);
        }
        self.press = Presses([PressOwner::None; 5]);
    }

    /// Call this after any timer or wake, with `editor.poll_layout` before it, and after
    /// any change to the editor that did not come through `handle`. It does no work that
    /// grows with the document, and it asks for a timer only while something is due.
    pub fn sync(
        &mut self,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        let now = self.now();
        let status = editor.store_status();
        self.update_large(&status);
        self.fit_margins(editor, &status, measure, ctx);
        let (text_changed, lines_changed) = self.read_changes(editor, &status);

        // Fade first: it needs no read of the text.
        let before = self.fade.alpha();
        self.fade.step(now);
        if self.fade.alpha() != before {
            self.invalidate_box(ctx);
        }

        let scroll = editor.scroll_offset();
        let moved = scroll != self.seen_scroll || self.force_read;
        self.seen_scroll = scroll;

        // The Mac's rule: typing inside a line waits 0.1 s, and every other change shows
        // at once. A key takes this path and does no read.
        if text_changed && !lines_changed && !moved {
            if self.gutter_w > 0.0 {
                self.gutter_due.get_or_insert(now + GUTTER_REDRAW_SECONDS);
            }
        } else if lines_changed {
            self.gutter_due = None;
        }
        if (text_changed || lines_changed) && self.map_w > 0.0 {
            self.map_due.get_or_insert(now + MAP_REDRAW_SECONDS);
        }
        let waiting = |due: Option<f64>| due.is_some_and(|due| due > now);
        let read_gutter = self.gutter_w > 0.0 && !waiting(self.gutter_due);
        let read_map = self.map_w > 0.0 && (moved || !waiting(self.map_due));
        if read_gutter || read_map {
            self.counts.row_reads += 1;
            let visible = editor.visible_rows();
            if read_gutter {
                self.gutter_due = None;
                // With no scroll, a line that has lost its layout for a moment keeps its
                // number. After a scroll, only what is known shows.
                if visible.complete || moved {
                    self.read_gutter(&visible, measure, ctx);
                }
            }
            if read_map {
                if !waiting(self.map_due) {
                    self.map_due = None;
                }
                self.read_map(editor, &status, &visible, ctx);
            }
        }
        self.force_read = false;
        if self.map_w > 0.0 {
            self.take_rows(ctx);
        }
        self.ask_for_timer(now, ctx);
    }

    /// Paints the paper, the text, the gutter and the map. Each list paints the opaque
    /// background of its parts, which `qe-render` needs for a partial repaint.
    pub fn draw(
        &self,
        editor: &EditorView,
        list: &mut DisplayList,
        theme: &Theme,
        measure: &dyn TextMeasurer,
    ) {
        if self.page_wrap {
            self.draw_paper(editor, list, theme);
            // Pages are paper: white with dark text in a dark window too, as on the Mac.
            editor.draw(list, &Theme::new(ThemeKind::Light), measure);
            return;
        }
        editor.draw(list, theme, measure);
        if self.gutter_w > 0.0 {
            self.draw_gutter(list, theme);
        }
        if self.map_w > 0.0 {
            self.draw_map(list, theme);
        }
    }

    // ---- Input ----

    fn route(
        &mut self,
        editor: &mut EditorView,
        event: &InputEvent,
        ctx: &mut ViewContext,
    ) -> Handled {
        match &event.kind {
            InputKind::MouseMove { at } => {
                let at = *at;
                self.set_near(self.in_hover_area(at), ctx);
                match self.press.move_owner() {
                    PressOwner::Map => {
                        self.drag_to(editor, at.y.0, ctx);
                        Handled::Yes
                    }
                    PressOwner::Side => Handled::Yes,
                    PressOwner::Text => editor.handle(event, ctx),
                    PressOwner::None => {
                        if self.in_side_part(at) {
                            ctx.set_cursor(MouseCursor::Arrow);
                            Handled::Yes
                        } else {
                            editor.handle(event, ctx)
                        }
                    }
                }
            }
            InputKind::MouseDown { at, button, .. } => {
                let (at, button) = (*at, *button);
                if self.press.any(PressOwner::Map) {
                    // A second button during a drag in the map does nothing.
                    self.press.set(button, PressOwner::Side);
                    Handled::Yes
                } else if contains(self.map_area(), at) {
                    if button == MouseButton::Left {
                        self.press.set(button, PressOwner::Map);
                        ctx.capture_mouse(true);
                        self.press_map(editor, at.y.0, ctx);
                    } else {
                        self.press.set(button, PressOwner::Side);
                    }
                    Handled::Yes
                } else if contains(self.gutter_area(), at) {
                    // The Mac's ruler takes no clicks, and a click there selects nothing.
                    self.press.set(button, PressOwner::Side);
                    Handled::Yes
                } else {
                    self.press.set(button, PressOwner::Text);
                    editor.handle(event, ctx)
                }
            }
            InputKind::MouseUp { at, button, .. } => {
                let at = *at;
                match self.press.take(*button) {
                    PressOwner::Map => {
                        ctx.capture_mouse(false);
                        // Where the pointer is first, so a drag that ended outside the
                        // area does not flash the box back on.
                        self.near = self.in_hover_area(at);
                        self.drag_offset = None;
                        self.drag_fraction = None;
                        self.aim_fade(ctx);
                        Handled::Yes
                    }
                    PressOwner::Side => Handled::Yes,
                    PressOwner::Text | PressOwner::None => editor.handle(event, ctx),
                }
            }
            InputKind::Resize { width, height } => {
                self.bounds.width = *width;
                self.bounds.height = *height;
                self.force_read = true;
                ctx.invalidate(self.bounds);
                editor.handle(event, ctx)
            }
            _ => editor.handle(event, ctx),
        }
    }

    fn in_side_part(&self, at: Point) -> bool {
        contains(self.map_area(), at) || contains(self.gutter_area(), at)
    }
    fn in_hover_area(&self, at: Point) -> bool {
        if self.map_w <= 0.0 {
            return false;
        }
        let map = self.map_area();
        contains(
            Rect {
                x: Dip(map.x.0 - MAP_HOVER_MARGIN),
                width: Dip(map.width.0 + MAP_HOVER_MARGIN),
                ..map
            },
            at,
        )
    }
    fn set_near(&mut self, near: bool, ctx: &mut ViewContext) {
        if near != self.near {
            self.near = near;
            self.aim_fade(ctx);
        }
    }
    fn aim_fade(&mut self, ctx: &mut ViewContext) {
        let now = self.now();
        let show = self.near || self.drag_offset.is_some();
        let jump = self.reduce_motion || self.map_w <= 0.0;
        let before = self.fade.alpha();
        self.fade.aim(show, now, jump);
        if self.fade.alpha() != before {
            self.invalidate_box(ctx);
        }
    }

    /// A press on the box holds it where it was pressed. A press elsewhere centers the box
    /// on the pointer, which is the Mac's "a click centers the clicked line".
    fn press_map(&mut self, editor: &mut EditorView, y: f32, ctx: &mut ViewContext) {
        let Some(geometry) = self.geometry else {
            return;
        };
        let top = self.bounds.y.0 as f64 + geometry.box_top();
        let height = geometry.box_height();
        let at = f64::from(y);
        if at >= top && at < top + height {
            self.drag_offset = Some(at - top);
            self.aim_fade(ctx);
        } else {
            self.drag_offset = Some(height / 2.0);
            self.aim_fade(ctx);
            self.drag_to(editor, y, ctx);
        }
    }
    /// Puts the box's top where the pointer holds it, through `set_scroll_position`.
    fn drag_to(&mut self, editor: &mut EditorView, y: f32, ctx: &mut ViewContext) {
        let (Some(offset), Some(geometry)) = (self.drag_offset, self.geometry) else {
            return;
        };
        let box_top = f64::from(y - self.bounds.y.0) - offset;
        let first = first_line_for_box_top(
            box_top,
            geometry.line_count,
            geometry.on_screen,
            geometry.rows,
            f64::from(self.bounds.height.0),
        );
        let scrollable = geometry.line_count.saturating_sub(geometry.on_screen);
        if scrollable > 0 {
            let fraction = first / scrollable as f64;
            self.drag_fraction = Some(fraction);
            editor.set_scroll_position(fraction * editor.max_scroll_position(), ctx);
        }
    }

    // ---- Keeping up with the editor ----

    fn now(&self) -> f64 {
        match &self.clock {
            Some(clock) => clock(),
            None => self.start.elapsed().as_secs_f64(),
        }
    }
    fn update_large(&mut self, status: &StoreStatus) {
        let bytes = self.file_bytes.unwrap_or(status.known_bytes.0 as u64);
        self.large = bytes > LARGE_FILE_BYTES;
    }
    fn page_hook(&self) -> Option<PageWrap> {
        self.page_wrap.then(|| PageWrap {
            text_width: Dip(self.paper.text_width_dips() * self.scale),
            margin: Dip(self.paper.margin_dips() * self.scale),
        })
    }
    fn number_metrics(&mut self, measure: &dyn TextMeasurer) -> NumberMetrics {
        let points = gutter_font_points(self.base_points * self.scale);
        if let Some(metrics) = self.metrics.filter(|m| m.points == points) {
            return metrics;
        }
        let measured = measure.measure("8", &format!("{GUTTER_FONT} {points}"));
        let metrics = NumberMetrics {
            points,
            digit_width: measured.width.0,
            height: measured.height.0,
            baseline: measured.baseline.0,
        };
        self.metrics = Some(metrics);
        metrics
    }
    /// Changes the editor's margins only when a width really changes, which is the one
    /// case that moves the text. So a key never does it unless the digit count grows.
    fn fit_margins(
        &mut self,
        editor: &mut EditorView,
        status: &StoreStatus,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        let shown = !self.page_wrap && !self.large;
        let gutter = if shown && self.line_numbers {
            let metrics = self.number_metrics(measure);
            self.digits = gutter_digits(status.known_lines);
            gutter_width(self.digits, metrics.digit_width)
        } else {
            0.0
        };
        let map = if shown && self.document_map {
            MAP_WIDTH
        } else {
            0.0
        };
        if gutter == self.gutter_w && map == self.map_w {
            return;
        }
        let map_went = map <= 0.0 && self.map_w > 0.0;
        let map_came = map > 0.0 && self.map_w <= 0.0;
        self.gutter_w = gutter;
        self.map_w = map;
        editor.set_margins(Dip(gutter), Dip(map), ctx);
        ctx.invalidate(self.bounds);
        self.force_read = true;
        if gutter <= 0.0 {
            self.labels.clear();
            self.gutter_due = None;
        }
        if map_went {
            // The job now running can never be shown. It must not hold a timer or do more
            // work, so the frame stops waiting for it and tells the worker to stop.
            self.job_number += 1;
            self.job_done = self.job_number;
            if let Some(worker) = &mut self.worker {
                worker.abandon(self.job_number);
            }
            self.last_job = None;
            self.rows = MapRows::default();
            self.geometry = None;
            self.map_due = None;
            self.near = false;
            self.drag_offset = None;
            self.drag_fraction = None;
            self.aim_fade(ctx);
        }
        if map_came {
            self.map_stale = true;
        }
    }
    /// Reads `changes_since`, and returns whether the text and the line count changed.
    fn read_changes(&mut self, editor: &EditorView, status: &StoreStatus) -> (bool, bool) {
        let document = editor.document_number();
        let revision = editor.revision();
        let mut text_changed = false;
        let mut lines_changed = false;
        if document != self.seen_document {
            self.seen_document = document;
            self.map_stale = true;
            lines_changed = true;
        } else if revision != self.seen_revision {
            match editor.changes_since(self.seen_revision) {
                ChangesSince::Current => {}
                ChangesSince::Rescan => {
                    self.map_stale = true;
                    text_changed = true;
                    lines_changed = true;
                }
                ChangesSince::Changes(changes) => {
                    for tracked in &changes {
                        text_changed = true;
                        lines_changed |= tracked.change.line_count_delta != 0;
                        // An edit past the rows the map holds cannot change them. A job
                        // that is running may be reading any rows, so those can change.
                        if tracked.change.replaced.start.0 <= self.rows.end_byte
                            || self.rows.row_ends.is_empty()
                            || self.map_job_running()
                        {
                            self.map_stale = true;
                        }
                    }
                }
            }
        }
        self.seen_revision = revision;
        // A mapped file's index grows with no new revision.
        if status.known_lines != self.seen_lines {
            lines_changed |= self.seen_lines != usize::MAX;
            self.seen_lines = status.known_lines;
        }
        (text_changed, lines_changed)
    }

    fn read_gutter(
        &mut self,
        visible: &VisibleRows,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        self.counts.gutter_reads += 1;
        let metrics = self.number_metrics(measure);
        let ink = metrics.points * 96.0 / 72.0 * DIGIT_INK_EM;
        let labels = gutter_labels(&visible.rows, metrics.height, ink);
        // Only a number that moved or changed repaints: the old place and the new one.
        let area = self.gutter_area();
        let band = |label: &GutterLabel| Rect {
            y: Dip(label.y.floor() - 1.0),
            height: Dip(metrics.height.ceil() + 2.0),
            ..area
        };
        for label in &labels {
            if !self.labels.contains(label) {
                invalidate_inside(ctx, band(label), area);
            }
        }
        for label in &self.labels {
            if !labels.contains(label) {
                invalidate_inside(ctx, band(label), area);
            }
        }
        self.labels = labels;
    }

    fn read_map(
        &mut self,
        editor: &EditorView,
        status: &StoreStatus,
        visible: &VisibleRows,
        ctx: &mut ViewContext,
    ) {
        let rows = ((self.bounds.height.0 / MAP_ROW_HEIGHT) as usize).max(1);
        let (Some(first_row), Some(last_row)) = (visible.rows.first(), visible.rows.last()) else {
            // No layout yet: the map keeps what it has, or shows only its background.
            if self.geometry.is_none() {
                self.send_job_if_needed(editor, status, 0..rows + 1, ctx);
            }
            return;
        };
        let (first, last) = (first_row.line.0, last_row.line.0);
        // Rows may still be missing, and a drag holds one height, so the box keeps it.
        let on_screen = match self.geometry {
            Some(old) if !visible.complete || self.drag_offset.is_some() => old.on_screen,
            _ => last - first + 1,
        };
        let count = status.known_lines.max(last + 1);
        let scrollable = count.saturating_sub(on_screen);
        let max = editor.max_scroll_position();
        let by_scroll = |fraction: f64| fraction * scrollable as f64;
        let first_line = match self.drag_fraction {
            Some(held) => by_scroll(held),
            // The rows say which line is really at the top. A share of the document's height
            // is not the same share of its lines once lines wrap to different heights.
            None if visible.complete => {
                first_line_on_screen(visible, self.bounds.y.0).min(scrollable as f64)
            }
            // Until the rows are known, the scroll position is the best guide there is.
            None if max > 0.0 => by_scroll((editor.scroll_offset() / max).clamp(0.0, 1.0)),
            None => 0.0,
        };
        let geometry = MapGeometry::new(first_line, on_screen, count, rows);
        let lines = geometry.lines_shown();
        if self.geometry != Some(geometry) {
            let rows_moved = self
                .geometry
                .is_none_or(|old| old.top_line != geometry.top_line || old.rows != geometry.rows);
            if rows_moved {
                ctx.invalidate(self.map_area());
            } else {
                self.invalidate_box(ctx);
            }
            self.geometry = Some(geometry);
            if !rows_moved {
                self.invalidate_box(ctx);
            }
        }
        self.send_job_if_needed(editor, status, lines, ctx);
    }

    fn send_job_if_needed(
        &mut self,
        editor: &EditorView,
        status: &StoreStatus,
        lines: std::ops::Range<usize>,
        ctx: &mut ViewContext,
    ) {
        if !self.map_stale {
            if self.rows.covers(&lines) || self.map_job_running() {
                return;
            }
            // Rows that are not known yet stay unknown until the index knows more. Without
            // this, each wake from a finished job starts the next job, forever if the
            // index has failed. Growth comes through `map_due`, at most 10 times a second.
            if let Some((asked, known)) = &self.last_job
                && asked.start <= lines.start
                && lines.end <= asked.end
                && *known == status.known_lines
            {
                return;
            }
        }
        if self.worker.is_none() {
            match MapWorker::start(self.wake.clone()) {
                Ok(worker) => self.worker = Some(worker),
                Err(problem) => {
                    self.problem = Some(problem);
                    return;
                }
            }
        }
        let first_line = lines.start.saturating_sub(MAP_SLACK_ROWS);
        let count = (lines.end - first_line + MAP_SLACK_ROWS).min(MAP_MOST_ROWS);
        self.job_number += 1;
        self.last_job = Some((first_line..first_line + count, status.known_lines));
        let job = Job {
            snapshot: editor.worker_snapshot(),
            first_line,
            count,
            tab_width: self.tab_width,
            columns: ((MAP_WIDTH - MAP_LEFT) / MAP_COLUMN_WIDTH) as usize,
            number: self.job_number,
        };
        self.map_stale = false;
        self.counts.map_jobs += 1;
        if let Some(worker) = &mut self.worker
            && let Err(problem) = worker.send(job)
        {
            self.problem = Some(problem);
            self.worker = None;
            self.job_done = self.job_number;
            ctx.invalidate(self.map_area());
        }
    }

    fn take_rows(&mut self, ctx: &mut ViewContext) {
        let Some(finished) = self.worker.as_mut().and_then(MapWorker::take) else {
            return;
        };
        if finished.number != self.job_number {
            return;
        }
        self.job_done = finished.number;
        // Only the rows that changed repaint.
        if let Some(geometry) = self.geometry {
            let area = self.map_area();
            for line in geometry.lines_shown() {
                let old = self.rows.row(line).unwrap_or(&[]);
                let new = finished.rows.row(line).unwrap_or(&[]);
                if old != new {
                    let y = area.y.0 as f64
                        + (line as f64 - geometry.top_line) * f64::from(MAP_ROW_HEIGHT);
                    invalidate_inside(
                        ctx,
                        Rect {
                            y: Dip(y.floor() as f32 - 1.0),
                            height: Dip(MAP_ROW_HEIGHT + 2.0),
                            ..area
                        },
                        area,
                    );
                }
            }
        } else {
            ctx.invalidate(self.map_area());
        }
        self.rows = finished.rows;
    }

    /// The one timer the shell has, shared with the editor. The shell keeps the last
    /// request, so the frame adds its request only if it is sooner than any made so far.
    fn ask_for_timer(&self, now: f64, ctx: &mut ViewContext) {
        let mut wait: Option<u32> = None;
        let mut soonest = |ms: u32| wait = Some(wait.map_or(ms, |w: u32| w.min(ms)));
        if self.fade.fading() {
            soonest(FADE_FRAME_MS);
        }
        for due in [self.gutter_due, self.map_due].into_iter().flatten() {
            soonest((((due - now).max(0.001)) * 1000.0).ceil() as u32);
        }
        if self.map_job_running() && self.wake.is_none() {
            soonest(FADE_FRAME_MS);
        }
        let Some(wait) = wait else {
            return;
        };
        let asked = ctx
            .requests
            .iter()
            .any(|request| matches!(request, ViewRequest::RequestTimer(ms) if *ms <= wait));
        if !asked {
            ctx.request_timer(wait);
        }
    }

    // ---- Painting ----

    fn box_rect(&self, geometry: &MapGeometry) -> Rect {
        let map = self.map_area();
        Rect {
            y: Dip((f64::from(map.y.0) + geometry.box_top()) as f32),
            height: Dip(geometry.box_height() as f32),
            ..map
        }
    }
    fn invalidate_box(&self, ctx: &mut ViewContext) {
        if let Some(geometry) = &self.geometry {
            let rect = self.box_rect(geometry);
            invalidate_inside(
                ctx,
                Rect {
                    y: Dip(rect.y.0 - 2.0),
                    height: Dip(rect.height.0 + 4.0),
                    ..rect
                },
                self.map_area(),
            );
        }
    }

    fn draw_gutter(&self, list: &mut DisplayList, theme: &Theme) {
        let area = self.gutter_area();
        list.push(DrawCommand::PushClip(area));
        list.push(DrawCommand::FillRect {
            rect: area,
            color: theme.text_background,
        });
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(area.x.0 + area.width.0 - 1.0),
                width: Dip(1.0),
                ..area
            },
            color: theme.gutter_separator,
        });
        if let Some(metrics) = &self.metrics {
            let right = area.x.0 + area.width.0 - GUTTER_RIGHT_PADDING;
            for label in &self.labels {
                let text = label.number.to_string();
                list.push(DrawCommand::TextRun {
                    at: Point {
                        x: Dip(right - text.len() as f32 * metrics.digit_width),
                        y: Dip(label.y + metrics.baseline),
                    },
                    text,
                    font: GUTTER_FONT.into(),
                    font_size: Dip(metrics.points * 96.0 / 72.0),
                    color: theme.gutter_text,
                });
            }
        }
        list.push(DrawCommand::PopClip);
    }

    fn draw_map(&self, list: &mut DisplayList, theme: &Theme) {
        let area = self.map_area();
        list.push(DrawCommand::PushClip(area));
        list.push(DrawCommand::FillRect {
            rect: area,
            color: theme.text_background,
        });
        if let Some(geometry) = &self.geometry {
            let mut blocks = Vec::with_capacity(self.rows.runs.len().min(4096));
            for line in geometry.lines_shown() {
                let Some(runs) = self.rows.row(line) else {
                    continue;
                };
                let y = f64::from(area.y.0)
                    + (line as f64 - geometry.top_line) * f64::from(MAP_ROW_HEIGHT);
                blocks.extend(runs.iter().map(|&(column, length)| Rect {
                    x: Dip(area.x.0 + MAP_LEFT + f32::from(column) * MAP_COLUMN_WIDTH),
                    y: Dip(y as f32),
                    width: Dip(f32::from(length) * MAP_COLUMN_WIDTH),
                    height: Dip(MAP_BLOCK_HEIGHT),
                }));
            }
            if !blocks.is_empty() {
                list.push(DrawCommand::FillRects {
                    rects: blocks,
                    color: theme.map_text,
                });
            }
            let alpha = self.fade.alpha();
            if alpha > 0.0 {
                let rect = self.box_rect(geometry);
                list.push(DrawCommand::PushOpacity(alpha));
                list.push(DrawCommand::FillRect {
                    rect,
                    color: theme.map_box_fill,
                });
                list.push(DrawCommand::StrokeRect {
                    rect,
                    radius: Dip(0.0),
                    width: Dip(1.0),
                    color: theme.map_box_outline,
                });
                list.push(DrawCommand::PopOpacity);
            }
        }
        list.push(DrawCommand::PopClip);
    }

    /// One sheet from the view's top to its bottom, because page gaps are not in the text's
    /// layout. It moves with the sideways scroll, and has the Mac's frame around the
    /// text's sides.
    fn draw_paper(&self, editor: &EditorView, list: &mut DisplayList, theme: &Theme) {
        let bounds = self.bounds;
        list.push(DrawCommand::PushClip(bounds));
        list.push(DrawCommand::FillRect {
            rect: bounds,
            color: theme.under_page,
        });
        let left = bounds.x.0 - editor.horizontal_offset();
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(left),
                width: Dip(self.paper.width_dips() * self.scale),
                ..bounds
            },
            color: Color::rgb(255, 255, 255),
        });
        let text_left = left + self.paper.margin_dips() * self.scale;
        let text_right = text_left + self.paper.text_width_dips() * self.scale;
        for x in [text_left - 1.0, text_right] {
            list.push(DrawCommand::FillRect {
                rect: Rect {
                    x: Dip(x),
                    width: Dip(1.0),
                    ..bounds
                },
                color: PAGE_FRAME,
            });
        }
        list.push(DrawCommand::PopClip);
    }
}

/// The line at the view's top, with how much of it has scrolled away. A line that wraps
/// counts as whole until its last row has gone, because the map has one row for each line.
fn first_line_on_screen(visible: &VisibleRows, top: f32) -> f64 {
    let Some(first) = visible.rows.first() else {
        return 0.0;
    };
    let line = first.line.0 as f64;
    let wraps = first.continuation || visible.rows.get(1).is_some_and(|next| next.continuation);
    if wraps || first.height.0 <= 0.0 {
        return line;
    }
    line + f64::from(((top - first.y.0) / first.height.0).clamp(0.0, 1.0))
}

fn contains(rect: Rect, at: Point) -> bool {
    rect.width.0 > 0.0
        && at.x.0 >= rect.x.0
        && at.x.0 < rect.x.0 + rect.width.0
        && at.y.0 >= rect.y.0
        && at.y.0 < rect.y.0 + rect.height.0
}

fn invalidate_inside(ctx: &mut ViewContext, rect: Rect, inside: Rect) {
    let top = rect.y.0.max(inside.y.0);
    let bottom = (rect.y.0 + rect.height.0).min(inside.y.0 + inside.height.0);
    if bottom > top {
        ctx.invalidate(Rect {
            y: Dip(top),
            height: Dip(bottom - top),
            ..rect
        });
    }
}
