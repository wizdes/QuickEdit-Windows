//! QuickEdit's editable text area.
//!
//! # Rules this file keeps
//! - Caret stops come from the text, in `stops.rs`. A missing layout never changes
//!   what a key removes or skips.
//! - One function answers "where is this byte on screen" (`place`) and one answers
//!   "which byte is at this point" (`resolve`). Both ask about long lines first.
//! - One function applies every text change (`after_change`): typing, paste, undo,
//!   redo and groups. It moves heights, long-line maps and queued work to the lines'
//!   new numbers.
//! - One check refuses edits (`can_edit`). All edit commands pass through `run_inner`.
//! - Only `finish` asks for a timer, so exactly one timer request leaves each event.
//!
//! # When a line has no layout yet
//! The UI thread may lay out a line itself only when that costs no system call:
//! printable ASCII of at most 4 KiB, in a font on the layout crate's fast path. Any
//! other line is treated by estimate until its worker result arrives: it wraps every
//! `width / cell width` characters, and the x position becomes a character count. The
//! estimate lands on a caret stop, and the remembered x stays, so the next Up or Down
//! moves from the true position once layout arrives.
mod load;
mod stops;

pub use load::{EDITABLE_FILE_LIMIT, FileLoad, LoadOptions, LoadResult, LoadedFile};
pub use stops::preferred_line_ending_bytes;

use qe_api::{
    Affinity, ByteOffset, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, LineIndex,
    LineLayout, LookupError, Modifiers, MouseButton, MouseCursor, Point, Rect, StoreStatus,
    TextChange, TextMeasurer, TextPosition, TextSnapshot, TextStore, Theme, Utf16Offset, View,
    ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};
use qe_buffer::{MappedStore, PieceStore, UndoHistory};
use qe_layout::{CacheTicket, DirectWriteLayout, HeightMap, LayoutError, LineCache};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use stops::Stops;

const BLINK: Duration = Duration::from_millis(530);
const WORK_TIMER_MS: u32 = 16;
/// How long an arrow key waits for a mapped file's page to reach memory.
const KEY_RETRY: Duration = Duration::from_millis(300);
/// How long a Page key waits for its target. A press is not dropped for a slow disk.
const PAGE_RETRY: Duration = Duration::from_secs(5);
/// How long a Home or End waits for its line's layout.
const DEFER_LIMIT: Duration = Duration::from_secs(2);
/// Each waiting key has its own `DEFER_LIMIT`, but no input waits longer than this, however
/// many keys wait in front of it. Keys that still wait at this limit go at once.
const DEFER_TOTAL_LIMIT: Duration = Duration::from_secs(6);
/// A drag scroll step for each this much time, and at most this many steps at once.
const DRAG_STEP: Duration = Duration::from_millis(WORK_TIMER_MS as u64);
const DRAG_MOST_STEPS: u32 = 8;
/// How long after the last input history memory may be freed in one go.
const IDLE_BEFORE_RELEASE: Duration = Duration::from_secs(2);
/// The longest line the UI thread may lay out itself.
const SYNC_LINE_BYTES: usize = 4096;
/// What the UI thread may lay out itself for one event.
const SYNC_BUDGET_BYTES: usize = 16 * 1024;
/// Selections above this are read on a thread for Copy.
const COPY_ON_UI_BYTES: usize = 1024 * 1024;
const COPY_LIMIT_BYTES: usize = 512 * 1024 * 1024;
const MAX_WINDOW_LINES: usize = 1024;
const WHOLE: usize = usize::MAX;
const CACHE_BYTES: usize = 32 << 20;
/// How far ink may reach past a row, so a repaint of the row clears it. The renderer's
/// README explains why a row alone is not enough.
const INK_MARGIN: f32 = 4.0;
const WARNING_STRIP: f32 = 24.0;
/// How many edited lines keep showing their old layout.
const MAX_GHOST_LINES: usize = 8;

/// The slow first step of a clipboard write, safe to run on another thread.
pub type ClipboardPreparer =
    Arc<dyn Fn(&str, &AtomicBool) -> Result<Box<dyn std::any::Any + Send>, String> + Send + Sync>;

pub trait Clipboard {
    /// `Ok(None)` means the clipboard holds no text.
    fn read(&mut self) -> Result<Option<String>, String>;
    fn write(&mut self, value: &str) -> Result<(), String>;
    /// A clipboard whose write has a slow part offers it here. A big Copy then runs it
    /// on the copy's thread and calls `write_prepared` on the UI thread.
    fn preparer(&self) -> Option<ClipboardPreparer> {
        None
    }
    fn write_prepared(&mut self, _prepared: Box<dyn std::any::Any + Send>) -> Result<(), String> {
        Err("This clipboard has no prepared writes.".to_string())
    }
}
#[derive(Default)]
pub struct MemoryClipboard(pub String);
impl Clipboard for MemoryClipboard {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(Some(self.0.clone()))
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        self.0 = value.into();
        Ok(())
    }
}

enum LayoutOutcome {
    Done(Result<LineLayout, LayoutError>),
    /// The line left the wanted window while the task waited in the queue.
    Skipped,
}
struct LayoutResult {
    id: u64,
    ticket: CacheTicket,
    outcome: LayoutOutcome,
}
struct MapResult {
    id: u64,
    generation: u64,
    result: Result<HeightMap, LayoutError>,
}
struct KnownResult {
    revision: u64,
    generation: u64,
    result: Result<LineLayout, String>,
}

enum WorkerTask {
    Layout(
        u64,
        CacheTicket,
        Arc<dyn TextSnapshot>,
        Arc<DirectWriteLayout>,
    ),
    LongMap(
        u64,
        LineIndex,
        Dip,
        Arc<dyn TextSnapshot>,
        Arc<DirectWriteLayout>,
        u64,
    ),
    KnownFirst(
        usize,
        Dip,
        Arc<dyn TextSnapshot>,
        Arc<DirectWriteLayout>,
        u64,
    ),
    Drop(Arc<dyn TextSnapshot>),
    /// A whole old document, with its history, to free away from the UI thread.
    DropDocument(Box<dyn std::any::Any + Send>),
}

/// State the view and its workers share.
struct WorkerShared {
    closing: AtomicBool,
    alive: AtomicUsize,
    /// The lines worth laying out now. A queued task outside them is skipped.
    wanted_first: AtomicUsize,
    wanted_last: AtomicUsize,
    skipped: AtomicU64,
    /// A test sets this to make one line's layout always fail.
    fail_line: AtomicUsize,
    test_paused: AtomicBool,
    /// Old snapshots sent to a worker that it has not dropped yet.
    drops_out: AtomicUsize,
    wake: std::sync::Mutex<Option<qe_buffer::Notify>>,
}
impl WorkerShared {
    fn wake_host(&self) {
        if let Ok(slot) = self.wake.lock()
            && let Some(wake) = slot.as_ref()
        {
            wake();
        }
    }
}

struct WorkerExit(Arc<WorkerShared>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.0.alive.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A mapped store's revision grows with its index, but its text never changes, and a
/// layout is only made of a line whose end is known. The line cache starts over when it
/// sees a new revision, so the view shows it this one revision for the whole file.
const MAPPED_REVISION: u64 = 0;

/// A mapped snapshot for a worker. Its lookups may wait.
struct WorkerMapped(Arc<dyn TextSnapshot>);
impl TextSnapshot for WorkerMapped {
    fn revision(&self) -> u64 {
        MAPPED_REVISION
    }
    fn len_bytes(&self) -> ByteOffset {
        self.0.len_bytes()
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.0.len_utf16()
    }
    fn line_count(&self) -> usize {
        self.0.line_count()
    }
    fn byte_to_utf16(&self, b: ByteOffset) -> Option<Utf16Offset> {
        self.0.byte_to_utf16(b)
    }
    fn utf16_to_byte(&self, u: Utf16Offset) -> Option<ByteOffset> {
        self.0.utf16_to_byte(u)
    }
    fn byte_to_position(&self, b: ByteOffset) -> Option<TextPosition> {
        self.0.byte_to_position(b)
    }
    fn position_to_byte(&self, p: TextPosition) -> Option<ByteOffset> {
        self.0.position_to_byte(p)
    }
    fn line_range(&self, l: LineIndex) -> Option<Range<ByteOffset>> {
        self.0.line_range(l)
    }
    fn visit_chunks(&self, r: Range<ByteOffset>, v: &mut dyn FnMut(&str)) -> bool {
        self.0.visit_chunks(r, v)
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        self.0.status()
    }
    fn try_known_line(&self, l: LineIndex) -> Result<qe_api::KnownLine, LookupError> {
        self.0.try_known_line(l)
    }
    fn try_len_bytes(&self) -> Result<ByteOffset, LookupError> {
        self.0.try_len_bytes()
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        self.0.try_len_utf16()
    }
    fn try_line_count(&self) -> Result<usize, LookupError> {
        self.0.try_line_count()
    }
    fn try_byte_to_utf16(&self, b: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.0.try_byte_to_utf16(b)
    }
    fn try_utf16_to_byte(&self, u: Utf16Offset) -> Result<ByteOffset, LookupError> {
        self.0.try_utf16_to_byte(u)
    }
    fn try_byte_to_position(&self, b: ByteOffset) -> Result<TextPosition, LookupError> {
        self.0.try_byte_to_position(b)
    }
    fn try_position_to_byte(&self, p: TextPosition) -> Result<ByteOffset, LookupError> {
        self.0.try_position_to_byte(p)
    }
    fn try_line_range(&self, l: LineIndex) -> Result<Range<ByteOffset>, LookupError> {
        self.0.try_line_range(l)
    }
    fn try_visit_chunks(
        &self,
        r: Range<ByteOffset>,
        v: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        self.0.try_visit_chunks(r, v)
    }
}

/// Converts cache's plain lookups to nonwaiting lookups on a mapped snapshot.
struct NonBlocking(Arc<dyn TextSnapshot>);
impl TextSnapshot for NonBlocking {
    fn revision(&self) -> u64 {
        MAPPED_REVISION
    }
    fn len_bytes(&self) -> ByteOffset {
        self.0.status().known_bytes
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.0.status().known_utf16
    }
    fn line_count(&self) -> usize {
        self.0.status().known_lines
    }
    fn byte_to_utf16(&self, b: ByteOffset) -> Option<Utf16Offset> {
        self.0.try_byte_to_utf16(b).ok()
    }
    fn utf16_to_byte(&self, u: Utf16Offset) -> Option<ByteOffset> {
        self.0.try_utf16_to_byte(u).ok()
    }
    fn byte_to_position(&self, b: ByteOffset) -> Option<TextPosition> {
        self.0.try_byte_to_position(b).ok()
    }
    fn position_to_byte(&self, p: TextPosition) -> Option<ByteOffset> {
        self.0.try_position_to_byte(p).ok()
    }
    fn line_range(&self, l: LineIndex) -> Option<Range<ByteOffset>> {
        self.0.try_line_range(l).ok()
    }
    fn visit_chunks(&self, r: Range<ByteOffset>, v: &mut dyn FnMut(&str)) -> bool {
        self.0.try_visit_chunks(r, v).is_ok()
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        self.0.status()
    }
    fn try_known_line(&self, l: LineIndex) -> Result<qe_api::KnownLine, LookupError> {
        self.0.try_known_line(l)
    }
    fn try_len_bytes(&self) -> Result<ByteOffset, LookupError> {
        self.0.try_len_bytes()
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        self.0.try_len_utf16()
    }
    fn try_line_count(&self) -> Result<usize, LookupError> {
        self.0.try_line_count()
    }
    fn try_byte_to_utf16(&self, b: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.0.try_byte_to_utf16(b)
    }
    fn try_utf16_to_byte(&self, u: Utf16Offset) -> Result<ByteOffset, LookupError> {
        self.0.try_utf16_to_byte(u)
    }
    fn try_byte_to_position(&self, b: ByteOffset) -> Result<TextPosition, LookupError> {
        self.0.try_byte_to_position(b)
    }
    fn try_position_to_byte(&self, p: TextPosition) -> Result<ByteOffset, LookupError> {
        self.0.try_position_to_byte(p)
    }
    fn try_line_range(&self, l: LineIndex) -> Result<Range<ByteOffset>, LookupError> {
        self.0.try_line_range(l)
    }
    fn try_visit_chunks(
        &self,
        r: Range<ByteOffset>,
        v: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        self.0.try_visit_chunks(r, v)
    }
}

/// Where a byte is, in document coordinates: `y` from the top of the document and `x`
/// from the text's left edge.
#[derive(Clone, Copy, Debug)]
struct Place {
    x: f64,
    y: f64,
    height: f32,
    /// False when this comes from an estimate because layout has not arrived.
    exact: bool,
    /// False when `x` means nothing yet, so the view must not move sideways for it.
    x_known: bool,
}

/// The visual row that holds a caret.
struct RowHere {
    line: usize,
    segment: Option<usize>,
    layout: Arc<LineLayout>,
    row: usize,
    /// The layout's left edge in document coordinates.
    x0: f64,
    x: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DragUnit {
    Char,
    Word,
    Line,
}

/// Input that takes its turn behind a key that waits.
#[derive(Clone)]
enum Queued {
    Event(InputEvent),
    Command(EditorCommand),
}

struct Warning {
    text: String,
    /// A store failure stays until the store changes. Other warnings stay until the user
    /// edits, because reading one must not depend on moving the mouse.
    sticky: bool,
}

/// What a copy's thread hands to the UI thread.
enum CopyOutput {
    Text(String),
    Prepared(Box<dyn std::any::Any + Send>),
}

impl Queued {
    /// True for input that would change the text when its turn comes.
    fn changes_text(&self) -> bool {
        match self {
            Queued::Command(command) => command.edits(),
            Queued::Event(event) => match &event.kind {
                InputKind::Char(_) => true,
                InputKind::KeyDown {
                    virtual_key,
                    modifiers,
                    ..
                } => {
                    matches!(*virtual_key, 0x08 | 0x0d | 0x2e)
                        || (modifiers.ctrl && matches!(*virtual_key, 0x56 | 0x58 | 0x59 | 0x5a))
                }
                _ => false,
            },
        }
    }
}
/// What a key held in the queue waits for.
#[derive(Clone, Copy, PartialEq)]
enum WaitsFor {
    /// The file's index, to learn where the file ends.
    Index,
    /// A line's layout, to learn its rows.
    Layout,
}
/// A size in words a person can use: "0 MiB" would say nothing about 64 KiB.
fn plain_size(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{} MiB", bytes >> 20)
    } else if bytes >= 1 << 10 {
        format!("{} KiB", bytes >> 10)
    } else {
        format!("{bytes} bytes")
    }
}
struct CopyJob {
    /// Turned on when a newer action replaces this copy, so its thread stops soon.
    cancel: Arc<AtomicBool>,
    results: Receiver<Result<CopyOutput, String>>,
    /// The edit already happened, so a failure must say where the text went.
    is_cut: bool,
}

/// Room for text, or a sentence that says there is none. A failed allocation must not
/// end the app, because other tabs may hold unsaved text.
fn reserve_text(bytes: usize) -> Result<String, String> {
    let mut text = String::new();
    text.try_reserve_exact(bytes).map_err(|_| {
        format!(
            "There is not enough memory to copy this selection of {} MiB.",
            bytes >> 20
        )
    })?;
    Ok(text)
}

/// What a key or menu asks the editor to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    SelectAll,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    InsertLineEnding,
}

impl EditorCommand {
    /// True when this command changes text, so read-only states refuse it.
    fn edits(self) -> bool {
        !matches!(self, Self::SelectAll | Self::Copy)
    }
}

pub struct EditorView {
    store: PieceStore,
    mapped: Option<MappedStore>,
    history: UndoHistory,
    engine: Arc<DirectWriteLayout>,
    font: Option<(String, f32)>,
    row_height: f32,
    cell_width: f32,
    fast_font: bool,
    cache: RefCell<LineCache>,
    heights: HeightMap,
    tasks: Sender<WorkerTask>,
    results: Receiver<LayoutResult>,
    map_results: Receiver<MapResult>,
    known_results: Receiver<KnownResult>,
    shared: Arc<WorkerShared>,
    known_partial: Option<(u64, Arc<LineLayout>)>,
    known_inflight: Option<(u64, u64)>,
    long_maps: HashMap<usize, HeightMap>,
    /// Map tasks by id, with the line's current number. `None` means the line changed.
    map_tasks: HashMap<u64, Option<usize>>,
    /// Layout tasks by id, with the line's current number and the segment.
    layout_tasks: HashMap<u64, (Option<usize>, Option<usize>)>,
    /// How many tasks are out for each line and segment.
    inflight: HashMap<(usize, Option<usize>), u32>,
    next_task: u64,
    /// Lines whose layout failed. They wait for an edit or a new size.
    failed: HashSet<(usize, Option<usize>)>,
    /// What the line showed before an edit, painted until its new layout arrives.
    ghosts: HashMap<(usize, usize), Arc<LineLayout>>,
    layout_generation: u64,
    bounds: Rect,
    caret: ByteOffset,
    anchor: ByteOffset,
    affinity: Affinity,
    /// The caret's line. A mapped store can lose the page that answers this, and the
    /// caret must still paint. `None` waits for the store.
    caret_line: Option<usize>,
    /// The x that a run of Up and Down keys keeps. Any other move or edit clears it.
    goal_x: Option<f64>,
    scroll: f64,
    /// In the view's own state this is an `f64`, because an `f32` has steps of 64 DIP on
    /// a 100 MB line. It becomes `f32` only as a distance from the view.
    scroll_x: f64,
    wheel_lines: u32,
    /// Page keys that wait for their target, as a signed count of pages.
    pending_pages: i32,
    /// Input held back in order, behind a key that waits for layout or for the index.
    deferred: std::collections::VecDeque<Queued>,
    /// The text that was at the top of the view before a new width, wrap or font, and
    /// when that change happened. The view goes back to it as layouts arrive.
    pending_anchor: Option<(ByteOffset, Instant)>,
    /// The caret was on screen before that change, so it must be on screen after it.
    keep_caret_visible: bool,
    /// The width the current layouts are for. A minimized window has width 0, and
    /// nothing is laid out for that.
    laid_width: Dip,
    /// When drag scrolling may next move. It follows a clock and not the events.
    drag_due: Instant,
    deferred_since: Instant,
    /// When the queue last went from empty to holding input.
    deferred_first: Instant,
    /// The selection a "covers only part of the file" warning speaks of.
    partial_selection: Option<Range<ByteOffset>>,
    /// The view's height, the last time it had one.
    last_view_height: f64,
    /// A Paste that waits for the copy in progress.
    pending_paste: bool,
    loading: bool,
    /// How far ink may reach past a row. Tall marks raise it.
    ink_margin: f32,
    /// The widest text seen, which limits horizontal scrolling.
    widest: f64,
    /// An edit changed the widest line, so the limit may come down once its layout is in.
    widest_dirty: bool,
    reveal_pending: bool,
    focused: bool,
    visible: bool,
    /// A key showed the hidden caret, and it may not have moved or repainted.
    caret_dirty: bool,
    blink_due: Instant,
    last_input: Instant,
    dragging: bool,
    drag_point: Option<Point>,
    drag_unit: DragUnit,
    drag_origin: Range<ByteOffset>,
    wrap: bool,
    prevent_editing: bool,
    pending: bool,
    last_click_us: u64,
    last_click_at: Option<Point>,
    click_count: u8,
    double_click_us: u64,
    double_click_width: f32,
    double_click_height: f32,
    preferred_ending: &'static str,
    warning: Option<Warning>,
    warning_dirty: bool,
    refused_edits: u64,
    copy_job: Option<CopyJob>,
    copy_limit: usize,
    /// A move on a mapped file that found its text not in memory, and when it first ran.
    retry_key: Option<(u16, Modifiers, Instant)>,
    clipboard: Box<dyn Clipboard>,
}

