//! Long lines are cut into segments on grapheme boundaries. Every read here is
//! bounded, whatever the length of the line.
use crate::{DirectWriteLayout, HeightMap, LayoutError, shaped};
use qe_api::{Affinity, ByteOffset, Dip, LineIndex, LineLayout, LookupError, TextSnapshot};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// A line on the fast path stays whole up to this many content bytes. Past it, every
/// line is laid out only in segments.
pub const LONG_LINE_BYTES: usize = 16 * 1024;
/// A line that needs shaping stays whole up to this many content bytes. Shaping has
/// no fast form, so past this it is laid out in segments too.
pub const SHAPED_LINE_BYTES: usize = 4 * 1024;
/// The nominal size of one segment. A real segment ends at the first good boundary
/// at or after a multiple of this, up to 1 KiB later.
///
/// DirectWrite's shaping sets this size. Plain Arabic meets 5 ms a segment at this
/// size. Text that changes script at each character does not. A smaller size would
/// cost little in time but would break wrapped rows every row or two, because a row
/// ends at each segment edge. So those texts rely on workers. See the crate docs.
pub const SEGMENT_BYTES: usize = 2 * 1024;
const SNAP_BEHIND: usize = 256;
const SNAP_AHEAD: usize = 1024;
/// A segment's end depends on text up to this far past it.
pub(crate) const SEGMENT_READ_AHEAD: usize = SNAP_AHEAD;
const PREFER_SPACE_WITHIN: usize = 64;
const LOOK_BACK_FOR_TAB: usize = 1024;
/// How far back to look for the start of a run of regional indicators. A run longer
/// than this pairs up by its distance from the line start, which can cut flags.
const FLAG_RUN_LOOK_BACK: usize = 1024 * 1024;
/// How far each side of a slice edge is read to check that it cuts no cluster.
const EDGE_CONTEXT: usize = 128;
const FLAG_BYTES: usize = 8;

const ZWJ: char = '\u{200D}';
pub(crate) const LINE_ENDINGS: [char; 5] = ['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}'];

fn is_regional(ch: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch)
}

fn is_lam_alef(before: Option<char>, after: Option<char>) -> bool {
    before == Some('\u{644}')
        && after.is_some_and(|c| matches!(c, '\u{622}' | '\u{623}' | '\u{625}' | '\u{627}'))
}

/// A caret may stop where Unicode's grapheme rules say, except right after a
/// zero-width joiner, so that `a ZWJ b` stays whole, and between lam and alef, which
/// draw as one letter even in a font that keeps two glyphs for them.
fn allowed(s: &str, at: usize) -> bool {
    let before = s[..at].chars().next_back();
    before != Some(ZWJ) && !is_lam_alef(before, s[at..].chars().next())
}

/// Byte offsets in `s` where a caret may stop. `flag_phase` is how many bytes of a
/// flag lie before `s`, when `s` starts inside a run of regional indicators: 0 or 4.
fn grapheme_stops(s: &str, flag_phase: usize) -> Vec<usize> {
    let natural = |from: usize| {
        s[from..]
            .grapheme_indices(true)
            .map(move |(at, _)| from + at)
            .chain(Some(s.len()))
            .filter(|at| allowed(s, *at))
    };
    if flag_phase == 0 {
        return natural(0).collect();
    }
    let run_end = s
        .char_indices()
        .find(|(_, c)| !is_regional(*c))
        .map_or(s.len(), |(at, _)| at);
    // The first flag edge in `s`, then one flag at a time to the end of the run.
    let first = FLAG_BYTES - flag_phase;
    if first > run_end {
        return natural(run_end).collect();
    }
    let mut stops: Vec<usize> = (first..run_end).step_by(FLAG_BYTES).collect();
    let tail = stops.pop().unwrap_or(first);
    stops.extend(natural(tail));
    stops
}

/// For each byte of `s`, and its end, whether a caret may stop there. `s` starts on
/// a real edge, so its flags pair up from its own start.
pub(crate) fn stop_table(s: &str) -> Vec<bool> {
    let mut table = vec![false; s.len() + 1];
    for at in grapheme_stops(s, 0) {
        table[at] = true;
    }
    table
}

