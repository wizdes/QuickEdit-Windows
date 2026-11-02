//! FindText. The search runs on the thread that called, not on the window's thread.
//!
//! UIA calls a provider on its own worker thread, so that thread waits for the answer. A
//! client's normal patience is about 20 seconds (from memory of UIA's default transaction
//! timeout), so a search stops at `FIND_PATIENCE` and fails once, honestly, instead of
//! returning "not found" for text it did not reach.
//!
//! The cost is linear in the text plus the query. Only a direct caller on the window's own
//! thread stays inside `FIND_ON_WINDOW` units, and it never waits for disk.
use crate::text::{ReadError, TextRange, byte_of, known_len, read_units, utf16_of};
use qe_api::{ByteOffset, TextSnapshot, Utf16Offset};
use std::time::{Duration, Instant};

/// The longest one search runs before it gives up.
pub const FIND_PATIENCE: Duration = Duration::from_secs(15);
/// The most text searched by a direct call on the window's thread, in UTF-16 units.
pub const FIND_ON_WINDOW: usize = 64 * 1024;
/// A query this long or shorter uses the standard library's search, which is fastest. It
/// keeps this much of each chunk for the next one, so a longer query uses the search below.
const SHORT_QUERY_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindRequest {
    pub range: TextRange,
    pub query: String,
    pub backward: bool,
    pub ignore_case: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindError {
    /// The search hit its limit, so the answer is not known.
    TooLong,
    Read(ReadError),
}
impl From<ReadError> for FindError {
    fn from(error: ReadError) -> Self {
        Self::Read(error)
    }
}

/// Searches on this thread. It may wait for disk, so it must not run on the window's thread.
pub fn find_waiting(
    text: &dyn TextSnapshot,
    request: &FindRequest,
    patience: Duration,
) -> Result<Option<TextRange>, FindError> {
    if nothing_to_find(request) {
        return Ok(None);
    }
    let byte = |unit: usize| -> Result<usize, FindError> {
        text.utf16_to_byte(Utf16Offset(unit))
            .or_else(|| text.utf16_to_byte(Utf16Offset(unit.saturating_sub(1))))
            .map(|b| b.0)
            .ok_or(FindError::Read(ReadError::OutOfRange))
    };
    let from = byte(request.range.start)?;
    let to = byte(request.range.end)?;
    let began = Instant::now();
    let mut scanner = Scanner::new(request);
    let mut at = from;
    let mut timed_out = false;
    let good = text.visit_chunks_while(ByteOffset(from)..ByteOffset(to), &mut |part| {
        if began.elapsed() > patience {
            timed_out = true;
            return false;
        }
        let going = scanner.feed(at, part);
        at += part.len();
        going
    });
    if timed_out {
        return Err(FindError::TooLong);
    }
    if !good {
        return Err(FindError::Read(ReadError::Failed(
            "The text could not be read for the search.".into(),
        )));
    }
    let Some((start, end)) = scanner.found else {
        return Ok(None);
    };
    let unit = |b: usize| {
        text.byte_to_utf16(ByteOffset(b))
            .map(|u| u.0)
            .ok_or(FindError::Read(ReadError::OutOfRange))
    };
    Ok(Some(TextRange::new(unit(start)?, unit(end)?)))
}

/// For a direct call on the window's thread: only text in memory, and only so much of it.
pub fn find_on_window(
    text: &dyn TextSnapshot,
    request: &FindRequest,
) -> Result<Option<TextRange>, FindError> {
    if nothing_to_find(request) {
        return Ok(None);
    }
    if request.range.end - request.range.start > FIND_ON_WINDOW {
        return Err(FindError::TooLong);
    }
    let (len, _) = known_len(text)?;
    if request.range.end > len {
        return Err(FindError::Read(ReadError::OutOfRange));
    }
    let body = read_units(text, request.range.start, request.range.end)?;
    let from = byte_of(text, request.range.start)?;
    let to = byte_of(text, request.range.end)?;
    if body.len() != to.saturating_sub(from) {
        return Err(FindError::Read(ReadError::NotYetKnown));
    }
    let mut scanner = Scanner::new(request);
    scanner.feed(from, &body);
    match scanner.found {
        Some((start, end)) => Ok(Some(TextRange::new(
            utf16_of(text, start)?,
            utf16_of(text, end)?,
        ))),
        None => Ok(None),
    }
}

fn nothing_to_find(request: &FindRequest) -> bool {
    request.query.is_empty()
        || request.range.start >= request.range.end
        // Each query character needs at least one unit of text, in any case.
        || request.query.chars().count() > request.range.end - request.range.start
}

/// What makes two characters match. Equal keys mean equal text, or equal text after
/// `to_lowercase` when the case does not count. A character that folds to several still
/// matches only itself, and a match stays on character edges.
fn key(c: char, ignore_case: bool) -> [char; 3] {
    if !ignore_case {
        return [c, '\0', '\0'];
    }
    if c.is_ascii() {
        return [c.to_ascii_lowercase(), '\0', '\0'];
    }
    let mut lower = c.to_lowercase();
    [
        lower.next().unwrap_or(c),
        lower.next().unwrap_or('\0'),
        lower.next().unwrap_or('\0'),
    ]
}

/// The prefix-table search (Knuth, Morris and Pratt). Each character of text costs a constant
/// amount on average, whatever the query, and a chunk edge needs no special care.
struct Prefix {
    keys: Vec<[char; 3]>,
    fail: Vec<u32>,
    ignore_case: bool,
    matched: usize,
    /// The byte of each of the last `keys.len()` characters.
    bytes: Vec<usize>,
    seen: usize,
}

impl Prefix {
    fn new(query: &str, ignore_case: bool) -> Self {
        let keys: Vec<[char; 3]> = query.chars().map(|c| key(c, ignore_case)).collect();
        let mut fail = vec![0u32; keys.len()];
        let mut length = 0usize;
        for i in 1..keys.len() {
            while length > 0 && keys[i] != keys[length] {
                length = fail[length - 1] as usize;
            }
            if keys[i] == keys[length] {
                length += 1;
            }
            fail[i] = length as u32;
        }
        let bytes = vec![0; keys.len()];
        Self {
            keys,
            fail,
            ignore_case,
            matched: 0,
            bytes,
            seen: 0,
        }
    }
    /// Takes one chunk. Each match goes to `found`, and returning false stops the scan.
    fn feed(&mut self, at: usize, part: &str, found: &mut dyn FnMut(usize, usize) -> bool) -> bool {
        let count = self.keys.len();
        for (offset, c) in part.char_indices() {
            let k = key(c, self.ignore_case);
            while self.matched > 0 && self.keys[self.matched] != k {
                self.matched = self.fail[self.matched - 1] as usize;
            }
            if self.keys[self.matched] == k {
                self.matched += 1;
            }
            self.bytes[self.seen % count] = at + offset;
            self.seen += 1;
            if self.matched == count {
                let start = self.bytes[(self.seen - count) % count];
                if !found(start, at + offset + c.len_utf8()) {
                    return false;
                }
                self.matched = self.fail[count - 1] as usize;
            }
        }
        true
    }
}

enum Method {
    /// A short exact query. It keeps the end of each chunk for the next one.
    Short {
        buffer: String,
        base: usize,
    },
    Prefix(Box<Prefix>),
}

/// Scans chunks in order. A backward search keeps the last match it sees.
struct Scanner<'a> {
    query: &'a str,
    backward: bool,
    method: Method,
    found: Option<(usize, usize)>,
}

