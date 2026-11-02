//! The one rule for where a caret may stop.
//!
//! Every move, deletion and edit asks this module. The answer comes from the text
//! around the caret, so it is the same whether or not layout has arrived and whatever
//! the line's length:
//! - A caret never stops inside a line ending, CRLF included.
//! - A caret never stops inside a grapheme cluster. The test is the one the layout
//!   crate uses: its caret stops on a short window of text around the caret. Plain
//!   ASCII answers without that step, so this module calls no system code for it.
//!
//! Every read is bounded and uses `try_` queries, so the UI thread never waits. When
//! the text is not in memory, the answer is `None`, and the caret does not move.
use qe_api::{ByteOffset, Dip, LineIndex, LookupError, TextSnapshot};
use qe_buffer::PieceStore;
use qe_layout::DirectWriteLayout;
use std::{cell::RefCell, ops::Range};

/// The first window read on each side of the caret. It holds any common cluster.
const WINDOW: usize = 64;
/// The widest window tried. A cluster longer than this counts as ending at the window.
const WIDEST_WINDOW: usize = 2048;
/// How far back to look for the start of a run of regional indicators.
const FLAG_LOOK_BACK: usize = 64 * 1024;
const FLAG_BYTES: usize = 8;
/// One read for word movement.
const WORD_READ: usize = 64 * 1024;
/// Word movement over blank text stops after this many reads.
const WORD_READS: usize = 16;

/// A line's edges in bytes.
#[derive(Clone, Debug)]
pub(crate) struct LineEdges {
    pub index: usize,
    pub start: usize,
    pub content_end: usize,
}

pub(crate) struct Stops<'a> {
    pub text: &'a dyn TextSnapshot,
    pub engine: &'a DirectWriteLayout,
    /// A shaping failure. The answer then uses character edges, and the caller shows this.
    pub problem: RefCell<Option<String>>,
}