/// When a window starts in a run of regional indicators, how many bytes of a flag
/// lie before it. It finds the start of the run, reading a bounded amount and
/// allocating nothing. A look-back the store cannot read is an error, because a guess
/// here would cut flags in two.
fn flag_phase(
    text: &dyn TextSnapshot,
    line_start: usize,
    at: usize,
    window: &str,
) -> Result<usize, LayoutError> {
    if at <= line_start || !window.chars().next().is_some_and(is_regional) {
        return Ok(0);
    }
    let limit = at.saturating_sub(FLAG_RUN_LOOK_BACK).max(line_start);
    let mut pos = at;
    let mut run_start = None;
    while pos > limit && run_start.is_none() {
        let from = char_edge_forward(text, pos.saturating_sub(4096).max(limit), pos)?;
        if from == pos {
            break;
        }
        let mut trailing = 0;
        let read_all = text.visit_chunks(ByteOffset(from)..ByteOffset(pos), &mut |part| {
            let tail: usize = part
                .chars()
                .rev()
                .take_while(|c| is_regional(*c))
                .map(char::len_utf8)
                .sum();
            trailing = if tail == part.len() {
                trailing + tail
            } else {
                tail
            };
        });
        if !read_all {
            return Err(LayoutError::BadRange(ByteOffset(from)..ByteOffset(pos)));
        }
        if trailing < pos - from {
            run_start = Some(pos - trailing);
        }
        pos = from;
    }
    let start = run_start.unwrap_or(line_start);
    Ok((at - start) % FLAG_BYTES)
}

pub(crate) fn read(
    text: &dyn TextSnapshot,
    range: Range<ByteOffset>,
) -> Result<String, LayoutError> {
    let mut out = String::with_capacity(range.end.0.saturating_sub(range.start.0));
    if text.visit_chunks(range.clone(), &mut |part| out.push_str(part)) {
        Ok(out)
    } else {
        Err(LayoutError::BadRange(range))
    }
}

/// Whether a character starts at `at`.
///
/// `byte_to_utf16` gives `None` for a byte inside a character, and also when the
/// store has no answer: the page is not in memory, the bytes lie past a snapshot's
/// pinned part, or the file failed. Only the `try_` form tells these apart, and only
/// the first is a reason to look at the next byte. Taking the others for it moves a
/// segment edge by however far a search got before a page arrived, or to the line's
/// end if none did.
///
/// A store whose plain query waits can still say "not yet" to the `try_` form, if other
/// threads pushed the page out of memory between the two calls. So "not yet" gets a
/// second round, in which the plain query can wait for the page again. A store that
/// never waits says the same both times, and that is the error.
fn is_char_edge(text: &dyn TextSnapshot, at: usize) -> Result<bool, LayoutError> {
    for _ in 0..2 {
        if text.byte_to_utf16(ByteOffset(at)).is_some() {
            return Ok(true);
        }
        match text.try_byte_to_utf16(ByteOffset(at)) {
            Ok(_) => return Ok(true),
            Err(LookupError::OutOfRange) => return Ok(false),
            Err(LookupError::NotYetKnown { .. }) => {}
            Err(_) => break,
        }
    }
    Err(LayoutError::BadRange(ByteOffset(at)..ByteOffset(at)))
}

/// UTF-8 has at most 3 bytes after a character's first byte, so a search for an edge
/// looks at most at this many places. A store that denies them all is reporting
/// trouble, and the error keeps this read bounded whatever the length of the line.
const CHAR_EDGE_LOOKUPS: usize = 4;

fn char_edge_forward(
    text: &dyn TextSnapshot,
    at: usize,
    limit: usize,
) -> Result<usize, LayoutError> {
    let mut at = at.min(limit);
    for _ in 0..CHAR_EDGE_LOOKUPS {
        if at >= limit || is_char_edge(text, at)? {
            return Ok(at);
        }
        at += 1;
    }
    Err(LayoutError::BadRange(ByteOffset(at)..ByteOffset(at)))
}

