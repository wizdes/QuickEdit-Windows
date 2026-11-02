//! A bounded cache of laid-out lines and long-line segments for one document.
//!
//! The crate documentation shows how the UI thread and workers share it.
use crate::{DirectWriteLayout, LONG_LINE_BYTES, LayoutError, segments::SEGMENT_READ_AHEAD};
use qe_api::{
    ByteOffset, CaretStop, Dip, LayoutGlyphRun, LayoutRow, LineIndex, LineLayout, Point,
    TextChange, TextSnapshot, VisualStop,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

const NO_SLOT: u32 = u32::MAX;
/// What the allocation behind an `Arc` adds to its value: two counters.
const ARC_HEADER: usize = 2 * size_of::<usize>();
/// More pieces than this and the cache starts over. It happens only after this
/// many edits that changed the line count, and a screen of lines is cheap.
const MAX_PIECES: usize = 256;
/// Line ids are `base + offset`, and one piece holds at most this many lines.
const PIECE_SPAN: u64 = 1 << 40;
const WHOLE_LINE: u32 = u32::MAX;
/// How many changed lines are remembered for judging a worker's late result. An
/// older result than this memory reaches is refused.
const MAX_REMEMBERED_EDITS: usize = 256;
/// An edit over more lines than this refuses every older worker result.
const MAX_LINES_PER_EDIT: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    /// Stays with the line when lines above it come and go.
    id: u64,
    segment: u32,
    width: u32,
    font: u64,
    tab_width: u32,
    wrap: bool,
    force_shaping: bool,
    /// Full rows move the edges of a long line's segments.
    full_row_cells: Option<usize>,
}
struct Slot {
    key: Key,
    layout: Arc<LineLayout>,
    bytes: usize,
    /// Where the line started when `layout`'s byte offsets were last correct.
    line_start: usize,
    /// The revision at which `layout`'s line number and byte offsets were last correct.
    revision: u64,
    newer: u32,
    older: u32,
}
/// Lines from `first` on, until the next piece, have ids `base`, `base + 1`, ...
#[derive(Clone, Copy)]
struct Piece {
    first: usize,
    base: u64,
}

