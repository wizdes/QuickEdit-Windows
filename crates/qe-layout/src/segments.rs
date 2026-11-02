//! Long lines are cut into segments on grapheme boundaries. Every read here is
//! bounded, whatever the length of the line.
use crate::{DirectWriteLayout, HeightMap, LayoutError, shaped};
use qe_api::{ByteOffset, Dip, LineIndex, LineLayout, TextSnapshot};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Lines with more content bytes than this are laid out only in segments.
pub const LONG_LINE_BYTES: usize = 16 * 1024;
/// The nominal size of one segment. A real segment ends at the first good
/// boundary at or after a multiple of this, up to 1 KiB later. DirectWrite's
/// shaping sets this size. Arabic took about 1.9 ms per KiB on a PC at 100% CPU, so
/// 2 KiB keeps a cold segment under the 5 ms limit even then. A screen costs the
/// same in total, because it just uses more small segments.
pub const SEGMENT_BYTES: usize = 2 * 1024;
const SNAP_BEHIND: usize = 256;
const SNAP_AHEAD: usize = 1024;
const PREFER_SPACE_WITHIN: usize = 64;
const LOOK_BACK_FOR_TAB: usize = 1024;
/// How far each side of a slice edge is read to check that it cuts no cluster.
const EDGE_CONTEXT: usize = 128;

const ZWJ: char = '\u{200D}';
pub(crate) const LINE_ENDINGS: [char; 5] = ['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}'];

fn is_regional(ch: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch)
}

fn is_lam_alef(before: Option<char>, after: Option<char>) -> bool {
    before == Some('\u{644}')
        && after.is_some_and(|c| matches!(c, '\u{622}' | '\u{623}' | '\u{625}' | '\u{627}'))
}

/// Byte offsets in `s` where a caret may stop by Unicode's grapheme rules, plus
/// two rules of ours. A caret never stops right after a zero-width joiner, so that
/// `a ZWJ b` stays whole. It never stops between lam and alef, which draw as one
/// letter even in a font that keeps two glyphs for them. `line_offset` is how far `s` starts from the line start. It
/// settles which regional indicators pair up when `s` starts inside a run of them.
fn grapheme_stops(s: &str, line_offset: usize) -> Vec<usize> {
    let run_end = s
        .char_indices()
        .find(|(_, c)| !is_regional(*c))
        .map_or(s.len(), |(at, _)| at);
    let mut stops: Vec<usize> = s
        .grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(Some(s.len()))
        .filter(|at| {
            let before = s[..*at].chars().next_back();
            before != Some(ZWJ) && !is_lam_alef(before, s[*at..].chars().next())
        })
        .collect();
    if line_offset > 0 && run_end > 0 {
        stops.retain(|at| *at >= run_end || (line_offset + at).is_multiple_of(8));
    }
    stops
}

/// For each byte of `s`, and its end, whether a caret may stop there.
pub(crate) fn stop_table(s: &str) -> Vec<bool> {
    let mut table = vec![false; s.len() + 1];
    for at in grapheme_stops(s, 0) {
        table[at] = true;
    }
    table
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

fn char_edge_forward(text: &dyn TextSnapshot, at: usize, limit: usize) -> usize {
    let mut at = at.min(limit);
    while at < limit && text.byte_to_utf16(ByteOffset(at)).is_none() {
        at += 1;
    }
    at
}
fn char_edge_backward(text: &dyn TextSnapshot, at: usize, limit: usize) -> usize {
    let mut at = at.max(limit);
    while at > limit && text.byte_to_utf16(ByteOffset(at)).is_none() {
        at -= 1;
    }
    at
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
    );
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
    let from = char_edge_forward(text, nominal.saturating_sub(SNAP_BEHIND).max(start), end);
    let to = char_edge_backward(text, nominal.saturating_add(SNAP_AHEAD).min(end), from);
    let window = read(text, ByteOffset(from)..ByteOffset(to))?;
    let stops = grapheme_stops(&window, from - start);
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
    Ok(char_edge_forward(text, nominal, end))
}

fn segment_count_of(content: &Range<ByteOffset>) -> usize {
    let bytes = content.end.0 - content.start.0;
    if bytes <= LONG_LINE_BYTES {
        1
    } else {
        bytes.div_ceil(SEGMENT_BYTES)
    }
}