fn char_edge_backward(
    text: &dyn TextSnapshot,
    at: usize,
    limit: usize,
) -> Result<usize, LayoutError> {
    let mut at = at.max(limit);
    for _ in 0..CHAR_EDGE_LOOKUPS {
        if at <= limit || is_char_edge(text, at)? {
            return Ok(at);
        }
        at -= 1;
    }
    Err(LayoutError::BadRange(ByteOffset(at)..ByteOffset(at)))
}

/// The line's bytes without its line ending. Reads only the last few bytes.
pub(crate) fn content_range(
    text: &dyn TextSnapshot,
    line: LineIndex,
) -> Result<Range<ByteOffset>, LayoutError> {
    let range = text.line_range(line).ok_or(LayoutError::NoSuchLine(line))?;
    // The longest line ending is 3 bytes.
    let tail = char_edge_forward(
        text,
        range.end.0.saturating_sub(3).max(range.start.0),
        range.end.0,
    )?;
    let part = read(text, ByteOffset(tail)..range.end)?;
    let kept = part.trim_end_matches(LINE_ENDINGS).len();
    Ok(range.start..ByteOffset(tail + kept))
}

/// The boundary a segment starts at, for the nominal offset `nominal`. It depends
/// only on the text near `nominal`, so a segment's edges do not depend on which
/// segments were laid out before.
fn snap(
    text: &dyn TextSnapshot,
    line: &Range<ByteOffset>,
    nominal: usize,
) -> Result<usize, LayoutError> {
    let (start, end) = (line.start.0, line.end.0);
    if nominal <= start {
        return Ok(start);
    }
    if nominal >= end {
        return Ok(end);
    }
    let from = char_edge_forward(text, nominal.saturating_sub(SNAP_BEHIND).max(start), end)?;
    let to = char_edge_backward(text, nominal.saturating_add(SNAP_AHEAD).min(end), from)?;
    let window = read(text, ByteOffset(from)..ByteOffset(to))?;
    let stops = grapheme_stops(&window, flag_phase(text, start, from, &window)?);
    // `from` cannot pass `nominal`. It is `start`, or it lies at most 3 bytes past a
    // place 256 before `nominal`. An edge search that finds nothing returns an error.
    let target = nominal - from;
    let candidates = || {
        stops
            .iter()
            .copied()
            .filter(|at| *at >= target && (*at < window.len() || to == end))
    };
    let near_space = candidates()
        .take_while(|at| *at <= target + PREFER_SPACE_WITHIN)
        .find(|at| window[..*at].ends_with([' ', '\t']));
    if let Some(at) = near_space.or_else(|| candidates().next()) {
        return Ok(from + at);
    }
    // One cluster is longer than the window. Cut at a character edge, in a place
    // that depends on nothing else, and accept the wrong look at that one spot.
    char_edge_forward(text, nominal, end)
}

/// How a line is laid out: its content, and how many segments it has. One means whole.
struct LinePlan {
    content: Range<ByteOffset>,
    count: usize,
}

