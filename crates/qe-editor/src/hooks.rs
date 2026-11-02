//! The public hooks that the crates of M10 to M14 use. `HOOKS.md` explains them.
//!
//! Nothing here shapes text or waits for disk on the UI thread:
//! - Geometry answers from layouts that are already cached, or says `NotLaidOut`.
//! - Composition text is laid out by a worker.
//! - A big edit is prepared on the caller's thread, and applied as one edit.
use crate::{EditorView, Queued};
use qe_api::{
    Affinity, ByteOffset, Dip, DisplayList, DrawCommand, HitTest, KnownLine, LayoutRow, LineIndex,
    LineLayout, LookupError, Point, Rect, StoreStatus, TextPosition, TextSnapshot, TextStore,
    Theme, TrackedChange, Utf16Change, Utf16Offset, ViewContext,
};
use qe_buffer::PieceStore;
use std::{
    collections::VecDeque,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

/// The low bits of a revision value count edits. The high bits name the document, and
/// each view and each `adopt` gets a new name. So a revision value from one document never
/// equals, or passes for, a value from another.
const REVISION_BITS: u32 = 40;
static NEXT_DOCUMENT: AtomicU64 = AtomicU64::new(1);
fn new_document() -> u64 {
    NEXT_DOCUMENT.fetch_add(1, Ordering::Relaxed)
}

/// How many changes `changes_since` remembers.
const CHANGE_LOG: usize = 4096;
/// `range_rects` refuses a range over more lines than this.
const RECT_LINES: usize = 1024;
/// The most segments of one long line that a visible-rows answer looks at.
const VISIBLE_SEGMENTS: usize = 128;

/// Why a geometry hook has no answer now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    /// The place is known, but its layout has not arrived. Ask again after the next
    /// `poll_layout`. It may never arrive for a line far off screen.
    NotLaidOut,
    /// The store cannot answer. `LookupError::NotYetKnown` means the index has not
    /// reached this text yet.
    Lookup(LookupError),
    /// The range covers too many lines for this call. Ask about the visible part.
    TooManyLines { most: usize },
}
impl From<LookupError> for GeometryError {
    fn from(error: LookupError) -> Self {
        Self::Lookup(error)
    }
}

/// One rectangle of a byte range, in window coordinates. A row with one direction
/// gives one of these. A row with mixed directions gives one for each run.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeRect {
    pub rect: Rect,
    pub right_to_left: bool,
    /// The part of the range that this rectangle shows.
    pub range: Range<ByteOffset>,
}

/// One row on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleRow {
    pub line: LineIndex,
    pub first_byte: ByteOffset,
    /// The row's top in window coordinates.
    pub y: Dip,
    pub height: Dip,
    /// True when this row continues the line from the row above.
    pub continuation: bool,
}

/// The rows on screen that have a layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisibleRows {
    pub rows: Vec<VisibleRow>,
    /// False when some line on screen has no layout yet, so its rows are missing here.
    pub complete: bool,
}

/// What changed since a revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangesSince {
    /// Nothing changed.
    Current,
    /// In order, oldest first. Each change's offsets are in the text just before it.
    Changes(Vec<TrackedChange>),
    /// The log does not reach back that far, or the file was replaced. Scan again.
    Rescan,
}

/// The kinds of marks on text. Each kind has one list, and a new list replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationKind {
    /// Behind the text, in `Theme::find_match`.
    FindMatch,
    /// Behind the text, in `Theme::current_find_match`.
    CurrentFindMatch,
    /// A wavy line under the text, in `Theme::spelling_underline`.
    SpellingUnderline,
    /// A straight line under the text, in `Theme::text`.
    CompositionUnderline,
}
const KINDS: usize = 4;
impl DecorationKind {
    fn index(self) -> usize {
        self as usize
    }
}

/// Sorted ranges with no overlap, which an edit moves in time that follows the distance
/// between edits and not the length of the list. Like a gap buffer: `before` holds
/// real offsets, and `after`, in reverse order, holds offsets less `delta`.
#[derive(Debug, Default)]
pub(crate) struct RangeList {
    before: Vec<Range<usize>>,
    after: Vec<(isize, isize)>,
    delta: isize,
}
impl RangeList {
    fn set(&mut self, mut ranges: Vec<Range<usize>>) {
        ranges.retain(|r| r.start < r.end);
        ranges.sort_unstable_by_key(|r| r.start);
        let mut joined: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match joined.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => joined.push(range),
            }
        }
        self.before = joined;
        self.after = Vec::new();
        self.delta = 0;
    }
    fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }
    fn shifted(&self, range: &(isize, isize)) -> Range<usize> {
        (range.0 + self.delta) as usize..(range.1 + self.delta) as usize
    }
    /// The rule: a range before the edit stays, a range after it moves with its text, and
    /// a range the edit touches inside is dropped. An insertion at either end of a range
    /// leaves the range as it was, without the new text.
    fn edit(&mut self, start: usize, old_end: usize, new_len: usize) {
        while self.before.last().is_some_and(|r| r.end > start) {
            if let Some(range) = self.before.pop() {
                self.after.push((
                    range.start as isize - self.delta,
                    range.end as isize - self.delta,
                ));
            }
        }
        while let Some(range) = self.after.last().map(|r| self.shifted(r)) {
            if range.end > start {
                break;
            }
            self.after.pop();
            self.before.push(range);
        }
        while let Some(range) = self.after.last().map(|r| self.shifted(r)) {
            if range.start >= old_end {
                break;
            }
            self.after.pop();
        }
        self.delta += new_len as isize - (old_end - start) as isize;
    }
    /// The ranges that overlap `window`, in order.
    fn visit_overlaps(&self, window: Range<usize>, mut visit: impl FnMut(Range<usize>)) {
        let first = self.before.partition_point(|r| r.end <= window.start);
        for range in &self.before[first..] {
            if range.start >= window.end {
                return;
            }
            visit(range.clone());
        }
        // `after` is in reverse order, so its first ranges in the text are at its end.
        let past = self
            .after
            .partition_point(|r| self.shifted(r).start >= window.end);
        for range in self.after[past..].iter().rev() {
            let range = self.shifted(range);
            if range.end > window.start {
                visit(range);
            }
        }
    }
    fn to_vec(&self) -> Vec<Range<usize>> {
        let mut all = self.before.clone();
        all.extend(self.after.iter().rev().map(|r| self.shifted(r)));
        all
    }
}

/// How a composition's layout, whose bytes are its own, maps to the document.
#[derive(Clone, Debug, Default)]
pub(crate) struct ComposedBytes {
    /// The document byte of the layout's byte 0.
    window_start: usize,
    /// The composition's place in the document.
    at: usize,
    /// The document byte where the text after the composition goes on.
    resume: usize,
    /// In the layout's own bytes.
    pub(crate) text: Range<ByteOffset>,
    /// In the layout's own bytes.
    pub(crate) cursor: ByteOffset,
}
impl ComposedBytes {
    /// Like `doc_of`, for the end of a part: the end of the text before the
    /// composition is its place, not the text after it.
    fn doc_end_of(&self, byte: ByteOffset) -> ByteOffset {
        if byte <= self.text.start {
            ByteOffset(self.window_start + byte.0)
        } else {
            self.doc_of(byte)
        }
    }
    fn doc_of(&self, byte: ByteOffset) -> ByteOffset {
        if byte < self.text.start {
            ByteOffset(self.window_start + byte.0)
        } else if byte < self.text.end {
            ByteOffset(self.at)
        } else {
            ByteOffset(self.resume + byte.0 - self.text.end.0)
        }
    }
    /// The parts of a document range that show in this layout, in its bytes: one on each
    /// side of the composition. Text hidden by it shows as an empty range at its place.
    fn in_layout(&self, range: &Range<ByteOffset>) -> Vec<Range<ByteOffset>> {
        let to_layout = |doc: usize| -> usize {
            if doc <= self.at {
                doc.saturating_sub(self.window_start)
            } else if doc < self.resume {
                self.text.start.0
            } else {
                doc - self.resume + self.text.end.0
            }
        };
        let (start, end) = (range.start.0, range.end.0);
        let mut parts = Vec::new();
        if start < self.at {
            parts.push(ByteOffset(to_layout(start))..ByteOffset(to_layout(end.min(self.at))));
        }
        if end > self.resume {
            let from = start.max(self.resume);
            parts.push(ByteOffset(to_layout(from))..ByteOffset(to_layout(end)));
        }
        if parts.is_empty() {
            parts.push(ByteOffset(to_layout(start))..ByteOffset(to_layout(start)));
        }
        parts
    }
}