impl<'a> Stops<'a> {
    pub fn new(text: &'a dyn TextSnapshot, engine: &'a DirectWriteLayout) -> Self {
        Self {
            text,
            engine,
            problem: RefCell::new(None),
        }
    }
    pub fn line(&self, index: usize) -> Result<LineEdges, LookupError> {
        let known = self.text.try_known_line(LineIndex(index))?;
        let ending = if known.complete {
            self.text.try_line_ending_len(LineIndex(index))?
        } else {
            0
        };
        Ok(LineEdges {
            index,
            start: known.range.start.0,
            content_end: known.range.end.0 - ending,
        })
    }
    pub fn line_of(&self, at: usize) -> Result<LineEdges, LookupError> {
        let position = self.text.try_byte_to_position(ByteOffset(at))?;
        self.line(position.line.0)
    }
    pub fn edge_forward(&self, at: usize, limit: usize) -> Result<usize, LookupError> {
        let mut at = at.min(limit);
        while at < limit {
            match self.text.try_byte_to_utf16(ByteOffset(at)) {
                Ok(_) => break,
                Err(LookupError::OutOfRange) => at += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(at)
    }
    pub fn edge_backward(&self, at: usize, limit: usize) -> Result<usize, LookupError> {
        let mut at = at.max(limit);
        while at > limit {
            match self.text.try_byte_to_utf16(ByteOffset(at)) {
                Ok(_) => break,
                Err(LookupError::OutOfRange) => at -= 1,
                Err(e) => return Err(e),
            }
        }
        Ok(at)
    }
    fn read(&self, range: Range<usize>) -> Result<String, LookupError> {
        let mut out = String::with_capacity(range.end.saturating_sub(range.start));
        self.text
            .try_visit_chunks(ByteOffset(range.start)..ByteOffset(range.end), &mut |s| {
                out.push_str(s)
            })?;
        Ok(out)
    }
    /// The two characters before and the two after `at`, inside its line's content.
    fn around(&self, line: &LineEdges, at: usize) -> Result<[Option<char>; 4], LookupError> {
        let before = self.edge_forward(at.saturating_sub(8).max(line.start), at)?;
        let after = self.edge_backward((at + 8).min(line.content_end), at)?;
        let head = self.read(before..at)?;
        let tail = self.read(at..after)?;
        let mut back = head.chars().rev();
        let c1 = back.next();
        let c0 = back.next();
        let mut forward = tail.chars();
        let c2 = forward.next();
        let c3 = forward.next();
        Ok([c0, c1, c2, c3])
    }
    /// The caret stops in a window around `at`, as whole-document bytes, sorted. The
    /// bool is true when the window's first stop is a real edge of the line.
    fn window_stops(
        &self,
        line: &LineEdges,
        at: usize,
        reach: usize,
    ) -> Result<(Vec<usize>, bool, bool), LookupError> {
        let mut from = self.edge_forward(at.saturating_sub(reach).max(line.start), at)?;
        let to = self.edge_backward((at + reach).min(line.content_end), at)?;
        let mut text = self.read(from..to)?;
        let whole_start = from == line.start;
        if !whole_start {
            // Start where a cluster must start: at an ASCII character that does not
            // follow a joiner, far enough behind the caret. Otherwise pair up flags.
            let limit = at - from;
            if let Some(i) = text[..limit].find(|c: char| c.is_ascii()) {
                from += i;
                text.drain(..i);
            } else if text.chars().next().is_some_and(is_regional) {
                let skip = self.flag_skip(line, from)?;
                from += skip;
                text.drain(..skip);
            }
        }
        let shaped = PieceStore::new(&text);
        let stops = match self
            .engine
            .try_layout_line(&shaped, LineIndex(0), Dip(1_000_000.0))
        {
            Ok(layout) => {
                let mut stops: Vec<usize> = layout
                    .rows
                    .iter()
                    .flat_map(|r| r.caret_stops.iter().map(|s| from + s.byte.0))
                    .collect();
                stops.sort_unstable();
                stops.dedup();
                stops
            }
            Err(e) => {
                *self.problem.borrow_mut() = Some(format!(
                    "Text shaping failed, so the caret moved one character: {e}"
                ));
                text.char_indices()
                    .map(|(i, _)| from + i)
                    .chain(std::iter::once(from + text.len()))
                    .collect()
            }
        };
        Ok((stops, whole_start, to == line.content_end))
    }
    /// How many bytes to skip from `from`, which is in a run of regional indicators,
    /// to reach the next flag edge.
    fn flag_skip(&self, line: &LineEdges, from: usize) -> Result<usize, LookupError> {
        let start = self.edge_forward(from.saturating_sub(FLAG_LOOK_BACK).max(line.start), from)?;
        let text = self.read(start..from)?;
        let run: usize = text
            .chars()
            .rev()
            .take_while(|c| is_regional(*c))
            .map(char::len_utf8)
            .sum();
        Ok((FLAG_BYTES - run % FLAG_BYTES) % FLAG_BYTES)
    }
    pub fn is_stop(&self, at: usize) -> Result<bool, LookupError> {
        let line = self.line_of(at)?;
        if at == line.start || at == line.content_end {
            return Ok(true);
        }
        if at > line.content_end {
            return Ok(false);
        }
        let [_, c1, c2, _] = self.around(&line, at)?;
        if c1.is_some_and(|c| c.is_ascii()) && c2.is_some_and(|c| c.is_ascii()) {
            return Ok(true);
        }
        let (stops, _, _) = self.window_stops(&line, at, WINDOW)?;
        Ok(stops.binary_search(&at).is_ok())
    }
    /// The stop before `at`. `None` at the start of the text.
    pub fn before(&self, at: usize) -> Result<Option<usize>, LookupError> {
        let line = self.line_of(at)?;
        if at <= line.start {
            if line.index == 0 {
                return Ok(None);
            }
            return Ok(Some(self.line(line.index - 1)?.content_end));
        }
        if at > line.content_end {
            return Ok(Some(line.content_end));
        }
        let [c0, c1, _, _] = self.around(&line, at)?;
        if let Some(c1) = c1
            && c1.is_ascii()
            && (c0.is_none_or(|c| c.is_ascii()))
        {
            return Ok(Some(at - 1));
        }
        let mut reach = WINDOW;
        loop {
            let (stops, whole_start, _) = self.window_stops(&line, at, reach)?;
            let found = stops.iter().rev().find(|s| **s < at).copied();
            let at_window_edge = found.is_none_or(|s| s == stops[0]) && !whole_start;
            if !at_window_edge || reach >= WIDEST_WINDOW {
                return Ok(Some(found.unwrap_or(line.start.max(
                    self.edge_forward(at.saturating_sub(reach).max(line.start), at)?,
                ))));
            }
            reach *= 4;
        }
    }
    /// The stop after `at`. `None` at the end of the text.
    pub fn after(&self, at: usize) -> Result<Option<usize>, LookupError> {
        let line = self.line_of(at)?;
        if at >= line.content_end {
            return match self.line(line.index + 1) {
                Ok(next) => Ok(Some(next.start)),
                Err(LookupError::OutOfRange) => Ok(None),
                Err(e) => Err(e),
            };
        }
        let [_, _, c2, c3] = self.around(&line, at)?;
        if let Some(c2) = c2
            && c2.is_ascii()
            && (c3.is_none_or(|c| c.is_ascii()))
        {
            return Ok(Some(at + 1));
        }
        let mut reach = WINDOW;
        loop {
            let (stops, _, whole_end) = self.window_stops(&line, at, reach)?;
            let found = stops.iter().find(|s| **s > at).copied();
            let last = stops.last().copied().unwrap_or(at);
            let at_window_edge = found.is_none_or(|s| s == last) && !whole_end;
            if !at_window_edge || reach >= WIDEST_WINDOW {
                let edge = self.edge_backward((at + reach).min(line.content_end), at)?;
                return Ok(Some(found.unwrap_or(edge)));
            }
            reach *= 4;
        }
    }
    /// `at` when it is a stop, otherwise the next stop. An edit can leave a caret
    /// inside a cluster or between CR and LF, because it joins the text on its two
    /// sides. The caret stays with the text it followed, so it moves forward.
    pub fn snap(&self, at: usize) -> Result<usize, LookupError> {
        if self.is_stop(at)? {
            return Ok(at);
        }
        Ok(self.after(at)?.unwrap_or(at))
    }
    /// The byte `count` characters away inside the line's content, or `None` when the
    /// line has fewer. An estimate of one wrapped row uses this.
    pub fn chars_away(
        &self,
        line: &LineEdges,
        at: usize,
        count: usize,
        forward: bool,
    ) -> Result<Option<usize>, LookupError> {
        let reach = count.saturating_mul(4).min(WORD_READ);
        if forward {
            let to = self.edge_backward(at.saturating_add(reach).min(line.content_end), at)?;
            let text = self.read(at..to)?;
            let mut indices = text.char_indices().map(|(i, _)| at + i);
            Ok(indices.nth(count))
        } else {
            let from = self.edge_forward(at.saturating_sub(reach).max(line.start), at)?;
            let text = self.read(from..at)?;
            let mut indices = text.char_indices().rev().map(|(i, _)| from + i);
            Ok(count.checked_sub(1).and_then(|back| indices.nth(back)))
        }
    }
    fn read_after(&self, at: usize, end: usize) -> Result<(String, usize), LookupError> {
        let to = self.edge_backward((at + WORD_READ).min(end), at)?;
        Ok((self.read(at..to)?, to))
    }
    fn read_before(&self, at: usize) -> Result<(String, usize), LookupError> {
        let from = self.edge_forward(at.saturating_sub(WORD_READ), at)?;
        Ok((self.read(from..at)?, from))
    }
    /// AppKit's move word forward: past any gap, then to the end of the next word,
    /// across lines. A word longer than one read ends at the read's edge.
    pub fn word_end_after(&self, at: usize) -> Result<usize, LookupError> {
        let end = self.text.status().known_bytes.0;
        let mut pos = at;
        for _ in 0..WORD_READS {
            let (text, to) = self.read_after(pos, end)?;
            if let Some(found) = next_word_end(&text) {
                return self.snap(pos + found);
            }
            if to >= end {
                return Ok(end);
            }
            pos = to;
        }
        self.snap(pos)
    }
    /// AppKit's move word backward: past any gap, then to the start of that word.
    pub fn word_start_before(&self, at: usize) -> Result<usize, LookupError> {
        let mut pos = at;
        for _ in 0..WORD_READS {
            let (text, from) = self.read_before(pos)?;
            if let Some(found) = previous_word_start(&text) {
                return self.snap(from + found);
            }
            if from == 0 {
                return Ok(0);
            }
            pos = from;
        }
        self.snap(pos)
    }
    /// What a double click selects, inside one line. `look_back` is true when the
    /// click landed on the character before `at`.
    pub fn word_at(&self, at: usize, look_back: bool) -> Result<Range<usize>, LookupError> {
        let line = self.line_of(at)?;
        if line.start == line.content_end {
            return Ok(line.start..line.start);
        }
        let at = at.min(line.content_end);
        let from = self.edge_forward(at.saturating_sub(WORD_READ / 2).max(line.start), at)?;
        let to = self.edge_backward((at + WORD_READ / 2).min(line.content_end), at)?;
        let text = self.read(from..to)?;
        let local = at - from;
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        if chars.is_empty() {
            return Ok(at..at);
        }
        let mut index = chars.partition_point(|(i, _)| *i < local);
        if index > 0 && (index == chars.len() || look_back) {
            index -= 1;
        }
        let class = class_at(&chars, index);
        let mut first = index;
        let mut last = index + 1;
        if class != Class::Other {
            while first > 0 && class_at(&chars, first - 1) == class {
                first -= 1;
            }
            while last < chars.len() && class_at(&chars, last) == class {
                last += 1;
            }
        }
        let start = from + chars[first].0;
        let end = from + chars.get(last).map_or(text.len(), |(i, _)| *i);
        let end = self.snap(end)?.min(line.content_end);
        Ok(start..end)
    }
}

fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

fn is_mark(c: char) -> bool {
    matches!(c as u32,
        0x300..=0x36F | 0x483..=0x489 | 0x591..=0x5BD | 0x610..=0x61A | 0x64B..=0x65F
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200D | 0x20D0..=0x20F0 | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F | 0xE0100..=0xE01EF)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_mark(c)
}

/// A mark that stays inside a word when the same kind of character is on both sides,
/// as in `don't`, `a.b` and `1,000`, following Unicode's word rules.
fn joins_word(before: char, c: char, after: char) -> bool {
    match c {
        '\'' | '.' | '\u{2019}' => is_word(before) && is_word(after),
        ',' | ';' => before.is_numeric() && after.is_numeric(),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class_at(chars: &[(usize, char)], i: usize) -> Class {
    let c = chars[i].1;
    if is_word(c) {
        return Class::Word;
    }
    if i > 0
        && let Some((_, after)) = chars.get(i + 1)
        && joins_word(chars[i - 1].1, c, *after)
    {
        return Class::Word;
    }
    if c == ' ' || c == '\t' || (c.is_whitespace() && !c.is_control()) {
        return Class::Space;
    }
    Class::Other
}

/// The byte where the first word in `text` ends. `None` when no word starts here, or
/// when it reaches the end of `text`, unless that is a real end the caller handles.
fn next_word_end(text: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let first = (0..chars.len()).find(|i| class_at(&chars, *i) == Class::Word)?;
    let last = (first..chars.len())
        .find(|i| class_at(&chars, *i) != Class::Word)
        .unwrap_or(chars.len());
    Some(chars.get(last).map_or(text.len(), |(i, _)| *i))
}

/// The byte where the last word in `text` starts.
fn previous_word_start(text: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let last = (0..chars.len())
        .rev()
        .find(|i| class_at(&chars, *i) == Class::Word)?;
    let first = (0..=last)
        .rev()
        .find(|i| class_at(&chars, *i) != Class::Word)
        .map_or(0, |i| i + 1);
    Some(chars[first].0)
}

/// A line ending chosen as the Mac's `mostCommonLineEnding` does: it counts LF, CRLF
/// and lone CR only. Ties go to LF, then CRLF, then CR. No ending at all means LF.
pub fn preferred_line_ending_bytes(bytes: &[u8]) -> &'static str {
    // As on the Mac: most files have no CR, and this answers those in one fast search.
    if !bytes.contains(&b'\r') {
        return "\n";
    }
    let (mut lf_total, mut cr_total, mut crlf) = (0_u64, 0_u64, 0_u64);
    let mut previous = 0_u8;
    for byte in bytes {
        lf_total += u64::from(*byte == b'\n');
        cr_total += u64::from(*byte == b'\r');
        crlf += u64::from(previous == b'\r' && *byte == b'\n');
        previous = *byte;
    }
    let (lf, cr) = (lf_total - crlf, cr_total - crlf);
    if lf >= crlf && lf >= cr {
        "\n"
    } else if crlf >= cr {
        "\r\n"
    } else {
        "\r"
    }
}