fn make_engine(font: &Option<(String, f32)>, wrap: bool) -> Result<DirectWriteLayout, String> {
    let engine = match font {
        Some((family, points)) => DirectWriteLayout::with_font(
            family,
            *points,
            windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
        )
        .map_err(|e| e.to_string())?,
        None => {
            DirectWriteLayout::with_default_font()
                .map_err(|e| e.to_string())?
                .0
        }
    };
    Ok(engine.with_wrap(wrap))
}

/// One row's height, one cell's width, and whether plain ASCII lays out with no system
/// call. That takes equally wide characters, which `i` and `W` show in practice.
fn measure_font(engine: &DirectWriteLayout) -> Result<(f32, f32, bool), String> {
    let sample = PieceStore::new("iW");
    let layout = engine
        .try_layout_line(&sample, LineIndex(0), Dip(1_000_000.0))
        .map_err(|e| e.to_string())?;
    let row = &layout.rows[0];
    let stops = &row.caret_stops;
    let fast = stops.len() == 3 && {
        let first = stops[1].x.0 - stops[0].x.0;
        let second = stops[2].x.0 - stops[1].x.0;
        (first - second).abs() < 0.01
    };
    Ok((row.height.0.max(1.0), engine.cell_width().0.max(1.0), fast))
}

struct LineFrame<'a> {
    theme: &'a Theme,
    selection: Range<ByteOffset>,
    x: f32,
    y: f32,
    caret: Option<(ByteOffset, Affinity)>,
    /// Where a selected line ending's highlight stops.
    view_right: f32,
    /// The line's content end. A row that ends here holds the line ending.
    content_end: Option<ByteOffset>,
    /// With wrap on, the next text sits on the next row, even past a segment's end.
    wrap: bool,
}

fn paint_line_layout(list: &mut DisplayList, layout: &LineLayout, frame: &LineFrame) {
    let selection = &frame.selection;
    let (x, y) = (frame.x, frame.y);
    for (index, row) in layout.rows.iter().enumerate() {
        let last_row = index + 1 == layout.rows.len();
        let hard_end = last_row && frame.content_end == Some(row.range.end);
        let selected_past_end = selection.end > row.range.end
            && selection.start <= row.range.end
            && !selection.is_empty();
        let overlaps = selection.start < row.range.end && selection.end > row.range.start;
        if overlaps || (selected_past_end && hard_end) {
            let first = selection.start.max(row.range.start);
            let last = selection.end.min(row.range.end);
            let left = layout.caret_rect(first, Affinity::After);
            let right = layout.caret_rect(last, Affinity::Before);
            if !row.visual_stops.is_empty() {
                let reach_edge = selected_past_end && (hard_end || !last_row || frame.wrap);
                paint_mixed_selection(list, row, frame, first..last, reach_edge);
            } else if let (Some(a), Some(b)) = (left, right) {
                let start = a.x.0.min(b.x.0);
                let mut end = a.x.0.max(b.x.0);
                if selected_past_end && (hard_end || !last_row || frame.wrap) {
                    end = end.max(frame.view_right - x);
                }
                list.push(DrawCommand::FillRect {
                    rect: Rect {
                        x: Dip(x + start),
                        y: Dip(y + row.y.0),
                        width: Dip((end - start).max(1.0)),
                        height: row.height,
                    },
                    color: qe_api::Color {
                        a: 64,
                        ..frame.theme.text
                    },
                });
            }
        }
        for run in &row.glyph_runs {
            list.push(DrawCommand::GlyphRun {
                at: Point {
                    x: Dip(x + run.origin.x.0),
                    y: Dip(y + run.origin.y.0),
                },
                bidi_level: run.bidi_level,
                font_id: run.font_id,
                font_em_size: run.font_em_size,
                glyph_ids: run.glyph_ids.clone(),
                advances: run.advances.clone(),
                offsets: run.offsets.clone(),
                color: frame.theme.text,
            });
        }
    }
    if let Some((byte, affinity)) = frame.caret
        && let Some(rect) = layout.caret_rect(byte, affinity)
    {
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(x + rect.x.0),
                y: Dip(y + rect.y.0),
                width: rect.width,
                height: rect.height,
            },
            color: frame.theme.text,
        });
    }
}

/// A row with right-to-left text. Selected text can sit in several places, so each
/// cluster gets its own span, and touching spans join into one rectangle.
fn paint_mixed_selection(
    list: &mut DisplayList,
    row: &qe_api::LayoutRow,
    frame: &LineFrame,
    selected: Range<ByteOffset>,
    reach_edge: bool,
) {
    let mut spans: Vec<(f32, f32)> = row
        .caret_stops
        .windows(2)
        .filter(|pair| pair[0].byte >= selected.start && pair[1].byte <= selected.end)
        .map(|pair| {
            let (a, b) = (pair[0].x.0, pair[1].x_before.0);
            (a.min(b), a.max(b))
        })
        .collect();
    if reach_edge {
        let right = row
            .caret_stops
            .iter()
            .map(|stop| stop.x.0.max(stop.x_before.0))
            .fold(0.0, f32::max);
        spans.push((right, (frame.view_right - frame.x).max(right)));
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut joined: Vec<(f32, f32)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match joined.last_mut() {
            Some(last) if start <= last.1 + 0.5 => last.1 = last.1.max(end),
            _ => joined.push((start, end)),
        }
    }
    for (start, end) in joined {
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(frame.x + start),
                y: Dip(frame.y + row.y.0),
                width: Dip((end - start).max(1.0)),
                height: row.height,
            },
            color: qe_api::Color {
                a: 64,
                ..frame.theme.text
            },
        });
    }
}

/// The caret for a layout from before an edit: at its nearest stop at or before the byte.
fn ghost_caret(layout: &LineLayout, byte: ByteOffset) -> Option<Rect> {
    let mut best = None;
    for row in &layout.rows {
        for stop in &row.caret_stops {
            if stop.byte <= byte {
                best = Some(Rect {
                    x: stop.x,
                    y: row.y,
                    width: Dip(1.0),
                    height: row.height,
                });
            }
        }
    }
    best
}

/// Joins a group's changes, each in the coordinates the one before it left, into one
/// change of the text from before the first to the text after the last.
fn merge_changes(changes: &[TextChange]) -> Option<TextChange> {
    let first = changes.first()?;
    let mut start = first.replaced.start.0;
    let mut old_end = first.replaced.end.0;
    let mut new_end = start + first.new_len;
    let mut delta = first.line_count_delta;
    let mut revision = first.revision;
    for change in &changes[1..] {
        let (s, e) = (change.replaced.start.0, change.replaced.end.0);
        // Where this change ends, in the text from before the group.
        let e_in_old = if e >= new_end {
            e - new_end + old_end
        } else {
            old_end
        };
        let end_after = new_end.max(e) - (e - s) + change.new_len;
        start = start.min(s);
        old_end = old_end.max(e_in_old);
        new_end = end_after;
        delta += change.line_count_delta;
        revision = change.revision;
    }
    Some(TextChange {
        replaced: ByteOffset(start)..ByteOffset(old_end),
        new_len: new_end - start,
        line_count_delta: delta,
        revision,
    })
}

/// Text the layout crate lays out with no system call. It wraps only text with no tabs,
/// so a wrapped line with tabs is shaped, which belongs on a worker.
fn lays_out_with_no_system_call(bytes: &[u8], wrap: bool) -> bool {
    bytes
        .iter()
        .all(|b| (0x20..0x7F).contains(b) || (*b == b'\t' && !wrap))
}

fn snapshot_of(store: &PieceStore, mapped: &Option<MappedStore>) -> Arc<dyn TextSnapshot> {
    match mapped {
        Some(mapped) => Arc::new(NonBlocking(mapped.snapshot())),
        None => store.snapshot(),
    }
}