/// Text from an input method that shows at the caret and is not in the store.
pub(crate) struct Composition {
    pub(crate) text: String,
    pub(crate) cursor_utf16: usize,
    /// Where the text sits: the selection's start when the composition began.
    pub(crate) at: ByteOffset,
    pub(crate) line: usize,
    /// The segment of a long line, or `None` for a whole line.
    pub(crate) segment: Option<usize>,
    /// The latest finished layout, of the line or the segment with the text in it.
    pub(crate) layout: Option<Arc<LineLayout>>,
    /// The bytes of `layout`.
    pub(crate) shown: ComposedBytes,
    /// The bytes of the layout that `task` will bring.
    wanted: ComposedBytes,
    /// The task whose result is wanted.
    pub(crate) task: Option<u64>,
    /// The line's height before the composition, to put back at its end.
    pub(crate) old_height: Option<Dip>,
    /// The parts that the input method marks. Empty means one underline for all of it.
    pub(crate) clauses: Vec<CompositionClause>,
}

/// A part of a composition that the input method treats as one piece. In UTF-16 units of
/// the composition's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionClause {
    pub range_utf16: Range<usize>,
    /// The part that is being converted now, which gets the heavier line.
    pub target: bool,
}

/// How the last composition ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositionEnd {
    /// Its text went into the store, at this revision.
    Committed { revision: u64 },
    /// Esc or an explicit cancel threw it away.
    Cancelled,
}

/// What a commit did to the text, to move a place asked for in the text before it.
pub(crate) struct CompositionCommit {
    start: usize,
    old_end: usize,
    new_len: usize,
}
impl CompositionCommit {
    pub(crate) fn moved(&self, at: ByteOffset) -> ByteOffset {
        ByteOffset(if at.0 <= self.start {
            at.0
        } else if at.0 >= self.old_end {
            at.0 + self.new_len - (self.old_end - self.start)
        } else {
            self.start + self.new_len
        })
    }
}

/// What a host can read about a composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionState {
    pub text: String,
    pub cursor_utf16: usize,
    /// Where the text would go.
    pub at: ByteOffset,
    /// False until a worker has laid the text out and it shows.
    pub showing: bool,
    /// False while a newer text waits for its layout, so the rectangles are of older text.
    pub current: bool,
}

/// A fixed text width, for Wrap to Page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageWrap {
    /// The width of the text, which does not change with the view.
    pub text_width: Dip,
    /// The space to the left of the text. The same space is left to its right when the
    /// view scrolls sideways.
    pub margin: Dip,
}

/// One replacement in the text as it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<ByteOffset>,
    pub text: String,
}

/// Replacements made ready on any thread, to apply as one edit and one undo step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedEdits {
    base_revision: u64,
    range: Range<ByteOffset>,
    text: String,
    count: usize,
}
impl PreparedEdits {
    /// The revision these edits were made for. They apply only to that text.
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn edit_count(&self) -> usize {
        self.count
    }
    /// The bytes the one edit replaces, from the first edit's start to the last one's end.
    pub fn replaced_range(&self) -> Range<ByteOffset> {
        self.range.clone()
    }
    /// The bytes of text the one edit puts there.
    pub fn new_len(&self) -> usize {
        self.text.len()
    }
}

/// Merges edits into one edit of `snapshot`. It may wait for disk, and it takes time and
/// memory in proportion to the bytes from the first edit to the last, however few the
/// edits are. So run it on a worker unless that span is small and the snapshot is of an
/// editable view. `edits` must be sorted, must not overlap, and must start and end on
/// character edges. It stops soon after `cancel` turns true.
pub fn prepare_edits(
    snapshot: &dyn TextSnapshot,
    edits: &[TextEdit],
    cancel: &AtomicBool,
) -> Result<PreparedEdits, String> {
    let (Some(first), Some(last)) = (edits.first(), edits.last()) else {
        return Err("There is nothing to replace.".to_string());
    };
    if edits.windows(2).any(|p| p[0].range.end > p[1].range.start)
        || edits.iter().any(|e| e.range.start > e.range.end)
    {
        return Err("The ranges to replace are out of order or overlap.".to_string());
    }
    for edit in edits {
        if snapshot.byte_to_utf16(edit.range.start).is_none()
            || snapshot.byte_to_utf16(edit.range.end).is_none()
        {
            return Err("A range to replace ends inside a character or past the text.".to_string());
        }
    }
    let span = first.range.start..last.range.end;
    let removed: usize = edits.iter().map(|e| e.range.end.0 - e.range.start.0).sum();
    let added: usize = edits.iter().map(|e| e.text.len()).sum();
    let kept = (span.end.0 - span.start.0) - removed;
    let mut text = String::new();
    text.try_reserve_exact(kept.saturating_add(added))
        .map_err(|_| {
            format!(
                "There is not enough memory to replace this text ({}).",
                crate::plain_size(kept.saturating_add(added))
            )
        })?;
    let mut next = span.start;
    for edit in edits {
        if cancel.load(Ordering::Acquire) {
            return Err("The replacement was cancelled.".to_string());
        }
        let read = snapshot.visit_chunks_while(next..edit.range.start, &mut |part| {
            let go_on = !cancel.load(Ordering::Acquire);
            if go_on {
                text.push_str(part);
            }
            go_on
        });
        if !read {
            return Err("The text between the replacements could not be read.".to_string());
        }
        text.push_str(&edit.text);
        next = edit.range.end;
    }
    Ok(PreparedEdits {
        base_revision: snapshot.revision(),
        range: span,
        text,
        count: edits.len(),
    })
}

/// What a host asks of the editor, in order with the keys before it.
#[derive(Clone, Debug, PartialEq)]
pub enum HostCommand {
    PreventEditing(bool),
    Wrap(bool),
    Font {
        family: String,
        points: f32,
    },
    /// `None` wraps to the view's width again.
    PageWrap(Option<PageWrap>),
    /// Space for a gutter and a document map.
    Margins {
        left: Dip,
        right: Dip,
    },
    ScrollTo(f64),
    Select {
        range: Range<ByteOffset>,
        reveal: bool,
    },
    /// One edit and one undo step, with the selection the caller chooses after it.
    ApplyEdits {
        edits: Arc<PreparedEdits>,
        selection: Range<ByteOffset>,
    },
}

/// A store's snapshot with a revision value that names its document.
struct NamedSnapshot {
    text: Arc<dyn TextSnapshot>,
    revision: u64,
}
impl TextSnapshot for NamedSnapshot {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_bytes(&self) -> ByteOffset {
        self.text.len_bytes()
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.text.len_utf16()
    }
    fn line_count(&self) -> usize {
        self.text.line_count()
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.text.byte_to_utf16(byte)
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        self.text.utf16_to_byte(utf16)
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.text.byte_to_position(byte)
    }
    fn position_to_byte(&self, position: TextPosition) -> Option<ByteOffset> {
        self.text.position_to_byte(position)
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        self.text.line_range(line)
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        self.text.visit_chunks(range, visitor)
    }
    fn is_read_only(&self) -> bool {
        self.text.is_read_only()
    }
    fn status(&self) -> StoreStatus {
        self.text.status()
    }
    fn try_known_line(&self, line: LineIndex) -> Result<KnownLine, LookupError> {
        self.text.try_known_line(line)
    }
    fn try_line_ending_len(&self, line: LineIndex) -> Result<usize, LookupError> {
        self.text.try_line_ending_len(line)
    }
    fn visit_chunks_while(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str) -> bool,
    ) -> bool {
        self.text.visit_chunks_while(range, visitor)
    }
    fn try_len_bytes(&self) -> Result<ByteOffset, LookupError> {
        self.text.try_len_bytes()
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        self.text.try_len_utf16()
    }
    fn try_line_count(&self) -> Result<usize, LookupError> {
        self.text.try_line_count()
    }
    fn try_byte_to_utf16(&self, byte: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.text.try_byte_to_utf16(byte)
    }
    fn try_utf16_to_byte(&self, utf16: Utf16Offset) -> Result<ByteOffset, LookupError> {
        self.text.try_utf16_to_byte(utf16)
    }
    fn try_byte_to_position(&self, byte: ByteOffset) -> Result<TextPosition, LookupError> {
        self.text.try_byte_to_position(byte)
    }
    fn try_position_to_byte(&self, position: TextPosition) -> Result<ByteOffset, LookupError> {
        self.text.try_position_to_byte(position)
    }
    fn try_line_range(&self, line: LineIndex) -> Result<Range<ByteOffset>, LookupError> {
        self.text.try_line_range(line)
    }
    fn try_visit_chunks(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        self.text.try_visit_chunks(range, visitor)
    }
}

