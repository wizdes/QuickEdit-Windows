//! Text units over a snapshot: reads, stops, moves. No COM here. Every read uses a `try_`
//! query, so nothing here waits for disk or shapes text.
use crate::clusters::{is_regional, is_stop};
use qe_api::{ByteOffset, LineIndex, LookupError, TextSnapshot, Utf16Offset};
use std::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Largest answer to one GetText call, in UTF-16 units, whatever limit the caller gives.
/// A client reads on with a later range. The measured cost is in the README.
pub const GET_TEXT_CAP: usize = 1 << 20;
/// One read. A mapped store refuses a single read over 448 KiB, and this many UTF-16 units
/// are at most 384 KiB.
const READ_SLICE: usize = 128 * 1024;
/// A move stops here and returns how far it got, as UIA allows, because it runs on the window's
/// thread. It always makes one step, so a pause on a busy machine cannot look like the document's
/// edge to a client that moves by one.
pub const MOVE_BUDGET: Duration = Duration::from_millis(8);
static MOVE_LIMIT_MICROS: AtomicU64 = AtomicU64::new(MOVE_BUDGET.as_micros() as u64);

/// A test that checks exact counts can widen the limit, because a pause on a busy machine would
/// otherwise stop its move short, as the limit allows.
#[doc(hidden)]
pub fn set_move_limit_for_tests(limit: Duration) {
    MOVE_LIMIT_MICROS.store(limit.as_micros() as u64, Ordering::Relaxed);
}

fn within_limit(began: Instant) -> bool {
    began.elapsed().as_micros() as u64 <= MOVE_LIMIT_MICROS.load(Ordering::Relaxed)
}
const FIRST_WINDOW: usize = 64;
const WIDE_WINDOW: usize = 2048;
/// Runs of blank text a word move crosses before it counts the run's end as a stop.
const WORD_STEPS: usize = 8;
/// How far back a character move looks for the start of a run of flags, in UTF-16 units. A run
/// of up to 16,384 flags stays paired in both directions. In a longer run the pairs can differ.
pub const FLAG_RUN_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}
impl TextRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The index or memory does not hold this text yet. Asking again later can work.
    NotYetKnown,
    OutOfRange,
    Failed(String),
    /// This document has no caret stops of its own. See `TextDoc::stop_after`.
    NoStops,
}
impl From<LookupError> for ReadError {
    fn from(error: LookupError) -> Self {
        match error {
            LookupError::NotYetKnown { .. } => Self::NotYetKnown,
            LookupError::OutOfRange => Self::OutOfRange,
            // A single read over the store's limit. Only a bug here could cause it.
            LookupError::TooLong { .. } => Self::Failed("One text read was too long.".into()),
            LookupError::Failed(failure) => Self::Failed(failure.message),
        }
    }
}
pub type Answer<T> = Result<T, ReadError>;

/// The units this crate acts on. UIA's Format and Page become `Document`, because plain
/// text has one format and no pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Character,
    Word,
    Line,
    Paragraph,
    Document,
}

/// What the movement rules need from a document.
pub trait TextDoc {
    fn snapshot(&self) -> &dyn TextSnapshot;
    /// The word or the run of other text at a byte, within its line.
    fn word_at(&mut self, byte: usize) -> Answer<Range<usize>>;
    /// Where the rows on screen on this line start and end, in bytes. Empty when no layout is
    /// known. A row's end matters where the next row is not on screen.
    fn row_stops(&mut self, line: usize) -> Vec<usize>;
    /// The caret stop after this byte, by the same rule the arrow keys use. A document with
    /// no editor behind it, such as a field, says `NoStops`, and `clusters` serves it.
    fn stop_after(&mut self, _byte: usize) -> Answer<Option<usize>> {
        Err(ReadError::NoStops)
    }
    /// The caret stop before this byte. See `stop_after`.
    fn stop_before(&mut self, _byte: usize) -> Answer<Option<usize>> {
        Err(ReadError::NoStops)
    }
    fn line_of(&self, byte: usize) -> Answer<usize> {
        Ok(self
            .snapshot()
            .try_byte_to_position(ByteOffset(byte))?
            .line
            .0)
    }
}