/// What a worker needs to lay out one line or segment, and what the cache needs to
/// take the result back. Make it on the UI thread with `LineCache::ticket`.
#[derive(Clone)]
pub struct CacheTicket {
    key: Key,
    line: LineIndex,
    segment: Option<usize>,
    width: Dip,
    revision: u64,
    /// Only a ticket made at the cache's own revision has a line id it can trust.
    trusted: bool,
    epoch: u64,
    line_start: usize,
}
impl CacheTicket {
    pub fn line(&self) -> LineIndex {
        self.line
    }
    pub fn segment(&self) -> Option<usize> {
        self.segment
    }
    pub fn width(&self) -> Dip {
        self.width
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Does the slow work. It takes no cache and holds no lock, so call it on a
    /// worker. `text` must be the snapshot of the ticket's revision.
    pub fn layout(
        &self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
    ) -> Result<LineLayout, LayoutError> {
        if text.revision() != self.revision {
            return Err(LayoutError::StaleTicket {
                ticket: self.revision,
                snapshot: text.revision(),
            });
        }
        match self.segment {
            Some(index) => engine.layout_segment(text, self.line, index, self.width),
            None => engine.try_layout_line(text, self.line, self.width),
        }
    }
}

/// Bounded cache of laid-out lines.
///
/// A warm get at an unchanged revision reads no text and is O(1): it checks the
/// revision, looks up one key and clones an `Arc`.
///
/// The first get of an entry after an edit costs more. Layouts hold whole-document
/// byte offsets, because `LayoutRow` and `CaretStop` are public values that callers
/// compare with buffer offsets. So if an edit moved the line, that one get asks the
/// snapshot for the line's start and rewrites each offset in the layout. It is O(the
/// layout's stops) once per entry and per revision, and it also copies the layout
/// if a caller still holds its `Arc`.
///
/// One cache belongs to one document. Call `invalidate` for each edit, in order. A
/// snapshot newer than the last `invalidate` empties the cache, because an edit was
/// not reported. A snapshot older than the cache never touches its entries: the
/// result is laid out fresh and not kept.
pub struct LineCache {
    cap: usize,
    /// Bytes of the layouts held. `memory_bytes` adds the tables.
    used: usize,
    /// The capacity of the lists in `by_line`.
    list_bytes: usize,
    slots: Vec<Option<Slot>>,
    free: Vec<u32>,
    newest: u32,
    oldest: u32,
    keys: HashMap<Key, u32>,
    by_line: HashMap<u64, Vec<u32>>,
    pieces: Vec<Piece>,
    next_base: u64,
    revision: Option<u64>,
    /// Counts `clear` calls. Ids restart there, so an older ticket means nothing.
    epoch: u64,
    /// Lines changed by recent edits, oldest first, for judging late worker results.
    edits: VecDeque<(u64, u64)>,
    /// Worker results from before this revision are refused.
    oldest_trusted: u64,
}

pub(crate) fn layout_bytes(layout: &LineLayout) -> usize {
    let row = |r: &LayoutRow| {
        r.caret_stops.capacity() * size_of::<CaretStop>()
            + r.visual_stops.capacity() * size_of::<VisualStop>()
            + r.glyph_runs.capacity() * size_of::<LayoutGlyphRun>()
            + r.glyph_runs
                .iter()
                .map(|g: &LayoutGlyphRun| {
                    g.glyph_ids.capacity() * size_of::<u16>()
                        + g.advances.capacity() * size_of::<Dip>()
                        + g.offsets.capacity() * size_of::<Point>()
                })
                .sum::<usize>()
    };
    ARC_HEADER
        + size_of::<LineLayout>()
        + layout.rows.capacity() * size_of::<LayoutRow>()
        + layout.rows.iter().map(row).sum::<usize>()
}

/// The bytes a `HashMap` of this capacity allocates: one slot per bucket, a control
/// byte per bucket, and one group of extra control bytes. Its layout comes from
/// `hashbrown`, which the standard library uses.
fn map_bytes<K, V>(map: &HashMap<K, V>) -> usize {
    let capacity = map.capacity();
    if capacity == 0 {
        return 0;
    }
    let buckets = if capacity < 8 {
        capacity + 1
    } else {
        (capacity * 8 / 7).next_power_of_two()
    };
    buckets * (size_of::<(K, V)>() + 1) + 16
}

/// Moves a layout's byte offsets to a new line start.
fn move_layout(layout: &mut LineLayout, delta: isize) -> Option<()> {
    let shift = |b: &mut ByteOffset| -> Option<()> {
        b.0 = b.0.checked_add_signed(delta)?;
        Some(())
    };
    for row in &mut layout.rows {
        shift(&mut row.range.start)?;
        shift(&mut row.range.end)?;
        for stop in &mut row.caret_stops {
            shift(&mut stop.byte)?;
        }
        for stop in &mut row.visual_stops {
            shift(&mut stop.byte)?;
        }
        for run in &mut row.glyph_runs {
            shift(&mut run.range.start)?;
            shift(&mut run.range.end)?;
        }
    }
    Some(())
}

impl LineCache {
    pub fn new(memory_cap: usize) -> Self {
        let mut cache = Self {
            cap: memory_cap,
            used: 0,
            list_bytes: 0,
            slots: Vec::new(),
            free: Vec::new(),
            newest: NO_SLOT,
            oldest: NO_SLOT,
            keys: HashMap::new(),
            by_line: HashMap::new(),
            pieces: Vec::new(),
            next_base: 0,
            revision: None,
            epoch: 0,
            edits: VecDeque::new(),
            oldest_trusted: 0,
        };
        cache.clear();
        cache
    }
    /// Every byte the cache holds: the layouts, at their real capacity, and its own
    /// tables. It counts only what this cache owns. A layout that a caller still holds
    /// after the cache dropped it is the caller's. It does not count the allocator's
    /// own overhead per allocation.
    pub fn memory_bytes(&self) -> usize {
        self.used + self.table_bytes()
    }
    fn table_bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Option<Slot>>()
            + self.free.capacity() * size_of::<u32>()
            + map_bytes(&self.keys)
            + map_bytes(&self.by_line)
            + self.list_bytes
            + self.pieces.capacity() * size_of::<Piece>()
            + self.edits.capacity() * size_of::<(u64, u64)>()
    }
    /// Gives the tables' spare room back after many small entries left. It renumbers
    /// the slots, so it costs O(entries), and it runs only when the slots are less than
    /// half used, which pays for it.
    fn compact(&mut self) {
        let mut old = std::mem::take(&mut self.slots);
        let mut order = Vec::with_capacity(self.keys.len());
        let mut at = self.oldest;
        while at != NO_SLOT {
            let slot = old[at as usize].take().expect("live cache slot");
            at = slot.newer;
            order.push(slot);
        }
        self.free = Vec::new();
        self.keys = HashMap::with_capacity(order.len());
        self.by_line = HashMap::new();
        self.list_bytes = 0;
        self.slots = Vec::with_capacity(order.len());
        self.newest = NO_SLOT;
        self.oldest = NO_SLOT;
        for slot in order {
            let index = self.slots.len() as u32;
            let key = slot.key;
            self.slots.push(Some(slot));
            self.link_newest(index);
            self.note_new_entry(key, index);
        }
        self.by_line.shrink_to_fit();
    }
    fn note_new_entry(&mut self, key: Key, at: u32) {
        self.keys.insert(key, at);
        let list = self.by_line.entry(key.id).or_default();
        let before = list.capacity();
        list.push(at);
        self.list_bytes += (list.capacity() - before) * size_of::<u32>();
    }
    fn trim(&mut self) {
        while self.memory_bytes() > self.cap && self.oldest != NO_SLOT {
            if self.slots.len() > 2 * self.keys.len() + 16 {
                self.compact();
                continue;
            }
            self.remove(self.oldest);
        }
        if self.keys.is_empty() {
            self.slots = Vec::new();
            self.free = Vec::new();
            self.keys = HashMap::new();
            self.by_line = HashMap::new();
            self.list_bytes = 0;
        }
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    /// Empties the cache and gives back its tables' memory.
    pub fn clear(&mut self) {
        self.slots = Vec::new();
        self.free = Vec::new();
        self.keys = HashMap::new();
        self.by_line = HashMap::new();
        self.list_bytes = 0;
        self.pieces = vec![Piece { first: 0, base: 0 }];
        self.next_base = PIECE_SPAN;
        self.newest = NO_SLOT;
        self.oldest = NO_SLOT;
        self.used = 0;
        self.epoch += 1;
        self.edits = VecDeque::new();
    }
    fn line_id(&self, line: usize) -> u64 {
        let at = self.pieces.partition_point(|p| p.first <= line) - 1;
        let piece = self.pieces[at];
        piece.base + (line - piece.first) as u64
    }
    fn slot(&self, at: u32) -> &Slot {
        self.slots[at as usize].as_ref().expect("live cache slot")
    }
    fn slot_mut(&mut self, at: u32) -> &mut Slot {
        self.slots[at as usize].as_mut().expect("live cache slot")
    }
    fn unlink(&mut self, at: u32) {
        let (newer, older) = {
            let s = self.slot(at);
            (s.newer, s.older)
        };
        if newer == NO_SLOT {
            self.newest = older;
        } else {
            self.slot_mut(newer).older = older;
        }
        if older == NO_SLOT {
            self.oldest = newer;
        } else {
            self.slot_mut(older).newer = newer;
        }
    }
    fn link_newest(&mut self, at: u32) {
        let old = self.newest;
        {
            let s = self.slot_mut(at);
            s.newer = NO_SLOT;
            s.older = old;
        }
        if old == NO_SLOT {
            self.oldest = at;
        } else {
            self.slot_mut(old).newer = at;
        }
        self.newest = at;
    }
    fn remove(&mut self, at: u32) {
        self.unlink(at);
        let slot = self.slots[at as usize].take().expect("live cache slot");
        self.free.push(at);
        self.used -= slot.bytes;
        self.keys.remove(&slot.key);
        if let Some(list) = self.by_line.get_mut(&slot.key.id) {
            list.retain(|s| *s != at);
            if list.is_empty() {
                self.list_bytes -= list.capacity() * size_of::<u32>();
                self.by_line.remove(&slot.key.id);
            }
        }
    }
    /// True when `text` is at the cache's revision. A newer snapshot starts the cache
    /// over at that revision. An older one changes nothing.
    fn sync_revision(&mut self, text: &dyn TextSnapshot) -> bool {
        let now = text.revision();
        match self.revision {
            Some(known) if now < known => false,
            Some(known) if now == known => true,
            _ => {
                self.clear();
                self.revision = Some(now);
                true
            }
        }
    }
    fn key(&self, engine: &DirectWriteLayout, line: LineIndex, segment: u32, width: Dip) -> Key {
        let wrap = engine.wrap_enabled();
        Key {
            id: self.line_id(line.0),
            segment,
            // Width does not change a layout that does not wrap.
            width: if wrap { width.0.to_bits() } else { 0 },
            font: engine.font_uid(),
            tab_width: engine.tab_width() as u32,
            wrap,
            force_shaping: engine.forced_shaping(),
            full_row_cells: engine.full_row_cells(),
        }
    }
    /// Returns the cached layout after making its line and byte offsets current.
    fn warm(
        &mut self,
        at: u32,
        text: &dyn TextSnapshot,
        line: LineIndex,
    ) -> Result<Arc<LineLayout>, LayoutError> {
        let revision = text.revision();
        if self.slot(at).revision != revision {
            let start = text
                .line_range(line)
                .ok_or(LayoutError::NoSuchLine(line))?
                .start;
            let slot = self.slot_mut(at);
            let delta = start.0 as isize - slot.line_start as isize;
            if delta != 0 || slot.layout.line != line {
                let layout = Arc::make_mut(&mut slot.layout);
                move_layout(layout, delta).ok_or(LayoutError::BadRange(start..start))?;
                layout.line = line;
            }
            slot.line_start = start.0;
            slot.revision = revision;
        }
        self.unlink(at);
        self.link_newest(at);
        Ok(self.slot(at).layout.clone())
    }
    fn store(
        &mut self,
        key: Key,
        layout: LineLayout,
        line_start: usize,
        revision: u64,
    ) -> Arc<LineLayout> {
        let bytes = layout_bytes(&layout);
        let layout = Arc::new(layout);
        if bytes > self.cap {
            return layout;
        }
        let slot = Slot {
            key,
            layout: layout.clone(),
            bytes,
            line_start,
            revision,
            newer: NO_SLOT,
            older: NO_SLOT,
        };
        let at = if let Some(at) = self.free.pop() {
            self.slots[at as usize] = Some(slot);
            at
        } else {
            self.slots.push(Some(slot));
            (self.slots.len() - 1) as u32
        };
        self.link_newest(at);
        self.note_new_entry(key, at);
        self.used += bytes;
        self.trim();
        layout
    }
    /// A whole line, or the first segment of a line that only exists as segments.
    /// A miss shapes here, on the caller's thread and holding `&mut self`. On the UI
    /// thread, use `cached` and a worker instead when a miss would cost a frame.
    pub fn get_or_layout(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Result<Arc<LineLayout>, LayoutError> {
        if !self.sync_revision(text) {
            return engine.try_layout_line(text, line, width).map(Arc::new);
        }
        let key = self.key(engine, line, WHOLE_LINE, width);
        if let Some(&at) = self.keys.get(&key) {
            return self.warm(at, text, line);
        }
        let start = text
            .line_range(line)
            .ok_or(LayoutError::NoSuchLine(line))?
            .start;
        let result = engine.try_layout_line(text, line, width)?;
        Ok(self.store(key, result, start.0, text.revision()))
    }
    /// One segment of a long line. A miss shapes here, as with `get_or_layout`.
    pub fn get_or_layout_segment(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        segment: usize,
        width: Dip,
    ) -> Result<Arc<LineLayout>, LayoutError> {
        if !self.sync_revision(text) {
            return engine
                .layout_segment(text, line, segment, width)
                .map(Arc::new);
        }
        let index = u32::try_from(segment).map_err(|_| LayoutError::NoSuchSegment {
            line,
            index: segment,
        })?;
        let key = self.key(engine, line, index, width);
        if let Some(&at) = self.keys.get(&key) {
            return self.warm(at, text, line);
        }
        let start = text
            .line_range(line)
            .ok_or(LayoutError::NoSuchLine(line))?
            .start;
        let result = engine.layout_segment(text, line, segment, width)?;
        Ok(self.store(key, result, start.0, text.revision()))
    }
    /// The layout if it is cached. It never shapes, so the UI thread can always call it.
    pub fn cached(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<Arc<LineLayout>> {
        if !self.sync_revision(text) {
            return None;
        }
        let key = self.key(engine, line, WHOLE_LINE, width);
        let at = *self.keys.get(&key)?;
        self.warm(at, text, line).ok()
    }
    /// The segment if it is cached. It never shapes.
    pub fn cached_segment(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        segment: usize,
        width: Dip,
    ) -> Option<Arc<LineLayout>> {
        if !self.sync_revision(text) {
            return None;
        }
        let key = self.key(engine, line, u32::try_from(segment).ok()?, width);
        let at = *self.keys.get(&key)?;
        self.warm(at, text, line).ok()
    }
    fn make_ticket(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        segment: Option<usize>,
        width: Dip,
    ) -> Result<CacheTicket, LayoutError> {
        let trusted = self.sync_revision(text);
        let index = match segment {
            Some(index) => {
                u32::try_from(index).map_err(|_| LayoutError::NoSuchSegment { line, index })?
            }
            None => WHOLE_LINE,
        };
        let line_start = text
            .line_range(line)
            .ok_or(LayoutError::NoSuchLine(line))?
            .start
            .0;
        Ok(CacheTicket {
            key: self.key(engine, line, index, width),
            line,
            segment,
            width,
            revision: text.revision(),
            trusted,
            epoch: self.epoch,
            line_start,
        })
    }
    /// A ticket for laying out a line on a worker. `layout_line` semantics.
    pub fn ticket(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Result<CacheTicket, LayoutError> {
        self.make_ticket(engine, text, line, None, width)
    }
    /// A ticket for laying out a segment on a worker.
    pub fn segment_ticket(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        segment: usize,
        width: Dip,
    ) -> Result<CacheTicket, LayoutError> {
        self.make_ticket(engine, text, line, Some(segment), width)
    }
    /// Takes a worker's result. Returns false and keeps nothing when the result is no
    /// longer true: the line changed after the ticket was made, or the cache started
    /// over. A result for a line that only moved is kept, and the next get moves it.
    pub fn insert(&mut self, ticket: &CacheTicket, layout: LineLayout) -> bool {
        let usable = ticket.trusted
            && ticket.epoch == self.epoch
            && ticket.revision >= self.oldest_trusted
            && self.revision.is_some_and(|now| ticket.revision <= now)
            && !self
                .edits
                .iter()
                .any(|(revision, id)| *revision > ticket.revision && *id == ticket.key.id);
        if !usable {
            return false;
        }
        if !self.keys.contains_key(&ticket.key) {
            self.store(ticket.key, layout, ticket.line_start, ticket.revision);
        }
        true
    }
    fn remember_edit(&mut self, first: usize, last: usize, revision: u64) {
        if last - first >= MAX_LINES_PER_EDIT {
            self.oldest_trusted = revision;
            self.edits.clear();
            return;
        }
        for line in first..=last {
            let id = self.line_id(line);
            self.edits.push_back((revision, id));
        }
        while self.edits.len() > MAX_REMEMBERED_EDITS {
            if let Some((forgotten, _)) = self.edits.pop_front() {
                self.oldest_trusted = self.oldest_trusted.max(forgotten);
            }
        }
    }
    fn drop_lines(&mut self, first: usize, last: usize) {
        let touched = (last - first + 1) as u64;
        if touched <= self.by_line.len() as u64 {
            for line in first..=last {
                let id = self.line_id(line);
                if let Some(list) = self.by_line.get(&id).cloned() {
                    for at in list {
                        self.remove(at);
                    }
                }
            }
        } else {
            // More lines changed than the cache holds, so check each entry once.
            let doomed: Vec<u32> = self
                .keys
                .iter()
                .filter(|(key, _)| self.id_in_lines(key.id, first, last))
                .map(|(_, at)| *at)
                .collect();
            for at in doomed {
                self.remove(at);
            }
        }
    }
    /// An edit inside a line that stays in segments: segments that end well before
    /// the edit stay right, because a segment's edges depend only on nearby text.
    fn drop_segments_after(&mut self, line: usize, edit_start: usize) {
        let id = self.line_id(line);
        let Some(list) = self.by_line.get(&id).cloned() else {
            return;
        };
        for at in list {
            let slot = self.slot(at);
            let end = slot.layout.rows.last().map_or(0, |r| r.range.end.0);
            let before_edit = end + SEGMENT_READ_AHEAD < edit_start;
            if slot.key.segment == WHOLE_LINE || !before_edit {
                self.remove(at);
            }
        }
    }
    fn id_in_lines(&self, id: u64, first: usize, last: usize) -> bool {
        self.pieces.iter().enumerate().any(|(i, piece)| {
            if id < piece.base || id >= piece.base + PIECE_SPAN {
                return false;
            }
            let line = piece.first as u64 + (id - piece.base);
            let inside_piece = self
                .pieces
                .get(i + 1)
                .is_none_or(|next| line < next.first as u64);
            inside_piece && line >= first as u64 && line <= last as u64
        })
    }
    /// Drops the changed lines. Lines above and below stay cached. Costs
    /// O(changed lines) or O(entries), whichever is smaller. The old snapshot
    /// identifies the changed lines.
    ///
    /// An edit inside a line that stays in segments keeps the segments that end
    /// before the edit. Later ones are dropped, because their byte edges move.
    pub fn invalidate(&mut self, old: &dyn TextSnapshot, change: &TextChange) {
        if self.revision.is_some_and(|now| change.revision <= now) {
            return;
        }
        if self.revision != Some(old.revision()) {
            self.clear();
            self.revision = Some(change.revision);
            return;
        }
        self.revision = Some(change.revision);
        let Some(mut first) = old
            .byte_to_position(change.replaced.start)
            .map(|p| p.line.0)
        else {
            self.clear();
            return;
        };
        let Some(last) = old.byte_to_position(change.replaced.end).map(|p| p.line.0) else {
            self.clear();
            return;
        };
        // Text placed after a CR can join it into CRLF, which changes the line before.
        if first > 0
            && change.replaced.start.0 > 0
            && old_byte_is_cr(old, change.replaced.start.0 - 1)
        {
            first -= 1;
        }
        self.remember_edit(first, last, change.revision);
        if first == last
            && change.line_count_delta == 0
            && let Some(range) = old.line_range(LineIndex(first))
        {
            // The line ending takes at most 3 bytes.
            let old_len = range.end.0 - range.start.0;
            let removed = change.replaced.end.0 - change.replaced.start.0;
            let new_len = old_len - removed + change.new_len;
            if old_len.min(new_len) > LONG_LINE_BYTES + 3 {
                self.drop_segments_after(first, change.replaced.start.0);
                return;
            }
        }
        self.drop_lines(first, last);
        if change.line_count_delta != 0 {
            self.shift_lines(first, last, change.line_count_delta);
        }
    }
    /// Changed lines get new ids, and lines below keep theirs at a new number.
    fn shift_lines(&mut self, first: usize, last: usize, delta: isize) {
        let new_end = (last + 1)
            .checked_add_signed(delta)
            .filter(|end| *end >= first);
        let Some(new_end) = new_end else {
            self.clear();
            return;
        };
        let after = self.line_id(last + 1);
        let keep = self.pieces.partition_point(|p| p.first < first);
        let below = self.pieces.partition_point(|p| p.first <= last + 1);
        let mut pieces: Vec<Piece> = Vec::with_capacity(self.pieces.len() + 2);
        pieces.extend_from_slice(&self.pieces[..keep]);
        if new_end > first {
            pieces.push(Piece {
                first,
                base: self.next_base,
            });
            self.next_base += PIECE_SPAN;
        }
        pieces.push(Piece {
            first: new_end,
            base: after,
        });
        for piece in &self.pieces[below..] {
            let Some(moved) = piece.first.checked_add_signed(delta) else {
                self.clear();
                return;
            };
            pieces.push(Piece {
                first: moved,
                base: piece.base,
            });
        }
        if pieces.len() > MAX_PIECES {
            self.clear();
        } else {
            self.pieces = pieces;
        }
    }
}

fn old_byte_is_cr(old: &dyn TextSnapshot, at: usize) -> bool {
    let mut is_cr = false;
    old.visit_chunks(ByteOffset(at)..ByteOffset(at + 1), &mut |part| {
        is_cr = part == "\r";
    });
    is_cr
}