/// The hooks' state inside the view.
pub(crate) struct HookState {
    /// Changes in order, and the revision before the first of them.
    log: VecDeque<TrackedChange>,
    log_base: Option<u64>,
    /// Goes up each time another document replaces this one.
    pub(crate) document: u64,
    decorations: [RangeList; KINDS],
    pub(crate) composition: Option<Composition>,
    pub(crate) outer: Rect,
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) page: Option<PageWrap>,
    pub(crate) no_background: bool,
    pub(crate) next_task: u64,
    pub(crate) test_refuse_edit: bool,
    /// A write ended with other text than the view holds, so the history's save point
    /// no longer names what is on disk.
    pub(crate) save_point_gone: bool,
    /// A layout, a width or a font changed, so the composition lays out again.
    pub(crate) composition_stale: bool,
    pub(crate) composition_end: Option<CompositionEnd>,
}
impl Default for HookState {
    fn default() -> Self {
        Self {
            log: VecDeque::new(),
            log_base: None,
            document: new_document(),
            decorations: Default::default(),
            composition: None,
            outer: Rect::default(),
            left: 0.0,
            right: 0.0,
            page: None,
            no_background: false,
            next_task: 0,
            test_refuse_edit: false,
            save_point_gone: false,
            composition_stale: false,
            composition_end: None,
        }
    }
}

impl EditorView {
    // ---- Reading text ----

    /// A snapshot for worker threads. It never changes. On a mapped file its plain queries
    /// may wait for disk, so it must not be used on the UI thread. Use its `try_` queries
    /// there, which never wait.
    pub fn worker_snapshot(&self) -> Arc<dyn TextSnapshot> {
        let text = match &self.mapped {
            Some(mapped) => mapped.snapshot(),
            None => self.store.snapshot(),
        };
        Arc::new(NamedSnapshot {
            text,
            revision: self.revision(),
        })
    }
    /// Changes with each edit, and names this document. Values from other documents and
    /// other views never match. A mapped file keeps one value while its index grows, so
    /// read `store_status` to learn how much of it is known.
    pub fn revision(&self) -> u64 {
        let edits = if self.mapped.is_some() {
            0
        } else {
            self.store.revision()
        };
        self.revision_in_this_document(edits)
    }
    fn revision_in_this_document(&self, edits: u64) -> u64 {
        (self.hooks.document << REVISION_BITS) | (edits & ((1 << REVISION_BITS) - 1))
    }
    /// Differs for each view and goes up when another document replaces this one. Its text
    /// is then all new. `revision` already holds this number.
    pub fn document_number(&self) -> u64 {
        self.hooks.document
    }
    /// What changed after `revision`, for a search or a spell check that stays current.
    pub fn changes_since(&self, revision: u64) -> ChangesSince {
        if revision == self.revision() {
            return ChangesSince::Current;
        }
        let Some(base) = self.hooks.log_base else {
            return ChangesSince::Rescan;
        };
        // A value from another document is older than this one's or newer than it. Both
        // fall outside this range, because the document's number is in the high bits.
        if revision < base || revision > self.revision() {
            return ChangesSince::Rescan;
        }
        ChangesSince::Changes(
            self.hooks
                .log
                .iter()
                .filter(|c| c.change.revision > revision)
                .cloned()
                .collect(),
        )
    }
    pub fn byte_to_utf16(&self, byte: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.source().try_byte_to_utf16(byte)
    }
    pub fn utf16_to_byte(&self, utf16: Utf16Offset) -> Result<ByteOffset, LookupError> {
        self.source().try_utf16_to_byte(utf16)
    }
    /// The selection in UTF-16 units, or `None` when the store cannot say yet.
    pub(crate) fn selection_utf16(&self) -> Option<Range<usize>> {
        let selection = self.selection();
        let start = self.byte_to_utf16(selection.start).ok()?;
        let end = self.byte_to_utf16(selection.end).ok()?;
        Some(start.0..end.0)
    }
    /// Called by `after_change`, once the store holds the new text.
    pub(crate) fn note_change(&mut self, old: &dyn TextSnapshot, change: &qe_api::TextChange) {
        let revision = self.revision();
        let before = self.revision_in_this_document(old.revision());
        for list in &mut self.hooks.decorations {
            if !list.is_empty() {
                list.edit(
                    change.replaced.start.0,
                    change.replaced.end.0,
                    change.new_len,
                );
            }
        }
        let new_end = ByteOffset(change.replaced.start.0 + change.new_len);
        let utf16 = (
            old.try_byte_to_utf16(change.replaced.start),
            old.try_byte_to_utf16(change.replaced.end),
            self.source().try_byte_to_utf16(new_end),
        );
        let (Ok(start), Ok(old_end), Ok(new_end)) = utf16 else {
            // A change the log cannot describe: readers scan again.
            self.hooks.log.clear();
            self.hooks.log_base = Some(revision);
            return;
        };
        if self.hooks.log_base.is_none() {
            self.hooks.log_base = Some(before);
        }
        let mut change = change.clone();
        change.revision = revision;
        self.hooks.log.push_back(TrackedChange {
            change,
            utf16: Utf16Change {
                start,
                old_end,
                new_end,
            },
        });
        if self.hooks.log.len() > CHANGE_LOG
            && let Some(dropped) = self.hooks.log.pop_front()
        {
            self.hooks.log_base = Some(dropped.change.revision);
        }
    }
    /// Called by `adopt`: nothing known about the old text applies to the new.
    pub(crate) fn forget_document(&mut self) {
        self.hooks.document = new_document();
        self.hooks.save_point_gone = false;
        self.hooks.composition_stale = false;
        if self.hooks.composition.is_some() {
            self.hooks.composition_end = Some(CompositionEnd::Cancelled);
        }
        self.hooks.log.clear();
        self.hooks.log_base = None;
        self.hooks.decorations = Default::default();
        self.hooks.composition = None;
    }

    // ---- Word and line queries ----

    /// The word or the run of other text at a byte, within its line.
    pub fn word_range_at(&mut self, byte: ByteOffset) -> Result<Range<ByteOffset>, LookupError> {
        self.with_stops(|stops| stops.word_at(byte.0, false))
            .map(|range| ByteOffset(range.start)..ByteOffset(range.end))
    }
    /// The bytes of a line, with its line ending.
    pub fn line_range(&self, line: LineIndex) -> Result<Range<ByteOffset>, LookupError> {
        self.source().try_line_range(line)
    }
    pub fn line_of_byte(&self, byte: ByteOffset) -> Result<LineIndex, LookupError> {
        // On a mapped file a page out of memory cannot answer, and a layout on hand can.
        match self.source().try_byte_to_position(byte) {
            Ok(position) => Ok(position.line),
            Err(error) => self.line_from_layouts(byte).map(LineIndex).ok_or(error),
        }
    }

    // ---- Geometry ----