/// The document's length in UTF-16 units. While a mapped file's index grows, this is the
/// part known so far, and the bool is false.
pub fn known_len(text: &dyn TextSnapshot) -> Answer<(usize, bool)> {
    match text.try_len_utf16() {
        Ok(len) => Ok((len.0, true)),
        Err(LookupError::NotYetKnown { .. }) => {
            let status = text.status();
            if let Some(failure) = status.failure {
                return Err(ReadError::Failed(failure.message));
            }
            Ok((status.known_utf16.0, false))
        }
        Err(other) => Err(other.into()),
    }
}

/// The byte of a UTF-16 position. A position inside a surrogate pair counts as the start of
/// the pair.
pub fn byte_of(text: &dyn TextSnapshot, utf16: usize) -> Answer<usize> {
    match text.try_utf16_to_byte(Utf16Offset(utf16)) {
        Ok(byte) => Ok(byte.0),
        Err(LookupError::OutOfRange) if utf16 > 0 && utf16 <= known_len(text)?.0 => {
            Ok(text.try_utf16_to_byte(Utf16Offset(utf16 - 1))?.0)
        }
        Err(error) => Err(error.into()),
    }
}
pub fn utf16_of(text: &dyn TextSnapshot, byte: usize) -> Answer<usize> {
    Ok(text.try_byte_to_utf16(ByteOffset(byte))?.0)
}

/// Read at most `limit` UTF-16 units, and never more than `GET_TEXT_CAP`. A negative limit
/// means the cap. A limit that ends inside a surrogate pair stops before the pair. If any part
/// cannot be read, the answer is the error, so a short answer always means the limit or the cap.
pub fn read_text(text: &dyn TextSnapshot, range: TextRange, limit: i32) -> Answer<String> {
    let (len, _) = known_len(text)?;
    if range.start > range.end || range.end > len {
        return Err(ReadError::OutOfRange);
    }
    let most = usize::try_from(limit).map_or(GET_TEXT_CAP, |n| n.min(GET_TEXT_CAP));
    read_units(
        text,
        range.start,
        range.start.saturating_add(most).min(range.end),
    )
}

/// `read_text` for a thread that may wait for disk, which is any thread but the window's. So a
/// read needs no pages to be in memory already, and it answers in one call. It must not run on
/// the window's thread.
pub fn read_text_waiting(text: &dyn TextSnapshot, range: TextRange, limit: i32) -> Answer<String> {
    let (len, _) = known_len(text)?;
    if range.start > range.end || range.end > len {
        return Err(ReadError::OutOfRange);
    }
    let most = usize::try_from(limit).map_or(GET_TEXT_CAP, |n| n.min(GET_TEXT_CAP));
    let stop = range.start.saturating_add(most).min(range.end);
    // A position inside a surrogate pair has no byte, so it counts as the start of the pair.
    let byte = |unit: usize| {
        text.utf16_to_byte(Utf16Offset(unit))
            .or_else(|| text.utf16_to_byte(Utf16Offset(unit.saturating_sub(1))))
            .map(|found| found.0)
            .ok_or(ReadError::OutOfRange)
    };
    let (from, to) = (byte(range.start)?, byte(stop)?);
    let mut answer = String::with_capacity(to.saturating_sub(from));
    if !text.visit_chunks(ByteOffset(from)..ByteOffset(to), &mut |part| {
        answer.push_str(part);
    }) {
        return Err(ReadError::Failed(
            "The text could not be read from its file.".into(),
        ));
    }
    Ok(answer)
}

/// Reads the units in `start..stop`, with no cap, in slices a mapped store accepts.
pub(crate) fn read_units(text: &dyn TextSnapshot, start: usize, stop: usize) -> Answer<String> {
    let mut answer = String::new();
    let mut at = start;
    let mut from = byte_of(text, at)?;
    while at < stop {
        let next = at.saturating_add(READ_SLICE).min(stop);
        let to = byte_of(text, next)?;
        let result = text.try_visit_chunks(ByteOffset(from)..ByteOffset(to), &mut |part| {
            answer.push_str(part)
        });
        result?;
        at = next;
        from = to;
    }
    Ok(answer)
}

