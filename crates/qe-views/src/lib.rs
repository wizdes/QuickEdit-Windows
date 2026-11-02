//! Side views and page controls for an editor window.
use qe_api::{
    Dip, DisplayList, DrawCommand, LineIndex, LookupError, Point, Rect, TextMeasurer, TextSnapshot,
    Theme, View, ViewContext,
};
use qe_editor::{EditorView, PageWrap, VisibleRow};
use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};

pub const LARGE_FILE_BYTES: usize = 50 * 1024 * 1024;

pub fn gutter_width(lines: usize, digit_width: f32) -> f32 {
    lines.max(1).to_string().len().max(3) as f32 * digit_width + 14.0
}
pub fn gutter_labels(rows: &[VisibleRow]) -> Vec<(usize, f32)> {
    rows.iter()
        .filter(|r| !r.continuation)
        .map(|r| (r.line.0 + 1, r.y.0))
        .collect()
}
pub fn map_runs(text: &str, columns: usize) -> Vec<(usize, usize)> {
    let (mut col, mut start, mut runs) = (0usize, None, Vec::new());
    let flush = |runs: &mut Vec<(usize, usize)>, start: &mut Option<usize>, col| {
        if let Some(s) = start.take() {
            if col > s {
                runs.push((s, col - s));
            }
        }
    };
    for unit in text.encode_utf16() {
        if col >= columns {
            break;
        }
        match unit {
            9 => {
                flush(&mut runs, &mut start, col);
                col = (col / 8 + 1) * 8;
            }
            32 | 0xa0 => {
                flush(&mut runs, &mut start, col);
                col += 1;
            }
            10 | 13 | 0x85 | 0x2028 | 0x2029 => flush(&mut runs, &mut start, col),
            _ => {
                if start.is_none() {
                    start = Some(col);
                }
                col += 1;
            }
        }
    }
    flush(&mut runs, &mut start, col.min(columns));
    runs
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paper {
    pub width: f32,
    pub height: f32,
}
impl Paper {
    pub fn for_region(region: &str) -> Self {
        if region.eq_ignore_ascii_case("US") || region.eq_ignore_ascii_case("CA") {
            Self {
                width: 612.0,
                height: 792.0,
            }
        } else {
            Self {
                width: 595.0,
                height: 842.0,
            }
        }
    }
    pub fn text_width(self) -> f32 {
        self.width - 144.0
    }
}
pub fn zoom_step(percent: u16, inward: bool) -> u16 {
    if inward {
        percent.saturating_mul(2).min(1600)
    } else {
        (percent / 2).max(25)
    }
}
#[derive(Default, Debug)]
pub struct Fade {
    alpha: f32,
    from: f32,
    target: f32,
    start: f64,
    active: bool,
}
impl Fade {
    pub fn alpha(&self) -> f32 {
        self.alpha
    }
    pub fn fading(&self) -> bool {
        self.active
    }
    pub fn target(&mut self, visible: bool, now: f64, reduce_motion: bool) {
        self.step(now);
        self.from = self.alpha;
        self.target = if visible { 1.0 } else { 0.0 };
        self.start = now;
        self.active = !reduce_motion && (self.target - self.alpha).abs() > f32::EPSILON;
        if !self.active {
            self.alpha = self.target;
        }
    }
    pub fn step(&mut self, now: f64) -> f32 {
        if self.active {
            let duration = if self.target > self.from { 0.15 } else { 0.25 }
                * f64::from((self.target - self.from).abs());
            let p = if duration > 0.0 {
                ((now - self.start) / duration).clamp(0.0, 1.0)
            } else {
                1.0
            };
            self.alpha = self.from + (self.target - self.from) * p as f32;
            if p > 0.999 {
                self.alpha = self.target;
                self.active = false;
            }
        }
        self.alpha
    }
}

struct MapResult {
    revision: u64,
    first: usize,
    runs: Vec<(usize, usize, usize)>,
}
pub struct ViewFrame {
    pub line_numbers: bool,
    pub document_map: bool,
    pub page_wrap: bool,
    pub zoom: u16,
    pub paper: Paper,
    pub fade: Fade,
    bounds: Rect,
    gutter: f32,
    map: Option<MapResult>,
    pending: Option<Receiver<MapResult>>,
    dragging: Option<f32>,
    family: String,
    base_points: f32,
}
impl ViewFrame {
    pub fn new(region: &str, family: &str, base_points: f32) -> Self {
        Self {
            line_numbers: false,
            document_map: true,
            page_wrap: false,
            zoom: 100,
            paper: Paper::for_region(region),
            fade: Fade::default(),
            bounds: Rect::default(),
            gutter: 0.0,
            map: None,
            pending: None,
            dragging: None,
            family: family.into(),
            base_points,
        }
    }
    pub fn layout(
        &mut self,
        editor: &mut EditorView,
        bounds: Rect,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        self.bounds = bounds;
        editor.layout(bounds, measure);
        self.sync(editor, measure, ctx);
    }
    /// Call after editor edits, scrolling, layout delivery, and view setting changes.
    pub fn sync(
        &mut self,
        editor: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        let status = editor.store_status();
        let actual_size = editor.worker_snapshot().try_len_bytes().ok().map(|n| n.0);
        let enabled = actual_size.unwrap_or(status.known_bytes.0) <= LARGE_FILE_BYTES;
        let width = if enabled && self.line_numbers {
            let size = self.gutter_font_size();
            let digit = measure
                .measure("8", &format!("{} {}", self.family, size * 72.0 / 96.0))
                .width
                .0;
            gutter_width(status.known_lines, digit)
        } else {
            0.0
        };
        let map_width = if enabled && self.document_map {
            100.0
        } else {
            0.0
        };
        if (width - self.gutter).abs() > 0.01
            || (editor.text_area().x.0 - self.bounds.x.0 - width).abs() > 0.01
            || (editor.text_area().width.0 - (self.bounds.width.0 - width - map_width).max(0.0))
                .abs()
                > 0.01
        {
            self.gutter = width;
            editor.set_margins(Dip(width), Dip(map_width), ctx);
            ctx.invalidate(self.bounds);
        }
        if map_width > 0.0 {
            self.poll_map(editor, ctx);
        } else {
            self.map = None;
            self.pending = None;
            self.fade.target(false, 0.0, true);
        }
    }
    fn gutter_font_size(&self) -> f32 {
        (self.base_points * self.zoom as f32 / 100.0 * 0.85)
            .round()
            .max(9.0)
            * 96.0
            / 72.0
    }
    pub fn set_page_wrap(
        &mut self,
        editor: &mut EditorView,
        on: bool,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        if on {
            editor.set_wrap(true)?;
        }
        self.page_wrap = on;
        editor.set_page_wrap(
            on.then_some(PageWrap {
                text_width: Dip(self.paper.text_width()),
                margin: Dip(72.0),
            }),
            ctx,
        );
        editor.set_paints_background(!on, ctx);
        Ok(())
    }
    pub fn set_zoom(
        &mut self,
        editor: &mut EditorView,
        percent: u16,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        if !(25..=1600).contains(&percent) {
            return Err("Zoom must be between 25% and 1600%".into());
        }
        editor.apply_font(&self.family, self.base_points * percent as f32 / 100.0, ctx)?;
        self.zoom = percent;
        Ok(())
    }
    pub fn draw(
        &self,
        editor: &EditorView,
        list: &mut DisplayList,
        theme: &Theme,
        measure: &dyn TextMeasurer,
    ) {
        if self.page_wrap {
            list.push(DrawCommand::FillRect {
                rect: self.bounds,
                color: theme.under_page,
            });
            list.push(DrawCommand::FillRect {
                rect: Rect {
                    x: Dip(self.bounds.x.0 + self.gutter),
                    y: self.bounds.y,
                    width: Dip(self.paper.width),
                    height: self.bounds.height,
                },
                color: theme.text_background,
            });
        }
        editor.draw(list, theme, measure);
        if self.gutter > 0.0 {
            let rect = Rect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: Dip(self.gutter),
                height: self.bounds.height,
            };
            list.push(DrawCommand::FillRect {
                rect,
                color: theme.text_background,
            });
            list.push(DrawCommand::FillRect {
                rect: Rect {
                    x: Dip(rect.x.0 + rect.width.0 - 1.0),
                    width: Dip(1.0),
                    ..rect
                },
                color: theme.gutter_separator,
            });
            let rows = editor.visible_rows();
            let mut last_bottom = f32::NEG_INFINITY;
            for row in rows.rows.iter().filter(|r| !r.continuation) {
                let text = (row.line.0 + 1).to_string();
                let metrics = measure.measure(
                    &text,
                    &format!("{} {}", self.family, self.gutter_font_size() * 72.0 / 96.0),
                );
                let top = row.y.0 + (row.height.0 - metrics.height.0) / 2.0;
                if top < last_bottom {
                    continue;
                }
                last_bottom = top + metrics.height.0;
                list.push(DrawCommand::TextRun {
                    at: Point {
                        x: Dip(rect.x.0 + self.gutter - 8.0 - metrics.width.0),
                        y: Dip(top + metrics.baseline.0),
                    },
                    text,
                    font: self.family.clone(),
                    font_size: Dip(self.gutter_font_size()),
                    color: theme.gutter_text,
                });
            }
        }
        if self.document_map
            && let Some(map) = &self.map
        {
            let rect = self.map_area();
            list.push(DrawCommand::FillRect {
                rect,
                color: theme.text_background,
            });
            for &(line, col, width) in &map.runs {
                list.push(DrawCommand::FillRect {
                    rect: Rect {
                        x: Dip(rect.x.0 + 2.0 + col as f32),
                        y: Dip(rect.y.0 + line.saturating_sub(map.first) as f32 * 2.0),
                        width: Dip(width as f32),
                        height: Dip(1.4),
                    },
                    color: theme.map_text,
                });
            }
            if self.fade.alpha() > 0.0 {
                let box_rect = self.map_box(editor);
                list.push(DrawCommand::PushOpacity(self.fade.alpha()));
                list.push(DrawCommand::FillRect {
                    rect: box_rect,
                    color: theme.map_box_fill,
                });
                list.push(DrawCommand::StrokeRect {
                    rect: box_rect,
                    radius: Dip(0.0),
                    width: Dip(1.0),
                    color: theme.map_box_outline,
                });
                list.push(DrawCommand::PopOpacity);
            }
        }
    }
    fn map_area(&self) -> Rect {
        Rect {
            x: Dip(self.bounds.x.0 + self.bounds.width.0 - 100.0),
            y: self.bounds.y,
            width: Dip(100.0),
            height: self.bounds.height,
        }
    }
    pub fn map_box(&self, editor: &EditorView) -> Rect {
        let max = editor.max_scroll_position();
        let fraction = if max > 0.0 {
            (editor.scroll_offset() / max).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let total = editor.total_height().max(1.0);
        let h = (self.bounds.height.0 as f64 * self.bounds.height.0 as f64 / total)
            .clamp(2.0, self.bounds.height.0.max(2.0) as f64) as f32;
        Rect {
            y: Dip(self.bounds.y.0 + fraction * (self.bounds.height.0 - h)),
            height: Dip(h),
            ..self.map_area()
        }
    }
    pub fn pointer_near(
        &mut self,
        x: f32,
        y: f32,
        now: f64,
        reduce_motion: bool,
        ctx: &mut ViewContext,
    ) {
        let near = self.document_map
            && x >= self.bounds.x.0 + self.bounds.width.0 - 108.0
            && x <= self.bounds.x.0 + self.bounds.width.0
            && y >= self.bounds.y.0
            && y < self.bounds.y.0 + self.bounds.height.0;
        self.fade
            .target(near || self.dragging.is_some(), now, reduce_motion);
        if self.fade.fading() {
            ctx.request_timer(16);
        }
    }
    pub fn tick(&mut self, now: f64, ctx: &mut ViewContext) {
        let before = self.fade.alpha();
        self.fade.step(now);
        if before != self.fade.alpha() {
            ctx.invalidate(self.map_area());
        }
        if self.fade.fading() {
            ctx.request_timer(16);
        }
    }
    pub fn map_down(&mut self, editor: &mut EditorView, y: f32, ctx: &mut ViewContext) {
        let box_rect = self.map_box(editor);
        self.dragging = Some(
            if y >= box_rect.y.0 && y <= box_rect.y.0 + box_rect.height.0 {
                y - box_rect.y.0
            } else {
                box_rect.height.0 / 2.0
            },
        );
        self.map_drag(editor, y, ctx);
    }
    pub fn map_drag(&self, editor: &mut EditorView, y: f32, ctx: &mut ViewContext) {
        if let Some(offset) = self.dragging {
            let travel = (self.bounds.height.0 - self.map_box(editor).height.0).max(1.0);
            let fraction = ((y - offset - self.bounds.y.0) / travel).clamp(0.0, 1.0);
            editor.set_scroll_position(f64::from(fraction) * editor.max_scroll_position(), ctx);
        }
    }
    pub fn map_up(&mut self) {
        self.dragging = None;
    }
    fn poll_map(&mut self, editor: &EditorView, ctx: &mut ViewContext) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    if editor.revision() == result.revision {
                        self.map = Some(result);
                        ctx.invalidate(self.map_area());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_timer(16);
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                }
            }
        }
        let status = editor.store_status();
        let target_first = if status.complete {
            (editor.scroll_offset() / editor.total_height().max(1.0) * status.known_lines as f64)
                as usize
        } else {
            0
        };
        let target_first = target_first
            .saturating_sub(4)
            .min(status.known_lines.saturating_sub(1));
        if self
            .map
            .as_ref()
            .is_some_and(|m| m.revision == editor.revision() && m.first == target_first)
        {
            return;
        }
        let snapshot = editor.worker_snapshot();
        let status = snapshot.status();
        let first = if status.complete {
            (editor.scroll_offset() / editor.total_height().max(1.0) * status.known_lines as f64)
                as usize
        } else {
            0
        };
        let first = first
            .saturating_sub(4)
            .min(status.known_lines.saturating_sub(1));
        let rows = ((self.bounds.height.0 / 2.0).ceil() as usize)
            .saturating_add(2)
            .min(10000);
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let result = build_map(snapshot, first, rows);
            let _ = tx.send(result);
        });
        ctx.request_timer(16);
    }
}
fn build_map(snapshot: Arc<dyn TextSnapshot>, first: usize, rows: usize) -> MapResult {
    let mut runs = Vec::new();
    for line in first..first.saturating_add(rows) {
        let range = match snapshot.try_known_line(LineIndex(line)) {
            Ok(line) => line.range,
            Err(LookupError::NotYetKnown { .. } | LookupError::OutOfRange) => break,
            Err(LookupError::Failed(_)) | Err(LookupError::TooLong { .. }) => break,
        };
        let mut text = String::new();
        let limit = range.start.0.saturating_add(400).min(range.end.0);
        let mut read = false;
        for back in 0..=3 {
            let end = qe_api::ByteOffset(limit.saturating_sub(back).max(range.start.0));
            text.clear();
            if snapshot
                .try_visit_chunks(range.start..end, &mut |s| text.push_str(s))
                .is_ok()
            {
                read = true;
                break;
            }
        }
        if !read {
            continue;
        }
        for (col, width) in map_runs(&text, 98) {
            runs.push((line, col, width));
        }
    }
    MapResult {
        revision: snapshot.revision(),
        first,
        runs,
    }
}