    /// The window x of the text's left edge in a document with `dx` of its own.
    fn window_x(&self, dx: f64) -> f32 {
        self.text_left() + (dx - self.scroll_x) as f32
    }
    fn window_y(&self, doc_y: f64) -> f32 {
        self.bounds.y.0 + (doc_y - self.scroll) as f32
    }
    fn cached_whole(&self, line: usize) -> Option<Arc<LineLayout>> {
        let snapshot = self.snapshot();
        self.cache.borrow_mut().cached(
            &self.engine,
            snapshot.as_ref(),
            LineIndex(line),
            self.width(),
        )
    }
    fn cached_part(&self, line: usize, segment: usize) -> Option<Arc<LineLayout>> {
        let snapshot = self.snapshot();
        self.cache.borrow_mut().cached_segment(
            &self.engine,
            snapshot.as_ref(),
            LineIndex(line),
            segment,
            self.width(),
        )
    }
    /// A long line's segment as an offset from the line's start: down with wrap on, and
    /// to the right with wrap off.
    fn segment_offset(&self, line: usize, segment: usize) -> Option<(f64, f64)> {
        let along = self
            .long_maps
            .get(&line)?
            .line_to_scroll(LineIndex(segment));
        Some(if self.wrap {
            (0.0, along)
        } else {
            (along, 0.0)
        })
    }
    /// The composition's layout, if it stands in for this line or segment.
    fn composed(&self, line: usize, segment: Option<usize>) -> Option<(&Composition, &LineLayout)> {
        let composition = self.hooks.composition.as_ref()?;
        let layout = composition.layout.as_ref()?;
        (composition.line == line && composition.segment == segment)
            .then_some((composition, &**layout))
    }
    fn composed_rects(
        &self,
        composition: &Composition,
        layout: &LineLayout,
        range: &Range<ByteOffset>,
        out: &mut Vec<RangeRect>,
    ) {
        let (x, y) = self.composition_origin(composition);
        for part in composition.shown.in_layout(range) {
            for row in &layout.rows {
                for (start, end, right_to_left, inside) in row_spans(row, &part) {
                    out.push(RangeRect {
                        rect: Rect {
                            x: Dip(x + start),
                            y: Dip(y + row.y.0),
                            width: Dip((end - start).max(1.0)),
                            height: row.height,
                        },
                        right_to_left,
                        range: composition.shown.doc_of(inside.start)
                            ..composition.shown.doc_end_of(inside.end),
                    });
                }
            }
        }
    }
    /// The rectangles of a range: one for each row with one direction, and one for each
    /// run on a row with mixed directions. An empty range gives the caret's place.
    pub fn range_rects(&self, range: Range<ByteOffset>) -> Result<Vec<RangeRect>, GeometryError> {
        let source = self.source();
        let first = self.line_of_byte(range.start)?.0;
        let last = self.line_of_byte(range.end)?.0;
        if last - first >= RECT_LINES {
            return Err(GeometryError::TooManyLines { most: RECT_LINES });
        }
        let snapshot = self.snapshot();
        let mut rects = Vec::new();
        for line in first..=last {
            let top = self.heights.line_to_scroll(LineIndex(line));
            if self.long_maps.contains_key(&line) {
                let segment_of = |byte| {
                    self.engine
                        .segment_at(snapshot.as_ref(), byte, Affinity::After)
                        .map(|(_, segment, _)| segment)
                        .map_err(|_| GeometryError::NotLaidOut)
                };
                let line_range = source.try_line_range(LineIndex(line))?;
                let from = segment_of(range.start.max(line_range.start))?;
                let to = segment_of(range.end.min(line_range.end))?;
                for segment in from..=to {
                    if let Some((composition, layout)) = self.composed(line, Some(segment)) {
                        self.composed_rects(composition, layout, &range, &mut rects);
                        continue;
                    }
                    let layout = self
                        .cached_part(line, segment)
                        .ok_or(GeometryError::NotLaidOut)?;
                    let (dx, dy) = self.segment_offset(line, segment).unwrap_or((0.0, 0.0));
                    self.layout_rects(&layout, &range, dx, top + dy, &mut rects);
                }
            } else if let Some((composition, layout)) = self.composed(line, None) {
                self.composed_rects(composition, layout, &range, &mut rects);
            } else {
                let layout = self.cached_whole(line).ok_or(GeometryError::NotLaidOut)?;
                self.layout_rects(&layout, &range, 0.0, top, &mut rects);
            }
        }
        Ok(rects)
    }
    fn layout_rects(
        &self,
        layout: &LineLayout,
        range: &Range<ByteOffset>,
        dx: f64,
        doc_y: f64,
        out: &mut Vec<RangeRect>,
    ) {
        for row in &layout.rows {
            for (start, end, right_to_left, part) in row_spans(row, range) {
                out.push(RangeRect {
                    rect: Rect {
                        x: Dip(self.window_x(dx) + start),
                        y: Dip(self.window_y(doc_y) + row.y.0),
                        width: Dip((end - start).max(1.0)),
                        height: row.height,
                    },
                    right_to_left,
                    range: part,
                });
            }
        }
    }
    /// The caret in window coordinates, at the composition's cursor while one shows.
    pub fn caret_rect(&self) -> Result<Rect, GeometryError> {
        if let Some(found) = self.composition_place() {
            return Ok(found);
        }
        let Some(place) = self.place(self.caret, self.affinity) else {
            return Err(self
                .source()
                .try_byte_to_position(self.caret)
                .err()
                .map_or(GeometryError::NotLaidOut, GeometryError::Lookup));
        };
        if !place.exact {
            return Err(GeometryError::NotLaidOut);
        }
        Ok(Rect {
            x: Dip(self.window_x(place.x)),
            y: Dip(self.window_y(place.y)),
            width: Dip(1.0),
            height: Dip(place.height),
        })
    }
    /// The byte under a point in window coordinates, from layouts already made.
    pub fn byte_at_point(&self, at: Point) -> Result<HitTest, GeometryError> {
        let x = f64::from(at.x.0 - self.text_left()) + self.scroll_x;
        let y = f64::from(at.y.0 - self.bounds.y.0) + self.scroll;
        if y >= self.heights.total_height() {
            return Ok(HitTest {
                byte: self.source().status().known_bytes,
                affinity: Affinity::After,
            });
        }
        let y = y.max(0.0);
        let line = self.heights.scroll_to_line(y);
        let local = y - self.heights.line_to_scroll(line);
        let point = |x: f64, y: f64| Point {
            x: Dip(x as f32),
            y: Dip(y as f32),
        };
        if let Some(map) = self.long_maps.get(&line.0) {
            let along = if self.wrap { local } else { x };
            let segment = map.scroll_to_line(along).0;
            let start = map.line_to_scroll(LineIndex(segment));
            let inside = if self.wrap {
                point(x, local - start)
            } else {
                point(x - start, local)
            };
            if let Some((composition, layout)) = self.composed(line.0, Some(segment)) {
                return Ok(composed_hit(composition, layout, inside));
            }
            let layout = self
                .cached_part(line.0, segment)
                .ok_or(GeometryError::NotLaidOut)?;
            return Ok(layout.hit_test_point(inside));
        }
        if let Some((composition, layout)) = self.composed(line.0, None) {
            return Ok(composed_hit(composition, layout, point(x, local)));
        }
        let layout = self.cached_whole(line.0).ok_or(GeometryError::NotLaidOut)?;
        Ok(layout.hit_test_point(point(x, local)))
    }
    /// The rows on screen, from layouts already made. Lines with no layout are left out,
    /// and `complete` is then false.
    pub fn visible_rows(&self) -> VisibleRows {
        let mut answer = VisibleRows {
            rows: Vec::new(),
            complete: true,
        };
        let view_bottom = self.scroll + f64::from(self.bounds.height.0);
        let known_lines = self.source().status().known_lines;
        let mut line = self.heights.scroll_to_line(self.scroll).0;
        let mut line_top = self.heights.line_to_scroll(LineIndex(line));
        while line < known_lines && line_top < view_bottom {
            let height = self
                .heights
                .line_height(LineIndex(line))
                .unwrap_or(Dip(self.row_height));
            if let Some(map) = self.long_maps.get(&line) {
                let along = if self.wrap {
                    (self.scroll - line_top).max(0.0)
                } else {
                    self.scroll_x
                };
                let start = map.scroll_to_line(along).0;
                let last = if self.wrap {
                    start + VISIBLE_SEGMENTS
                } else {
                    start + 1
                };
                for segment in start..map.len().min(last) {
                    let dy = if self.wrap {
                        map.line_to_scroll(LineIndex(segment))
                    } else {
                        0.0
                    };
                    if line_top + dy >= view_bottom {
                        break;
                    }
                    if let Some((composition, layout)) = self.composed(line, Some(segment)) {
                        self.push_composed_rows(
                            composition,
                            layout,
                            line,
                            line_top + dy,
                            segment > 0,
                            &mut answer.rows,
                        );
                        continue;
                    }
                    let Some(layout) = self.cached_part(line, segment) else {
                        answer.complete = false;
                        continue;
                    };
                    self.push_rows(&layout, line, line_top + dy, segment > 0, &mut answer.rows);
                }
            } else if let Some((composition, layout)) = self.composed(line, None) {
                self.push_composed_rows(
                    composition,
                    layout,
                    line,
                    line_top,
                    false,
                    &mut answer.rows,
                );
            } else if let Some(layout) = self.cached_whole(line) {
                self.push_rows(&layout, line, line_top, false, &mut answer.rows);
            } else {
                answer.complete = false;
            }
            line += 1;
            line_top += f64::from(height.0);
        }
        answer
    }
    fn push_composed_rows(
        &self,
        composition: &Composition,
        layout: &LineLayout,
        line: usize,
        top: f64,
        later_segment: bool,
        rows: &mut Vec<VisibleRow>,
    ) {
        let first = rows.len();
        self.push_rows(layout, line, top, later_segment, rows);
        for row in &mut rows[first..] {
            row.first_byte = composition.shown.doc_of(row.first_byte);
        }
    }
    fn push_rows(
        &self,
        layout: &LineLayout,
        line: usize,
        top: f64,
        later_segment: bool,
        rows: &mut Vec<VisibleRow>,
    ) {
        let view_bottom = self.scroll + f64::from(self.bounds.height.0);
        for (index, row) in layout.rows.iter().enumerate() {
            let y = top + f64::from(row.y.0);
            if y + f64::from(row.height.0) <= self.scroll || y >= view_bottom {
                continue;
            }
            rows.push(VisibleRow {
                line: LineIndex(line),
                first_byte: row.range.start,
                y: Dip(self.window_y(y)),
                height: row.height,
                continuation: index > 0 || later_segment,
            });
        }
    }
    /// The bytes from the first row on screen to the end of the last one.
    pub fn visible_range(&self) -> Result<Range<ByteOffset>, GeometryError> {
        let answer = self.visible_rows();
        if !answer.complete {
            return Err(GeometryError::NotLaidOut);
        }
        let first = answer.rows.first().ok_or(GeometryError::NotLaidOut)?;
        let last = answer.rows.last().ok_or(GeometryError::NotLaidOut)?;
        let line = self.source().try_line_range(last.line)?;
        let end = self.row_end(last).unwrap_or(line.end);
        Ok(first.first_byte..end)
    }
    fn row_end(&self, row: &VisibleRow) -> Option<ByteOffset> {
        let look = |layout: &LineLayout| {
            layout
                .rows
                .iter()
                .find(|r| r.range.start == row.first_byte)
                .map(|r| r.range.end)
        };
        if let Some(composition) = &self.hooks.composition
            && composition.line == row.line.0
            && let Some(layout) = &composition.layout
        {
            let shown = &composition.shown;
            return layout
                .rows
                .iter()
                .find(|r| shown.doc_of(r.range.start) == row.first_byte)
                .map(|r| shown.doc_of(r.range.end));
        }
        if self.long_maps.contains_key(&row.line.0) {
            let snapshot = self.snapshot();
            let (_, segment, _) = self
                .engine
                .segment_at(snapshot.as_ref(), row.first_byte, Affinity::After)
                .ok()?;
            look(&*self.cached_part(row.line.0, segment)?)
        } else {
            look(&*self.cached_whole(row.line.0)?)
        }
    }

