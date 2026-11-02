//! A bounded cache of laid-out lines and long-line segments for one document.
use crate::DirectWriteLayout;
use qe_api::{
    ByteOffset, CaretStop, Dip, Layout, LayoutGlyphRun, LayoutRow, LineIndex, LineLayout, Point,
    TextChange, TextSnapshot, VisualStop,
};
use std::{collections::HashMap, sync::Arc};

const NO_SLOT: u32 = u32::MAX;
/// The part of a slot that the bookkeeping adds for one entry. Includes the key
/// in both maps and the slot itself.
const ENTRY_OVERHEAD: usize = 2 * size_of::<Key>() + size_of::<Slot>() + 64;
/// More pieces than this and the cache starts over. It happens only after this
/// many edits that changed the line count, and a screen of lines is cheap.
const MAX_PIECES: usize = 256;
/// Line ids are `base + offset`, and one piece holds at most this many lines.
const PIECE_SPAN: u64 = 1 << 40;
const WHOLE_LINE: u32 = u32::MAX;

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

/// Bounded cache of laid-out lines.
///
/// A get does not read the line. It checks the snapshot's revision, looks up one
/// key and returns an `Arc`, so a warm get is O(1).
///
/// One cache belongs to one document. Call `invalidate` for each edit, in order.
/// If the snapshot's revision differs from what `invalidate` last reported, the
/// cache empties itself instead of giving an old layout.
pub struct LineCache {
    cap: usize,
    used: usize,
    slots: Vec<Option<Slot>>,
    free: Vec<u32>,
    newest: u32,
    oldest: u32,
    keys: HashMap<Key, u32>,
    by_line: HashMap<u64, Vec<u32>>,
    pieces: Vec<Piece>,
    next_base: u64,
    revision: Option<u64>,
}

pub(crate) fn layout_bytes(layout: &LineLayout) -> usize {
    let row = |r: &LayoutRow| {
        size_of::<LayoutRow>()
            + r.caret_stops.capacity() * size_of::<CaretStop>()
            + r.visual_stops.capacity() * size_of::<VisualStop>()
            + r.glyph_runs
                .iter()
                .map(|g: &LayoutGlyphRun| {
                    size_of::<LayoutGlyphRun>()
                        + g.glyph_ids.capacity() * size_of::<u16>()
                        + g.advances.capacity() * size_of::<Dip>()
                        + g.offsets.capacity() * size_of::<Point>()
                })
                .sum::<usize>()
    };
    size_of::<LineLayout>() + layout.rows.iter().map(row).sum::<usize>()
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
            slots: Vec::new(),
            free: Vec::new(),
            newest: NO_SLOT,
            oldest: NO_SLOT,
            keys: HashMap::new(),
            by_line: HashMap::new(),
            pieces: Vec::new(),
            next_base: 0,
            revision: None,
        };
        cache.clear();
        cache
    }
    /// Every byte the entries hold, including their part of the bookkeeping.
    pub fn memory_bytes(&self) -> usize {
        self.used
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
        self.keys.clear();
        self.by_line.clear();
        self.pieces.clear();
        self.pieces.push(Piece { first: 0, base: 0 });
        self.next_base = PIECE_SPAN;
        self.newest = NO_SLOT;
        self.oldest = NO_SLOT;
        self.used = 0;
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
                self.by_line.remove(&slot.key.id);
            }
        }
    }
    fn sync_revision(&mut self, text: &dyn TextSnapshot) {
        let now = text.revision();
        if self.revision != Some(now) {
            self.clear();
            self.revision = Some(now);
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
        }
    }
    /// Returns the cached layout after making its line and byte offsets current.
    fn warm(
        &mut self,
        at: u32,
        text: &dyn TextSnapshot,
        line: LineIndex,
        revision: u64,
    ) -> Option<Arc<LineLayout>> {
        if self.slot(at).revision != revision {
            let start = text.line_range(line)?.start;
            let slot = self.slot_mut(at);
            let delta = start.0 as isize - slot.line_start as isize;
            if delta != 0 || slot.layout.line != line {
                let layout = Arc::make_mut(&mut slot.layout);
                move_layout(layout, delta)?;
                layout.line = line;
            }
            slot.line_start = start.0;
            slot.revision = revision;
        }
        self.unlink(at);
        self.link_newest(at);
        Some(self.slot(at).layout.clone())
    }
    fn store(
        &mut self,
        key: Key,
        layout: LineLayout,
        line_start: usize,
        revision: u64,
    ) -> Arc<LineLayout> {
        let bytes = layout_bytes(&layout) + ENTRY_OVERHEAD;
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
        self.keys.insert(key, at);
        self.by_line.entry(key.id).or_default().push(at);
        self.used += bytes;
        while self.used > self.cap && self.oldest != NO_SLOT {
            self.remove(self.oldest);
        }
        layout
    }
    /// A whole line, or the first segment when the line is long. See the crate
    /// documentation for long lines.
    pub fn get_or_layout(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<Arc<LineLayout>> {
        self.sync_revision(text);
        let revision = text.revision();
        let key = self.key(engine, line, WHOLE_LINE, width);
        if let Some(&at) = self.keys.get(&key) {
            return self.warm(at, text, line, revision);
        }
        let start = text.line_range(line)?.start.0;
        let result = engine.layout_line(text, line, width)?;
        Some(self.store(key, result, start, revision))
    }
    /// One segment of a long line.
    pub fn get_or_layout_segment(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        segment: usize,
        width: Dip,
    ) -> Option<Arc<LineLayout>> {
        self.sync_revision(text);
        let revision = text.revision();
        let key = self.key(engine, line, u32::try_from(segment).ok()?, width);
        if let Some(&at) = self.keys.get(&key) {
            return self.warm(at, text, line, revision);
        }
        let start = text.line_range(line)?.start.0;
        let result = engine.layout_segment(text, line, segment, width).ok()?;
        Some(self.store(key, result, start, revision))
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
    pub fn invalidate(&mut self, old: &dyn TextSnapshot, change: &TextChange) {
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
        self.drop_lines(first, last);
        if change.line_count_delta == 0 {
            return;
        }
        self.shift_lines(first, last, change.line_count_delta);
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
            let revision = self.revision;
            self.clear();
            self.revision = revision;
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
