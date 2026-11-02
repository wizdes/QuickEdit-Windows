//! Text layout in device-independent pixels.
//!
//! # Two paths
//! Printable ASCII and tabs in a fixed-width font take the fast path, which needs
//! no DirectWrite call and agrees with shaping on rows and advances. Everything
//! else is shaped. Ligatures are off for ASCII in fixed-width fonts on both paths,
//! because the Mac app draws plain fixed-pitch text. Other scripts keep their own
//! shaping features.
//!
//! # Long lines
//! A line on the fast path stays whole up to [`LONG_LINE_BYTES`]. A line that needs
//! shaping stays whole up to [`SHAPED_LINE_BYTES`]. Past those, a line is only ever
//! laid out in segments of [`SEGMENT_BYTES`] or a little more, cut on grapheme
//! boundaries. `layout_line` on such a line gives the first segment and sets
//! `LineLayout::partial`, so it can never do unbounded work. `layout_slice` refuses a
//! range over the same limits.
//!
//! The editor scrolls a very long line like this. "Along" means down with wrap on and
//! across with wrap off, since with wrap off the segments sit side by side on one row.
//! 1. Ask `is_long_line`. If true, call `long_line_map` once and keep the map. It has
//!    one entry per segment, in very few runs. Entries are heights with wrap on and
//!    widths with wrap off.
//! 2. Give the document's `HeightMap` the line's height:
//!    `doc.set(line, engine.long_line_height(&map))`. Do not use `map.total_height()`
//!    directly, which with wrap off is a total width.
//! 3. For an offset `y` along the line, `map.scroll_to_line(y)` gives the segment and
//!    `map.line_to_scroll(segment)` gives its start.
//! 4. Get that segment and the next ones until the window is full, from
//!    `LineCache::cached_segment` or a worker (below). Draw each at its start minus `y`.
//! 5. After each cold layout, call `map.set(segment, engine.segment_extent(&layout))`.
//!    With wrap on, also repeat step 2. With wrap off, raise the line's height in the
//!    document map if `layout.height` is taller than what it holds.
//! 6. For a click, find the segment with `scroll_to_line` and call `hit_test_point` on
//!    its layout. For a caret, call `segment_at(byte, affinity)` with that affinity,
//!    then `caret_rect` on that segment's layout with the same affinity. The byte
//!    where two segments meet belongs to the earlier one with `Before` and to the later
//!    one with `After`, so the caret stays on the row that was clicked.
//!
//! Keeping the view still: when step 5 corrects a segment above the first one on
//! screen, what the user sees must not move. Read
//! `before = map.line_to_scroll(top_segment)` before the `set` and
//! `after = map.line_to_scroll(top_segment)` after it, then add `after - before` to the
//! scroll offset. Do the same with the document map for a correction to a line above.
//!
//! After an edit inside the line, call `update_long_line_map`. It keeps every measured
//! value and matches the entry count. `LineCache::invalidate` keeps the cached segments
//! that end before the edit. Segments after the edit are laid out again when seen,
//! because their byte edges moved. Typing near the start of a line therefore loses the
//! cached layouts of its later segments, and costs one segment layout for each one on
//! screen. Their measured sizes stay as close guesses.
//!
//! Known limits of long lines: a wrapped row ends at each segment edge. Shaping and text
//! direction are worked out per segment, so a joined letter at an edge takes its end
//! form. A cluster longer than 1 KiB can be cut at an edge. A run of flags longer than
//! 1 MiB can be cut too, because the look-back for the run's start stops there. A tab
//! with no tab in the 1 KiB before its segment assumes one cell per byte since the line
//! start.
//!
//! # Laying out ahead on workers
//! Shaping can take more than a frame: a screen of wide CJK takes tens of milliseconds,
//! and DirectWrite's cost cannot be reduced from here. So the UI thread must not shape
//! on a miss. `DirectWriteLayout` is safe to share, and a worker needs no cache lock.
//! 1. On the UI thread, for each line of the next and previous page, call
//!    `cache.cached(...)`. On a `None`, call `cache.ticket(...)` (or `segment_ticket`).
//! 2. Send the ticket and an `Arc` of the snapshot to a worker.
//! 3. On the worker, call `ticket.layout(&engine, snapshot)`. It takes no cache.
//! 4. Send the result back. On the UI thread, call `cache.insert(&ticket, layout)`. It
//!    returns false and keeps nothing if that line changed after the ticket was made,
//!    or if the cache started over. If the line only moved, it is kept, and the next
//!    get moves it.
//! 5. To paint, use `cached`. For a line that is still a miss, paint it blank or from the
//!    last frame, or call `get_or_layout` if the frame can afford it.
//!
//! An edit between steps 1 and 4 needs no special care: call `invalidate` as usual.
//! Tickets of snapshots older than the cache are never trusted, and a get with an older
//! snapshot never disturbs newer entries.
//!
//! # Slices
//! `layout_slice_from_column` keeps tabs on the same stops as the text before the
//! slice. Text direction cannot be kept: DirectWrite resolves it from the slice
//! alone, so digits after a Hebrew word that the slice cut off can change order.
//!
//! # Threads
//! `DirectWriteLayout`, `LineLayout` and hit tests are safe to use from any thread.
//! Microsoft does not document `IDWriteTextFormat` as thread-safe, so each thread
//! makes its own. A hit test reads only the `LineLayout`.
#![allow(hidden_glob_reexports)]