    // ---- Decorations ----

    /// Replaces all the marks of one kind. Ranges may come in any order. Overlapping
    /// ranges join, and empty ones go. It repaints only the marks on screen.
    pub fn set_decorations(
        &mut self,
        kind: DecorationKind,
        ranges: Vec<Range<ByteOffset>>,
        ctx: &mut ViewContext,
    ) {
        self.repaint_decorations(kind, ctx);
        self.hooks.decorations[kind.index()]
            .set(ranges.into_iter().map(|r| r.start.0..r.end.0).collect());
        self.repaint_decorations(kind, ctx);
        self.finish(Instant::now(), ctx);
    }
    /// The marks of one kind as they stand after the edits since they were set.
    pub fn decorations(&self, kind: DecorationKind) -> Vec<Range<ByteOffset>> {
        self.hooks.decorations[kind.index()]
            .to_vec()
            .into_iter()
            .map(|r| ByteOffset(r.start)..ByteOffset(r.end))
            .collect()
    }
    fn repaint_decorations(&self, kind: DecorationKind, ctx: &mut ViewContext) {
        let list = &self.hooks.decorations[kind.index()];
        if list.is_empty() {
            return;
        }
        // Each line on screen with a mark repaints, and no line off screen costs anything.
        let view_bottom = self.scroll + f64::from(self.bounds.height.0);
        let known_lines = self.source().status().known_lines;
        let mut line = self.heights.scroll_to_line(self.scroll).0;
        let mut top = self.heights.line_to_scroll(LineIndex(line));
        while line < known_lines && top < view_bottom {
            if let Ok(range) = self.source().try_line_range(LineIndex(line)) {
                let mut marked = false;
                list.visit_overlaps(range.start.0..range.end.0.max(range.start.0 + 1), |_| {
                    marked = true;
                });
                if marked {
                    self.repaint_line(line, ctx);
                }
            }
            top += f64::from(
                self.heights
                    .line_height(LineIndex(line))
                    .unwrap_or(Dip(self.row_height))
                    .0,
            );
            line += 1;
        }
    }
    /// Paints what goes behind a layout's text. Its cost follows the marks on these rows.
    pub(crate) fn paint_under(
        &self,
        list: &mut DisplayList,
        layout: &LineLayout,
        theme: &Theme,
        x: f32,
        y: f32,
    ) {
        for (kind, color) in [
            (DecorationKind::FindMatch, theme.find_match),
            (DecorationKind::CurrentFindMatch, theme.current_find_match),
        ] {
            self.visit_marks(kind, layout, |row, range| {
                for (start, end, _, _) in row_spans(row, &range) {
                    list.push(DrawCommand::FillRect {
                        rect: Rect {
                            x: Dip(x + start),
                            y: Dip(y + row.y.0),
                            width: Dip((end - start).max(1.0)),
                            height: row.height,
                        },
                        color,
                    });
                }
            });
        }
    }
    /// Paints what goes over a layout's text.
    pub(crate) fn paint_over(
        &self,
        list: &mut DisplayList,
        layout: &LineLayout,
        theme: &Theme,
        x: f32,
        y: f32,
    ) {
        self.visit_marks(DecorationKind::SpellingUnderline, layout, |row, range| {
            for (start, end, _, _) in row_spans(row, &range) {
                let under = y + row.y.0 + row.height.0 - 2.0;
                list.push(DrawCommand::WavyUnderline {
                    from: Point {
                        x: Dip(x + start),
                        y: Dip(under),
                    },
                    to: Point {
                        x: Dip(x + end),
                        y: Dip(under),
                    },
                    color: theme.spelling_underline,
                });
            }
        });
        self.visit_marks(
            DecorationKind::CompositionUnderline,
            layout,
            |row, range| {
                for (start, end, _, _) in row_spans(row, &range) {
                    push_straight_underline(list, row, x + start, x + end, y, theme);
                }
            },
        );
    }
    fn visit_marks(
        &self,
        kind: DecorationKind,
        layout: &LineLayout,
        mut visit: impl FnMut(&LayoutRow, Range<ByteOffset>),
    ) {
        let list = &self.hooks.decorations[kind.index()];
        if list.is_empty() {
            return;
        }
        for row in &layout.rows {
            list.visit_overlaps(row.range.start.0..row.range.end.0, |range| {
                visit(row, ByteOffset(range.start)..ByteOffset(range.end));
            });
        }
    }

    // ---- Input method ----