impl DirectWriteLayout {
    /// True when `layout_line` gives only the first segment of this line.
    pub fn is_long_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
    ) -> Result<bool, LayoutError> {
        let range = content_range(text, line)?;
        Ok(range.end.0 - range.start.0 > LONG_LINE_BYTES)
    }
    /// How many segments a line has. A line that is not long has one.
    pub fn segment_count(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
    ) -> Result<usize, LayoutError> {
        Ok(segment_count_of(&content_range(text, line)?))
    }
    /// The bytes of one segment. Reads a bounded amount of text.
    pub fn segment_range(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        index: usize,
    ) -> Result<Range<ByteOffset>, LayoutError> {
        let range = content_range(text, line)?;
        let count = segment_count_of(&range);
        if index >= count {
            return Err(LayoutError::NoSuchSegment { line, index });
        }
        if count == 1 {
            return Ok(range);
        }
        let start = snap(text, &range, range.start.0 + index * SEGMENT_BYTES)?;
        let end = snap(text, &range, range.start.0 + (index + 1) * SEGMENT_BYTES)?;
        Ok(ByteOffset(start)..ByteOffset(end))
    }
    /// The segment that holds a byte: its line, its index and its bytes.
    pub fn segment_at(
        &self,
        text: &dyn TextSnapshot,
        byte: ByteOffset,
    ) -> Result<(LineIndex, usize, Range<ByteOffset>), LayoutError> {
        let line = text
            .byte_to_position(byte)
            .ok_or(LayoutError::BadRange(byte..byte))?
            .line;
        let content = content_range(text, line)?;
        let count = segment_count_of(&content);
        let at = byte.0.min(content.end.0).saturating_sub(content.start.0);
        let mut index = (at / SEGMENT_BYTES).min(count - 1);
        let mut range = self.segment_range(text, line, index)?;
        if byte < range.start && index > 0 {
            index -= 1;
            range = self.segment_range(text, line, index)?;
        } else if byte >= range.end && index + 1 < count {
            index += 1;
            range = self.segment_range(text, line, index)?;
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
        let range = self.segment_range(text, line, index)?;
        let s = read(text, range.clone())?;
        let column = if s.contains('\t') {
            self.column_before(text, line, range.start)?
        } else {
            0
        };
        let partial = self.is_long_line(text, line)?;
        self.layout_text(&s, range.start, line, width, column, partial)
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
        );
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
    /// A map with one entry per segment, for scrolling inside a long line. With
    /// wrap on, its values are heights. With wrap off they are widths, because
    /// the segments then sit side by side. The estimate comes from the character
    /// count of the first segment. The crate documentation shows how to use it.
    pub fn long_line_map(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Result<HeightMap, LayoutError> {
        let count = self.segment_count(text, line)?;
        let sample = read(text, self.segment_range(text, line, 0)?)?;
        let chars_per_byte = sample.chars().count() as f64 / sample.len().max(1) as f64;
        let across = SEGMENT_BYTES as f64 * chars_per_byte * self.font().advance_exact;
        let estimate = if self.wrap_enabled() {
            let rows = (across / (width.0 as f64).max(self.font().advance_exact)).ceil();
            rows.max(1.0) * self.font().height as f64
        } else {
            across
        };
        HeightMap::new(count, Dip(estimate as f32)).map_err(LayoutError::Height)
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
        );
        let right = char_edge_backward(
            text,
            byte.0.saturating_add(context).min(content.end.0),
            byte.0,
        );
        let window = read(text, ByteOffset(left)..ByteOffset(right))?;
        let edges: Vec<usize> = self
            .stops_in_window(&window, left - content.start.0)?
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
    fn stops_in_window(&self, window: &str, line_offset: usize) -> Result<Vec<usize>, LayoutError> {
        // Ligatures are off for ASCII only in fonts that can take the fast path.
        // Another font, such as Calibri, can join "fi" into one cluster.
        if window.is_ascii() && self.font().fast_ok {
            return Ok((0..=window.len()).collect());
        }
        let clusters = shaped::cluster_edges(self.font(), window)?;
        let mut stops = grapheme_stops(window, line_offset);
        stops.retain(|at| clusters.binary_search(at).is_ok());
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
        );
        let to = char_edge_backward(
            text,
            at.0.saturating_add(EDGE_CONTEXT).min(content.end.0),
            at.0,
        );
        let window = read(text, ByteOffset(from)..ByteOffset(to))?;
        if self
            .stops_in_window(&window, from - content.start.0)?
            .binary_search(&(at.0 - from))
            .is_err()
        {
            return Err(LayoutError::CutCluster(at));
        }
        Ok(())
    }
}