// `#[implement]` writes paths that start at `windows_core`. This crate stands in
// for that crate, inside this crate only, so that it needs no extra dependency.
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod cache;
mod fonts;
mod heights;
mod segments;
mod shaped;
mod wrap;

pub use cache::{CacheTicket, LineCache};
pub use fonts::{
    MAX_LOADED_FONTS, font_face, loaded_font_count, registered_face_count, thread_format_count,
};
pub use heights::{HeightError, HeightMap};
pub use segments::{LONG_LINE_BYTES, SEGMENT_BYTES, SHAPED_LINE_BYTES};

use fonts::{Font, family_exists, font};
use qe_api::{
    ByteOffset, CaretStop, Dip, Layout, LayoutGlyphRun, LayoutRow, LineIndex, LineLayout, Point,
    TextMeasurer, TextMetrics, TextSnapshot,
};
use segments::{LINE_ENDINGS, content_range, read};
use std::cell::RefCell;
use std::{fmt, ops::Range, result::Result, sync::Arc};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL};

/// The default editor fonts, in the order they are tried.
pub const DEFAULT_FONTS: [&str; 2] = ["Cascadia Mono", "Consolas"];
const DEFAULT_POINTS: f32 = 10.0;

/// Why layout could not do what was asked.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutError {
    /// This font family is not installed.
    MissingFont(String),
    /// None of these font families is installed.
    NoFontFound(Vec<String>),
    NoFontCollection,
    DirectWrite {
        call: &'static str,
        message: String,
    },
    /// A lock was left broken by a panic on another thread.
    Poisoned(&'static str),
    NoSuchLine(LineIndex),
    NoSuchSegment {
        line: LineIndex,
        index: usize,
    },
    /// The range is outside the text, is not on character edges, or holds a line
    /// ending.
    BadRange(Range<ByteOffset>),
    /// A slice is longer than its limit: `LONG_LINE_BYTES`, or `SHAPED_LINE_BYTES` if
    /// it needs shaping. Use segments.
    SliceTooLong {
        bytes: usize,
        limit: usize,
    },
    /// A slice edge falls inside a cluster. `Layout` callers can use
    /// `DirectWriteLayout::slice_around` to pick a safe range.
    CutCluster(ByteOffset),
    Height(HeightError),
    /// A `CacheTicket` was given a snapshot of another revision.
    StaleTicket {
        ticket: u64,
        snapshot: u64,
    },
}
thread_local! {
    static LAST_ERROR: RefCell<Option<LayoutError>> = const { RefCell::new(None) };
    static LAST_FALLBACK: RefCell<Option<MeasureFallback>> = const { RefCell::new(None) };
}
impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFont(family) => write!(f, "font family {family:?} is not installed"),
            Self::NoFontFound(families) => write!(f, "none of {families:?} is installed"),
            Self::NoFontCollection => write!(f, "DirectWrite gave no system font collection"),
            Self::DirectWrite { call, message } => write!(f, "{call} failed: {message}"),
            Self::Poisoned(what) => write!(f, "{what} lock is broken"),
            Self::NoSuchLine(line) => write!(f, "no line {}", line.0),
            Self::NoSuchSegment { line, index } => {
                write!(f, "line {} has no segment {index}", line.0)
            }
            Self::BadRange(range) => write!(f, "bad text range {range:?}"),
            Self::SliceTooLong { bytes, limit } => {
                write!(f, "a slice of {bytes} bytes is over the {limit} byte limit")
            }
            Self::CutCluster(at) => write!(f, "byte {} is inside a cluster", at.0),
            Self::Height(error) => error.fmt(f),
            Self::StaleTicket { ticket, snapshot } => {
                write!(
                    f,
                    "a ticket of revision {ticket} got a snapshot of revision {snapshot}"
                )
            }
        }
    }
}
impl std::error::Error for LayoutError {}