    /// True while composition text waits to be committed or cancelled.
    pub fn composition(&self) -> Option<CompositionState> {
        self.hooks
            .composition
            .as_ref()
            .map(|composition| CompositionState {
                text: composition.text.clone(),
                cursor_utf16: composition.cursor_utf16,
                at: composition.at,
                showing: composition.layout.is_some(),
                current: composition.layout.is_some() && composition.task.is_none(),
            })
    }
    /// Says once how the last composition ended, so that a host can tell an input method
    /// whether its text went in. `ViewRequest::CancelComposition` asks the host to look.
    pub fn take_composition_end(&mut self) -> Option<CompositionEnd> {
        self.hooks.composition_end.take()
    }
    /// The parts of the composition that the input method marks.
    pub fn composition_clauses(&self) -> Vec<CompositionClause> {
        self.hooks
            .composition
            .as_ref()
            .map_or_else(Vec::new, |composition| composition.clauses.clone())
    }
    /// Sets the parts to underline. One clause of a composition's text gets one line, and
    /// the target clause gets a heavier one. It costs one repaint of the line.
    pub fn ime_set_clauses(&mut self, clauses: Vec<CompositionClause>, ctx: &mut ViewContext) {
        let Some(composition) = self.hooks.composition.as_mut() else {
            return;
        };
        if composition.clauses == clauses {
            return;
        }
        composition.clauses = clauses;
        let line = composition.line;
        self.repaint_line(line, ctx);
        self.finish(Instant::now(), ctx);
    }
    /// The rectangles of part of the composition, in UTF-16 units of its text, in window
    /// coordinates. An empty range gives the place between two characters. It says
    /// `NotLaidOut` while the text waits for a layout, because the old one has other text.
    pub fn composition_range_rects(
        &self,
        range: Range<usize>,
    ) -> Result<Vec<RangeRect>, GeometryError> {
        let composition = self
            .hooks
            .composition
            .as_ref()
            .ok_or(GeometryError::NotLaidOut)?;
        let layout = composition
            .layout
            .as_ref()
            .ok_or(GeometryError::NotLaidOut)?;
        if composition.task.is_some() {
            return Err(GeometryError::NotLaidOut);
        }
        let bytes = composed_bytes(composition, &range);
        let (x, y) = self.composition_origin(composition);
        let mut rects = Vec::new();
        for row in &layout.rows {
            for (start, end, right_to_left, part) in row_spans(row, &bytes) {
                rects.push(RangeRect {
                    rect: Rect {
                        x: Dip(x + start),
                        y: Dip(y + row.y.0),
                        width: Dip((end - start).max(1.0)),
                        height: row.height,
                    },
                    right_to_left,
                    range: part,
                });
            }
        }
        Ok(rects)
    }
    /// The UTF-16 place in the composition's text under a point, if the point is on the text.
    pub fn composition_place_at_point(&self, at: Point) -> Option<usize> {
        let composition = self.hooks.composition.as_ref()?;
        let layout = composition.layout.as_ref()?;
        let (x, y) = self.composition_origin(composition);
        let hit = layout.hit_test_point(Point {
            x: Dip(at.x.0 - x),
            y: Dip(at.y.0 - y),
        });
        let inside = at.y.0 >= y
            && at.y.0 < y + layout.height.0
            && composition.shown.text.start <= hit.byte
            && hit.byte <= composition.shown.text.end
            && !composition.shown.text.is_empty();
        if !inside {
            return None;
        }
        let bytes = hit.byte.0 - composition.shown.text.start.0;
        Some(utf16_of_shown(&composition.text, bytes))
    }
    /// Puts the composition's text in the store, as its own edit, before something else
    /// changes the text or the caret. Nothing an input method typed is dropped on its own.
    pub(crate) fn commit_composition_first(
        &mut self,
        ctx: &mut ViewContext,
    ) -> Option<CompositionCommit> {
        let text = self.hooks.composition.as_ref()?.text.clone();
        self.end_composition(ctx);
        ctx.cancel_composition();
        if text.is_empty() {
            self.hooks.composition_end = Some(CompositionEnd::Cancelled);
            return None;
        }
        if !self.can_edit() {
            self.hooks.composition_end = Some(CompositionEnd::Cancelled);
            self.refused_edits += 1;
            self.set_warning(
                format!("This view cannot be edited, so this text was not added: {text}"),
                false,
            );
            return None;
        }
        let range = self.selection();
        self.touch();
        self.history.break_group();
        self.apply_edit(range.clone(), &text, false, ctx);
        self.history.break_group();
        self.hooks.composition_end = Some(CompositionEnd::Committed {
            revision: self.revision(),
        });
        Some(CompositionCommit {
            start: range.start.0,
            old_end: range.end.0,
            new_len: text.len(),
        })
    }
    /// Sets the selection with its caret at either end, with no move of the view.
    pub fn select_directed(
        &mut self,
        anchor: ByteOffset,
        caret: ByteOffset,
        ctx: &mut ViewContext,
    ) {
        let known = self.source().status().known_bytes.0;
        let edge = |view: &mut Self, at: ByteOffset| {
            let at = at.0.min(known);
            view.with_stops(|s| s.edge_backward(at, 0))
                .map_or(ByteOffset(at), ByteOffset)
        };
        let (anchor, caret) = (edge(self, anchor), edge(self, caret));
        self.set_selection(anchor, caret, Affinity::After, false, ctx);
        self.finish(Instant::now(), ctx);
    }
    /// The composition's rectangles in window coordinates, for a candidate list.
    pub fn composition_rects(&self) -> Result<Vec<RangeRect>, GeometryError> {
        let composition = self
            .hooks
            .composition
            .as_ref()
            .ok_or(GeometryError::NotLaidOut)?;
        let layout = composition
            .layout
            .as_ref()
            .ok_or(GeometryError::NotLaidOut)?;
        let (x, y) = self.composition_origin(composition);
        let mut rects = Vec::new();
        for row in &layout.rows {
            for (start, end, right_to_left, part) in row_spans(row, &composition.shown.text) {
                rects.push(RangeRect {
                    rect: Rect {
                        x: Dip(x + start),
                        y: Dip(y + row.y.0),
                        width: Dip((end - start).max(1.0)),
                        height: row.height,
                    },
                    right_to_left,
                    range: part,
                });
            }
        }
        Ok(rects)
    }
    pub(crate) fn composition_origin(&self, composition: &Composition) -> (f32, f32) {
        let top = self.heights.line_to_scroll(LineIndex(composition.line));
        let (dx, dy) = composition
            .segment
            .and_then(|segment| self.segment_offset(composition.line, segment))
            .unwrap_or((0.0, 0.0));
        (self.window_x(dx), self.window_y(top + dy))
    }
    fn composition_place(&self) -> Option<Rect> {
        let composition = self.hooks.composition.as_ref()?;
        let layout = composition.layout.as_ref()?;
        let rect = layout.caret_rect(composition.shown.cursor, Affinity::After)?;
        let (x, y) = self.composition_origin(composition);
        Some(Rect {
            x: Dip(x + rect.x.0),
            y: Dip(y + rect.y.0),
            width: rect.width,
            height: rect.height,
        })
    }
    /// Starts a composition at the selection. The store and the selection do not change
    /// until a commit, so a cancel has nothing to restore.
    pub fn ime_start(&mut self) {
        if !self.can_edit() {
            self.refused_edits += 1;
            return;
        }
        if self.hooks.composition.is_some() {
            return;
        }
        self.touch();
        let at = self.selection().start;
        let line = self.line_at(at).unwrap_or(0);
        self.hooks.composition = Some(Composition {
            text: String::new(),
            cursor_utf16: 0,
            at,
            line,
            segment: None,
            layout: None,
            shown: ComposedBytes::default(),
            wanted: ComposedBytes::default(),
            task: None,
            old_height: None,
            clauses: Vec::new(),
        });
    }
    /// Shows `text` at the composition's place. A worker lays it out, and until then the
    /// last text that was laid out stays on screen.
    pub fn ime_update(&mut self, text: &str, cursor_utf16: usize, ctx: &mut ViewContext) {
        if self.hooks.composition.is_none() {
            self.ime_start();
        }
        let selection = self.selection();
        let Some((line, at)) = self
            .hooks
            .composition
            .as_ref()
            .map(|composition| (composition.line, composition.at))
        else {
            return;
        };
        let snapshot = self.snapshot();
        // A long line shows its composition inside one segment, so its cost has a limit.
        // This asks the engine, because a line's map may not have arrived yet.
        let long = self
            .engine
            .is_long_line(snapshot.as_ref(), LineIndex(line))
            .unwrap_or(false);
        let (segment, window) = if long {
            match self
                .engine
                .segment_at(snapshot.as_ref(), at, Affinity::After)
            {
                Ok((_, segment, range)) => (Some(segment), range),
                Err(error) => {
                    self.set_warning(format!("The composition cannot show: {error}"), false);
                    return;
                }
            }
        } else {
            match self.source().try_line_range(LineIndex(line)) {
                Ok(range) => (None, range),
                Err(error) => {
                    self.set_warning(format!("The composition cannot show: {error:?}"), false);
                    return;
                }
            }
        };
        // A selection inside this line gives way on screen. One that goes past it stays
        // as it is, because the lines it covers still have to paint.
        let resume = if selection.end <= window.end {
            selection.end
        } else {
            at
        };
        // A composition is one line of text, whatever it holds.
        let shown = shown_composition_text(text);
        let mut full = String::new();
        let visited = self
            .source()
            .try_visit_chunks(window.start..at, &mut |s| full.push_str(s))
            .is_ok();
        let text_start = ByteOffset(full.len());
        full.push_str(&shown);
        let text_end = ByteOffset(full.len());
        let tail = self
            .source()
            .try_visit_chunks(resume..window.end, &mut |s| full.push_str(s))
            .is_ok();
        if !visited || !tail {
            self.set_warning("The composition cannot show here.".to_string(), false);
            return;
        }
        self.hooks.next_task += 1;
        let task = self.hooks.next_task;
        let store = PieceStore::new(&full);
        let width = self.width();
        let cursor = ByteOffset(text_start.0 + byte_of_utf16(&shown, cursor_utf16));
        let sent = self.tasks.send(crate::WorkerTask::Composition(
            task,
            store.snapshot(),
            self.engine.clone(),
            width,
            self.wrap,
        ));
        if let Err(error) = sent {
            self.set_warning(format!("The layout workers stopped: {error}"), true);
            return;
        }
        if let Some(composition) = self.hooks.composition.as_mut() {
            composition.text = text.to_string();
            composition.cursor_utf16 = cursor_utf16;
            composition.segment = segment;
            composition.task = Some(task);
            composition.wanted = ComposedBytes {
                window_start: window.start.0,
                at: at.0,
                resume: resume.0,
                text: text_start..text_end,
                cursor,
            };
        }
        self.pending = true;
        self.repaint_line(line, ctx);
        self.finish(Instant::now(), ctx);
    }
    /// A restart made the composition's layout useless. It lays out again, at the new
    /// width and font. A task still on its way is for the old ones, so it is forgotten.
    pub(crate) fn composition_after_restart(&mut self) {
        if let Some(composition) = self.hooks.composition.as_mut() {
            composition.layout = None;
            composition.task = None;
            composition.old_height = None;
            self.hooks.composition_stale = true;
        }
    }
    pub(crate) fn lay_out_stale_composition(&mut self, ctx: &mut ViewContext) {
        if !std::mem::take(&mut self.hooks.composition_stale) {
            return;
        }
        let Some((text, cursor)) = self
            .hooks
            .composition
            .as_ref()
            .map(|c| (c.text.clone(), c.cursor_utf16))
        else {
            return;
        };
        if !text.is_empty() {
            self.ime_update(&text, cursor, ctx);
        }
    }
    /// Called with each finished composition layout.
    pub(crate) fn note_composition(
        &mut self,
        task: u64,
        layout: Result<LineLayout, String>,
        ctx: &mut ViewContext,
    ) {
        let wanted = self
            .hooks
            .composition
            .as_ref()
            .is_some_and(|c| c.task == Some(task));
        if !wanted {
            return;
        }
        let layout = match layout {
            Ok(layout) => Arc::new(layout),
            Err(problem) => {
                // Nothing more will come for this task. The last good layout stays.
                if let Some(composition) = self.hooks.composition.as_mut() {
                    composition.task = None;
                }
                self.set_warning(format!("The composition cannot show: {problem}"), false);
                return;
            }
        };
        let height = layout.height;
        let mut line = 0;
        let mut whole_line = false;
        if let Some(composition) = self.hooks.composition.as_mut() {
            composition.task = None;
            composition.layout = Some(layout);
            composition.shown = composition.wanted.clone();
            line = composition.line;
            whole_line = composition.segment.is_none();
        }
        if whole_line && self.wrap {
            let old = self.heights.line_height(LineIndex(line));
            if let Some(composition) = self.hooks.composition.as_mut()
                && composition.old_height.is_none()
            {
                composition.old_height = old;
            }
            self.set_line_height(line, height, ctx);
        }
        self.repaint_line_down(line, ctx);
        self.reveal_composition(ctx);
    }
    /// Scrolls just enough to show the composition's cursor.
    fn reveal_composition(&mut self, ctx: &mut ViewContext) {
        let Some(rect) = self.composition_place() else {
            return;
        };
        let (old_y, old_x) = (self.scroll, self.scroll_x);
        let height = f64::from(self.bounds.height.0);
        let width = f64::from(self.bounds.width.0);
        let top = f64::from(rect.y.0 - self.bounds.y.0);
        let bottom = top + f64::from(rect.height.0);
        if top < 0.0 {
            self.scroll += top;
        } else if bottom > height && height > 0.0 {
            self.scroll += bottom - height;
        }
        if !self.wrap || self.hooks.page.is_some() {
            let x = f64::from(rect.x.0 - self.bounds.x.0);
            if x < 0.0 {
                self.scroll_x = (self.scroll_x + x).max(0.0);
            } else if x > width - 2.0 && width > 0.0 {
                if !self.wrap {
                    self.widest = self.widest.max(self.scroll_x + x);
                }
                self.scroll_x += x - width + 2.0;
            }
        }
        if (self.scroll, self.scroll_x) != (old_y, old_x) {
            ctx.invalidate(self.bounds);
            self.schedule(ctx);
        }
    }
    /// Puts the text in the store as one edit, in place of the selection.
    pub fn ime_commit(&mut self, text: &str, ctx: &mut ViewContext) {
        let ended = self.hooks.composition.is_some();
        self.end_composition(ctx);
        if text.is_empty() || !self.can_edit() {
            if ended {
                self.hooks.composition_end = Some(CompositionEnd::Cancelled);
            }
            if !text.is_empty() {
                self.refused_edits += 1;
            }
            return;
        }
        self.touch();
        let range = self.selection();
        self.history.break_group();
        self.apply_edit(range, text, false, ctx);
        self.hooks.composition_end = Some(CompositionEnd::Committed {
            revision: self.revision(),
        });
        self.finish(Instant::now(), ctx);
    }
    /// Ends the composition. The store and the selection are as they were.
    pub fn ime_cancel(&mut self, ctx: &mut ViewContext) {
        if self.hooks.composition.is_some() {
            self.hooks.composition_end = Some(CompositionEnd::Cancelled);
        }
        self.end_composition(ctx);
        self.finish(Instant::now(), ctx);
    }
    pub(crate) fn end_composition(&mut self, ctx: &mut ViewContext) {
        let Some(composition) = self.hooks.composition.take() else {
            return;
        };
        if let Some(height) = composition.old_height {
            self.set_line_height(composition.line, height, ctx);
        }
        self.repaint_line_down(composition.line, ctx);
    }