/// The text around `at`: the text, `at` as a byte inside it, and its first byte.
fn window(
    text: &dyn TextSnapshot,
    at: usize,
    reach: usize,
    len: usize,
) -> Answer<(String, usize, usize)> {
    let start = byte_of(text, at.saturating_sub(reach))?;
    let middle = byte_of(text, at)?;
    let end = byte_of(text, at.saturating_add(reach).min(len))?;
    let mut out = String::with_capacity(end.saturating_sub(start));
    text.try_visit_chunks(ByteOffset(start)..ByteOffset(end), &mut |part| {
        out.push_str(part)
    })?;
    Ok((out, middle.saturating_sub(start), start))
}

/// `window`, widened while it starts inside a run of flags, so the pairs count from the
/// run's start.
fn paired_window(
    text: &dyn TextSnapshot,
    at: usize,
    reach: usize,
    len: usize,
) -> Answer<(String, usize, usize)> {
    let mut reach = reach;
    loop {
        let found = window(text, at, reach, len)?;
        let starts_in_run = found.2 > 0 && found.0.chars().next().is_some_and(is_regional);
        if !starts_in_run || reach >= FLAG_RUN_LIMIT {
            return Ok(found);
        }
        reach = reach.saturating_mul(4).min(FLAG_RUN_LIMIT);
    }
}

/// The editor's caret stops, in UTF-16 units. `NoStops` means this document has none, and the
/// table in `clusters` serves it, as for a field's short text.
fn next_stop(doc: &mut dyn TextDoc, at: usize) -> Answer<Option<usize>> {
    let byte = byte_of(doc.snapshot(), at)?;
    match doc.stop_after(byte)? {
        Some(stop) => Ok(Some(utf16_of(doc.snapshot(), stop)?)),
        None => Ok(None),
    }
}
fn prev_stop(doc: &mut dyn TextDoc, at: usize) -> Answer<Option<usize>> {
    let byte = byte_of(doc.snapshot(), at)?;
    match doc.stop_before(byte)? {
        Some(stop) => Ok(Some(utf16_of(doc.snapshot(), stop)?)),
        None => Ok(None),
    }
}
fn stop_at_or_before(doc: &mut dyn TextDoc, at: usize, len: usize) -> Answer<usize> {
    if at >= len {
        return Ok(at.min(len));
    }
    let byte = byte_of(doc.snapshot(), at)?;
    let Some(next) = doc.stop_after(byte)? else {
        return Ok(at);
    };
    match doc.stop_before(next)? {
        Some(stop) => utf16_of(doc.snapshot(), stop),
        None => Ok(0),
    }
}

fn char_next(text: &dyn TextSnapshot, at: usize, len: usize) -> Answer<Option<usize>> {
    if at >= len {
        return Ok(None);
    }
    for reach in [FIRST_WINDOW, WIDE_WINDOW] {
        let (body, local, base) = paired_window(text, at, reach, len)?;
        let Some(tail) = body.get(local..) else {
            return Err(ReadError::OutOfRange);
        };
        for (offset, c) in tail.char_indices() {
            let edge = local + offset + c.len_utf8();
            if edge == body.len() {
                break;
            }
            if is_stop(&body, edge) {
                return Ok(Some(utf16_of(text, base + edge)?));
            }
        }
        if at.saturating_add(reach) >= len {
            return Ok(Some(len));
        }
    }
    // A cluster longer than the widest window ends at the window, as in the editor, but always
    // on a character's edge.
    Ok(Some(utf16_of(text, byte_of(text, at + WIDE_WINDOW)?)?))
}