/// Which font an engine ended up with, when the caller gave a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontChoice {
    /// The first family the caller asked for.
    pub requested: String,
    /// The family in use.
    pub used: String,
}
impl FontChoice {
    /// True when the first choice was not installed.
    pub fn is_fallback(&self) -> bool {
        self.requested != self.used
    }
}

/// A measure that could not use the font it was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasureFallback {
    pub requested: String,
    pub used: String,
}

/// What `measure_with_font` found: real sizes, and any font it had to replace.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub metrics: TextMetrics,
    pub fallback: Option<MeasureFallback>,
}

pub struct DirectWriteLayout {
    font: Arc<Font>,
    choice: FontChoice,
    wrap: bool,
    force_shaping: bool,
    tab_width: usize,
    /// See `with_full_rows_at`. The cells on one wrapped row of a long line.
    full_row_cells: Option<usize>,
}

/// Splits a font token such as "Segoe UI 9". A name that exists as written is a
/// family, even when it ends in a number. Otherwise a final number is a point size.
pub(crate) fn parse_font_token(token: &str, exists: impl Fn(&str) -> bool) -> (&str, Option<f32>) {
    if exists(token) {
        return (token, None);
    }
    let split = token
        .rsplit_once(' ')
        .and_then(|(family, value)| value.parse::<f32>().ok().map(|size| (family, size)))
        .filter(|(family, size)| !family.is_empty() && size.is_finite() && *size > 0.0);
    match split {
        Some((family, points)) => (family, Some(points)),
        None => (token, None),
    }
}