impl EditorView {
    pub fn from_text(text: &str) -> Result<Self, String> {
        let mut view = Self::from_store(PieceStore::new(text))?;
        view.preferred_ending = preferred_line_ending_bytes(text.as_bytes());
        Ok(view)
    }
    pub fn from_store(store: PieceStore) -> Result<Self, String> {
        let (engine, choice) = DirectWriteLayout::with_default_font().map_err(|e| e.to_string())?;
        let (row_height, cell_width, fast_font) = measure_font(&engine)?;
        let engine = Arc::new(engine.with_full_rows_at(Dip(800.0)));
        let (tasks, task_rx) = mpsc::channel::<WorkerTask>();
        let (result_tx, results) = mpsc::channel();
        let (map_tx, map_results) = mpsc::channel();
        let (known_tx, known_results) = mpsc::channel();
        let shared = Arc::new(WorkerShared {
            closing: AtomicBool::new(false),
            alive: AtomicUsize::new(0),
            wanted_first: AtomicUsize::new(0),
            wanted_last: AtomicUsize::new(usize::MAX),
            skipped: AtomicU64::new(0),
            fail_line: AtomicUsize::new(usize::MAX),
            test_paused: AtomicBool::new(false),
            drops_out: AtomicUsize::new(0),
            wake: std::sync::Mutex::new(None),
        });
        let shared_rx = Arc::new(std::sync::Mutex::new(task_rx));
        for worker in 0..4 {
            let result_tx: Sender<LayoutResult> = result_tx.clone();
            let map_tx: Sender<MapResult> = map_tx.clone();
            let known_tx: Sender<KnownResult> = known_tx.clone();
            let task_rx = shared_rx.clone();
            let worker_shared = shared.clone();
            shared.alive.fetch_add(1, Ordering::AcqRel);
            let spawned = thread::Builder::new()
                .name(format!("qe-editor-layout-{worker}"))
                .spawn(move || worker_loop(task_rx, result_tx, map_tx, known_tx, worker_shared));
            if let Err(e) = spawned {
                shared.alive.fetch_sub(1, Ordering::AcqRel);
                return Err(e.to_string());
            }
        }
        let heights = HeightMap::new(store.status().known_lines.max(1), Dip(row_height))
            .map_err(|e| e.to_string())?;
        let now = Instant::now();
        let warning = choice.is_fallback().then(|| Warning {
            text: format!("{} unavailable; using {}", choice.requested, choice.used),
            sticky: false,
        });
        Ok(Self {
            history: UndoHistory::for_store(&store),
            store,
            mapped: None,
            engine,
            font: None,
            row_height,
            cell_width,
            fast_font,
            cache: RefCell::new(LineCache::new(CACHE_BYTES)),
            heights,
            tasks,
            results,
            map_results,
            known_results,
            shared,
            known_partial: None,
            known_inflight: None,
            long_maps: HashMap::new(),
            map_tasks: HashMap::new(),
            layout_tasks: HashMap::new(),
            inflight: HashMap::new(),
            next_task: 0,
            failed: HashSet::new(),
            ghosts: HashMap::new(),
            layout_generation: 0,
            bounds: Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(800.0),
                height: Dip(600.0),
            },
            caret: ByteOffset(0),
            anchor: ByteOffset(0),
            affinity: Affinity::After,
            caret_line: Some(0),
            goal_x: None,
            scroll: 0.0,
            scroll_x: 0.0,
            wheel_lines: 3,
            pending_pages: 0,
            deferred: std::collections::VecDeque::new(),
            pending_anchor: None,
            keep_caret_visible: false,
            laid_width: Dip(800.0),
            drag_due: now,
            deferred_since: now,
            deferred_first: now,
            partial_selection: None,
            last_view_height: 0.0,
            pending_paste: false,
            loading: false,
            ink_margin: INK_MARGIN,
            widest: 0.0,
            widest_dirty: false,
            reveal_pending: false,
            focused: false,
            visible: true,
            caret_dirty: false,
            blink_due: now + BLINK,
            last_input: now,
            dragging: false,
            drag_point: None,
            drag_unit: DragUnit::Char,
            drag_origin: ByteOffset(0)..ByteOffset(0),
            wrap: true,
            prevent_editing: false,
            pending: false,
            last_click_us: 0,
            last_click_at: None,
            click_count: 0,
            // The Windows defaults. A host passes the user's with `set_double_click_limits`.
            double_click_us: 500_000,
            double_click_width: 4.0,
            double_click_height: 4.0,
            preferred_ending: "\n",
            warning_dirty: warning.is_some(),
            warning,
            refused_edits: 0,
            copy_job: None,
            copy_limit: COPY_LIMIT_BYTES,
            retry_key: None,
            clipboard: Box::<MemoryClipboard>::default(),
        })
    }
    /// The caller opens the mapped file on a worker before constructing this view.
    pub fn from_mapped(mapped: MappedStore) -> Result<Self, String> {
        let mut view = Self::from_text("")?;
        view.adopt(LoadedFile::ReadOnly(mapped))?;
        Ok(view)
    }
    /// Takes the document that `FileLoad` finished. The view starts over on it. Work
    /// queued for the old text is dropped when it arrives.
    pub fn adopt(&mut self, loaded: LoadedFile) -> Result<(), String> {
        if self.mapped.is_none() && !self.history.is_at_save_point(&self.store) {
            return Err("This view has edits, so a file cannot replace them.".to_string());
        }
        if self.can_edit() && self.deferred.iter().any(Queued::changes_text) {
            return Err(
                "This view has typing still waiting, so a file cannot replace it.".to_string(),
            );
        }
        let (store, mapped, ending) = match loaded {
            LoadedFile::Editable { store, ending } => (store, None, ending),
            LoadedFile::ReadOnly(mapped) => (PieceStore::new(""), Some(mapped), "\n"),
        };
        let lines = mapped
            .as_ref()
            .map_or_else(|| store.status().known_lines, |m| m.status().known_lines)
            .max(1);
        let heights = HeightMap::new(lines, Dip(self.row_height)).map_err(|e| e.to_string())?;
        for (line, _) in self.layout_tasks.values_mut() {
            *line = None;
        }
        for line in self.map_tasks.values_mut() {
            *line = None;
        }
        self.inflight.clear();
        self.send_away_document(store);
        // The old text's cache must not meet the new text's revisions.
        self.cache = RefCell::new(LineCache::new(CACHE_BYTES));
        self.mapped = mapped;
        self.loading = false;
        self.warning = None;
        self.warning_dirty = true;
        self.pending_pages = 0;
        self.deferred.clear();
        self.pending_paste = false;
        self.pending_anchor = None;
        self.cancel_drag();
        self.cancel_copy();
        self.preferred_ending = ending;
        self.heights = heights;
        self.long_maps.clear();
        self.ghosts.clear();
        self.failed.clear();
        self.known_partial = None;
        self.known_inflight = None;
        self.layout_generation = self.layout_generation.wrapping_add(1);
        self.caret = ByteOffset(0);
        self.anchor = ByteOffset(0);
        self.affinity = Affinity::After;
        self.caret_line = Some(0);
        self.goal_x = None;
        self.scroll = 0.0;
        self.scroll_x = 0.0;
        self.widest = 0.0;
        self.widest_dirty = false;
        self.reveal_pending = false;
        // The next timer lays out the new text.
        self.pending = true;
        Ok(())
    }
    /// Puts a new store in place and frees the old one and its history on a worker, as
    /// the qe-buffer README requires for big text.
    fn send_away_document(&mut self, store: PieceStore) {
        let history = UndoHistory::for_store(&store);
        let old_history = std::mem::replace(&mut self.history, history);
        let old_store = std::mem::replace(&mut self.store, store);
        let document: Box<dyn std::any::Any + Send> = Box::new((old_store, old_history));
        // A failed send gives the document back in the error, which frees it here.
        if self.tasks.send(WorkerTask::DropDocument(document)).is_err() {
            self.set_warning("The layout workers stopped.".to_string(), true);
        }
    }
    fn source(&self) -> &dyn TextSnapshot {
        self.mapped
            .as_ref()
            .map_or(&self.store as &dyn TextSnapshot, |s| s as &dyn TextSnapshot)
    }
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        snapshot_of(&self.store, &self.mapped)
    }
    pub fn caret(&self) -> ByteOffset {
        self.caret
    }
    pub fn selection(&self) -> Range<ByteOffset> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
    pub fn text(&self) -> Option<String> {
        let mut text = String::new();
        self.source()
            .try_visit_chunks(
                ByteOffset(0)..self.source().status().known_bytes,
                &mut |s| text.push_str(s),
            )
            .ok()?;
        Some(text)
    }
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = clipboard;
    }
    pub fn set_preferred_ending(&mut self, ending: &'static str) {
        self.preferred_ending = ending;
    }
    /// What the host reads from `GetDoubleClickTime`, `SM_CXDOUBLECLK` and `SM_CYDOUBLECLK`.
    pub fn set_double_click_limits(&mut self, microseconds: u64, width: f32, height: f32) {
        self.double_click_us = microseconds;
        self.double_click_width = width;
        self.double_click_height = height;
    }
    pub fn pending_layout(&self) -> bool {
        self.pending || self.copy_job.is_some()
    }
    pub fn pending_task_count(&self) -> usize {
        self.layout_tasks.len()
    }
    pub fn scroll_offset(&self) -> f64 {
        self.scroll
    }
    pub fn horizontal_offset(&self) -> f32 {
        self.scroll_x as f32
    }
    pub fn line_count(&self) -> usize {
        self.source().status().known_lines
    }
    pub fn height_run_count(&self) -> usize {
        self.heights.run_count()
    }
    /// What the store knows now: its size and whether its index is complete.
    pub fn store_status(&self) -> StoreStatus {
        self.source().status()
    }
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_ref().map(|w| w.text.as_str())
    }
    /// Shows a message from the host, such as a file that failed to open. The next edit
    /// or Esc clears it.
    pub fn show_warning(&mut self, text: impl Into<String>) {
        self.set_warning(text.into(), false);
    }
    pub fn refused_edits(&self) -> u64 {
        self.refused_edits
    }
    pub fn memory_to_release_bytes(&self) -> usize {
        self.history.memory_to_release_bytes()
    }
    /// Layout tasks that waited in the queue until their line left the wanted window.
    pub fn skipped_task_count(&self) -> u64 {
        self.shared.skipped.load(Ordering::Acquire)
    }
    /// A counter of live workers. It reaches 0 after the view is dropped.
    pub fn worker_counter(&self) -> impl Fn() -> usize + use<> {
        let shared = self.shared.clone();
        move || shared.alive.load(Ordering::Acquire)
    }
    /// How many tasks the view has sent to its workers, ever.
    pub fn layout_tasks_sent(&self) -> u64 {
        self.next_task
    }
    /// A file is on its way. The view refuses edits until `adopt` or `end_load`, as the
    /// Mac refuses edits during a reload.
    pub fn begin_load(&mut self) {
        self.loading = true;
    }
    /// The load failed or stopped, so the view takes edits again.
    pub fn end_load(&mut self) {
        self.loading = false;
    }
    /// What a worker calls when a result is ready, from its own thread. Without this a
    /// result waits for the view's next timer.
    pub fn set_wake(&mut self, wake: qe_buffer::Notify) {
        if let Ok(mut slot) = self.shared.wake.lock() {
            *slot = Some(wake);
        }
    }
    /// What the host reads from `SPI_GETWHEELSCROLLLINES`. `u32::MAX` is its value for
    /// one screen per notch.
    pub fn set_wheel_lines(&mut self, lines: u32) {
        self.wheel_lines = lines;
    }
    pub fn horizontal_offset_exact(&self) -> f64 {
        self.scroll_x
    }
    /// True when the screen shows the text as it is now at the caret: its line is off
    /// screen, or its layout has arrived. It is true once layout work has settled. A
    /// host's key timer reads this.
    pub fn caret_is_laid_out(&self) -> bool {
        let Some(line) = self.caret_line else {
            return false;
        };
        let Some(place) = self.place(self.caret, self.affinity) else {
            return false;
        };
        if self.ghost_covers_caret(line) {
            return false;
        }
        let view = f64::from(self.bounds.height.0);
        let off_screen =
            place.y + f64::from(place.height) <= self.scroll || place.y >= self.scroll + view;
        place.exact || off_screen
    }
    fn ghost_covers_caret(&self, line: usize) -> bool {
        if self.long_maps.contains_key(&line) {
            let snapshot = self.snapshot();
            let segment = self
                .engine
                .segment_at(snapshot.as_ref(), self.caret, self.affinity)
                .map_or(0, |(_, segment, _)| segment);
            self.ghosts.contains_key(&(line, segment))
        } else {
            self.ghosts.contains_key(&(line, WHOLE))
        }
    }
    #[doc(hidden)]
    pub fn test_workers_holding_snapshots(&self) -> usize {
        self.shared.drops_out.load(Ordering::Acquire)
    }
    /// A worker starts no task while this holds, so a test can hold results back.
    #[doc(hidden)]
    pub fn test_pause_workers(&mut self, paused: bool) {
        self.shared.test_paused.store(paused, Ordering::Release);
    }
    /// Input that has not taken effect yet: keys held in the queue, and Page presses that
    /// wait for their part of the file. A host's key timer runs until this is 0.
    pub fn queued_input_count(&self) -> usize {
        self.deferred.len() + self.pending_pages.unsigned_abs() as usize
    }
    #[doc(hidden)]
    pub fn test_byte_at_top(&mut self) -> usize {
        self.byte_at_top().0
    }
    #[doc(hidden)]
    pub fn test_total_height(&self) -> f64 {
        self.heights.total_height()
    }
    #[doc(hidden)]
    pub fn test_height_map_lines(&self) -> usize {
        self.heights.len()
    }
    #[doc(hidden)]
    pub fn test_fail_layout_of_line(&mut self, line: usize) {
        self.shared.fail_line.store(line, Ordering::Release);
    }
    #[doc(hidden)]
    pub fn test_set_copy_limit(&mut self, bytes: usize) {
        self.copy_limit = bytes;
    }
    pub fn set_prevent_editing(&mut self, enabled: bool) {
        self.prevent_editing = enabled;
    }
    /// The one check every text change passes.
    pub fn can_edit(&self) -> bool {
        !self.prevent_editing && self.mapped.is_none() && !self.loading
    }
    pub fn cancel_drag(&mut self) {
        self.dragging = false;
        self.drag_point = None;
    }
    pub fn is_at_save_point(&self) -> bool {
        self.mapped.is_none() && self.history.is_at_save_point(&self.store)
    }
    /// Edits until `end_edit_group` undo as one step. Replace All needs this.
    pub fn begin_edit_group(&mut self) {
        self.history.begin_group();
    }
    pub fn end_edit_group(&mut self) {
        self.history.end_group();
    }
    /// Changes a range of the text and puts the caret after the new text.
    pub fn replace_range(&mut self, range: Range<ByteOffset>, text: &str, ctx: &mut ViewContext) {
        if !self.can_edit() {
            self.refused_edits += 1;
            return;
        }
        self.apply_edit(range, text, false, ctx);
        self.finish(Instant::now(), ctx);
    }
    /// Selects a range with no move of the view.
    pub fn select(&mut self, range: Range<ByteOffset>, ctx: &mut ViewContext) {
        // A range from before the text changed comes back inside it, on character edges.
        let known = self.source().status().known_bytes.0;
        let start = range.start.0.min(known);
        let end = range.end.0.min(known);
        let start = self
            .with_stops(|s| s.edge_backward(start, 0))
            .map_or(ByteOffset(start), ByteOffset);
        let end = self
            .with_stops(|s| s.edge_backward(end, 0))
            .map_or(ByteOffset(end), ByteOffset);
        self.set_selection(start, end, Affinity::After, false, ctx);
        self.finish(Instant::now(), ctx);
    }
    pub fn on_timer(&mut self, ctx: &mut ViewContext) {
        self.on_timer_at(Instant::now(), ctx);
    }
    /// `on_timer` with the clock passed in. A timer can fire early, because the store
    /// and worker polling share it, so only the clock decides when the caret blinks.
    pub fn on_timer_at(&mut self, now: Instant, ctx: &mut ViewContext) {
        if self.pending || self.mapped.is_some() || self.copy_job.is_some() {
            self.poll_inner(now, ctx);
        }
        self.drag_step(now, ctx);
        if self.focused && self.selection().is_empty() && now >= self.blink_due {
            self.visible = !self.visible;
            self.blink_due = now + BLINK;
            self.repaint_caret(self.caret, self.affinity, ctx);
        }
        if !self.pending
            && !self.dragging
            && now.saturating_duration_since(self.last_input) >= IDLE_BEFORE_RELEASE
            && self.history.has_memory_to_release()
        {
            self.history.release_memory();
        }
        self.finish(now, ctx);
    }
    /// Asks for the one timer that the soonest job needs, and shows a changed warning.
    fn finish(&mut self, now: Instant, ctx: &mut ViewContext) {
        self.settle_partial_warning();
        if self.caret_dirty {
            self.caret_dirty = false;
            self.repaint_caret(self.caret, self.affinity, ctx);
        }
        if self.warning_dirty {
            self.warning_dirty = false;
            ctx.invalidate(Rect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: self.bounds.width,
                height: Dip(WARNING_STRIP.min(self.bounds.height.0)),
            });
        }
        let mut wait: Option<u32> = None;
        if self.pending || self.copy_job.is_some() || (self.dragging && self.drag_is_outside()) {
            wait = Some(WORK_TIMER_MS);
        }
        if self.focused && self.selection().is_empty() {
            let blink = self
                .blink_due
                .saturating_duration_since(now)
                .as_millis()
                .clamp(1, BLINK.as_millis()) as u32;
            wait = Some(wait.map_or(blink, |w| w.min(blink)));
        }
        if let Some(wait) = wait {
            ctx.request_timer(wait);
        }
    }
    /// Shows the caret and restarts its blink.
    fn show_caret(&mut self) {
        let now = Instant::now();
        self.caret_dirty = !self.visible;
        self.visible = true;
        self.blink_due = now + BLINK;
        self.last_input = now;
    }
    /// A key or a click. The newest action wins over work that still waits.
    fn touch(&mut self) {
        self.show_caret();
        self.retry_key = None;
        self.pending_pages = 0;
        self.pending_anchor = None;
        self.keep_caret_visible = false;
        if self.pending_paste {
            self.pending_paste = false;
            self.set_warning(
                "Paste was dropped, because you went on working while a long Copy finished."
                    .to_string(),
                false,
            );
        }
    }
    /// An edit is the user acting on what a warning said.
    fn clear_warning_after_edit(&mut self) {
        if self.warning.as_ref().is_some_and(|w| !w.sticky) {
            self.warning = None;
            self.warning_dirty = true;
        }
    }
    fn set_warning(&mut self, text: String, sticky: bool) {
        self.partial_selection = None;
        if self.warning.as_ref().is_none_or(|w| w.text != text) {
            self.warning = Some(Warning { text, sticky });
            self.warning_dirty = true;
        }
    }
    pub fn set_wrap(&mut self, enabled: bool) -> Result<(), String> {
        if enabled != self.wrap {
            let engine = make_engine(&self.font, enabled)?;
            self.remember_view();
            self.wrap = enabled;
            if enabled {
                self.scroll_x = 0.0;
            }
            self.engine = Arc::new(engine.with_full_rows_at(self.laid_width));
            self.restart_layout()?;
        }
        Ok(())
    }
    /// `set_wrap`, and asks for the repaint and the layout that it needs.
    pub fn apply_wrap(&mut self, enabled: bool, ctx: &mut ViewContext) -> Result<(), String> {
        self.set_wrap(enabled)?;
        self.after_restart(ctx);
        Ok(())
    }
    fn after_restart(&mut self, ctx: &mut ViewContext) {
        ctx.invalidate(self.bounds);
        self.poll_inner(Instant::now(), ctx);
        self.finish(Instant::now(), ctx);
    }
    /// `set_font`, and asks for the repaint and the layout that it needs.
    pub fn apply_font(
        &mut self,
        family: &str,
        points: f32,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.set_font(family, points)?;
        self.after_restart(ctx);
        Ok(())
    }
    pub fn set_font(&mut self, family: &str, points: f32) -> Result<(), String> {
        let font = Some((family.to_string(), points));
        let engine = make_engine(&font, self.wrap)?;
        self.remember_view();
        (self.row_height, self.cell_width, self.fast_font) = measure_font(&engine)?;
        self.font = font;
        self.engine = Arc::new(engine.with_full_rows_at(self.laid_width));
        self.restart_layout()
    }
    /// Forgets all measured sizes, and keeps the top line on screen.
    fn restart_layout(&mut self) -> Result<(), String> {
        let top = self.heights.scroll_to_line(self.scroll);
        self.cache.borrow_mut().clear();
        self.long_maps.clear();
        self.ghosts.clear();
        self.failed.clear();
        self.widest = 0.0;
        self.widest_dirty = false;
        self.layout_generation = self.layout_generation.wrapping_add(1);
        self.known_partial = None;
        self.known_inflight = None;
        self.heights = HeightMap::new(
            self.source().status().known_lines.max(1),
            Dip(self.row_height),
        )
        .map_err(|e| e.to_string())?;
        self.scroll = self.heights.line_to_scroll(top);
        self.clamp_scroll();
        // The next timer lays the text out again.
        self.pending = true;
        Ok(())
    }
    /// Remembers what the view shows, before a change of width, wrap or font makes the
    /// layouts that know it useless. A change while the view still heads for its text
    /// keeps heading for that text.
    fn remember_view(&mut self) {
        if self.pending_anchor.is_none() {
            let anchor = self.byte_at_top();
            self.pending_anchor = Some((anchor, Instant::now()));
            self.keep_caret_visible = self.caret_is_on_screen();
        } else if let Some((_, since)) = &mut self.pending_anchor {
            *since = Instant::now();
        }
    }
    /// The first byte of the row at the top of the view, or with wrap off of that row's
    /// part at the left of the view.
    fn byte_at_top(&mut self) -> ByteOffset {
        // With wrap off, what the user sees first is at the view's left edge. This asks
        // only for a place, so a request it makes has no use: a restart follows it.
        if !self.wrap
            && self.scroll_x > 0.0
            && let Some((byte, _)) =
                self.resolve(self.scroll_x, self.scroll, &mut ViewContext::default())
        {
            return byte;
        }
        let line = self.heights.scroll_to_line(self.scroll);
        let line_top = self.heights.line_to_scroll(line);
        let local = (self.scroll - line_top).max(0.0);
        let snapshot = self.snapshot();
        let row_start = |layout: &LineLayout, y: f64| {
            let at = layout
                .rows
                .partition_point(|r| f64::from(r.y.0 + r.height.0) <= y)
                .min(layout.rows.len() - 1);
            layout.rows[at].range.start
        };
        if let Some(map) = self.long_maps.get(&line.0) {
            if !self.wrap {
                return self
                    .source()
                    .try_line_range(line)
                    .map_or(self.caret, |r| r.start);
            }
            let segment = map.scroll_to_line(local);
            let start = map.line_to_scroll(segment);
            let layout = self.cache.borrow_mut().cached_segment(
                &self.engine,
                snapshot.as_ref(),
                line,
                segment.0,
                self.width(),
            );
            return match layout {
                Some(layout) => row_start(&layout, local - start),
                None => self
                    .engine
                    .segment_range(snapshot.as_ref(), line, segment.0)
                    .map_or(self.caret, |r| r.start),
            };
        }
        let layout =
            self.cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), line, self.width());
        match layout {
            Some(layout) => row_start(&layout, local),
            None => self
                .source()
                .try_line_range(line)
                .map_or(self.caret, |r| r.start),
        }
    }
    fn caret_line_failed(&self) -> bool {
        self.caret_line
            .is_some_and(|caret| self.failed.iter().any(|(line, _)| *line == caret))
    }
    fn caret_is_on_screen(&self) -> bool {
        let view = self.last_view_height;
        self.place(self.caret, self.affinity).is_some_and(|place| {
            let sideways = self.wrap
                || !place.x_known
                || (place.x >= self.scroll_x
                    && place.x <= self.scroll_x + f64::from(self.laid_width.0));
            sideways
                && place.y + f64::from(place.height) > self.scroll
                && place.y < self.scroll + view
        })
    }
    /// Takes the view back to the text it showed before a new width, wrap or font. Each
    /// new layout refines this, until the anchor's own row has its real place.
    fn apply_anchor(&mut self, now: Instant, ctx: &mut ViewContext) {
        let Some((anchor, since)) = self.pending_anchor else {
            return;
        };
        let Some(place) = self.place(anchor, Affinity::After) else {
            if now.saturating_duration_since(since) >= DEFER_LIMIT {
                self.pending_anchor = None;
            }
            return;
        };
        let before = (self.scroll, self.scroll_x);
        self.scroll = place.y.clamp(0.0, self.max_scroll());
        // With wrap off the text can be far to the side, and it must be on screen too.
        if !self.wrap && place.x_known {
            self.widest = self.widest.max(place.x);
            self.scroll_x = place.x.clamp(0.0, self.max_scroll_x());
        }
        if (self.scroll, self.scroll_x) != before {
            ctx.invalidate(self.bounds);
        }
        if place.exact || now.saturating_duration_since(since) >= DEFER_LIMIT {
            self.pending_anchor = None;
            if std::mem::take(&mut self.keep_caret_visible) {
                self.reveal_pending = true;
            }
        }
    }
    fn width(&self) -> Dip {
        if self.wrap {
            self.laid_width
        } else {
            Dip(1_000_000.0)
        }
    }
    fn max_scroll(&self) -> f64 {
        (self.heights.total_height() - f64::from(self.bounds.height.0)).max(0.0)
    }
    fn max_scroll_x(&self) -> f64 {
        if self.wrap {
            0.0
        } else {
            (self.widest + 2.0 - f64::from(self.bounds.width.0)).max(0.0)
        }
    }
    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        self.scroll_x = self.scroll_x.clamp(0.0, self.max_scroll_x());
    }
    fn set_bounds(&mut self, bounds: Rect) {
        // A minimized window shows nothing, so nothing is measured or moved for it.
        if bounds.width.0 <= 0.0 || bounds.height.0 <= 0.0 {
            self.bounds = bounds;
            return;
        }
        self.last_view_height = f64::from(bounds.height.0);
        let width_changed = self.wrap && bounds.width != self.laid_width;
        let moved = bounds != self.bounds;
        if width_changed {
            self.remember_view();
        }
        self.bounds = bounds;
        self.laid_width = bounds.width;
        if width_changed {
            // A long line's segment edges follow the width.
            self.engine = Arc::new(self.engine.with_full_rows_at(bounds.width));
            if let Err(e) = self.restart_layout() {
                self.set_warning(e, false);
            }
        }
        if moved {
            self.clamp_scroll();
        }
    }
    pub fn poll_layout(&mut self, ctx: &mut ViewContext) {
        self.poll_inner(Instant::now(), ctx);
        self.finish(Instant::now(), ctx);
    }
    fn poll_inner(&mut self, now: Instant, ctx: &mut ViewContext) {
        self.drain_results(ctx);
        self.poll_copy(ctx);
        self.settle_widest(ctx);
        if self.caret_line.is_none() {
            self.refresh_caret_line();
            if self.caret_line.is_some() {
                self.repaint_caret(self.caret, self.affinity, ctx);
            }
        }
        if let Some((vk, mods, first)) = self.retry_key.take() {
            let limit = if matches!(vk, 0x21 | 0x22) {
                PAGE_RETRY
            } else {
                KEY_RETRY
            };
            if now.saturating_duration_since(first) < limit {
                self.move_key(vk, mods, first, ctx);
            } else if self.pending_pages != 0 {
                self.pending_pages = 0;
                self.set_warning(
                    "Page Up or Page Down was dropped, because this part of the file did not \
                     arrive."
                        .to_string(),
                    false,
                );
            } else {
                self.set_warning(
                    "That key was dropped, because this part of the file did not arrive."
                        .to_string(),
                    false,
                );
            }
        }
        self.run_deferred(now, ctx);
        self.apply_anchor(now, ctx);
        self.schedule(ctx);
        if self.pending_anchor.is_none() && self.reveal_pending {
            self.reveal_caret(ctx);
        }
    }
    /// The store answers, or the layouts on screen do when its page is out of memory.
    fn refresh_caret_line(&mut self) {
        let known = self
            .source()
            .try_byte_to_position(self.caret)
            .ok()
            .map(|p| p.line.0);
        self.caret_line = known.or_else(|| self.line_from_layouts(self.caret));
    }
    fn line_from_layouts(&self, byte: ByteOffset) -> Option<usize> {
        let snapshot = self.snapshot();
        let view = f64::from(self.bounds.height.0);
        let first = self.heights.scroll_to_line((self.scroll - view).max(0.0)).0;
        let end = (self.heights.scroll_to_line(self.scroll + view * 2.0).0 + 1)
            .min(self.source().status().known_lines)
            .min(first + MAX_WINDOW_LINES);
        (first..end).find(|line| {
            let layout = self.cache.borrow_mut().cached(
                &self.engine,
                snapshot.as_ref(),
                LineIndex(*line),
                self.width(),
            );
            layout.is_some_and(|l| {
                l.rows.first().is_some_and(|r| r.range.start <= byte)
                    && l.rows.last().is_some_and(|r| byte <= r.range.end)
            })
        })
    }
    /// A byte's line, with the caret's known line when the store has no answer now.
    fn line_at(&self, byte: ByteOffset) -> Option<usize> {
        let known = self
            .source()
            .try_byte_to_position(byte)
            .ok()
            .map(|p| p.line.0);
        known.or_else(|| (byte == self.caret).then_some(self.caret_line).flatten())
    }
    fn take_task(&mut self, id: u64) -> (Option<usize>, Option<usize>) {
        let Some((line, segment)) = self.layout_tasks.remove(&id) else {
            return (None, None);
        };
        if let Some(line) = line
            && let Some(count) = self.inflight.get_mut(&(line, segment))
        {
            *count -= 1;
            if *count == 0 {
                self.inflight.remove(&(line, segment));
            }
        }
        (line, segment)
    }
    fn drain_results(&mut self, ctx: &mut ViewContext) {
        while let Ok(done) = self.results.try_recv() {
            let (line, segment) = self.take_task(done.id);
            let Some(line) = line else {
                continue;
            };
            match done.outcome {
                LayoutOutcome::Skipped => {}
                LayoutOutcome::Done(Ok(layout)) => {
                    self.note_ink(&layout);
                    let height = layout.height;
                    let extent = self.engine.segment_extent(&layout);
                    let width = self.widest_of(&layout);
                    if self.cache.borrow_mut().insert(&done.ticket, layout) {
                        self.ghosts.remove(&(line, segment.unwrap_or(WHOLE)));
                        match segment {
                            Some(segment) => {
                                self.note_segment(line, segment, extent, height, ctx);
                            }
                            None => {
                                self.widest = self.widest.max(width);
                                self.note_line_height(line, height, ctx);
                            }
                        }
                    }
                }
                LayoutOutcome::Done(Err(e)) => {
                    self.failed.insert((line, segment));
                    self.ghosts.remove(&(line, segment.unwrap_or(WHOLE)));
                    self.set_warning(format!("Line {} could not be shown: {e}", line + 1), false);
                    self.repaint_line(line, ctx);
                }
            }
        }
        while let Ok(done) = self.map_results.try_recv() {
            let line = self.map_tasks.remove(&done.id).flatten();
            let Some(line) = line else {
                continue;
            };
            if done.generation != self.layout_generation {
                continue;
            }
            match done.result {
                Ok(map) => {
                    let height = self.engine.long_line_height(&map);
                    if !self.wrap {
                        self.widest = self.widest.max(map.total_height());
                    }
                    self.long_maps.insert(line, map);
                    self.ghosts.remove(&(line, WHOLE));
                    self.set_line_height(line, height, ctx);
                    self.repaint_line_down(line, ctx);
                }
                Err(e) => {
                    self.failed.insert((line, Some(WHOLE)));
                    self.set_warning(format!("Line {} could not be shown: {e}", line + 1), false);
                }
            }
        }
        while let Ok(done) = self.known_results.try_recv() {
            if self.known_inflight == Some((done.revision, done.generation)) {
                self.known_inflight = None;
            }
            if done.generation != self.layout_generation
                || done.revision != self.snapshot().revision()
            {
                continue;
            }
            if self
                .source()
                .status()
                .failure
                .as_ref()
                .is_some_and(|f| f.valid_up_to == ByteOffset(0))
            {
                continue;
            }
            match done.result {
                Ok(layout) => {
                    self.known_partial = Some((done.revision, Arc::new(layout)));
                    self.repaint_line(0, ctx);
                }
                Err(e) => self.set_warning(e, false),
            }
        }
    }
    /// Marks with no advance of their own can stack far past a row, and only the layout
    /// knows. The margin stays high once seen, which only costs a larger repaint.
    fn note_ink(&mut self, layout: &LineLayout) {
        for run in layout.rows.iter().flat_map(|row| &row.glyph_runs) {
            let stacked =
                run.advances.iter().any(|a| a.0 == 0.0) || run.offsets.iter().any(|o| o.y.0 != 0.0);
            if stacked {
                let reach = run.offsets.iter().map(|o| o.y.0.abs()).fold(0.0, f32::max);
                self.ink_margin = self.ink_margin.max(run.font_em_size.0 * 1.5 + reach);
            }
        }
    }
    fn widest_of(&self, layout: &LineLayout) -> f64 {
        if self.wrap {
            return 0.0;
        }
        f64::from(self.engine.segment_extent(layout).0)
    }
    /// A line's measured height. The text on screen stays still when a line above
    /// changes, as the layout crate's docs say. Returns true when the height changed.
    fn set_line_height(&mut self, line: usize, height: Dip, ctx: &mut ViewContext) -> bool {
        let Some(old) = self.heights.line_height(LineIndex(line)) else {
            return false;
        };
        if old == height {
            return false;
        }
        let top = self.heights.scroll_to_line(self.scroll).0;
        if let Err(e) = self.heights.set(LineIndex(line), height) {
            self.set_warning(e.to_string(), false);
            return false;
        }
        if line < top {
            self.scroll += f64::from(height.0) - f64::from(old.0);
        }
        let before = self.scroll;
        self.clamp_scroll();
        if before != self.scroll {
            ctx.invalidate(self.bounds);
        } else if line >= top {
            self.repaint_line_down(line, ctx);
        }
        true
    }
    fn note_line_height(&mut self, line: usize, height: Dip, ctx: &mut ViewContext) {
        if !self.set_line_height(line, height, ctx) {
            self.repaint_line(line, ctx);
        }
    }
    /// Takes one segment's measure into its map and the document's map. See the layout
    /// crate's docs, steps 2 and 5.
    fn note_segment(
        &mut self,
        line: usize,
        segment: usize,
        extent: Dip,
        height: Dip,
        ctx: &mut ViewContext,
    ) {
        let line_top = self.heights.line_to_scroll(LineIndex(line));
        let wrap = self.wrap;
        let along = if wrap {
            (self.scroll - line_top).max(0.0)
        } else {
            self.scroll_x
        };
        let Some(map) = self.long_maps.get_mut(&line) else {
            self.repaint_line(line, ctx);
            return;
        };
        let top_line = self.heights.scroll_to_line(self.scroll).0;
        let first = map.scroll_to_line(along);
        let before = map.line_to_scroll(first);
        if let Err(e) = map.set(LineIndex(segment), extent) {
            self.set_warning(e.to_string(), false);
            return;
        }
        let after = map.line_to_scroll(first);
        let total = map.total_height();
        let new_height = self.engine.long_line_height(map);
        if wrap {
            // With the whole line above the view, `set_line_height` makes this correction.
            if line == top_line && segment < first.0 {
                self.scroll += after - before;
            }
            self.note_line_height(line, new_height, ctx);
        } else {
            self.widest = self.widest.max(total);
            let held = self
                .heights
                .line_height(LineIndex(line))
                .unwrap_or(Dip(0.0));
            if height > held {
                self.note_line_height(line, height, ctx);
            } else {
                self.repaint_line(line, ctx);
            }
        }
    }
    fn repaint_doc_y(&self, top: f64, bottom: f64, ctx: &mut ViewContext) {
        let view_top = f64::from(self.bounds.y.0);
        let view_bottom = view_top + f64::from(self.bounds.height.0);
        let margin = f64::from(self.ink_margin);
        let from = (top - self.scroll + view_top - margin).max(view_top);
        let to = (bottom - self.scroll + view_top + margin).min(view_bottom);
        if to > from {
            ctx.invalidate(Rect {
                x: self.bounds.x,
                y: Dip(from as f32),
                width: self.bounds.width,
                height: Dip((to - from) as f32),
            });
        }
    }
    fn repaint_line(&self, line: usize, ctx: &mut ViewContext) {
        let top = self.heights.line_to_scroll(LineIndex(line));
        let height = self
            .heights
            .line_height(LineIndex(line))
            .unwrap_or(Dip(0.0));
        self.repaint_doc_y(top, top + f64::from(height.0), ctx);
    }
    /// A line and everything below it to the bottom of the view.
    fn repaint_line_down(&self, line: usize, ctx: &mut ViewContext) {
        let top = self.heights.line_to_scroll(LineIndex(line));
        let bottom = self.scroll + f64::from(self.bounds.height.0);
        self.repaint_doc_y(top, bottom, ctx);
    }
    /// The caret's own rectangle, or its row when layout has not arrived.
    fn repaint_caret(&self, byte: ByteOffset, affinity: Affinity, ctx: &mut ViewContext) {
        let Some(place) = self.place(byte, affinity) else {
            // The store cannot say where it is yet, so repaint all that it could touch.
            ctx.invalidate(self.bounds);
            return;
        };
        if !place.exact {
            // Its old layout may paint it on any row of the line, so repaint all of them.
            match self.line_at(byte) {
                Some(line) => self.repaint_line(line, ctx),
                None => self.repaint_doc_y(place.y, place.y + f64::from(place.height), ctx),
            }
            return;
        }
        let x = place.x - self.scroll_x + f64::from(self.bounds.x.0) - 2.0;
        let y = place.y - self.scroll + f64::from(self.bounds.y.0);
        let left = x.max(f64::from(self.bounds.x.0));
        let right = (x + 5.0).min(f64::from(self.bounds.x.0 + self.bounds.width.0));
        let top = y.max(f64::from(self.bounds.y.0));
        let bottom =
            (y + f64::from(place.height)).min(f64::from(self.bounds.y.0 + self.bounds.height.0));
        if right > left && bottom > top {
            ctx.invalidate(Rect {
                x: Dip(left as f32),
                y: Dip(top as f32),
                width: Dip((right - left) as f32),
                height: Dip((bottom - top) as f32),
            });
        }
    }
    /// The rows from one byte's row to another's.
    fn repaint_span(&self, from: ByteOffset, to: ByteOffset, ctx: &mut ViewContext) {
        let (Some(a), Some(b)) = (
            self.place(from, Affinity::After),
            self.place(to, Affinity::Before),
        ) else {
            ctx.invalidate(self.bounds);
            return;
        };
        let top = a.y.min(b.y);
        let bottom = (a.y + f64::from(a.height)).max(b.y + f64::from(b.height));
        // A selected row reaches the right edge, so repaint whole rows.
        self.repaint_doc_y(top, bottom, ctx);
    }
    /// Every row whose selection changed.
    fn repaint_selection_change(
        &self,
        old: Range<ByteOffset>,
        new: Range<ByteOffset>,
        ctx: &mut ViewContext,
    ) {
        let disjoint = old.end < new.start || new.end < old.start;
        if disjoint {
            if !old.is_empty() {
                self.repaint_span(old.start, old.end, ctx);
            }
            if !new.is_empty() {
                self.repaint_span(new.start, new.end, ctx);
            }
        } else {
            if old.start != new.start {
                self.repaint_span(old.start.min(new.start), old.start.max(new.start), ctx);
            }
            if old.end != new.end {
                self.repaint_span(old.end.min(new.end), old.end.max(new.end), ctx);
            }
        }
    }
    fn send_layout_task(
        &mut self,
        line: usize,
        segment: Option<usize>,
        ticket: CacheTicket,
        snapshot: Arc<dyn TextSnapshot>,
    ) {
        let id = self.next_task;
        self.next_task += 1;
        let task = WorkerTask::Layout(id, ticket, snapshot, self.engine.clone());
        match self.tasks.send(task) {
            Ok(()) => {
                self.layout_tasks.insert(id, (Some(line), segment));
                *self.inflight.entry((line, segment)).or_insert(0) += 1;
                self.pending = true;
            }
            Err(e) => self.set_warning(format!("The layout workers stopped: {e}"), true),
        }
    }
    /// Lays out a line on this thread when that costs no system call.
    fn sync_line(
        &mut self,
        line: usize,
        budget: &mut usize,
        ctx: &mut ViewContext,
    ) -> Option<Arc<LineLayout>> {
        if !self.fast_font {
            return None;
        }
        let range = self.source().try_line_range(LineIndex(line)).ok()?;
        let bytes = range.end.0 - range.start.0;
        if bytes > SYNC_LINE_BYTES || bytes > *budget {
            return None;
        }
        let ending = self.source().try_line_ending_len(LineIndex(line)).ok()?;
        let wrap = self.wrap;
        let mut plain = true;
        self.source()
            .try_visit_chunks(range.start..ByteOffset(range.end.0 - ending), &mut |s| {
                plain = plain && lays_out_with_no_system_call(s.as_bytes(), wrap);
            })
            .ok()?;
        if !plain {
            return None;
        }
        let snapshot = self.snapshot();
        let layout = self
            .cache
            .borrow_mut()
            .get_or_layout(
                &self.engine,
                snapshot.as_ref(),
                LineIndex(line),
                self.width(),
            )
            .ok()?;
        *budget -= bytes;
        self.note_ink(&layout);
        self.ghosts.remove(&(line, WHOLE));
        self.widest = self.widest.max(self.widest_of(&layout));
        self.note_line_height(line, layout.height, ctx);
        Some(layout)
    }
    fn whole_layout(&mut self, line: usize, ctx: &mut ViewContext) -> Option<Arc<LineLayout>> {
        let snapshot = self.snapshot();
        let cached = self.cache.borrow_mut().cached(
            &self.engine,
            snapshot.as_ref(),
            LineIndex(line),
            self.width(),
        );
        cached.or_else(|| {
            let mut budget = SYNC_LINE_BYTES;
            self.sync_line(line, &mut budget, ctx)
        })
    }
    /// A long line's segment, from the cache, or laid out here when it is plain ASCII.
    fn segment_layout(
        &mut self,
        line: usize,
        segment: usize,
        ctx: &mut ViewContext,
    ) -> Option<Arc<LineLayout>> {
        let snapshot = self.snapshot();
        let cached = self.cache.borrow_mut().cached_segment(
            &self.engine,
            snapshot.as_ref(),
            LineIndex(line),
            segment,
            self.width(),
        );
        if cached.is_some() {
            return cached;
        }
        let mut budget = SYNC_LINE_BYTES;
        self.sync_segment(line, segment, &mut budget, ctx)
    }
    fn sync_segment(
        &mut self,
        line: usize,
        segment: usize,
        budget: &mut usize,
        ctx: &mut ViewContext,
    ) -> Option<Arc<LineLayout>> {
        if !self.fast_font {
            return None;
        }
        let snapshot = self.snapshot();
        let range = self
            .engine
            .segment_range(snapshot.as_ref(), LineIndex(line), segment)
            .ok()?;
        let bytes = range.end.0 - range.start.0;
        if bytes > *budget {
            return None;
        }
        let wrap = self.wrap;
        let mut plain = true;
        snapshot
            .try_visit_chunks(range, &mut |s| {
                plain = plain && lays_out_with_no_system_call(s.as_bytes(), wrap)
            })
            .ok()?;
        if !plain {
            return None;
        }
        let layout = self
            .cache
            .borrow_mut()
            .get_or_layout_segment(
                &self.engine,
                snapshot.as_ref(),
                LineIndex(line),
                segment,
                self.width(),
            )
            .ok()?;
        *budget -= bytes;
        self.note_ink(&layout);
        self.ghosts.remove(&(line, segment));
        let extent = self.engine.segment_extent(&layout);
        self.note_segment(line, segment, extent, layout.height, ctx);
        Some(layout)
    }
    fn schedule(&mut self, ctx: &mut ViewContext) {
        self.schedule_lines(false, ctx);
    }
    /// With `visible_only`, a key's work stays small: lines off screen wait for the next
    /// poll, which the pending flag asks for.
    fn schedule_lines(&mut self, visible_only: bool, ctx: &mut ViewContext) {
        self.pending = false;
        let status = self.source().status();
        if let Some(failure) = &status.failure {
            self.set_warning(failure.message.clone(), true);
        } else if self.mapped.is_some() && !status.complete {
            // The store wakes the view through its notify, and polling covers a store
            // opened without one. This work ends when the index does.
            self.pending = true;
        }
        let known_lines = status.known_lines;
        if known_lines > self.heights.len()
            && let Err(e) = self.heights.insert_lines(
                LineIndex(self.heights.len()),
                known_lines - self.heights.len(),
            )
        {
            self.set_warning(e.to_string(), false);
        }
        let worker_snapshot: Arc<dyn TextSnapshot> = match &self.mapped {
            Some(mapped) => Arc::new(WorkerMapped(mapped.snapshot())),
            None => self.store.snapshot(),
        };
        let snapshot = self.snapshot();
        if self.mapped.is_some()
            && let Ok(known) = snapshot.try_known_line(LineIndex(0))
            && !known.complete
            && known.range.end.0 > 0
        {
            if self.known_partial.is_none() && self.known_inflight.is_none() {
                let end = known.range.end.0.min(qe_layout::SEGMENT_BYTES);
                if self
                    .tasks
                    .send(WorkerTask::KnownFirst(
                        end,
                        self.width(),
                        worker_snapshot.clone(),
                        self.engine.clone(),
                        self.layout_generation,
                    ))
                    .is_ok()
                {
                    self.known_inflight = Some((snapshot.revision(), self.layout_generation));
                }
            }
            if self.known_inflight.is_some() {
                self.pending = true;
            }
        }
        let view = f64::from(self.bounds.height.0);
        let visible_first = self.heights.scroll_to_line(self.scroll).0;
        let visible_end = (self.heights.scroll_to_line(self.scroll + view).0 + 1)
            .min(known_lines)
            .min(visible_first + MAX_WINDOW_LINES);
        let first = self.heights.scroll_to_line((self.scroll - view).max(0.0)).0;
        let end = (self.heights.scroll_to_line(self.scroll + view * 2.0).0 + 1)
            .min(known_lines)
            .min(first + MAX_WINDOW_LINES);
        self.shared.wanted_first.store(first, Ordering::Release);
        self.shared.wanted_last.store(end, Ordering::Release);
        self.ghosts
            .retain(|(line, _), _| *line >= first && *line < end);
        let mut budget = SYNC_BUDGET_BYTES;
        for i in visible_first..visible_end {
            self.schedule_line(i, true, &snapshot, &worker_snapshot, &mut budget, ctx);
        }
        // A waiting key needs its line laid out, wherever the view went meanwhile.
        if (!self.deferred.is_empty() || self.reveal_pending)
            && let Some(line) = self.caret_line
            && !(visible_first..visible_end).contains(&line)
        {
            self.schedule_line(line, true, &snapshot, &worker_snapshot, &mut budget, ctx);
        }
        if visible_only {
            self.pending = true;
        } else {
            for i in (visible_end..end).chain(first..visible_first) {
                self.schedule_line(i, false, &snapshot, &worker_snapshot, &mut budget, ctx);
            }
        }
        // Results for changed lines are dropped when they arrive, so they must arrive.
        if !self.layout_tasks.is_empty()
            || !self.map_tasks.is_empty()
            || self.retry_key.is_some()
            || self.caret_line.is_none()
            || !self.deferred.is_empty()
            || (self.pending_anchor.is_some() && self.bounds.height.0 > 0.0)
            || (self.reveal_pending
                && self.bounds.height.0 > 0.0
                && !self.caret_line_failed()
                && status.failure.is_none())
        {
            self.pending = true;
        }
    }
    fn schedule_line(
        &mut self,
        i: usize,
        visible: bool,
        snapshot: &Arc<dyn TextSnapshot>,
        worker_snapshot: &Arc<dyn TextSnapshot>,
        budget: &mut usize,
        ctx: &mut ViewContext,
    ) {
        let line = LineIndex(i);
        if self.long_maps.contains_key(&i) {
            self.schedule_segments(i, visible, snapshot, worker_snapshot, budget, ctx);
            return;
        }
        if self
            .cache
            .borrow_mut()
            .cached(&self.engine, snapshot.as_ref(), line, self.width())
            .is_some()
        {
            return;
        }
        if self.failed.contains(&(i, None)) {
            return;
        }
        let long = match self.engine.is_long_line(snapshot.as_ref(), line) {
            Ok(long) => long,
            // The text is not in memory yet. The store's notify brings the view back.
            Err(_) => return,
        };
        if long {
            let wanted = !self.failed.contains(&(i, Some(WHOLE)));
            if wanted && !self.map_tasks.values().any(|l| *l == Some(i)) {
                let id = self.next_task;
                self.next_task += 1;
                if self
                    .tasks
                    .send(WorkerTask::LongMap(
                        id,
                        line,
                        self.width(),
                        worker_snapshot.clone(),
                        self.engine.clone(),
                        self.layout_generation,
                    ))
                    .is_ok()
                {
                    self.map_tasks.insert(id, Some(i));
                }
            }
            if wanted {
                self.pending = true;
            }
            self.schedule_segment(i, 0, visible, snapshot, worker_snapshot, budget, ctx);
            return;
        }
        if visible && self.sync_line(i, budget, ctx).is_some() {
            return;
        }
        if self.inflight.contains_key(&(i, None)) {
            self.pending = true;
            return;
        }
        if snapshot.try_line_range(line).is_err() {
            return;
        }
        let ticket =
            self.cache
                .borrow_mut()
                .ticket(&self.engine, snapshot.as_ref(), line, self.width());
        match ticket {
            Ok(ticket) => self.send_layout_task(i, None, ticket, worker_snapshot.clone()),
            Err(e) => self.set_warning(e.to_string(), false),
        }
    }
    fn schedule_segments(
        &mut self,
        i: usize,
        visible: bool,
        snapshot: &Arc<dyn TextSnapshot>,
        worker_snapshot: &Arc<dyn TextSnapshot>,
        budget: &mut usize,
        ctx: &mut ViewContext,
    ) {
        let (start, end) = {
            let Some(map) = self.long_maps.get(&i) else {
                return;
            };
            let line_top = self.heights.line_to_scroll(LineIndex(i));
            let along = if self.wrap {
                (self.scroll - line_top).max(0.0)
            } else {
                self.scroll_x
            };
            let start = map.scroll_to_line(along).0.saturating_sub(1);
            let limit = along
                + f64::from(if self.wrap {
                    self.bounds.height.0
                } else {
                    self.bounds.width.0
                }) * 2.0;
            let mut end = start;
            while end < map.len()
                && end < start + 128
                && map.line_to_scroll(LineIndex(end)) <= limit
            {
                end += 1;
            }
            (start, end.max(start + 1).min(map.len()))
        };
        // Only segments near the view get laid out again, so older ghosts would stay.
        self.ghosts
            .retain(|(line, segment), _| *line != i || (*segment >= start && *segment < end));
        for segment in start..end {
            self.schedule_segment(i, segment, visible, snapshot, worker_snapshot, budget, ctx);
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn schedule_segment(
        &mut self,
        i: usize,
        segment: usize,
        visible: bool,
        snapshot: &Arc<dyn TextSnapshot>,
        worker_snapshot: &Arc<dyn TextSnapshot>,
        budget: &mut usize,
        ctx: &mut ViewContext,
    ) {
        let line = LineIndex(i);
        if self
            .cache
            .borrow_mut()
            .cached_segment(&self.engine, snapshot.as_ref(), line, segment, self.width())
            .is_some()
            || self.failed.contains(&(i, Some(segment)))
        {
            return;
        }
        if visible
            && self.long_maps.contains_key(&i)
            && self.sync_segment(i, segment, budget, ctx).is_some()
        {
            return;
        }
        if self.inflight.contains_key(&(i, Some(segment))) {
            self.pending = true;
            return;
        }
        if snapshot.try_line_range(line).is_err() {
            return;
        }
        let ticket = self.cache.borrow_mut().segment_ticket(
            &self.engine,
            snapshot.as_ref(),
            line,
            segment,
            self.width(),
        );
        match ticket {
            Ok(ticket) => self.send_layout_task(i, Some(segment), ticket, worker_snapshot.clone()),
            Err(e) => self.set_warning(e.to_string(), false),
        }
    }
    /// Where a byte is. It asks about the long-line map first. With no layout yet, the
    /// answer is an estimate with `exact` false.
    fn place(&self, byte: ByteOffset, affinity: Affinity) -> Option<Place> {
        let line = LineIndex(self.line_at(byte)?);
        let top = self.heights.line_to_scroll(line);
        let snapshot = self.snapshot();
        if let Some(map) = self.long_maps.get(&line.0) {
            let (_, segment, range) = self
                .engine
                .segment_at(snapshot.as_ref(), byte, affinity)
                .ok()?;
            let start = map.line_to_scroll(LineIndex(segment));
            let layout = self.cache.borrow_mut().cached_segment(
                &self.engine,
                snapshot.as_ref(),
                line,
                segment,
                self.width(),
            );
            if let Some(rect) = layout.and_then(|l| l.caret_rect(byte, affinity)) {
                return Some(if self.wrap {
                    Place {
                        x: f64::from(rect.x.0),
                        y: top + start + f64::from(rect.y.0),
                        height: rect.height.0,
                        exact: true,
                        x_known: true,
                    }
                } else {
                    Place {
                        x: start + f64::from(rect.x.0),
                        y: top + f64::from(rect.y.0),
                        height: rect.height.0,
                        exact: true,
                        x_known: true,
                    }
                });
            }
            let end = map.line_to_scroll(LineIndex(segment + 1));
            let part = (byte.0.saturating_sub(range.start.0)) as f64
                / (range.end.0 - range.start.0).max(1) as f64;
            let along = (end - start) * part;
            return Some(if self.wrap {
                let row = f64::from(self.row_height);
                Place {
                    x: 0.0,
                    y: top + start + (along / row).floor() * row,
                    height: self.row_height,
                    exact: false,
                    x_known: true,
                }
            } else {
                Place {
                    x: start + along,
                    y: top,
                    height: self.row_height,
                    exact: false,
                    x_known: true,
                }
            });
        }
        let layout =
            self.cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), line, self.width());
        if let Some(rect) = layout.and_then(|l| l.caret_rect(byte, affinity)) {
            return Some(Place {
                x: f64::from(rect.x.0),
                y: top + f64::from(rect.y.0),
                height: rect.height.0,
                exact: true,
                x_known: true,
            });
        }
        Some(Place {
            x: 0.0,
            y: top,
            height: self.row_height,
            exact: false,
            x_known: false,
        })
    }
    /// Scrolls just enough to show the whole caret. An estimate is corrected when the
    /// line's layout arrives, unless the user scrolls first.
    fn reveal_caret(&mut self, ctx: &mut ViewContext) {
        // With no height no row fits. This waits for the size to come back.
        if self.bounds.height.0 <= 0.0 {
            self.reveal_pending = true;
            return;
        }
        let Some(place) = self.place(self.caret, self.affinity) else {
            self.reveal_pending = true;
            return;
        };
        let (old_y, old_x) = (self.scroll, self.scroll_x);
        let view = f64::from(self.bounds.height.0);
        let top = place.y;
        let bottom = place.y + f64::from(place.height);
        if top < self.scroll || bottom - top > view {
            self.scroll = top;
        } else if bottom > self.scroll + view {
            self.scroll = bottom - view;
        }
        if !self.wrap && place.x_known {
            let width = f64::from(self.bounds.width.0);
            self.widest = self.widest.max(place.x);
            if place.x < self.scroll_x {
                self.scroll_x = place.x;
            } else if place.x > self.scroll_x + width - 2.0 {
                self.scroll_x = (place.x - width + 2.0).max(0.0);
            }
        }
        self.scroll = self.scroll.max(0.0);
        self.reveal_pending = !place.exact;
        if self.scroll != old_y || self.scroll_x != old_x {
            ctx.invalidate(self.bounds);
            self.schedule(ctx);
        }
    }
    /// The one way to change the caret and the selection without changing text.
    fn set_selection(
        &mut self,
        anchor: ByteOffset,
        caret: ByteOffset,
        affinity: Affinity,
        reveal: bool,
        ctx: &mut ViewContext,
    ) {
        if caret != self.caret {
            self.history.cursor_moved();
        }
        let old = self.selection();
        let (old_caret, old_affinity) = (self.caret, self.affinity);
        self.repaint_caret(old_caret, old_affinity, ctx);
        self.anchor = anchor;
        self.caret = caret;
        self.affinity = affinity;
        self.goal_x = None;
        self.refresh_caret_line();
        let new = self.selection();
        self.repaint_selection_change(old, new, ctx);
        self.repaint_caret(caret, affinity, ctx);
        if reveal {
            self.reveal_caret(ctx);
        } else {
            self.reveal_pending = false;
        }
    }
    fn with_stops<R>(&mut self, ask: impl FnOnce(&Stops) -> R) -> R {
        let snapshot = self.snapshot();
        let stops = Stops::new(snapshot.as_ref(), &self.engine);
        let answer = ask(&stops);
        let problem = stops.problem.borrow_mut().take();
        if let Some(problem) = problem {
            self.set_warning(problem, false);
        }
        answer
    }
    /// What the screen should keep showing until the new layout arrives. `old` is the
    /// text from before the change, which the cache still holds layouts of.
    fn keep_ghosts(&mut self, old: &dyn TextSnapshot, change: &TextChange, lines: Range<usize>) {
        let width = self.width();
        for line in lines {
            if self.long_maps.contains_key(&line) {
                let segment = self
                    .engine
                    .segment_at(old, change.replaced.start, Affinity::After)
                    .map_or(0, |(_, segment, _)| segment);
                for near in segment.saturating_sub(1)..segment + 8 {
                    let layout = self.cache.borrow_mut().cached_segment(
                        &self.engine,
                        old,
                        LineIndex(line),
                        near,
                        width,
                    );
                    if let Some(layout) = layout {
                        self.ghosts.insert((line, near), layout);
                    }
                }
                continue;
            }
            let layout = self
                .cache
                .borrow_mut()
                .cached(&self.engine, old, LineIndex(line), width);
            if let Some(layout) = layout {
                self.ghosts.insert((line, WHOLE), layout);
            }
        }
    }
    /// Changes the text through the history, then applies the change everywhere.
    fn apply_edit(
        &mut self,
        range: Range<ByteOffset>,
        value: &str,
        typing: bool,
        ctx: &mut ViewContext,
    ) {
        if range.is_empty() && value.is_empty() {
            return;
        }
        let old = self.store.snapshot();
        match self
            .history
            .replace_tracked(&mut self.store, range.clone(), value, typing)
        {
            Ok(tracked) => {
                let caret = ByteOffset(range.start.0 + value.len());
                self.after_change(old, tracked.change, caret, ctx);
            }
            Err(e) => self.set_warning(format!("The edit failed: {e:?}"), false),
        }
    }
    /// The one path every text change takes. `old` is the text from before it, and
    /// `change` is one change of that text, which `merge_changes` makes of a group.
    fn after_change(
        &mut self,
        old: Arc<dyn TextSnapshot>,
        change: TextChange,
        caret: ByteOffset,
        ctx: &mut ViewContext,
    ) {
        // Text moved, so the place the view was heading for is no longer the one to keep.
        self.pending_anchor = None;
        self.keep_caret_visible = false;
        let first_of = old.try_byte_to_position(change.replaced.start);
        let last_of = old.try_byte_to_position(change.replaced.end);
        let (mut first, old_last) = match (first_of, last_of) {
            (Ok(a), Ok(b)) => (a.line.0, b.line.0),
            _ => (0, self.heights.len().saturating_sub(1)),
        };
        // The change ends any selection, and it can reach far from the changed lines.
        if !self.selection().is_empty() {
            ctx.invalidate(self.bounds);
        }
        // The caret leaves its old place, which an undo can put far from the change.
        self.repaint_old_caret(old.as_ref(), first, ctx);
        if !self.wrap && self.edit_touches_widest(old.as_ref(), first, old_last) {
            self.widest_dirty = true;
        }
        let kept_lines = first..(old_last + 1).min(first + MAX_GHOST_LINES);
        self.keep_ghosts(old.as_ref(), &change, kept_lines);
        self.cache.borrow_mut().invalidate(old.as_ref(), &change);
        // Text placed after a CR can join it into CRLF, which changes the line before.
        if first > 0 && change.replaced.start.0 > 0 {
            let mut before = false;
            let visited = old.try_visit_chunks(
                ByteOffset(change.replaced.start.0 - 1)..change.replaced.start,
                &mut |s| before = s == "\r",
            );
            if visited.is_ok() && before {
                first -= 1;
            }
        }
        self.shared.drops_out.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.tasks.send(WorkerTask::Drop(old)) {
            self.shared.drops_out.fetch_sub(1, Ordering::AcqRel);
            self.set_warning(format!("The layout workers stopped: {error}"), true);
        }
        let delta = change.line_count_delta;
        if delta > 0 {
            if let Err(e) = self
                .heights
                .insert_lines(LineIndex(first + 1), delta as usize)
            {
                self.set_warning(e.to_string(), false);
            }
        } else if delta < 0 {
            let removed = delta.unsigned_abs();
            if let Err(e) = self.heights.remove_lines(first + 1..first + 1 + removed) {
                self.set_warning(e.to_string(), false);
            }
        }
        self.renumber_lines(first, old_last, delta);
        // A long line that stays one line keeps its map. Any other change of it makes
        // a new one, and a line that is no longer long has none.
        let one_line = first == old_last && delta == 0;
        if one_line && self.long_maps.contains_key(&first) {
            let long = self
                .engine
                .is_long_line(&self.store, LineIndex(first))
                .unwrap_or(false);
            if long {
                if let Some(map) = self.long_maps.get_mut(&first) {
                    match self
                        .engine
                        .update_long_line_map(map, &self.store, LineIndex(first))
                    {
                        Ok(()) => {
                            let height = self.engine.long_line_height(map);
                            if self.wrap {
                                self.set_line_height(first, height, ctx);
                            }
                        }
                        Err(e) => self.set_warning(e.to_string(), false),
                    }
                }
            } else {
                self.long_maps.remove(&first);
                self.set_line_height(first, Dip(self.row_height), ctx);
            }
        }
        self.failed.clear();
        self.clear_warning_after_edit();
        let limit = self.max_scroll();
        if self.scroll > limit {
            self.scroll = limit;
            ctx.invalidate(self.bounds);
        }
        if delta != 0 || !one_line {
            self.repaint_line_down(first, ctx);
        } else {
            self.repaint_line(first, ctx);
        }
        let caret = self
            .with_stops(|stops| stops.snap(caret.0))
            .map_or(caret, ByteOffset);
        self.caret = caret;
        self.anchor = caret;
        self.affinity = Affinity::After;
        self.goal_x = None;
        self.visible = true;
        self.refresh_caret_line();
        // The caret's line first, so the reveal below knows where the caret is.
        if let Ok(position) = self.source().try_byte_to_position(caret) {
            let line = position.line.0;
            if !self.long_maps.contains_key(&line) {
                let mut budget = SYNC_LINE_BYTES;
                self.sync_line(line, &mut budget, ctx);
            }
        }
        self.settle_widest(ctx);
        self.reveal_caret(ctx);
        self.schedule_lines(true, ctx);
        self.reveal_caret(ctx);
    }
    /// Repaints the caret where it was. The store has already changed, so the old
    /// text answers for its line, and heights have not moved yet.
    fn repaint_old_caret(&self, old: &dyn TextSnapshot, first: usize, ctx: &mut ViewContext) {
        let line = old
            .try_byte_to_position(self.caret)
            .map_or(first, |p| p.line.0);
        self.repaint_line(line, ctx);
    }
    /// True when the text from before an edit had its widest line among the edited ones.
    /// Only then can the limit come down, so typing elsewhere never moves the view.
    fn edit_touches_widest(&self, old: &dyn TextSnapshot, first: usize, last: usize) -> bool {
        if last - first >= MAX_WINDOW_LINES {
            return true;
        }
        (first..=last).any(|line| {
            let width = self
                .long_maps
                .get(&line)
                .map(HeightMap::total_height)
                .or_else(|| {
                    self.cache
                        .borrow_mut()
                        .cached(&self.engine, old, LineIndex(line), self.width())
                        .map(|layout| self.widest_of(&layout))
                });
            width.is_some_and(|w| w + 1.0 >= self.widest)
        })
    }
    /// Brings the limit down after an edit changed the widest line. It waits for the
    /// caret's line, so a layout still on a worker does not count as a narrow line.
    fn settle_widest(&mut self, ctx: &mut ViewContext) {
        if !self.widest_dirty {
            return;
        }
        if self.wrap {
            self.widest_dirty = false;
            return;
        }
        let caret_ready = self
            .place(self.caret, self.affinity)
            .is_none_or(|place| place.exact);
        if !caret_ready {
            return;
        }
        self.widest_dirty = false;
        let before = self.scroll_x;
        self.widest = self.visible_widest();
        self.clamp_scroll();
        if self.scroll_x != before {
            ctx.invalidate(self.bounds);
            self.schedule(ctx);
        }
    }
    /// The widest text now on screen. A line off screen widens this again when it comes
    /// back.
    fn visible_widest(&self) -> f64 {
        let snapshot = self.snapshot();
        let view = f64::from(self.bounds.height.0);
        let first = self.heights.scroll_to_line(self.scroll).0;
        let end = (self.heights.scroll_to_line(self.scroll + view).0 + 1)
            .min(self.source().status().known_lines)
            .min(first + MAX_WINDOW_LINES);
        let mut widest: f64 = 0.0;
        for line in first..end {
            if let Some(map) = self.long_maps.get(&line) {
                widest = widest.max(map.total_height());
            } else if let Some(layout) = self.cache.borrow_mut().cached(
                &self.engine,
                snapshot.as_ref(),
                LineIndex(line),
                self.width(),
            ) {
                widest = widest.max(self.widest_of(&layout));
            }
        }
        widest
    }
    /// Moves everything kept by line number to the lines' new numbers. Lines `first`
    /// to `old_last` changed, and lines below moved by `delta`.
    fn renumber_lines(&mut self, first: usize, old_last: usize, delta: isize) {
        let moved = |line: usize| -> Option<usize> {
            if line < first {
                Some(line)
            } else if line > old_last {
                line.checked_add_signed(delta)
            } else {
                None
            }
        };
        if delta != 0 || first != old_last {
            let maps = std::mem::take(&mut self.long_maps);
            self.long_maps = maps
                .into_iter()
                .filter_map(|(line, map)| moved(line).map(|at| (at, map)))
                .collect();
            let ghosts = std::mem::take(&mut self.ghosts);
            self.ghosts = ghosts
                .into_iter()
                .filter_map(|((line, segment), layout)| {
                    if line == first || (delta == 0 && line >= first && line <= old_last) {
                        Some(((line, segment), layout))
                    } else {
                        moved(line).map(|at| ((at, segment), layout))
                    }
                })
                .collect();
            self.inflight.clear();
            for (line, segment) in self.layout_tasks.values_mut() {
                *line = line.and_then(moved);
                if let Some(line) = line {
                    *self.inflight.entry((*line, *segment)).or_insert(0) += 1;
                }
            }
        }
        // A map measures its line as it stood, so a change to the line ends its task.
        for line in self.map_tasks.values_mut() {
            *line = line.and_then(moved);
        }
    }
    /// True when a thread may read this much, and says the limit in words otherwise.
    fn copy_size_is_allowed(&mut self, bytes: usize) -> bool {
        if bytes > self.copy_limit {
            self.set_warning(
                format!(
                    "This selection is {} MiB. QuickEdit can copy at most {} MiB at a time.",
                    bytes >> 20,
                    self.copy_limit >> 20
                ),
                false,
            );
            return false;
        }
        true
    }
    /// The newest clipboard action wins, so any write ends the copy in progress. Its
    /// thread finishes its read and finds nobody listening. Its text may be big, so a
    /// worker frees what it hands over.
    fn cancel_copy(&mut self) {
        if let Some(job) = self.copy_job.take() {
            job.cancel.store(true, Ordering::Release);
            let results: Box<dyn std::any::Any + Send> = Box::new(job.results);
            if self.tasks.send(WorkerTask::DropDocument(results)).is_err() {
                self.set_warning("The layout workers stopped.".to_string(), true);
            }
        }
    }
    fn start_copy(
        &mut self,
        range: Range<ByteOffset>,
        snapshot: Arc<dyn TextSnapshot>,
        is_cut: bool,
    ) {
        self.cancel_copy();
        let bytes = range.end.0 - range.start.0;
        let (sender, results) = mpsc::channel();
        let shared = self.shared.clone();
        let preparer = self.clipboard.preparer();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let spawned = thread::Builder::new()
            .name("qe-editor-copy".into())
            .spawn(move || {
                let answer = reserve_text(bytes).and_then(|mut text| {
                    let read = snapshot.visit_chunks_while(range, &mut |part| {
                        let go_on = !cancelled.load(Ordering::Acquire);
                        if go_on {
                            text.push_str(part);
                        }
                        go_on
                    });
                    // A replaced copy has no reader. Its text is freed on this thread.
                    if cancelled.load(Ordering::Acquire) {
                        return Err("The copy was replaced by a newer one.".to_string());
                    }
                    if !read {
                        return Err("The selection could not be read from the file.".to_string());
                    }
                    match &preparer {
                        Some(prepare) => prepare(&text, &cancelled).map(CopyOutput::Prepared),
                        None => Ok(CopyOutput::Text(text)),
                    }
                });
                // A failed send means the view is gone, so the text has no reader.
                if sender.send(answer).is_ok() {
                    shared.wake_host();
                }
            });
        match spawned {
            Ok(_) => {
                self.copy_job = Some(CopyJob {
                    cancel,
                    results,
                    is_cut,
                })
            }
            Err(e) => self.set_warning(format!("Copy could not start: {e}"), false),
        }
    }
    fn poll_copy(&mut self, ctx: &mut ViewContext) {
        let Some(job) = &self.copy_job else {
            return;
        };
        let is_cut = job.is_cut;
        let answer = match job.results.try_recv() {
            Ok(answer) => answer,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("The copy thread stopped before it finished.".to_string())
            }
        };
        self.copy_job = None;
        let written = answer.and_then(|output| match output {
            CopyOutput::Text(text) => self.clipboard.write(&text),
            CopyOutput::Prepared(prepared) => self.clipboard.write_prepared(prepared),
        });
        match written {
            Ok(()) => {
                if std::mem::take(&mut self.pending_paste) {
                    self.run_inner(EditorCommand::Paste, ctx);
                }
            }
            Err(e) => {
                self.pending_paste = false;
                let text = if is_cut {
                    format!(
                        "Cut removed the text, but could not put it on the clipboard: {e}. \
                         Undo brings it back."
                    )
                } else {
                    e
                };
                self.set_warning(text, false);
            }
        }
    }
    /// Runs one command. Read-only states refuse every edit here, before anything
    /// else changes. The Mac's source has no message for a refused edit, so neither
    /// has this: the command counts in `refused_edits` and changes nothing.
    pub fn run(&mut self, command: EditorCommand, ctx: &mut ViewContext) {
        if !self.deferred.is_empty() || self.command_must_wait(command) {
            self.hold(Queued::Command(command));
        } else {
            self.touch();
            self.run_inner(command, ctx);
        }
        self.finish(Instant::now(), ctx);
    }
    /// Keeps input for later, behind what already waits.
    fn hold(&mut self, input: Queued) {
        if self.deferred.is_empty() {
            self.deferred_since = Instant::now();
            self.deferred_first = self.deferred_since;
        }
        self.deferred.push_back(input);
        self.show_caret();
        self.pending = true;
    }
    /// Select All needs the file's end, and only the index knows it.
    fn command_must_wait(&self, command: EditorCommand) -> bool {
        command == EditorCommand::SelectAll && self.index_is_running()
    }
    fn index_is_running(&self) -> bool {
        let status = self.source().status();
        self.mapped.is_some() && !status.complete && status.failure.is_none()
    }
    fn run_inner(&mut self, command: EditorCommand, ctx: &mut ViewContext) {
        if command.edits() && !self.can_edit() {
            self.refused_edits += 1;
            return;
        }
        match command {
            EditorCommand::SelectAll => {
                let end = self.known_end();
                self.set_selection(ByteOffset(0), end, Affinity::After, false, ctx);
            }
            EditorCommand::Copy => self.copy(),
            EditorCommand::Cut => {
                let range = self.selection();
                if range.is_empty() {
                    return;
                }
                let bytes = range.end.0 - range.start.0;
                if bytes > COPY_ON_UI_BYTES {
                    // The edit happens now. A snapshot keeps the text for the thread.
                    if self.copy_size_is_allowed(bytes) {
                        let snapshot = self.store.snapshot();
                        self.apply_edit(range.clone(), "", false, ctx);
                        self.start_copy(range, snapshot, true);
                    }
                    return;
                }
                let mut text = String::new();
                if !self
                    .store
                    .visit_chunks(range.clone(), &mut |s| text.push_str(s))
                {
                    self.set_warning("Cut could not read the selection.".into(), false);
                    return;
                }
                self.cancel_copy();
                match self.clipboard.write(&text) {
                    Ok(()) => self.apply_edit(range, "", false, ctx),
                    Err(e) => self.set_warning(e, false),
                }
            }
            EditorCommand::Paste if self.copy_job.is_some() => {
                // The user means what they just copied, which is still on its way.
                self.pending_paste = true;
            }
            EditorCommand::Paste => match self.clipboard.read() {
                Ok(Some(text)) if !text.is_empty() => {
                    let range = self.selection();
                    self.apply_edit(range, &text, false, ctx);
                }
                Ok(_) => {}
                Err(e) => self.set_warning(e, false),
            },
            EditorCommand::Undo => self.undo_or_redo(true, ctx),
            EditorCommand::Redo => self.undo_or_redo(false, ctx),
            EditorCommand::DeleteBackward => {
                let range = self.selection();
                if !range.is_empty() {
                    self.apply_edit(range, "", true, ctx);
                    return;
                }
                let at = self.caret.0;
                if let Ok(Some(stop)) = self.with_stops(|s| s.before(at)) {
                    self.apply_edit(ByteOffset(stop)..self.caret, "", true, ctx);
                }
            }
            EditorCommand::DeleteForward => {
                let range = self.selection();
                if !range.is_empty() {
                    self.apply_edit(range, "", false, ctx);
                    return;
                }
                let at = self.caret.0;
                if let Ok(Some(stop)) = self.with_stops(|s| s.after(at)) {
                    self.apply_edit(self.caret..ByteOffset(stop), "", false, ctx);
                }
            }
            EditorCommand::DeleteWordBackward => {
                let range = self.selection();
                if !range.is_empty() {
                    self.apply_edit(range, "", false, ctx);
                    return;
                }
                let at = self.caret.0;
                if let Ok(stop) = self.with_stops(|s| s.word_start_before(at)) {
                    self.apply_edit(ByteOffset(stop)..self.caret, "", false, ctx);
                }
            }
            EditorCommand::DeleteWordForward => {
                let range = self.selection();
                if !range.is_empty() {
                    self.apply_edit(range, "", false, ctx);
                    return;
                }
                let at = self.caret.0;
                if let Ok(stop) = self.with_stops(|s| s.word_end_after(at)) {
                    self.apply_edit(self.caret..ByteOffset(stop), "", false, ctx);
                }
            }
            EditorCommand::InsertLineEnding => {
                let range = self.selection();
                self.apply_edit(range, self.preferred_ending, true, ctx);
            }
        }
    }
    fn copy_snapshot(&self) -> Arc<dyn TextSnapshot> {
        self.mapped
            .as_ref()
            .map_or_else(|| self.store.snapshot(), TextStore::snapshot)
    }
    fn copy(&mut self) {
        let range = self.selection();
        if range.is_empty() {
            return;
        }
        let bytes = range.end.0 - range.start.0;
        if bytes > COPY_ON_UI_BYTES {
            if self.copy_size_is_allowed(bytes) {
                let snapshot = self.copy_snapshot();
                self.start_copy(range, snapshot, false);
            }
            return;
        }
        let mut text = String::new();
        match self
            .source()
            .try_visit_chunks(range.clone(), &mut |s| text.push_str(s))
        {
            Ok(()) => {
                self.cancel_copy();
                if let Err(e) = self.clipboard.write(&text) {
                    self.set_warning(e, false);
                }
            }
            // The text is not in memory yet, so a thread reads it.
            Err(LookupError::NotYetKnown { .. } | LookupError::TooLong { .. }) => {
                let snapshot = self.copy_snapshot();
                self.start_copy(range, snapshot, false);
            }
            Err(e) => self.set_warning(format!("Copy failed: {e:?}"), false),
        }
    }
    fn undo_or_redo(&mut self, undo: bool, ctx: &mut ViewContext) {
        let old = self.store.snapshot();
        let answer = if undo {
            self.history.undo_tracked(&mut self.store)
        } else {
            self.history.redo_tracked(&mut self.store)
        };
        match answer {
            Ok(tracked) => {
                let changes: Vec<TextChange> = tracked.into_iter().map(|t| t.change).collect();
                let Some(last) = changes.last() else {
                    return;
                };
                let caret = ByteOffset(last.replaced.start.0 + last.new_len);
                let Some(merged) = merge_changes(&changes) else {
                    return;
                };
                self.after_change(old, merged, caret, ctx);
            }
            Err(e) => self.set_warning(format!("Undo failed: {e:?}"), false),
        }
    }
    /// The end of the known text. An index that is still running can stop inside a
    /// character, and a caret never may.
    fn known_end(&mut self) -> ByteOffset {
        let known = self.source().status().known_bytes;
        self.with_stops(|s| s.edge_backward(known.0, 0))
            .map_or(known, ByteOffset)
    }
    fn cols(&self) -> usize {
        if self.wrap {
            ((self.bounds.width.0 / self.cell_width).floor() as usize).max(1)
        } else {
            usize::MAX / 4
        }
    }
    /// An estimate inside a range of one line, when layout has not arrived: the range
    /// wraps every `cols` characters, and `x` becomes a character count.
    fn estimate_in(
        &mut self,
        range: Range<usize>,
        x: f64,
        row: usize,
    ) -> Option<(ByteOffset, Affinity)> {
        let cols = self.cols();
        let column = ((x / f64::from(self.cell_width)).round().max(0.0) as usize).min(cols);
        let want = row.saturating_mul(cols).saturating_add(column);
        let byte = self.with_stops(|stops| -> Result<usize, LookupError> {
            let line = stops.line_of(range.start)?;
            let limit = range.end.min(range.start + 16 * 1024).min(line.content_end);
            let mut text = String::new();
            let mut to = limit;
            loop {
                match stops
                    .text
                    .try_visit_chunks(ByteOffset(range.start)..ByteOffset(to), &mut |s| {
                        text.push_str(s)
                    }) {
                    Ok(()) => break,
                    Err(LookupError::OutOfRange) if to > range.start => {
                        to -= 1;
                        text.clear();
                    }
                    Err(e) => return Err(e),
                }
            }
            let local = text.char_indices().nth(want).map_or(text.len(), |(i, _)| i);
            stops.snap(range.start + local)
        });
        byte.ok().map(|b| (ByteOffset(b), Affinity::After))
    }
    /// The byte at a point in document coordinates.
    fn resolve(&mut self, x: f64, y: f64, ctx: &mut ViewContext) -> Option<(ByteOffset, Affinity)> {
        if y >= self.heights.total_height() {
            return Some((self.known_end(), Affinity::After));
        }
        let y = y.max(0.0);
        let line = self.heights.scroll_to_line(y).0;
        let line_top = self.heights.line_to_scroll(LineIndex(line));
        let local = y - line_top;
        let row_height = f64::from(self.row_height);
        if let Some(map) = self.long_maps.get(&line) {
            let along = if self.wrap { local } else { x };
            let segment = map.scroll_to_line(along).0;
            let start = map.line_to_scroll(LineIndex(segment));
            return if let Some(layout) = self.segment_layout(line, segment, ctx) {
                let point = if self.wrap {
                    Point {
                        x: Dip(x as f32),
                        y: Dip((along - start) as f32),
                    }
                } else {
                    Point {
                        x: Dip((x - start) as f32),
                        y: Dip(local as f32),
                    }
                };
                let hit = layout.hit_test_point(point);
                Some((hit.byte, hit.affinity))
            } else {
                let snapshot = self.snapshot();
                let range = self
                    .engine
                    .segment_range(snapshot.as_ref(), LineIndex(line), segment)
                    .ok()?;
                let row = if self.wrap {
                    ((along - start) / row_height).floor().max(0.0) as usize
                } else {
                    0
                };
                let local_x = if self.wrap { x } else { x - start };
                self.estimate_in(range.start.0..range.end.0, local_x, row)
            };
        }
        if let Some(layout) = self.whole_layout(line, ctx) {
            let hit = layout.hit_test_point(Point {
                x: Dip(x as f32),
                y: Dip(local as f32),
            });
            return Some((hit.byte, hit.affinity));
        }
        let edges = self.with_stops(|s| s.line(line)).ok()?;
        let row = (local / row_height).floor().max(0.0) as usize;
        self.estimate_in(edges.start..edges.content_end, x, row)
    }
    fn hit(&mut self, at: Point, ctx: &mut ViewContext) -> Option<(ByteOffset, Affinity)> {
        let x = f64::from(at.x.0 - self.bounds.x.0) + self.scroll_x;
        let y = f64::from(at.y.0 - self.bounds.y.0) + self.scroll;
        self.resolve(x, y, ctx)
    }
    /// The row that holds a caret, when its layout is known.
    fn row_of(
        &mut self,
        byte: ByteOffset,
        affinity: Affinity,
        ctx: &mut ViewContext,
    ) -> Option<RowHere> {
        let line = self.line_at(byte)?;
        let snapshot = self.snapshot();
        let (layout, segment, x0) = if self.long_maps.contains_key(&line) {
            let (_, segment, _) = self
                .engine
                .segment_at(snapshot.as_ref(), byte, affinity)
                .ok()?;
            let layout = self.segment_layout(line, segment, ctx)?;
            let x0 = if self.wrap {
                0.0
            } else {
                self.long_maps
                    .get(&line)?
                    .line_to_scroll(LineIndex(segment))
            };
            (layout, Some(segment), x0)
        } else {
            (self.whole_layout(line, ctx)?, None, 0.0)
        };
        let rect = layout.caret_rect(byte, affinity)?;
        let row = layout.rows.iter().position(|r| r.y == rect.y)?;
        Some(RowHere {
            line,
            segment,
            layout,
            row,
            x0,
            x: x0 + f64::from(rect.x.0),
        })
    }
    /// One visual row up or down from a known row.
    fn row_step(
        &mut self,
        here: &RowHere,
        down: bool,
        x: f64,
        ctx: &mut ViewContext,
    ) -> Option<(ByteOffset, Affinity)> {
        let target = if down {
            here.row.checked_add(1)
        } else {
            here.row.checked_sub(1)
        };
        if let Some(row) = target.and_then(|t| here.layout.rows.get(t)) {
            let hit = row.hit_test_x(Dip((x - here.x0) as f32));
            return Some((hit.byte, hit.affinity));
        }
        if self.wrap
            && let Some(segment) = here.segment
            && let Some(count) = self.long_maps.get(&here.line).map(HeightMap::len)
        {
            let next = if down {
                segment.checked_add(1).filter(|n| *n < count)
            } else {
                segment.checked_sub(1)
            };
            if let Some(next) = next {
                return self.onto_segment(here.line, next, x, down, ctx);
            }
        }
        self.onto_line(here.line, down, x, ctx)
    }
    fn onto_segment(
        &mut self,
        line: usize,
        segment: usize,
        x: f64,
        down: bool,
        ctx: &mut ViewContext,
    ) -> Option<(ByteOffset, Affinity)> {
        if let Some(layout) = self.segment_layout(line, segment, ctx) {
            let row = if down {
                layout.rows.first()
            } else {
                layout.rows.last()
            }?;
            let hit = row.hit_test_x(Dip(x as f32));
            return Some((hit.byte, hit.affinity));
        }
        let snapshot = self.snapshot();
        let range = self
            .engine
            .segment_range(snapshot.as_ref(), LineIndex(line), segment)
            .ok()?;
        let cols = self.cols();
        let row = if down {
            0
        } else {
            (range.end.0 - range.start.0).saturating_sub(1) / cols
        };
        self.estimate_in(range.start.0..range.end.0, x, row)
    }
    /// The first row of the next line, or the last row of the line before. At the ends
    /// of the text the caret goes to the start or the end, as AppKit's does.
    fn onto_line(
        &mut self,
        from: usize,
        down: bool,
        x: f64,
        ctx: &mut ViewContext,
    ) -> Option<(ByteOffset, Affinity)> {
        let line = if down {
            from + 1
        } else if from == 0 {
            return Some((ByteOffset(0), Affinity::After));
        } else {
            from - 1
        };
        if line >= self.source().status().known_lines {
            return Some((self.known_end(), Affinity::After));
        }
        let snapshot = self.snapshot();
        let long = matches!(
            self.engine.is_long_line(snapshot.as_ref(), LineIndex(line)),
            Ok(true)
        );
        if long {
            if let Some(count) = self.long_maps.get(&line).map(HeightMap::len) {
                let segment = if self.wrap {
                    if down { 0 } else { count - 1 }
                } else {
                    self.long_maps.get(&line)?.scroll_to_line(x).0
                };
                let x0 = if self.wrap {
                    x
                } else {
                    x - self
                        .long_maps
                        .get(&line)?
                        .line_to_scroll(LineIndex(segment))
                };
                return self.onto_segment(line, segment, x0, down, ctx);
            }
        } else if let Some(layout) = self.whole_layout(line, ctx) {
            let row = if down {
                layout.rows.first()
            } else {
                layout.rows.last()
            }?;
            let hit = row.hit_test_x(Dip(x as f32));
            return Some((hit.byte, hit.affinity));
        }
        let edges = self.with_stops(|s| s.line(line)).ok()?;
        if down {
            let to = edges.content_end.min(edges.start + 16 * 1024);
            self.estimate_in(edges.start..to, x, 0)
        } else {
            let tail = edges.content_end.saturating_sub(16 * 1024).max(edges.start);
            let from = self
                .with_stops(|s| s.edge_forward(tail, edges.content_end))
                .ok()?;
            let cols = self.cols();
            let bytes = edges.content_end - from;
            let row = bytes.saturating_sub(1) / cols;
            self.estimate_in(from..edges.content_end, x, row)
        }
    }
    /// The move for a caret on a line with no layout: inside the line by the estimate,
    /// or onto the next line.
    fn unlaid_step(
        &mut self,
        byte: ByteOffset,
        down: bool,
        x: f64,
        ctx: &mut ViewContext,
    ) -> Option<(ByteOffset, Affinity)> {
        let edges = self.with_stops(|s| s.line_of(byte.0)).ok()?;
        let cols = self.cols();
        if self.wrap {
            let inside = self.with_stops(|stops| {
                let target = stops.chars_away(&edges, byte.0, cols, down)?;
                target.map(|t| stops.snap(t)).transpose()
            });
            if let Ok(Some(target)) = inside {
                return Some((ByteOffset(target), Affinity::After));
            }
        }
        self.onto_line(edges.index, down, x, ctx)
    }
    fn estimated_x(&mut self, byte: ByteOffset) -> f64 {
        let cell = f64::from(self.cell_width);
        let cols = self.cols();
        let answer = self.with_stops(|stops| -> Result<usize, LookupError> {
            let line = stops.line_of(byte.0)?;
            let mut start = byte.0.saturating_sub(8 * 1024).max(line.start);
            let mut text = String::new();
            loop {
                match stops
                    .text
                    .try_visit_chunks(ByteOffset(start)..byte, &mut |s| text.push_str(s))
                {
                    Ok(()) => break,
                    Err(LookupError::OutOfRange) if start < byte.0 => {
                        start += 1;
                        text.clear();
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(text.chars().count())
        });
        answer.map_or(0.0, |count| (count % cols.max(1)) as f64 * cell)
    }
    fn base_for_move(&self, toward_end: bool, shift: bool) -> (ByteOffset, Affinity) {
        let selection = self.selection();
        if !shift && !selection.is_empty() {
            (
                if toward_end {
                    selection.end
                } else {
                    selection.start
                },
                Affinity::After,
            )
        } else {
            (self.caret, self.affinity)
        }
    }
    fn arrive(&mut self, byte: ByteOffset, affinity: Affinity, shift: bool, ctx: &mut ViewContext) {
        let anchor = if shift { self.anchor } else { byte };
        self.set_selection(anchor, byte, affinity, true, ctx);
    }
    fn move_vertical(&mut self, down: bool, shift: bool, ctx: &mut ViewContext) {
        let (base, affinity) = self.base_for_move(down, shift);
        let here = self.row_of(base, affinity, ctx);
        let goal = self.goal_x;
        let x = goal
            .or(here.as_ref().map(|h| h.x))
            .unwrap_or_else(|| self.estimated_x(base));
        let target = match &here {
            Some(here) => self.row_step(here, down, x, ctx),
            None => self.unlaid_step(base, down, x, ctx),
        };
        if let Some((byte, affinity)) = target {
            self.arrive(byte, affinity, shift, ctx);
            self.goal_x = Some(x);
        }
    }
    /// Moves the caret and the view by the rows that fit the view, for each page that
    /// waits. The view moves only with a known target, so it never leaves the caret, and
    /// presses made before the text arrived still count.
    fn move_page(&mut self, shift: bool, ctx: &mut ViewContext) {
        let pages = self.pending_pages;
        if pages == 0 {
            return;
        }
        let (base, affinity) = self.base_for_move(pages > 0, shift);
        let here = self.row_of(base, affinity, ctx);
        let Some(place) = self.place(base, affinity) else {
            return;
        };
        let x = self.goal_x.or(here.map(|h| h.x)).unwrap_or(place.x);
        let row = f64::from(self.row_height);
        let page = (f64::from(self.bounds.height.0) / row).floor().max(1.0) * row;
        let signed = page * f64::from(pages);
        let middle = place.y + f64::from(place.height) / 2.0 + signed;
        let target = if middle < 0.0 {
            Some((ByteOffset(0), Affinity::After))
        } else {
            self.resolve(x, middle, ctx)
        };
        let Some((byte, affinity)) = target else {
            return;
        };
        self.pending_pages = 0;
        let before = self.scroll;
        self.scroll = (self.scroll + signed).clamp(0.0, self.max_scroll());
        if self.scroll != before {
            ctx.invalidate(self.bounds);
        }
        self.arrive(byte, affinity, shift, ctx);
        self.goal_x = Some(x);
        self.schedule(ctx);
    }
    /// Home and End need the row, and only layout knows the row. An estimate could
    /// select more than the row, so these keys wait, and later input waits behind them.
    fn row_edge_must_wait(&mut self, vk: u16, mods: Modifiers, ctx: &mut ViewContext) -> bool {
        if mods.ctrl || !matches!(vk, 0x23 | 0x24) || !self.wrap {
            return false;
        }
        let (base, affinity) = self.base_for_move(vk == 0x23, mods.shift);
        if self.row_of(base, affinity, ctx).is_some() {
            return false;
        }
        let Some(line) = self.line_at(base) else {
            return !self.source().status().complete;
        };
        !self.failed.iter().any(|(failed, _)| *failed == line)
    }
    /// Runs held input in order, as far as the first key that still waits.
    fn run_deferred(&mut self, now: Instant, ctx: &mut ViewContext) {
        while let Some(input) = self.deferred.front().cloned() {
            let wait = self.wait_of(&input, ctx);
            let timed_out = now.saturating_duration_since(self.deferred_since) >= DEFER_LIMIT
                || now.saturating_duration_since(self.deferred_first) >= DEFER_TOTAL_LIMIT;
            if wait.is_some() && !timed_out {
                return;
            }
            self.deferred.pop_front();
            // The next key that waits starts its own wait from here.
            self.deferred_since = now;
            if wait == Some(WaitsFor::Layout) {
                self.set_warning(
                    "Home or End was dropped, because this line's layout did not arrive."
                        .to_string(),
                    false,
                );
                continue;
            }
            let select_all = matches!(input, Queued::Command(EditorCommand::SelectAll))
                || matches!(
                    input,
                    Queued::Event(InputEvent {
                        kind: InputKind::KeyDown {
                            virtual_key: 0x41,
                            ..
                        },
                        ..
                    })
                );
            match input {
                Queued::Event(event) => {
                    let _ = self.handle_inner(&event, true, ctx);
                }
                Queued::Command(command) => {
                    self.touch();
                    self.run_inner(command, ctx);
                }
            }
            if wait == Some(WaitsFor::Index) {
                // The action went ahead on what is known, and it says so, so nobody
                // copies part of a file without knowing.
                let known = plain_size(self.source().status().known_bytes.0);
                self.set_warning(
                    if select_all {
                        format!(
                            "This file is still being indexed, so this selects only its first {known}."
                        )
                    } else {
                        format!(
                            "This file is still being indexed, so this goes only to the end of its first {known}."
                        )
                    },
                    false,
                );
                self.partial_selection = Some(self.selection());
            }
        }
    }
    /// What a held input waits for, or `None` when it can run now.
    fn wait_of(&mut self, input: &Queued, ctx: &mut ViewContext) -> Option<WaitsFor> {
        match input {
            Queued::Event(InputEvent {
                kind:
                    InputKind::KeyDown {
                        virtual_key,
                        modifiers,
                        ..
                    },
                ..
            }) => {
                if modifiers.ctrl && matches!(*virtual_key, 0x23 | 0x41) {
                    self.index_is_running().then_some(WaitsFor::Index)
                } else {
                    self.row_edge_must_wait(*virtual_key, *modifiers, ctx)
                        .then_some(WaitsFor::Layout)
                }
            }
            Queued::Event(_) => None,
            Queued::Command(command) => self.command_must_wait(*command).then_some(WaitsFor::Index),
        }
    }
    /// A partial warning lasts while it is true: until the selection changes, or until
    /// the selection turns out to be the whole file.
    fn settle_partial_warning(&mut self) {
        let Some(range) = self.partial_selection.clone() else {
            return;
        };
        let selection = self.selection();
        let status = self.source().status();
        let whole =
            status.complete && selection.start.0 == 0 && selection.end == status.known_bytes;
        if self.warning.is_none() {
            self.partial_selection = None;
        } else if selection != range || whole {
            self.partial_selection = None;
            self.warning = None;
            self.warning_dirty = true;
        }
    }
    /// A key that needs something still on its way: the row, or the end of the file.
    fn key_must_wait(&mut self, vk: u16, mods: Modifiers, ctx: &mut ViewContext) -> bool {
        if mods.ctrl && matches!(vk, 0x23 | 0x41) {
            return self.index_is_running();
        }
        self.row_edge_must_wait(vk, mods, ctx)
    }
    /// True for input that must keep its order behind a waiting key.
    fn keeps_order(event: &InputEvent) -> bool {
        match &event.kind {
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } => {
                !modifiers.alt
                    && !modifiers.win
                    && (matches!(*virtual_key, 0x08 | 0x0d | 0x21..=0x28 | 0x2e)
                        || (modifiers.ctrl
                            && matches!(*virtual_key, 0x41 | 0x43 | 0x56 | 0x58 | 0x59 | 0x5a)))
            }
            InputKind::Char(c) => !c.is_control() || *c == '\t',
            InputKind::MouseDown { .. }
            | InputKind::MouseUp { .. }
            | InputKind::MouseMove { .. } => true,
            _ => false,
        }
    }
    fn move_row_edge(&mut self, home: bool, shift: bool, ctx: &mut ViewContext) {
        let (base, affinity) = self.base_for_move(!home, shift);
        let wrap = self.wrap;
        let target = self
            .row_of(base, affinity, ctx)
            .filter(|h| wrap || h.segment.is_none())
            .and_then(|h| {
                h.layout
                    .rows
                    .get(h.row)
                    .map(|r| if home { r.range.start } else { r.range.end })
            });
        // With wrap on and no layout, the key has waited and the layout failed. Only the
        // layout knows the row, and the whole line could be far more than a row.
        if target.is_none() && wrap {
            return;
        }
        let target = target.or_else(|| {
            self.with_stops(|s| s.line_of(base.0))
                .ok()
                .map(|edges| ByteOffset(if home { edges.start } else { edges.content_end }))
        });
        if let Some(target) = target {
            let affinity = if home {
                Affinity::After
            } else {
                Affinity::Before
            };
            self.arrive(target, affinity, shift, ctx);
        }
    }
    fn move_paragraph(&mut self, down: bool, shift: bool, ctx: &mut ViewContext) {
        let (base, _) = self.base_for_move(down, shift);
        let Ok(position) = self.source().try_byte_to_position(base) else {
            return;
        };
        let next = if down {
            position.line.0.checked_add(1)
        } else if let Ok(current) = self.source().try_line_range(position.line) {
            if base > current.start {
                Some(position.line.0)
            } else {
                position.line.0.checked_sub(1)
            }
        } else {
            None
        };
        if let Some(line) = next
            && let Ok(range) = self.source().try_line_range(LineIndex(line))
        {
            self.arrive(range.start, Affinity::After, shift, ctx);
        }
    }
    fn move_sideways(&mut self, forward: bool, word: bool, shift: bool, ctx: &mut ViewContext) {
        let selection = self.selection();
        if !shift && !selection.is_empty() && !word {
            let edge = if forward {
                selection.end
            } else {
                selection.start
            };
            self.arrive(edge, Affinity::After, false, ctx);
            return;
        }
        let (base, _) = self.base_for_move(forward, shift);
        let at = base.0;
        let target = self.with_stops(|stops| {
            if word {
                if forward {
                    stops.word_end_after(at).map(Some)
                } else {
                    stops.word_start_before(at).map(Some)
                }
            } else if forward {
                stops.after(at)
            } else {
                stops.before(at)
            }
        });
        if let Ok(Some(target)) = target {
            self.arrive(ByteOffset(target), Affinity::After, shift, ctx);
        }
    }
    fn key(&mut self, vk: u16, mods: Modifiers, forced: bool, ctx: &mut ViewContext) -> Handled {
        // Ctrl+Alt is AltGr, so its character arrives as text. Alt and Win are not ours.
        if mods.alt || mods.win {
            return Handled::No;
        }
        // Esc takes back a warning. A store failure stays, because its cause stays.
        if vk == 0x1b {
            if self.warning.as_ref().is_some_and(|w| !w.sticky) {
                self.warning = None;
                self.warning_dirty = true;
                return Handled::Yes;
            }
            return Handled::No;
        }
        if !forced && self.key_must_wait(vk, mods, ctx) {
            self.hold(Queued::Event(InputEvent {
                time_us: 0,
                kind: InputKind::KeyDown {
                    virtual_key: vk,
                    modifiers: mods,
                    repeat: false,
                },
            }));
            // Home or End needs the caret's line. It may be off screen, and then nothing
            // would lay it out. A wait for the index needs no line, so the view stays.
            if !mods.ctrl {
                self.reveal_caret(ctx);
            }
            self.schedule(ctx);
            return Handled::Yes;
        }
        let command = match (vk, mods.ctrl) {
            (0x41, true) => Some(EditorCommand::SelectAll),
            (0x43, true) => Some(EditorCommand::Copy),
            (0x58, true) => Some(EditorCommand::Cut),
            (0x56, true) => Some(EditorCommand::Paste),
            (0x5a, true) if mods.shift => Some(EditorCommand::Redo),
            (0x5a, true) => Some(EditorCommand::Undo),
            (0x59, true) => Some(EditorCommand::Redo),
            (0x08, true) => Some(EditorCommand::DeleteWordBackward),
            (0x08, false) => Some(EditorCommand::DeleteBackward),
            (0x2e, true) => Some(EditorCommand::DeleteWordForward),
            (0x2e, false) => Some(EditorCommand::DeleteForward),
            (0x0d, false) => Some(EditorCommand::InsertLineEnding),
            _ => None,
        };
        if let Some(command) = command {
            self.touch();
            self.run_inner(command, ctx);
            return Handled::Yes;
        }
        if matches!(vk, 0x21..=0x28) {
            let pages = self.pending_pages;
            self.touch();
            if matches!(vk, 0x21 | 0x22) {
                self.pending_pages = pages + if vk == 0x22 { 1 } else { -1 };
            }
            self.move_key(vk, mods, Instant::now(), ctx);
            return Handled::Yes;
        }
        Handled::No
    }
    /// Runs a move. On a mapped file its text may not be in memory, and the store then
    /// reads it in the background, so the move runs again on the next poll for a short
    /// time. A new key replaces it.
    fn move_key(&mut self, vk: u16, mods: Modifiers, first: Instant, ctx: &mut ViewContext) {
        let before = (self.anchor, self.caret);
        self.run_move(vk, mods, ctx);
        let stuck = self.mapped.is_some() && before == (self.anchor, self.caret);
        let wait = if matches!(vk, 0x21 | 0x22) {
            self.pending_pages != 0
        } else {
            stuck && (!self.source().status().complete || self.move_needs_a_page(vk))
        };
        self.retry_key = wait.then_some((vk, mods, first));
    }
    /// True when the store has not got the lines next to the caret in memory.
    fn move_needs_a_page(&self, vk: u16) -> bool {
        let source = self.source();
        let Ok(position) = source.try_byte_to_position(self.caret) else {
            return true;
        };
        let near = if matches!(vk, 0x21 | 0x24 | 0x25 | 0x26) {
            position.line.0.saturating_sub(1)
        } else {
            position.line.0 + 1
        };
        matches!(
            source.try_line_range(LineIndex(near)),
            Err(LookupError::NotYetKnown { .. })
        )
    }
    fn run_move(&mut self, vk: u16, mods: Modifiers, ctx: &mut ViewContext) {
        match vk {
            0x25 | 0x27 => {
                self.move_sideways(vk == 0x27, mods.ctrl, mods.shift, ctx);
            }
            0x26 | 0x28 => {
                if mods.ctrl {
                    self.move_paragraph(vk == 0x28, mods.shift, ctx);
                } else {
                    self.move_vertical(vk == 0x28, mods.shift, ctx);
                }
            }
            0x21 | 0x22 => {
                self.move_page(mods.shift, ctx);
            }
            0x24 | 0x23 => {
                if mods.ctrl {
                    let at = if vk == 0x24 {
                        ByteOffset(0)
                    } else {
                        self.known_end()
                    };
                    self.arrive(at, Affinity::After, mods.shift, ctx);
                } else {
                    self.move_row_edge(vk == 0x24, mods.shift, ctx);
                }
            }
            _ => {}
        }
    }
    fn drag_is_outside(&self) -> bool {
        let Some(at) = self.drag_point else {
            return false;
        };
        at.y.0 < self.bounds.y.0
            || at.y.0 >= self.bounds.y.0 + self.bounds.height.0
            || (!self.wrap
                && (at.x.0 < self.bounds.x.0 || at.x.0 >= self.bounds.x.0 + self.bounds.width.0))
    }
    /// One step of drag scrolling. The first runs on the mouse move that leaves the
    /// view, and the timer repeats it with no more mouse moves.
    fn drag_step(&mut self, now: Instant, ctx: &mut ViewContext) {
        if !self.dragging || !self.drag_is_outside() {
            return;
        }
        let Some(at) = self.drag_point else {
            return;
        };
        // One step for each `DRAG_STEP` of real time, whatever the timer's spacing or the
        // number of events. Time short of a step carries over. A stall earns only the
        // most steps, and the rest of it is forgotten, so it causes no great jump.
        let earned =
            now.saturating_duration_since(self.drag_due).as_micros() / DRAG_STEP.as_micros();
        if earned == 0 {
            return;
        }
        self.drag_due += DRAG_STEP * earned as u32;
        let steps = earned.min(u128::from(DRAG_MOST_STEPS)) as f64;
        let (old_y, old_x) = (self.scroll, self.scroll_x);
        let row = f64::from(self.row_height);
        let top = self.bounds.y.0;
        let bottom = top + self.bounds.height.0;
        if at.y.0 < top {
            self.scroll = (self.scroll - row * steps).max(0.0);
        } else if at.y.0 >= bottom {
            self.scroll = (self.scroll + row * steps).min(self.max_scroll());
        }
        if !self.wrap {
            let left = self.bounds.x.0;
            let right = left + self.bounds.width.0;
            let step = f64::from(self.cell_width) * 2.0 * steps;
            if at.x.0 < left {
                self.scroll_x = (self.scroll_x - step).max(0.0);
            } else if at.x.0 >= right {
                self.scroll_x = (self.scroll_x + step).min(self.max_scroll_x());
            }
        }
        if self.scroll != old_y || self.scroll_x != old_x {
            ctx.invalidate(self.bounds);
            self.schedule(ctx);
        }
        self.drag_select(at, ctx);
    }
    fn drag_select(&mut self, at: Point, ctx: &mut ViewContext) {
        // A point outside the view selects up to the row at the view's edge.
        let inside = Point {
            x: Dip(at
                .x
                .0
                .clamp(self.bounds.x.0, self.bounds.x.0 + self.bounds.width.0)),
            y: Dip(at.y.0.clamp(
                self.bounds.y.0,
                (self.bounds.y.0 + self.bounds.height.0 - 1.0).max(self.bounds.y.0),
            )),
        };
        let Some((byte, affinity)) = self.hit(inside, ctx) else {
            return;
        };
        let origin = self.drag_origin.clone();
        let unit = match self.drag_unit {
            DragUnit::Char => byte..byte,
            DragUnit::Word => self
                .with_stops(|s| s.word_at(byte.0, false))
                .map_or(byte..byte, |r| ByteOffset(r.start)..ByteOffset(r.end)),
            DragUnit::Line => self.line_span(byte).unwrap_or(byte..byte),
        };
        let (anchor, caret) = if byte < origin.start {
            (origin.end, unit.start)
        } else {
            (origin.start, unit.end.max(origin.end))
        };
        self.set_selection(anchor, caret, affinity, false, ctx);
    }
    fn line_span(&self, at: ByteOffset) -> Option<Range<ByteOffset>> {
        let position = self.source().try_byte_to_position(at).ok()?;
        self.source().try_line_range(position.line).ok()
    }
    fn mouse_down(&mut self, at: Point, shift: bool, time_us: u64, ctx: &mut ViewContext) {
        self.touch();
        let near = self.last_click_at.is_some_and(|last| {
            (at.x.0 - last.x.0).abs() <= self.double_click_width / 2.0
                && (at.y.0 - last.y.0).abs() <= self.double_click_height / 2.0
        });
        let soon = time_us.saturating_sub(self.last_click_us) <= self.double_click_us;
        self.click_count = if near && soon {
            self.click_count % 3 + 1
        } else {
            1
        };
        self.last_click_us = time_us;
        self.last_click_at = Some(at);
        self.dragging = true;
        self.drag_point = Some(at);
        self.drag_due = Instant::now();
        ctx.capture_mouse(true);
        ctx.set_cursor(MouseCursor::IBeam);
        let Some((byte, affinity)) = self.hit(at, ctx) else {
            return;
        };
        match self.click_count {
            2 => {
                // A click on a character's right half lands on the stop after it, but
                // the word is the one under the mouse.
                let doc_x = f64::from(at.x.0 - self.bounds.x.0) + self.scroll_x;
                let look_back = self
                    .place(byte, affinity)
                    .is_some_and(|p| p.exact && doc_x < p.x);
                let range = self
                    .with_stops(|s| s.word_at(byte.0, look_back))
                    .map_or(byte..byte, |r| ByteOffset(r.start)..ByteOffset(r.end));
                self.drag_unit = DragUnit::Word;
                self.drag_origin = range.clone();
                self.set_selection(range.start, range.end, Affinity::After, false, ctx);
            }
            3 => {
                let range = self.line_span(byte).unwrap_or(byte..byte);
                self.drag_unit = DragUnit::Line;
                self.drag_origin = range.clone();
                self.set_selection(range.start, range.end, Affinity::After, false, ctx);
            }
            _ => {
                self.drag_unit = DragUnit::Char;
                if shift {
                    let anchor = self.anchor;
                    self.drag_origin = anchor..anchor;
                    self.set_selection(anchor, byte, affinity, false, ctx);
                } else {
                    self.drag_origin = byte..byte;
                    self.set_selection(byte, byte, affinity, true, ctx);
                }
            }
        }
    }
    /// Input takes its turn behind any key that still waits for layout.
    fn handle_now(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        if !self.deferred.is_empty() && Self::keeps_order(event) {
            self.hold(Queued::Event(event.clone()));
            return Handled::Yes;
        }
        self.handle_inner(event, false, ctx)
    }
    /// `forced` runs a held key whose wait has ended.
    fn handle_inner(&mut self, event: &InputEvent, forced: bool, ctx: &mut ViewContext) -> Handled {
        match &event.kind {
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } => self.key(*virtual_key, *modifiers, forced, ctx),
            InputKind::Char(c) if !c.is_control() || *c == '\t' => {
                self.touch();
                if self.can_edit() {
                    let mut buffer = [0; 4];
                    let text = c.encode_utf8(&mut buffer);
                    let range = self.selection();
                    self.apply_edit(range, text, true, ctx);
                } else {
                    self.refused_edits += 1;
                }
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers,
            } => {
                self.mouse_down(*at, modifiers.shift, event.time_us, ctx);
                Handled::Yes
            }
            InputKind::MouseMove { at } if self.dragging => {
                let was_outside = self.drag_is_outside();
                self.drag_point = Some(*at);
                if self.drag_is_outside() {
                    let now = Instant::now();
                    // Time inside the view earns no steps. Going out earns the first.
                    if !was_outside {
                        self.drag_due = now.checked_sub(DRAG_STEP).unwrap_or(now);
                    }
                    self.drag_step(now, ctx);
                } else {
                    self.drag_select(*at, ctx);
                }
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = false;
                self.drag_point = None;
                ctx.capture_mouse(false);
                Handled::Yes
            }
            InputKind::Wheel {
                delta_x, delta_y, ..
            } => {
                // The shell gives whole notches, not Windows' units of 120.
                if !self.wrap {
                    let step = f64::from(*delta_x) * f64::from(self.cell_width) * 3.0;
                    self.scroll_x = (self.scroll_x + step).clamp(0.0, self.max_scroll_x());
                }
                let row = f64::from(self.row_height);
                let rows = if self.wheel_lines == u32::MAX {
                    (f64::from(self.bounds.height.0) / row).floor().max(1.0)
                } else {
                    f64::from(self.wheel_lines)
                };
                self.scroll =
                    (self.scroll - f64::from(*delta_y) * rows * row).clamp(0.0, self.max_scroll());
                self.reveal_pending = false;
                self.pending_anchor = None;
                self.keep_caret_visible = false;
                ctx.invalidate(self.bounds);
                self.schedule(ctx);
                Handled::Yes
            }
            InputKind::Resize { width, height } => {
                let bounds = Rect {
                    width: *width,
                    height: *height,
                    ..self.bounds
                };
                self.set_bounds(bounds);
                self.schedule(ctx);
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Focus => {
                self.show_caret();
                self.focused = true;
                self.repaint_caret(self.caret, self.affinity, ctx);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.visible = false;
                self.cancel_drag();
                self.repaint_caret(self.caret, self.affinity, ctx);
                // The user has left, so this is the idle moment to free history.
                self.history.release_memory();
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    /// The line as it was before an edit, with no selection, until its layout arrives.
    fn paint_ghost(&self, list: &mut DisplayList, ghost: &LineLayout, frame: &LineFrame) {
        let plain = LineFrame {
            theme: frame.theme,
            selection: ByteOffset(0)..ByteOffset(0),
            x: frame.x,
            y: frame.y,
            caret: None,
            view_right: frame.view_right,
            content_end: None,
            wrap: frame.wrap,
        };
        paint_line_layout(list, ghost, &plain);
        if frame.caret.is_some()
            && let Some(rect) = ghost_caret(ghost, self.caret)
        {
            list.push(DrawCommand::FillRect {
                rect: Rect {
                    x: Dip(frame.x + rect.x.0),
                    y: Dip(frame.y + rect.y.0),
                    width: rect.width,
                    height: rect.height,
                },
                color: frame.theme.text,
            });
        }
    }
}

impl Drop for EditorView {
    fn drop(&mut self) {
        self.send_away_document(PieceStore::new(""));
        self.cancel_copy();
        self.shared.closing.store(true, Ordering::Release);
    }
}

fn worker_loop(
    tasks: Arc<std::sync::Mutex<Receiver<WorkerTask>>>,
    results: Sender<LayoutResult>,
    maps: Sender<MapResult>,
    known: Sender<KnownResult>,
    shared: Arc<WorkerShared>,
) {
    let _exit = WorkerExit(shared.clone());
    loop {
        let next = {
            let Ok(queue) = tasks.lock() else {
                return;
            };
            queue.recv()
        };
        let Ok(task) = next else {
            return;
        };
        while shared.test_paused.load(Ordering::Acquire) {
            if shared.closing.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        if shared.closing.load(Ordering::Acquire) {
            return;
        }
        match task {
            WorkerTask::Layout(id, ticket, snapshot, engine) => {
                let line = ticket.line().0;
                let wanted = line >= shared.wanted_first.load(Ordering::Acquire)
                    && line < shared.wanted_last.load(Ordering::Acquire);
                let outcome = if !wanted {
                    shared.skipped.fetch_add(1, Ordering::AcqRel);
                    LayoutOutcome::Skipped
                } else if shared.fail_line.load(Ordering::Acquire) == line {
                    LayoutOutcome::Done(Err(LayoutError::NoSuchLine(ticket.line())))
                } else {
                    LayoutOutcome::Done(ticket.layout(&engine, snapshot.as_ref()))
                };
                // Before the result: a view that sees it must find no snapshot left here.
                drop(snapshot);
                let done = LayoutResult {
                    id,
                    ticket,
                    outcome,
                };
                if results.send(done).is_err() {
                    return;
                }
                shared.wake_host();
            }
            WorkerTask::LongMap(id, line, width, snapshot, engine, generation) => {
                let result = if shared.fail_line.load(Ordering::Acquire) == line.0 {
                    Err(LayoutError::NoSuchLine(line))
                } else {
                    engine.long_line_map(snapshot.as_ref(), line, width)
                };
                drop(snapshot);
                let done = MapResult {
                    id,
                    generation,
                    result,
                };
                if maps.send(done).is_err() {
                    return;
                }
                shared.wake_host();
            }
            WorkerTask::KnownFirst(end, width, snapshot, engine, generation) => {
                let mut result = Err("The known text has no UTF-8 edge.".to_string());
                for back in 0..4 {
                    let Some(last) = end.checked_sub(back) else {
                        break;
                    };
                    let mut text = String::new();
                    if snapshot
                        .try_visit_chunks(ByteOffset(0)..ByteOffset(last), &mut |part| {
                            text.push_str(part)
                        })
                        .is_ok()
                    {
                        let temporary = PieceStore::new(&text);
                        result = engine
                            .try_layout_line(&temporary, LineIndex(0), width)
                            .map_err(|e| e.to_string());
                        break;
                    }
                }
                let done = KnownResult {
                    revision: snapshot.revision(),
                    generation,
                    result,
                };
                drop(snapshot);
                if known.send(done).is_err() {
                    return;
                }
                shared.wake_host();
            }
            WorkerTask::Drop(snapshot) => {
                drop(snapshot);
                shared.drops_out.fetch_sub(1, Ordering::AcqRel);
            }
            WorkerTask::DropDocument(document) => drop(document),
        }
    }
}

impl View for EditorView {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        let handled = self.handle_now(event, ctx);
        if handled == Handled::Yes {
            self.finish(Instant::now(), ctx);
        }
        handled
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, _measure: &dyn TextMeasurer) {
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: theme.text_background,
        });
        let snapshot = self.snapshot();
        let known_lines = snapshot.status().known_lines;
        let selection = self.selection();
        let show_caret = self.focused && self.visible && selection.is_empty();
        let caret_line = self.caret_line;
        let view_bottom = self.scroll + f64::from(self.bounds.height.0);
        let view_right = self.bounds.x.0 + self.bounds.width.0;
        let left = self.bounds.x.0 - self.scroll_x as f32;
        let mut i = self.heights.scroll_to_line(self.scroll).0;
        let mut line_top = self.heights.line_to_scroll(LineIndex(i));
        while i < known_lines && line_top < view_bottom {
            let line = LineIndex(i);
            let height = self
                .heights
                .line_height(line)
                .unwrap_or(Dip(self.row_height));
            let top = self.bounds.y.0 + (line_top - self.scroll) as f32;
            let on_caret_line = show_caret && caret_line == Some(i);
            // Only a selection needs to know where the line's content ends.
            let content_end = if selection.is_empty() {
                None
            } else {
                self.source()
                    .try_line_range(line)
                    .ok()
                    .zip(self.source().try_line_ending_len(line).ok())
                    .map(|(range, ending)| ByteOffset(range.end.0 - ending))
            };
            let mut frame = LineFrame {
                theme,
                selection: selection.clone(),
                x: left,
                y: top,
                caret: on_caret_line.then_some((self.caret, self.affinity)),
                view_right,
                content_end,
                wrap: self.wrap,
            };
            if let Some(map) = self.long_maps.get(&i) {
                let along = if self.wrap {
                    (self.scroll - line_top).max(0.0)
                } else {
                    self.scroll_x
                };
                let start = map.scroll_to_line(along).0;
                let limit = along
                    + f64::from(if self.wrap {
                        self.bounds.height.0
                    } else {
                        self.bounds.width.0
                    }) * 2.0;
                let caret_segment = if on_caret_line {
                    self.engine
                        .segment_at(snapshot.as_ref(), self.caret, self.affinity)
                        .ok()
                        .map(|(_, segment, _)| segment)
                } else {
                    None
                };
                for segment in start..map.len().min(start + 128) {
                    let offset = map.line_to_scroll(LineIndex(segment));
                    if offset > limit {
                        break;
                    }
                    if self.wrap {
                        frame.x = self.bounds.x.0;
                        frame.y = self.bounds.y.0 + (line_top + offset - self.scroll) as f32;
                    } else {
                        // A distance from the view, which an f32 holds exactly enough.
                        frame.x = self.bounds.x.0 + (offset - self.scroll_x) as f32;
                    }
                    frame.caret =
                        (caret_segment == Some(segment)).then_some((self.caret, self.affinity));
                    let layout = self.cache.borrow_mut().cached_segment(
                        &self.engine,
                        snapshot.as_ref(),
                        line,
                        segment,
                        self.width(),
                    );
                    if let Some(layout) = layout {
                        paint_line_layout(list, &layout, &frame);
                    } else if let Some(ghost) = self.ghosts.get(&(i, segment)) {
                        self.paint_ghost(list, ghost, &frame);
                    }
                }
            } else {
                let whole = self.cache.borrow_mut().cached(
                    &self.engine,
                    snapshot.as_ref(),
                    line,
                    self.width(),
                );
                if let Some(layout) = whole {
                    paint_line_layout(list, &layout, &frame);
                } else if let Some(ghost) = self.ghosts.get(&(i, WHOLE)) {
                    self.paint_ghost(list, ghost, &frame);
                } else if matches!(self.engine.is_long_line(snapshot.as_ref(), line), Ok(true)) {
                    frame.caret = None;
                    let first = self.cache.borrow_mut().cached_segment(
                        &self.engine,
                        snapshot.as_ref(),
                        line,
                        0,
                        self.width(),
                    );
                    if let Some(layout) = first {
                        paint_line_layout(list, &layout, &frame);
                    }
                } else if i == 0
                    && let Some((_, layout)) = &self.known_partial
                    && !snapshot
                        .status()
                        .failure
                        .as_ref()
                        .is_some_and(|f| f.valid_up_to == ByteOffset(0))
                {
                    frame.caret = None;
                    paint_line_layout(list, layout, &frame);
                }
            }
            i += 1;
            line_top += f64::from(height.0);
        }
        if let Some(warning) = &self.warning {
            // A warning sits over the first row of text, so it needs its own background.
            list.push(DrawCommand::FillRect {
                rect: Rect {
                    x: self.bounds.x,
                    y: self.bounds.y,
                    width: self.bounds.width,
                    height: Dip(WARNING_STRIP.min(self.bounds.height.0)),
                },
                color: theme.text_background,
            });
            list.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(self.bounds.x.0 + 8.0),
                    y: Dip(self.bounds.y.0 + 16.0),
                },
                text: warning.text.clone(),
                font: self.engine.font_family().into(),
                font_size: Dip(theme.font_pt * 96.0 / 72.0),
                color: theme.warning,
            });
        }
    }
    fn layout(&mut self, bounds: Rect, _measure: &dyn TextMeasurer) {
        self.set_bounds(bounds);
    }
    fn widgets(&self) -> Option<Widget> {
        Some(Widget {
            id: WidgetId(900),
            bounds: self.bounds,
            role: WidgetRole::TextField,
            name: "Editor".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                focused: self.focused,
                read_only: !self.can_edit(),
                ..Default::default()
            },
            children: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_small_size_is_not_written_as_zero_mib() {
        assert_eq!(super::plain_size(65_536), "64 KiB");
        assert_eq!(super::plain_size(1_023), "1023 bytes");
        assert_eq!(super::plain_size(3 << 20), "3 MiB");
    }
    #[test]
    fn a_failed_allocation_gives_a_message_and_not_an_abort() {
        let message = super::reserve_text(usize::MAX / 2).unwrap_err();
        assert!(message.contains("not enough memory"), "{message}");
        assert!(super::reserve_text(16).is_ok());
    }
}