fn char_at_or_before(text: &dyn TextSnapshot, at: usize, len: usize) -> Answer<usize> {
    if at == 0 {
        return Ok(0);
    }
    for reach in [FIRST_WINDOW, WIDE_WINDOW] {
        let (body, local, base) = paired_window(text, at, reach, len)?;
        let Some(head) = body.get(..local) else {
            return Err(ReadError::OutOfRange);
        };
        let mut edge = local;
        let mut chars = head.char_indices().rev();
        while edge > 0 {
            if is_stop(&body, edge) {
                return utf16_of(text, base + edge);
            }
            match chars.next() {
                Some((offset, _)) => edge = offset,
                None => break,
            }
        }
        if at <= reach {
            return Ok(0);
        }
    }
    utf16_of(text, byte_of(text, at.saturating_sub(WIDE_WINDOW))?)
}

fn char_prev(text: &dyn TextSnapshot, at: usize, len: usize) -> Answer<Option<usize>> {
    if at == 0 {
        return Ok(None);
    }
    // One unit back from `at` is a character edge, or the inside of a pair.
    let byte = byte_of(text, at - 1)?;
    let edge = utf16_of(text, byte)?;
    char_at_or_before(text, edge, len).map(Some)
}

fn read_char(text: &dyn TextSnapshot, byte: usize) -> Answer<Option<char>> {
    let at = utf16_of(text, byte)?;
    let (len, _) = known_len(text)?;
    if at >= len {
        return Ok(None);
    }
    let end = byte_of(text, at.saturating_add(2).min(len))?;
    let mut out = String::new();
    text.try_visit_chunks(ByteOffset(byte)..ByteOffset(end.max(byte)), &mut |part| {
        out.push_str(part)
    })?;
    Ok(out.chars().next())
}

/// Blank text, by the same test as the editor's word rule.
pub fn is_space(c: char) -> bool {
    c == ' ' || c == '\t' || (c.is_whitespace() && !c.is_control())
}

struct LineSpan {
    index: usize,
    start: usize,
    content_end: usize,
    end: usize,
}

fn line_span(doc: &dyn TextDoc, byte: usize) -> Answer<LineSpan> {
    let index = doc.line_of(byte)?;
    let text = doc.snapshot();
    let range = text.try_line_range(LineIndex(index))?;
    let ending = text.try_line_ending_len(LineIndex(index))?;
    Ok(LineSpan {
        index,
        start: range.start.0,
        content_end: range.end.0.saturating_sub(ending).max(range.start.0),
        end: range.end.0,
    })
}

/// The start of the word unit holding a byte. A unit is a run with the blank text after it,
/// so a position in blank text belongs to the run before it.
fn word_start(doc: &mut dyn TextDoc, byte: usize, line: &LineSpan) -> Answer<usize> {
    let mut at = byte;
    for _ in 0..WORD_STEPS {
        if line.start == line.content_end {
            return Ok(line.start);
        }
        let run = doc.word_at(at)?;
        let start = run.start.max(line.start);
        if start == line.start || !read_char(doc.snapshot(), start)?.is_some_and(is_space) {
            return Ok(start);
        }
        // Step back onto the character before the blank run.
        let text = doc.snapshot();
        at = byte_of(text, utf16_of(text, start)?.saturating_sub(1))?;
    }
    Ok(at)
}

fn word_next(doc: &mut dyn TextDoc, byte: usize) -> Answer<usize> {
    let line = line_span(doc, byte)?;
    if byte >= line.content_end {
        return Ok(line.end);
    }
    let mut at = doc.word_at(byte)?.end;
    for _ in 0..WORD_STEPS {
        if at >= line.content_end {
            return Ok(line.end);
        }
        match read_char(doc.snapshot(), at)? {
            Some(c) if is_space(c) => {
                let run = doc.word_at(at)?;
                if run.end <= at {
                    return Ok(line.end);
                }
                at = run.end;
            }
            _ => return Ok(at),
        }
    }
    Ok(at)
}

fn word_prev(doc: &mut dyn TextDoc, byte: usize) -> Answer<Option<usize>> {
    if byte == 0 {
        return Ok(None);
    }
    let unit = utf16_of(doc.snapshot(), byte)?;
    let before = byte_of(doc.snapshot(), unit - 1)?;
    let line = line_span(doc, before)?;
    let start = word_start(doc, before.min(line.content_end), &line)?;
    Ok(Some(start))
}