impl DirectWriteLayout {
    /// The default font: Cascadia Mono, then Consolas. `font_choice` says which
    /// one was used, and `with_default_font` returns the same answer directly.
    pub fn new() -> Result<Self, LayoutError> {
        Self::with_default_font().map(|(engine, _)| engine)
    }
    pub fn with_default_font() -> Result<(Self, FontChoice), LayoutError> {
        Self::with_first_installed(&DEFAULT_FONTS, DEFAULT_POINTS, DWRITE_FONT_WEIGHT_NORMAL)
    }
    /// The first installed family of a list, and a value that says which one it was.
    pub fn with_first_installed(
        families: &[&str],
        points: f32,
        weight: DWRITE_FONT_WEIGHT,
    ) -> Result<(Self, FontChoice), LayoutError> {
        for family in families {
            match Self::with_font(family, points, weight) {
                Ok(mut engine) => {
                    let choice = FontChoice {
                        requested: families[0].to_owned(),
                        used: (*family).to_owned(),
                    };
                    engine.choice = choice.clone();
                    return Ok((engine, choice));
                }
                Err(LayoutError::MissingFont(_)) => continue,
                Err(other) => return Err(other),
            }
        }
        Err(LayoutError::NoFontFound(
            families.iter().map(|f| (*f).to_owned()).collect(),
        ))
    }
    /// A named font. A family that is not installed is an error.
    pub fn with_font(
        family: &str,
        points: f32,
        weight: DWRITE_FONT_WEIGHT,
    ) -> Result<Self, LayoutError> {
        Ok(Self {
            font: font(family, points * 96.0 / 72.0, weight)?,
            choice: FontChoice {
                requested: family.to_owned(),
                used: family.to_owned(),
            },
            wrap: true,
            force_shaping: false,
            tab_width: 8,
            full_row_cells: None,
        })
    }
    pub fn with_wrap(mut self, value: bool) -> Self {
        self.wrap = value;
        self
    }
    pub fn with_shaping(mut self, value: bool) -> Self {
        self.force_shaping = value;
        self
    }
    /// Makes a long line on the fast path wrap with no short rows at segment edges, for
    /// a view of this width. Segment edges then depend on the width, so a view makes a new
    /// engine when its width changes, and lays out again. See `segments.rs`.
    pub fn with_full_rows_at(&self, width: Dip) -> Self {
        let cells = if self.wrap {
            wrap::columns_that_fit(self.font.advance, width.0, SEGMENT_BYTES * 2)
        } else {
            0
        };
        Self {
            font: self.font.clone(),
            choice: self.choice.clone(),
            wrap: self.wrap,
            force_shaping: self.force_shaping,
            tab_width: self.tab_width,
            full_row_cells: (cells > 0).then_some(cells),
        }
    }
    pub fn with_tab_width(mut self, value: usize) -> Self {
        self.tab_width = value.max(1);
        self
    }
    pub fn font_family(&self) -> &str {
        &self.font.family
    }
    pub fn font_choice(&self) -> &FontChoice {
        &self.choice
    }
    pub fn cell_width(&self) -> Dip {
        Dip(self.font.advance)
    }
    /// Why the last `Layout` or `TextMeasurer` call on this thread failed, or `None`
    /// if it worked. Every such call clears it, and other threads do not touch it. The
    /// traits can only return `None` or sizes, so the reason waits here. Prefer the
    /// `try_` methods and `measure_with_font`, which return it directly.
    pub fn last_error(&self) -> Option<LayoutError> {
        LAST_ERROR.with(|last| last.borrow().clone())
    }
    /// The font that the last `TextMeasurer::measure` call on this thread had to
    /// replace, if any. `measure_with_font` returns this value directly.
    pub fn last_measure_fallback(&self) -> Option<MeasureFallback> {
        LAST_FALLBACK.with(|last| last.borrow().clone())
    }
    pub(crate) fn font(&self) -> &Font {
        &self.font
    }
    pub(crate) fn font_uid(&self) -> u64 {
        self.font.uid
    }
    pub(crate) fn full_row_cells(&self) -> Option<usize> {
        self.full_row_cells
    }
    pub(crate) fn wrap_enabled(&self) -> bool {
        self.wrap
    }
    pub(crate) fn forced_shaping(&self) -> bool {
        self.force_shaping
    }
    pub(crate) fn tab_width(&self) -> usize {
        self.tab_width
    }
    fn record<T>(&self, result: Result<T, LayoutError>) -> Option<T> {
        let (value, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        LAST_ERROR.with(|last| *last.borrow_mut() = error);
        value
    }
    pub(crate) fn takes_fast_path(&self, bytes: &[u8]) -> bool {
        !self.force_shaping
            && self.font.fast_ok
            && bytes
                .iter()
                .all(|b| *b == b'\t' || (0x20..0x7F).contains(b))
    }
    /// Like `Layout::layout_line`, with the reason for a failure.
    pub fn try_layout_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Result<LineLayout, LayoutError> {
        if self.is_long_line(text, line)? {
            return self.layout_segment(text, line, 0, width);
        }
        let content = content_range(text, line)?;
        let s = read(text, content.clone())?;
        self.layout_text(&s, content.start, line, width, 0, false)
    }
    /// Like `Layout::layout_slice`, with tabs placed as if the slice started at
    /// this column. The positions in the result still start at the slice's left edge.
    pub fn layout_slice_from_column(
        &self,
        text: &dyn TextSnapshot,
        range: Range<ByteOffset>,
        width: Dip,
        start_column: usize,
    ) -> Result<LineLayout, LayoutError> {
        if range.start > range.end || range.end > text.len_bytes() {
            return Err(LayoutError::BadRange(range));
        }
        let bytes = range.end.0 - range.start.0;
        if bytes > LONG_LINE_BYTES {
            return Err(LayoutError::SliceTooLong {
                bytes,
                limit: LONG_LINE_BYTES,
            });
        }
        let line = text
            .byte_to_position(range.start)
            .ok_or_else(|| LayoutError::BadRange(range.clone()))?
            .line;
        let s = read(text, range.clone())?;
        if s.contains(LINE_ENDINGS) {
            return Err(LayoutError::BadRange(range));
        }
        if bytes > SHAPED_LINE_BYTES && !self.takes_fast_path(s.as_bytes()) {
            return Err(LayoutError::SliceTooLong {
                bytes,
                limit: SHAPED_LINE_BYTES,
            });
        }
        let content = content_range(text, line)?;
        self.check_edge(text, &content, range.start)?;
        self.check_edge(text, &content, range.end)?;
        self.layout_text(&s, range.start, line, width, start_column, true)
    }
    pub(crate) fn layout_text(
        &self,
        s: &str,
        start: ByteOffset,
        line: LineIndex,
        width: Dip,
        start_column: usize,
        partial: bool,
    ) -> Result<LineLayout, LayoutError> {
        let fast = self.takes_fast_path(s.as_bytes());
        let lead = start_column % self.tab_width;
        if fast && let Some(rows) = self.fast_rows(s.as_bytes(), width, lead) {
            Ok(self.fast(s.as_bytes(), &rows, start, line, lead, partial))
        } else {
            shaped::shape(self, s, start, line, width, start_column, partial)
        }
    }
    /// The column after one character. `lead` is the slice's start column within
    /// its tab stop.
    fn next_column(&self, byte: u8, column: usize, lead: usize) -> usize {
        if byte == b'\t' {
            (column + lead) / self.tab_width * self.tab_width + self.tab_width - lead
        } else {
            column + 1
        }
    }
    fn fast(
        &self,
        s: &[u8],
        rows_end: &[usize],
        start: ByteOffset,
        line: LineIndex,
        lead: usize,
        partial: bool,
    ) -> LineLayout {
        let cell = self.font.advance_exact;
        let height = self.font.height as f64;
        let mut rows = Vec::with_capacity(rows_end.len());
        let mut begin = 0;
        for (index, &end) in rows_end.iter().enumerate() {
            let y = index as f64 * height;
            rows.push(self.fast_row(&s[begin..end], start.0 + begin, y, cell, lead));
            begin = end;
        }
        LineLayout {
            line,
            height: Dip((rows.len() as f64 * height) as f32),
            rows,
            partial,
        }
    }
    /// Where each row ends. `None` when only shaping can answer: a line with tabs
    /// that must wrap. DirectWrite sums tab stops in `f32`, so a row that just fits
    /// can go either way, and a line that needs no wrap does not depend on this.
    fn fast_rows(&self, s: &[u8], width: Dip, lead: usize) -> Option<Vec<usize>> {
        if !self.wrap {
            return Some(vec![s.len()]);
        }
        let widest_possible = s.len().saturating_mul(self.tab_width);
        let max_columns = wrap::columns_that_fit(self.font.advance, width.0, widest_possible);
        // Below one cell, DirectWrite has its own rule for what goes on a row.
        if max_columns == 0 {
            return None;
        }
        let mut column = 0;
        let mut has_tab = false;
        let fits = s.iter().all(|&b| {
            has_tab |= b == b'\t';
            column = self.next_column(b, column, lead);
            b == b' ' || b == b'\t' || column <= max_columns
        });
        if fits {
            return Some(vec![s.len()]);
        }
        if has_tab || s.contains(&b'\t') {
            return None;
        }
        Some(wrap::wrap_rows(s, &wrap::break_points(s), max_columns))
    }
    /// A long line's segment that is not its last, on the fast path, with its last rows
    /// chosen so that the segment ends in a full row. `None` means the plain way applies.
    pub(crate) fn fast_full_rows(
        &self,
        s: &str,
        start: ByteOffset,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout> {
        let cells = self.full_row_cells?;
        let bytes = s.as_bytes();
        if !self.wrap || !self.takes_fast_path(bytes) || bytes.contains(&b'\t') {
            return None;
        }
        let mut rows = self.fast_rows(bytes, width, 0)?;
        wrap::fill_last_row(bytes, &wrap::break_points(bytes), cells, &mut rows);
        Some(self.fast(bytes, &rows, start, line, 0, true))
    }
    fn fast_row(&self, s: &[u8], offset: usize, y: f64, cell: f64, lead: usize) -> LayoutRow {
        let mut stops = Vec::with_capacity(s.len() + 1);
        let mut glyph_runs = Vec::with_capacity(1);
        let baseline = Dip((y + self.font.baseline as f64) as f32);
        let advance = Dip(self.font.advance);
        let mut column = 0;
        // x comes from the column each time, so no error builds up along a row.
        let x_of = |column: usize| Dip((column as f64 * cell) as f32);
        let mut at = 0;
        while at < s.len() {
            if s[at] == b'\t' {
                // A tab draws no glyph, as in shaping. It only moves the next run.
                stops.push(CaretStop {
                    byte: ByteOffset(offset + at),
                    x: x_of(column),
                    x_before: x_of(column),
                });
                column = self.next_column(b'\t', column, lead);
                at += 1;
                continue;
            }
            let run_end = s[at..]
                .iter()
                .position(|b| *b == b'\t')
                .map_or(s.len(), |n| at + n);
            let count = run_end - at;
            let first_column = column;
            let mut ids = Vec::with_capacity(count);
            for (i, &byte) in s[at..run_end].iter().enumerate() {
                stops.push(CaretStop {
                    byte: ByteOffset(offset + at + i),
                    x: x_of(column),
                    x_before: x_of(column),
                });
                ids.push(self.font.ascii[byte as usize]);
                column += 1;
            }
            glyph_runs.push(LayoutGlyphRun {
                range: ByteOffset(offset + at)..ByteOffset(offset + run_end),
                origin: Point {
                    x: x_of(first_column),
                    y: baseline,
                },
                bidi_level: 0,
                font_id: self.font.id,
                font_em_size: Dip(self.font.em_size),
                glyph_ids: ids,
                advances: vec![advance; count],
                offsets: vec![Point::default(); count],
            });
            at = run_end;
        }
        let end = ByteOffset(offset + s.len());
        stops.push(CaretStop {
            byte: end,
            x: x_of(column),
            x_before: x_of(column),
        });
        let start = ByteOffset(offset);
        LayoutRow {
            range: start..end,
            y: Dip(y as f32),
            height: Dip(self.font.height),
            glyph_runs,
            caret_stops: stops,
            visual_stops: Vec::new(),
        }
    }
    /// The real size of text in the named font, and any font that had to replace it.
    /// A token is a family, a family and a point size such as "Segoe UI 9", or a
    /// family whose own name ends in a number. A missing family uses this engine's
    /// font, and the result says so.
    pub fn measure_with_font(&self, text: &str, font_token: &str) -> Result<Measured, LayoutError> {
        let lookup_error = RefCell::new(None);
        let (family, points) = parse_font_token(font_token, |name| {
            family_exists(name).unwrap_or_else(|e| {
                *lookup_error.borrow_mut() = Some(e);
                false
            })
        });
        if let Some(error) = lookup_error.into_inner() {
            return Err(error);
        }
        let em_size = points.map_or(self.font.em_size, |p| p * 96.0 / 72.0);
        let (selected, fallback) = match font(family, em_size, DWRITE_FONT_WEIGHT_NORMAL) {
            Ok(selected) => (selected, None),
            Err(LayoutError::MissingFont(_)) => (
                font(&self.font.family, em_size, DWRITE_FONT_WEIGHT_NORMAL)?,
                Some(MeasureFallback {
                    requested: family.to_owned(),
                    used: self.font.family.clone(),
                }),
            ),
            Err(other) => return Err(other),
        };
        Ok(Measured {
            metrics: shaped::measure_text(&selected, text)?,
            fallback,
        })
    }
}

impl Layout for DirectWriteLayout {
    fn layout_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout> {
        self.record(self.try_layout_line(text, line, width))
    }
    fn layout_slice(
        &self,
        text: &dyn TextSnapshot,
        range: Range<ByteOffset>,
        width: Dip,
    ) -> Option<LineLayout> {
        self.record(self.layout_slice_from_column(text, range, width, 0))
    }
}

impl TextMeasurer for DirectWriteLayout {
    /// See `measure_with_font`. If DirectWrite fails, the result is an estimate from
    /// the character count in this engine's font, so that widgets stay visible, and
    /// `last_error` names the failure.
    fn measure(&self, text: &str, font_token: &str) -> TextMetrics {
        self.metrics_or_estimate(text, self.measure_with_font(text, font_token))
    }
}

impl DirectWriteLayout {
    fn metrics_or_estimate(
        &self,
        text: &str,
        result: Result<Measured, LayoutError>,
    ) -> TextMetrics {
        let (metrics, fallback) = match self.record(result) {
            Some(measured) => (measured.metrics, measured.fallback),
            None => {
                let rows = text.split('\n').map(|row| row.chars().count());
                let widest = rows.clone().max().unwrap_or(0);
                let estimate = TextMetrics {
                    width: Dip((widest as f64 * self.font.advance_exact) as f32),
                    height: Dip(rows.count() as f32 * self.font.height),
                    baseline: Dip(self.font.baseline),
                };
                (estimate, None)
            }
        };
        LAST_FALLBACK.with(|last| *last.borrow_mut() = fallback);
        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::{DirectWriteLayout, LayoutError, parse_font_token};
    use qe_api::Dip;

    #[test]
    fn a_failed_measure_gives_an_estimate_and_names_the_failure() {
        let engine = DirectWriteLayout::new().unwrap();
        let failure = LayoutError::NoFontCollection;
        let got = engine.metrics_or_estimate("abc\nabcd", Err(failure.clone()));
        assert!((got.width.0 - engine.cell_width().0 * 4.0).abs() < 0.01);
        assert!(got.height.0 > Dip(0.0).0);
        assert_eq!(engine.last_error(), Some(failure));
    }

    #[test]
    fn family_names_ending_in_a_number_are_not_sizes() {
        let installed = |name: &str| name == "Font 7" || name == "Segoe UI";
        assert_eq!(parse_font_token("Font 7", installed), ("Font 7", None));
        assert_eq!(
            parse_font_token("Font 7 12", installed),
            ("Font 7", Some(12.0))
        );
        assert_eq!(
            parse_font_token("Segoe UI 9", installed),
            ("Segoe UI", Some(9.0))
        );
        assert_eq!(parse_font_token("Segoe UI", installed), ("Segoe UI", None));
        assert_eq!(parse_font_token("Other 0", installed), ("Other 0", None));
        assert_eq!(
            parse_font_token("Other NaN", installed),
            ("Other NaN", None)
        );
    }
}