impl DirectWriteLayout {
    /// The nominal size of one segment here. A row is cut mid-word only where a word
    /// fills it, and such rows fall on multiples of the row's cells from the line's start.
    /// A nominal edge on such a multiple lets text with no spaces wrap with no short row.
    fn segment_bytes(&self) -> usize {
        match self.full_row_cells() {
            Some(cells) if cells <= SEGMENT_BYTES => SEGMENT_BYTES / cells * cells,
            _ => SEGMENT_BYTES,
        }
    }
    /// Only the bytes of a line can tell if it may stay whole between the two limits.
    /// That read is bounded by `LONG_LINE_BYTES`.
    fn plan(&self, text: &dyn TextSnapshot, line: LineIndex) -> Result<LinePlan, LayoutError> {
        let content = content_range(text, line)?;
        let bytes = content.end.0 - content.start.0;
        let whole = bytes <= SHAPED_LINE_BYTES
            || (bytes <= LONG_LINE_BYTES && {
                let mut fast = true;
                let read_all = text.visit_chunks(content.clone(), &mut |part| {
                    fast = fast && self.takes_fast_path(part.as_bytes());
                });
                if !read_all {
                    return Err(LayoutError::BadRange(content));
                }
                fast
            });
        let mut count = 1;
        if !whole {
            let size = self.segment_bytes();
            count = bytes.div_ceil(size);
            // Snapping can carry the second last edge right to the end of the line.
            // No empty segment follows it.
            let last = content.start.0 + (count - 1) * size;
            if count > 1 && snap(text, &content, last)? >= content.end.0 {
                count -= 1;
            }
        }
        Ok(LinePlan { content, count })
    }
    /// True when `layout_line` gives only the first segment of this line.
    pub fn is_long_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
    ) -> Result<bool, LayoutError> {
        Ok(self.plan(text, line)?.count > 1)
    }
    /// How many segments a line has. A line that stays whole has one.
    pub fn segment_count(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
    ) -> Result<usize, LayoutError> {
        Ok(self.plan(text, line)?.count)
    }
    fn range_in_plan(
        &self,
        text: &dyn TextSnapshot,
        plan: &LinePlan,
        line: LineIndex,
        index: usize,
    ) -> Result<Range<ByteOffset>, LayoutError> {
        if index >= plan.count {
            return Err(LayoutError::NoSuchSegment { line, index });
        }
        if plan.count == 1 {
            return Ok(plan.content.clone());
        }
        let base = plan.content.start.0;
        let size = self.segment_bytes();
        let start = snap(text, &plan.content, base + index * size)?;
        let end = if index + 1 == plan.count {
            plan.content.end.0
        } else {
            snap(text, &plan.content, base + (index + 1) * size)?
        };
        Ok(ByteOffset(start)..ByteOffset(end))
    }
    /// The bytes of one segment. Reads a bounded amount of text.
    pub fn segment_range(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        index: usize,
    ) -> Result<Range<ByteOffset>, LayoutError> {
        self.range_in_plan(text, &self.plan(text, line)?, line, index)
    }
    /// The segment that holds a caret: its line, its index and its bytes.
    ///
    /// The byte where two segments meet has two places, as at a wrap point. With
    /// `Affinity::Before` it is the end of the earlier segment, and with `After` the
    /// start of the later one. Pass the affinity that `hit_test_point` gave, then
    /// call `caret_rect` on that segment's layout with the same affinity.
    pub fn segment_at(
        &self,
        text: &dyn TextSnapshot,
        byte: ByteOffset,
        affinity: Affinity,
    ) -> Result<(LineIndex, usize, Range<ByteOffset>), LayoutError> {
        let line = text
            .byte_to_position(byte)
            .ok_or(LayoutError::BadRange(byte..byte))?
            .line;
        let plan = self.plan(text, line)?;
        let at = byte
            .0
            .min(plan.content.end.0)
            .saturating_sub(plan.content.start.0);
        let mut index = (at / self.segment_bytes()).min(plan.count - 1);
        let mut range = self.range_in_plan(text, &plan, line, index)?;
        if byte < range.start && index > 0 {
            index -= 1;
            range = self.range_in_plan(text, &plan, line, index)?;
        } else if byte >= range.end && index + 1 < plan.count {
            index += 1;
            range = self.range_in_plan(text, &plan, line, index)?;
        }
        if affinity == Affinity::Before && byte == range.start && index > 0 {
            index -= 1;
            range = self.range_in_plan(text, &plan, line, index)?;
        }
        Ok((line, index, range))
    }
    /// Lays out one segment. Positions are relative to the segment's own top left.
    pub fn layout_segment(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        index: usize,
        width: Dip,
    ) -> Result<LineLayout, LayoutError> {
        let plan = self.plan(text, line)?;
        let range = self.range_in_plan(text, &plan, line, index)?;
        let s = read(text, range.clone())?;
        if index + 1 < plan.count
            && let Some(layout) = self.fast_full_rows(&s, range.start, line, width)
        {
            return Ok(layout);
        }
        let column = if s.contains('\t') {
            self.column_before(text, line, range.start)?
        } else {
            0
        };
        self.layout_text(&s, range.start, line, width, column, plan.count > 1)
    }
    /// The tab column at `at`. The look-back is bounded. With no tab inside it,
    /// each byte since the line start counts as one cell, which is right for
    /// ASCII and only shifts tab stops for other text.
    fn column_before(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        at: ByteOffset,
    ) -> Result<usize, LayoutError> {
        let start = text
            .line_range(line)
            .ok_or(LayoutError::NoSuchLine(line))?
            .start;
        if at == start {
            return Ok(0);
        }
        let from = char_edge_forward(
            text,
            at.0.saturating_sub(LOOK_BACK_FOR_TAB).max(start.0),
            at.0,
        )?;
        let before = read(text, ByteOffset(from)..at)?;
        let column = match before.rfind('\t') {
            Some(last) => {
                // The tab ended on a stop, so only the text after it counts.
                before[last + 1..].chars().count()
            }
            None => from - start.0 + before.chars().count(),
        };
        Ok(column % self.tab_width())
    }
    /// A map with one entry per segment, for scrolling inside a long line. With wrap
    /// on, its values are heights. With wrap off they are widths, because the
    /// segments then sit side by side.
    ///
    /// The estimate comes from really laying out the first and the middle segment,
    /// and those two entries are already correct. It costs those two layouts, so it
    /// can run on a worker. A line that changes its kind of text along the way is
    /// estimated less well, and the entries correct themselves as segments are seen.
    pub fn long_line_map(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Result<HeightMap, LayoutError> {
        let count = self.plan(text, line)?.count;
        let extent_of = |index: usize| -> Result<Dip, LayoutError> {
            Ok(self.segment_extent(&self.layout_segment(text, line, index, width)?))
        };
        let first = extent_of(0)?;
        let middle = count / 2;
        let middle_extent = if middle > 0 {
            extent_of(middle)?
        } else {
            first
        };
        let estimate = Dip(((first.0 as f64 + middle_extent.0 as f64) / 2.0) as f32);
        let mut map = HeightMap::new(count, estimate).map_err(LayoutError::Height)?;
        map.set(LineIndex(0), first).map_err(LayoutError::Height)?;
        if middle > 0 {
            map.set(LineIndex(middle), middle_extent)
                .map_err(LayoutError::Height)?;
        }
        Ok(map)
    }
    /// Keeps a `long_line_map` usable after an edit inside its line, by matching its
    /// entry count to the line's new segment count. It adds or removes entries at the
    /// end only.
    ///
    /// Measured values stay. Those at or after the edit become close guesses, since
    /// their byte edges moved by the size of the edit, and `map.set` corrects each one
    /// when its segment is next laid out. The cost is one segment count, a bounded read.
    pub fn update_long_line_map(
        &self,
        map: &mut HeightMap,
        text: &dyn TextSnapshot,
        line: LineIndex,
    ) -> Result<(), LayoutError> {
        let count = self.plan(text, line)?.count;
        if count > map.len() {
            map.insert_lines(LineIndex(map.len()), count - map.len())
                .map_err(LayoutError::Height)?;
        } else if count < map.len() {
            map.remove_lines(count..map.len())
                .map_err(LayoutError::Height)?;
        }
        Ok(())
    }
    /// The value to give `long_line_map` once a segment has its real layout.
    pub fn segment_extent(&self, layout: &LineLayout) -> Dip {
        if self.wrap_enabled() {
            return layout.height;
        }
        let widest = layout
            .rows
            .iter()
            .flat_map(|r| &r.caret_stops)
            .map(|s| s.x.0.max(s.x_before.0))
            .fold(0.0, f32::max);
        Dip(widest)
    }
    /// The height a long line takes in the document's `HeightMap`. With wrap on it is
    /// the map's total. With wrap off the map holds widths, and the line is one row:
    /// use the larger of this and the `height` of the segments on screen, because a
    /// fallback font can make a row taller.
    pub fn long_line_height(&self, map: &HeightMap) -> Dip {
        if self.wrap_enabled() {
            Dip(map.total_height() as f32)
        } else {
            Dip(self.font().height)
        }
    }
    /// A slice around `byte`, at most `max_bytes` long, that cuts no cluster. It
    /// uses this engine's font. `Ok(None)` means one cluster overflows the window
    /// or `byte` is not inside a line's content.
    pub fn slice_around(
        &self,
        text: &dyn TextSnapshot,
        byte: ByteOffset,
        max_bytes: usize,
    ) -> Result<Option<Range<ByteOffset>>, LayoutError> {
        if max_bytes == 0 {
            return Ok(None);
        }
        let Some(position) = text.byte_to_position(byte) else {
            return Ok(None);
        };
        let content = content_range(text, position.line)?;
        if byte < content.start || byte > content.end {
            return Ok(None);
        }
        let context = max_bytes.max(64);
        let left = char_edge_forward(
            text,
            byte.0.saturating_sub(context).max(content.start.0),
            byte.0,
        )?;
        let right = char_edge_backward(
            text,
            byte.0.saturating_add(context).min(content.end.0),
            byte.0,
        )?;
        let window = read(text, ByteOffset(left)..ByteOffset(right))?;
        let phase = flag_phase(text, content.start.0, left, &window)?;
        let edges: Vec<usize> = self
            .stops_in_window(&window, phase)?
            .into_iter()
            .filter(|at| {
                (*at > 0 || left == content.start.0)
                    && (*at < window.len() || right == content.end.0)
            })
            .map(|at| left + at)
            .collect();
        let lowest = byte.0.saturating_sub(max_bytes / 2);
        let Some(start) = edges.iter().copied().find(|b| *b >= lowest && *b <= byte.0) else {
            return Ok(None);
        };
        let highest = start.saturating_add(max_bytes);
        let end = edges
            .iter()
            .rev()
            .copied()
            .find(|b| *b >= byte.0 && *b <= highest);
        Ok(end.map(|end| ByteOffset(start)..ByteOffset(end)))
    }
    /// Offsets in `window` that are both grapheme stops and DirectWrite cluster edges.
    fn stops_in_window(&self, window: &str, phase: usize) -> Result<Vec<usize>, LayoutError> {
        // Ligatures are off for ASCII only in fonts that can take the fast path.
        // Another font, such as Calibri, can join "fi" into one cluster.
        if window.is_ascii() && self.font().fast_ok {
            return Ok((0..=window.len()).collect());
        }
        let clusters = shaped::cluster_edges(self.font(), window)?;
        let mut stops = grapheme_stops(window, phase);
        // DirectWrite pairs flags from the window's start, so it says nothing useful
        // about a window that starts in the middle of a flag.
        if phase == 0 {
            stops.retain(|at| clusters.binary_search(at).is_ok());
        }
        Ok(stops)
    }
    /// An error when `at` falls inside a cluster, judged from text on both sides.
    pub(crate) fn check_edge(
        &self,
        text: &dyn TextSnapshot,
        content: &Range<ByteOffset>,
        at: ByteOffset,
    ) -> Result<(), LayoutError> {
        if at <= content.start || at >= content.end {
            return Ok(());
        }
        let from = char_edge_forward(
            text,
            at.0.saturating_sub(EDGE_CONTEXT).max(content.start.0),
            at.0,
        )?;
        let to = char_edge_backward(
            text,
            at.0.saturating_add(EDGE_CONTEXT).min(content.end.0),
            at.0,
        )?;
        let window = read(text, ByteOffset(from)..ByteOffset(to))?;
        let phase = flag_phase(text, content.start.0, from, &window)?;
        if self
            .stops_in_window(&window, phase)?
            .binary_search(&(at.0 - from))
            .is_err()
        {
            return Err(LayoutError::CutCluster(at));
        }
        Ok(())
    }
}