    // ---- Space and scrolling ----

    /// Reserves space for a gutter and a document map. The text area and its hit tests
    /// move to match, and the same text stays at the top of the view.
    pub fn set_margins(&mut self, left: Dip, right: Dip, ctx: &mut ViewContext) {
        self.hooks.left = left.0.max(0.0);
        self.hooks.right = right.0.max(0.0);
        let inner = self.inner_bounds(self.hooks.outer);
        self.set_bounds(inner);
        self.after_restart(ctx);
    }
    /// The rectangle that the text paints in.
    pub fn text_area(&self) -> Rect {
        self.bounds
    }
    pub(crate) fn inner_bounds(&self, outer: Rect) -> Rect {
        Rect {
            x: Dip(outer.x.0 + self.hooks.left),
            y: outer.y,
            width: Dip((outer.width.0 - self.hooks.left - self.hooks.right).max(0.0)),
            height: outer.height,
        }
    }
    /// Scrolls to exactly this distance from the top of the text, within the limits. It
    /// ends any wait for a place, as the wheel does.
    pub fn set_scroll_position(&mut self, scroll: f64, ctx: &mut ViewContext) {
        self.pending_anchor = None;
        self.keep_caret_visible = false;
        self.reveal_pending = false;
        let target = scroll.clamp(0.0, self.max_scroll());
        if target != self.scroll {
            self.scroll = target;
            ctx.invalidate(self.bounds);
            self.schedule(ctx);
        }
        self.finish(Instant::now(), ctx);
    }
    /// The text's height so far. Lines with no layout count at their estimated height, so
    /// this changes as layouts arrive.
    pub fn total_height(&self) -> f64 {
        self.heights.total_height()
    }
    /// The largest value `set_scroll_position` accepts.
    pub fn max_scroll_position(&self) -> f64 {
        self.max_scroll()
    }
    /// Wraps at a fixed width, whatever the view's width, for Wrap to Page. `None` wraps
    /// to the view again. It needs wrap on, and the same text stays at the top of the view.
    pub fn set_page_wrap(&mut self, page: Option<PageWrap>, ctx: &mut ViewContext) {
        if self.hooks.page == page {
            return;
        }
        self.remember_view();
        self.hooks.page = page;
        self.engine = Arc::new(self.engine.with_full_rows_at(self.full_rows_width()));
        if let Err(problem) = self.restart_layout() {
            self.set_warning(problem, false);
        }
        self.clamp_scroll();
        self.after_restart(ctx);
    }
    /// A host that paints paper under the text turns off the text's own background.
    pub fn set_paints_background(&mut self, paints: bool, ctx: &mut ViewContext) {
        self.hooks.no_background = !paints;
        ctx.invalidate(self.bounds);
    }
    pub(crate) fn full_rows_width(&self) -> Dip {
        self.hooks.page.map_or(self.laid_width, |p| p.text_width)
    }
    /// The space to the left of the text, inside the view.
    pub(crate) fn page_margin(&self) -> f32 {
        match self.hooks.page {
            Some(page) if self.wrap => page.margin.0,
            _ => 0.0,
        }
    }
    pub(crate) fn text_left(&self) -> f32 {
        self.bounds.x.0 + self.page_margin()
    }

    // ---- Edits from tools ----

    /// Runs `apply_edits` with its edit refused by the history, as an out-of-step store
    /// would refuse it, so that a test can see how the failure comes out.
    #[doc(hidden)]
    pub fn test_apply_failing(
        &mut self,
        edits: &PreparedEdits,
        selection: Range<ByteOffset>,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.hooks.test_refuse_edit = true;
        let result = self.apply_edits(edits, selection, ctx);
        self.hooks.test_refuse_edit = false;
        result
    }
    /// Applies edits that `prepare_edits` made, as one edit and one undo step, and then
    /// sets the selection. It refuses edits made for another revision, because their
    /// offsets no longer mean the same text. Its cost follows `new_len`, and not how many
    /// replacements went into it.
    pub fn apply_edits(
        &mut self,
        edits: &PreparedEdits,
        selection: Range<ByteOffset>,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        let result = self.apply_edits_inner(edits, selection, ctx);
        self.finish(Instant::now(), ctx);
        result
    }
    fn apply_edits_inner(
        &mut self,
        edits: &PreparedEdits,
        selection: Range<ByteOffset>,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        if !self.can_edit() {
            self.refused_edits += 1;
            return Err("This view cannot be edited.".to_string());
        }
        // The commit changes the text, so edits prepared before it are refused below.
        self.commit_composition_first(ctx);
        if edits.base_revision != self.revision() {
            return Err("The text changed after these edits were prepared.".to_string());
        }
        self.touch();
        self.history.break_group();
        let before = self.revision();
        if self.hooks.test_refuse_edit {
            self.set_warning("The edit failed: refused by a test.".to_string(), false);
        } else {
            self.apply_edit(edits.range.clone(), &edits.text, false, ctx);
        }
        self.history.break_group();
        // A failed edit leaves the same revision, and its warning says why.
        let nothing_to_do = edits.range.is_empty() && edits.text.is_empty();
        if self.revision() == before && !nothing_to_do {
            return Err(self.warning().unwrap_or("The edit failed.").to_string());
        }
        let new_len = self.source().status().known_bytes;
        let start = selection.start.min(new_len);
        let end = selection.end.min(new_len).max(start);
        self.select(start..end, ctx);
        self.reveal_caret(ctx);
        Ok(())
    }