impl<'a> Scanner<'a> {
    fn new(request: &'a FindRequest) -> Self {
        let method = if !request.ignore_case && request.query.len() <= SHORT_QUERY_BYTES {
            Method::Short {
                buffer: String::new(),
                base: 0,
            }
        } else {
            Method::Prefix(Box::new(Prefix::new(&request.query, request.ignore_case)))
        };
        Self {
            query: &request.query,
            backward: request.backward,
            method,
            found: None,
        }
    }
    /// Returns false once the answer is known.
    fn feed(&mut self, at: usize, part: &str) -> bool {
        let backward = self.backward;
        match &mut self.method {
            Method::Prefix(prefix) => {
                let found = &mut self.found;
                prefix.feed(at, part, &mut |start, end| {
                    *found = Some((start, end));
                    backward
                })
            }
            Method::Short { buffer, base } => {
                if buffer.is_empty() {
                    *base = at;
                }
                buffer.push_str(part);
                let hit = if backward {
                    buffer.rfind(self.query)
                } else {
                    buffer.find(self.query)
                };
                if let Some(start) = hit {
                    self.found = Some((*base + start, *base + start + self.query.len()));
                    if !backward {
                        return false;
                    }
                }
                // Keep what could start a match that the next chunk finishes.
                let mut keep = buffer
                    .len()
                    .saturating_sub(self.query.len().saturating_sub(1));
                while !buffer.is_char_boundary(keep) {
                    keep += 1;
                }
                buffer.drain(..keep);
                *base += keep;
                true
            }
        }
    }
}