fn row_edges(doc: &mut dyn TextDoc, line: &LineSpan) -> Vec<usize> {
    let mut stops = doc.row_stops(line.index);
    // The last row holds the line's ending, so no stop stands between the text and its ending.
    stops.retain(|s| *s > line.start && *s < line.content_end);
    stops.sort_unstable();
    stops.dedup();
    stops.insert(0, line.start);
    stops
}

/// The moves the rules allow from a position. All positions are UTF-16 units.
pub struct Mover<'a> {
    pub doc: &'a mut dyn TextDoc,
}

impl Mover<'_> {
    fn len(&self) -> Answer<usize> {
        Ok(known_len(self.doc.snapshot())?.0)
    }
    /// The next stop after `at`, or `None` at the end of the document.
    pub fn next(&mut self, unit: Unit, at: usize) -> Answer<Option<usize>> {
        let len = self.len()?;
        if at >= len {
            return Ok(None);
        }
        match unit {
            Unit::Character => match next_stop(&mut *self.doc, at) {
                Err(ReadError::NoStops) => char_next(self.doc.snapshot(), at, len),
                other => other,
            },
            Unit::Document => Ok(Some(len)),
            Unit::Word => {
                let byte = byte_of(self.doc.snapshot(), at)?;
                let next = word_next(self.doc, byte)?;
                Ok(Some(utf16_of(self.doc.snapshot(), next)?))
            }
            Unit::Paragraph | Unit::Line => {
                let byte = byte_of(self.doc.snapshot(), at)?;
                let line = line_span(self.doc, byte)?;
                let mut next = line.end;
                if unit == Unit::Line {
                    let edges = row_edges(self.doc, &line);
                    if let Some(edge) = edges.iter().find(|s| **s > byte) {
                        next = *edge;
                    }
                }
                Ok(Some(utf16_of(self.doc.snapshot(), next)?))
            }
        }
    }
    /// The last stop before `at`, or `None` at the start of the document.
    pub fn previous(&mut self, unit: Unit, at: usize) -> Answer<Option<usize>> {
        let len = self.len()?;
        if at == 0 {
            return Ok(None);
        }
        let at = at.min(len);
        match unit {
            Unit::Character => match prev_stop(&mut *self.doc, at) {
                Err(ReadError::NoStops) => char_prev(self.doc.snapshot(), at, len),
                other => other,
            },
            Unit::Document => Ok(Some(0)),
            Unit::Word => {
                let byte = byte_of(self.doc.snapshot(), at)?;
                match word_prev(self.doc, byte)? {
                    Some(found) => Ok(Some(utf16_of(self.doc.snapshot(), found)?)),
                    None => Ok(None),
                }
            }
            Unit::Paragraph | Unit::Line => {
                let byte = byte_of(self.doc.snapshot(), at)?;
                let mut line = line_span(self.doc, byte)?;
                if byte == line.start {
                    let before = byte_of(self.doc.snapshot(), at.saturating_sub(1))?;
                    line = line_span(self.doc, before)?;
                }
                let mut found = line.start;
                if unit == Unit::Line {
                    let edges = row_edges(self.doc, &line);
                    if let Some(edge) = edges.iter().rev().find(|s| **s < byte) {
                        found = *edge;
                    }
                }
                Ok(Some(utf16_of(self.doc.snapshot(), found)?))
            }
        }
    }
    /// The unit that holds a position. At the end of the document only a line can hold it,
    /// because no character or word starts there.
    pub fn unit_at(&mut self, unit: Unit, at: usize) -> Answer<TextRange> {
        let len = self.len()?;
        let at = at.min(len);
        match unit {
            Unit::Document => Ok(TextRange::new(0, len)),
            Unit::Character => {
                if at >= len {
                    return Ok(TextRange::new(len, len));
                }
                let start = match stop_at_or_before(&mut *self.doc, at, len) {
                    Err(ReadError::NoStops) => char_at_or_before(self.doc.snapshot(), at, len)?,
                    other => other?,
                };
                let end = self.next(unit, start)?.unwrap_or(len);
                Ok(TextRange::new(start, end))
            }
            Unit::Word => {
                if at >= len {
                    return Ok(TextRange::new(len, len));
                }
                let byte = byte_of(self.doc.snapshot(), at)?;
                let line = line_span(self.doc, byte)?;
                let start = word_start(self.doc, byte.min(line.content_end), &line)?;
                let start = utf16_of(self.doc.snapshot(), start)?;
                let end = self.next(unit, start)?.unwrap_or(len);
                Ok(TextRange::new(start, end))
            }
            Unit::Paragraph | Unit::Line => {
                let byte = byte_of(self.doc.snapshot(), at)?;
                let line = line_span(self.doc, byte)?;
                let mut start = line.start;
                let mut end = line.end;
                if unit == Unit::Line {
                    let edges = row_edges(self.doc, &line);
                    if let Some(edge) = edges.iter().rev().find(|s| **s <= byte) {
                        start = *edge;
                    }
                    if let Some(edge) = edges.iter().find(|s| **s > byte) {
                        end = *edge;
                    }
                }
                let text = self.doc.snapshot();
                Ok(TextRange::new(utf16_of(text, start)?, utf16_of(text, end)?))
            }
        }
    }
    fn last_line_is_empty(&self, len: usize) -> Answer<bool> {
        let byte = byte_of(self.doc.snapshot(), len)?;
        Ok(line_span(&*self.doc, byte)?.start == byte)
    }
    /// UIA's Move: a range with text stays one unit long, and an empty range stays empty.
    /// A range with text moves to the very end only onto a final empty line.
    pub fn move_range(
        &mut self,
        range: TextRange,
        unit: Unit,
        count: i32,
    ) -> Answer<(TextRange, i32)> {
        if count == 0 {
            return Ok((range, 0));
        }
        let len = self.len()?;
        let empty = range.start == range.end;
        // From memory of Edit and RichEdit: a final line ending makes a last, empty line that a
        // caret can stand on, so a line move can land there. No character or word starts there.
        let ends_on_empty_line = matches!(unit, Unit::Line | Unit::Paragraph)
            && len > 0
            && self.last_line_is_empty(len)?;
        let began = Instant::now();
        // UIA first normalizes a range with text to the start of the unit it is in.
        let mut at = if empty {
            range.start
        } else {
            self.unit_at(unit, range.start)?.start
        };
        let mut moved = 0i32;
        while moved != count && (moved == 0 || within_limit(began)) {
            let step = if count > 0 {
                self.next(unit, at)?
            } else {
                self.previous(unit, at)?
            };
            let Some(step) = step else { break };
            if !empty && count > 0 && step >= len && !ends_on_empty_line {
                break;
            }
            at = step;
            moved += count.signum();
            if unit == Unit::Document {
                break;
            }
        }
        if moved == 0 {
            return Ok((range, 0));
        }
        if empty {
            return Ok((TextRange::new(at, at), moved));
        }
        let end = self.next(unit, at)?.unwrap_or(len);
        Ok((TextRange::new(at, end), moved))
    }
    /// UIA's MoveEndpointByUnit. If one end passes the other, the other comes along.
    pub fn move_endpoint(
        &mut self,
        range: TextRange,
        moving_start: bool,
        unit: Unit,
        count: i32,
    ) -> Answer<(TextRange, i32)> {
        let began = Instant::now();
        let mut at = if moving_start { range.start } else { range.end };
        let mut moved = 0i32;
        while moved != count && (moved == 0 || within_limit(began)) {
            let step = if count > 0 {
                self.next(unit, at)?
            } else {
                self.previous(unit, at)?
            };
            let Some(step) = step else { break };
            at = step;
            moved += count.signum();
            if unit == Unit::Document {
                break;
            }
        }
        let next = if moving_start {
            TextRange::new(at, range.end.max(at))
        } else {
            TextRange::new(range.start.min(at), at)
        };
        Ok((next, moved))
    }
}