    // ---- Commands in order, and Save ----

    /// Runs a command in order with the keys before it. If a key is waiting, the command
    /// waits behind it. A command that fails shows its reason as a warning.
    pub fn run_host(&mut self, command: HostCommand, ctx: &mut ViewContext) {
        if self.deferred.is_empty() {
            self.run_host_now(command, ctx);
        } else {
            self.hold(Queued::Host(command));
        }
        self.finish(Instant::now(), ctx);
    }
    pub(crate) fn run_host_now(&mut self, command: HostCommand, ctx: &mut ViewContext) {
        let result = match command {
            HostCommand::PreventEditing(enabled) => {
                if enabled {
                    self.commit_composition_first(ctx);
                }
                self.set_prevent_editing(enabled);
                Ok(())
            }
            HostCommand::Wrap(enabled) => self.apply_wrap(enabled, ctx),
            HostCommand::Font { family, points } => self.apply_font(&family, points, ctx),
            HostCommand::PageWrap(page) => {
                self.set_page_wrap(page, ctx);
                Ok(())
            }
            HostCommand::Margins { left, right } => {
                self.set_margins(left, right, ctx);
                Ok(())
            }
            HostCommand::ScrollTo(scroll) => {
                self.set_scroll_position(scroll, ctx);
                Ok(())
            }
            HostCommand::Select { range, reveal } => {
                self.touch();
                self.select(range, ctx);
                if reveal {
                    self.reveal_caret(ctx);
                }
                Ok(())
            }
            HostCommand::ApplyEdits { edits, selection } => {
                self.apply_edits_inner(&edits, selection, ctx)
            }
        };
        if let Err(problem) = result {
            self.set_warning(problem, false);
        }
    }
    /// Runs all waiting input now, for a Save. Keys still waiting for a layout are dropped
    /// with their warning, and keys waiting for an index run on what is known. Typing
    /// behind them goes in. After this, `worker_snapshot` holds all that was typed.
    pub fn flush_input(&mut self, ctx: &mut ViewContext) {
        let past_every_limit = Instant::now() + crate::DEFER_TOTAL_LIMIT * 2;
        self.run_deferred(past_every_limit, ctx);
        self.deferred_since = Instant::now();
        self.finish(Instant::now(), ctx);
    }
    /// True while a Cut's text is still on its way to the clipboard. `adopt` refuses
    /// while this is true. Poll with `poll_layout` until it ends.
    pub fn cut_is_running(&self) -> bool {
        self.copy_job.as_ref().is_some_and(|job| job.is_cut)
    }
    /// Marks the current text as saved.
    pub fn mark_save_point(&mut self) {
        self.history.save_point();
        self.hooks.save_point_gone = false;
    }
    /// A write of the snapshot with this `revision` ended well. True means the view holds
    /// that text, and it is now saved. False means one of two things:
    /// - The revision is from another document or view, and nothing changes here.
    /// - The user changed the text during the write. The disk now holds text this view
    ///   cannot name, so the view reads as unsaved until the next mark, even after an Undo
    ///   back to the old save point or to the written text.
    pub fn mark_save_point_at(&mut self, revision: u64) -> bool {
        if revision >> REVISION_BITS != self.hooks.document {
            return false;
        }
        if revision == self.revision() {
            self.mark_save_point();
            true
        } else {
            self.hooks.save_point_gone = true;
            false
        }
    }
}

/// `(left, right, right_to_left, bytes)` of the part of `range` on a row, relative to the
/// row's left edge: one span for each direction run. An empty range on this row gives a
/// span with no width at its caret place.
fn row_spans(
    row: &LayoutRow,
    range: &Range<ByteOffset>,
) -> Vec<(f32, f32, bool, Range<ByteOffset>)> {
    let first = range.start.max(row.range.start);
    let last = range.end.min(row.range.end);
    if first > last || (first == last && !range.is_empty()) {
        return Vec::new();
    }
    let x_of = |byte: ByteOffset, before: bool| -> f32 {
        let at = row
            .caret_stops
            .partition_point(|s| s.byte <= byte)
            .saturating_sub(1);
        row.caret_stops
            .get(at)
            .map_or(0.0, |s| if before { s.x_before.0 } else { s.x.0 })
    };
    if row.visual_stops.is_empty() {
        let (a, b) = (x_of(first, false), x_of(last, true));
        return vec![(a.min(b), a.max(b), false, first..last)];
    }
    let mut spans: Vec<(f32, f32, bool, Range<ByteOffset>)> = Vec::new();
    for run in &row.glyph_runs {
        let from = first.max(run.range.start);
        let to = last.min(run.range.end);
        if from >= to {
            continue;
        }
        let (a, b) = (x_of(from, false), x_of(to, true));
        spans.push((a.min(b), a.max(b), run.bidi_level % 2 == 1, from..to));
    }
    if spans.is_empty() {
        let (a, b) = (x_of(first, false), x_of(last, true));
        spans.push((a.min(b), a.max(b), false, first..last));
    }
    spans
}

fn composed_hit(composition: &Composition, layout: &LineLayout, at: Point) -> HitTest {
    let hit = layout.hit_test_point(at);
    HitTest {
        byte: composition.shown.doc_of(hit.byte),
        affinity: hit.affinity,
    }
}

fn push_straight_underline(
    list: &mut DisplayList,
    row: &LayoutRow,
    from: f32,
    to: f32,
    y: f32,
    theme: &Theme,
) {
    let under = y + row.y.0 + row.height.0 - 1.0;
    list.push(DrawCommand::Line {
        from: Point {
            x: Dip(from),
            y: Dip(under),
        },
        to: Point {
            x: Dip(to),
            y: Dip(under),
        },
        width: Dip(1.0),
        color: theme.text,
    });
}

/// The straight line under composition text.
pub(crate) fn paint_composition_underline(
    list: &mut DisplayList,
    layout: &LineLayout,
    composition: &Composition,
    x: f32,
    y: f32,
    theme: &Theme,
) {
    if composition.clauses.is_empty() {
        for row in &layout.rows {
            for (start, end, _, _) in row_spans(row, &composition.shown.text) {
                push_straight_underline(list, row, x + start, x + end, y, theme);
            }
        }
        return;
    }
    // A small gap keeps two clauses apart, and the target's line is drawn twice as heavy.
    const GAP: f32 = 1.0;
    for clause in &composition.clauses {
        let bytes = composed_bytes(composition, &clause.range_utf16);
        for row in &layout.rows {
            for (start, end, _, _) in row_spans(row, &bytes) {
                let to = (x + end - GAP).max(x + start + GAP);
                push_straight_underline(list, row, x + start, to, y, theme);
                if clause.target {
                    push_straight_underline(list, row, x + start, to, y - 1.0, theme);
                }
            }
        }
    }
}

/// What a composition's text looks like on screen: a line break shows as a space.
fn shown_composition_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}
/// UTF-16 units of a composition's text as bytes of its layout, inside its text.
fn composed_bytes(composition: &Composition, range: &Range<usize>) -> Range<ByteOffset> {
    let shown = shown_composition_text(&composition.text);
    let text = &composition.shown.text;
    let place =
        |units: usize| ByteOffset((text.start.0 + byte_of_utf16(&shown, units)).min(text.end.0));
    place(range.start)..place(range.end.max(range.start))
}
/// Bytes of a composition's layout as UTF-16 units of its text.
fn utf16_of_shown(text: &str, bytes: usize) -> usize {
    let shown = shown_composition_text(text);
    shown.get(..bytes.min(shown.len())).map_or_else(
        || shown.encode_utf16().count(),
        |part| part.encode_utf16().count(),
    )
}

/// The byte of a UTF-16 offset in `text`. Past the end, or inside a pair, it steps back.
fn byte_of_utf16(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units + c.len_utf16() > utf16 {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

// ---- For the UI Automation bridge (qe-a11y) ----

impl EditorView {
    /// True when long lines wrap, so a line can have several rows. With wrap off, a row in
    /// `visible_rows` can be only the part of a long line at the view's left edge.
    pub fn wraps(&self) -> bool {
        self.wrap
    }
    /// The byte after the last byte of this row from `visible_rows`. `None` when its layout
    /// has gone. It reads a layout already made, so it does not shape text or wait for disk.
    pub fn visible_row_end(&self, row: &VisibleRow) -> Option<ByteOffset> {
        self.row_end(row)
    }
}
