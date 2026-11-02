//! FindText. A short range is searched in the call. A long range, or text not in memory,
//! is searched by a worker on a snapshot, and the call waits at most `FIND_WAIT`. If the
//! worker is not done, the call says `Searching`, and the same call later gets the answer.
//! The worker stops when it is done, when another search starts, or when the window closes.
use crate::text::{ReadError, TextRange, byte_of, read_units, utf16_of};
use qe_api::{ByteOffset, TextSnapshot};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// The most text searched inside the call, in UTF-16 units.
pub const FIND_INLINE: usize = 64 * 1024;
/// The longest a call waits for a worker. It leaves room inside a 16 ms frame.
pub const FIND_WAIT: Duration = Duration::from_millis(6);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindRequest {
    /// Which view, which document in it, and which revision.
    pub identity: (u64, u64, u64),
    pub range: TextRange,
    pub query: String,
    pub backward: bool,
    pub ignore_case: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindError {
    /// A worker is still searching. The same call later gets the answer.
    Searching,
    Read(ReadError),
}
impl From<ReadError> for FindError {
    fn from(error: ReadError) -> Self {
        Self::Read(error)
    }
}

type Outcome = Result<Option<TextRange>, ReadError>;

struct Job {
    request: FindRequest,
    done: Arc<Mutex<Option<Outcome>>>,
    cancel: Arc<AtomicBool>,
}

/// One search per window. A new search stops the one before it.
#[derive(Default)]
pub struct SearchSlot {
    job: Mutex<Option<Job>>,
}

impl SearchSlot {
    pub fn cancel(&self) {
        if let Ok(mut job) = self.job.lock()
            && let Some(job) = job.take()
        {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn find(
        &self,
        text: &Arc<dyn TextSnapshot>,
        request: FindRequest,
    ) -> Result<Option<TextRange>, FindError> {
        if request.query.is_empty() || request.range.start >= request.range.end {
            return Ok(None);
        }
        if request.range.end - request.range.start <= FIND_INLINE {
            match search_inline(text.as_ref(), &request) {
                Err(ReadError::NotYetKnown) => {}
                answer => return answer.map_err(Into::into),
            }
        }
        let poisoned = || ReadError::Failed("The search lock failed.".into());
        let mut slot = self.job.lock().map_err(|_| poisoned())?;
        if slot.as_ref().is_none_or(|job| job.request != request) {
            if let Some(old) = slot.take() {
                old.cancel.store(true, Ordering::Relaxed);
            }
            *slot = Some(start_worker(text, request).map_err(FindError::Read)?);
        }
        let Some(job) = slot.as_ref() else {
            return Err(poisoned().into());
        };
        let done = job.done.clone();
        drop(slot);
        // This yields rather than sleeps, because a timed sleep on Windows can last 16 ms,
        // which is a whole frame.
        let began = Instant::now();
        let answer = loop {
            if let Some(answer) = done.lock().map_err(|_| poisoned())?.take() {
                break answer;
            }
            if began.elapsed() >= FIND_WAIT {
                return Err(FindError::Searching);
            }
            std::thread::yield_now();
        };
        self.cancel();
        answer.map_err(Into::into)
    }
}

fn start_worker(text: &Arc<dyn TextSnapshot>, request: FindRequest) -> Result<Job, ReadError> {
    let from = byte_of(text.as_ref(), request.range.start)?;
    let to = byte_of(text.as_ref(), request.range.end)?;
    let done = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let job = Job {
        request: request.clone(),
        done: done.clone(),
        cancel: cancel.clone(),
    };
    let text = text.clone();
    std::thread::Builder::new()
        .name("qe-a11y-find".into())
        .spawn(move || {
            let answer = search_worker(text.as_ref(), &request, from, to, &cancel);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if let Ok(mut slot) = done.lock() {
                *slot = Some(answer);
            }
        })
        .map_err(|e| ReadError::Failed(format!("The search could not start: {e}")))?;
    Ok(job)
}

fn search_inline(text: &dyn TextSnapshot, request: &FindRequest) -> Outcome {
    let body = read_units(text, request.range.start, request.range.end)?;
    let from = byte_of(text, request.range.start)?;
    let to = byte_of(text, request.range.end)?;
    if body.len() != to.saturating_sub(from) {
        return Err(ReadError::NotYetKnown);
    }
    let mut scanner = Scanner::new(&request.query, request.backward, request.ignore_case);
    scanner.feed(from, &body);
    match scanner.found {
        Some((start, end)) => Ok(Some(TextRange::new(
            utf16_of(text, start)?,
            utf16_of(text, end)?,
        ))),
        None => Ok(None),
    }
}

fn search_worker(
    text: &dyn TextSnapshot,
    request: &FindRequest,
    from: usize,
    to: usize,
    cancel: &AtomicBool,
) -> Outcome {
    let mut scanner = Scanner::new(&request.query, request.backward, request.ignore_case);
    let mut at = from;
    let good = text.visit_chunks_while(ByteOffset(from)..ByteOffset(to), &mut |part| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let going = scanner.feed(at, part);
        at += part.len();
        going
    });
    if !good {
        return Err(ReadError::Failed(
            "The text could not be read for the search.".into(),
        ));
    }
    // A worker may wait for disk, so it uses the plain queries here.
    match scanner.found {
        Some((start, end)) => {
            let unit = |byte| {
                text.byte_to_utf16(ByteOffset(byte))
                    .map(|u| u.0)
                    .ok_or(ReadError::OutOfRange)
            };
            Ok(Some(TextRange::new(unit(start)?, unit(end)?)))
        }
        None => Ok(None),
    }
}

/// Compares a character at a time, so a match stays on character edges. Folding is by
/// `to_lowercase`, and a character that folds to several still matches only itself.
fn same(a: char, b: char) -> bool {
    a == b
        || if a.is_ascii() && b.is_ascii() {
            a.eq_ignore_ascii_case(&b)
        } else {
            a.to_lowercase().eq(b.to_lowercase())
        }
}

/// Scans chunks in order and keeps the end of each one, so a match across two chunks counts.
struct Scanner<'a> {
    query: &'a str,
    chars: Vec<char>,
    backward: bool,
    ignore_case: bool,
    most_bytes: usize,
    buffer: String,
    /// The document byte of `buffer`'s first byte.
    base: usize,
    found: Option<(usize, usize)>,
}

impl<'a> Scanner<'a> {
    fn new(query: &'a str, backward: bool, ignore_case: bool) -> Self {
        let chars: Vec<char> = query.chars().collect();
        Self {
            query,
            most_bytes: if ignore_case {
                chars.len().saturating_mul(4)
            } else {
                query.len()
            },
            chars,
            backward,
            ignore_case,
            buffer: String::new(),
            base: 0,
            found: None,
        }
    }
    /// Returns false once the answer is known.
    fn feed(&mut self, at: usize, part: &str) -> bool {
        if self.buffer.is_empty() {
            self.base = at;
        }
        self.buffer.push_str(part);
        let hit = if self.ignore_case {
            self.scan_folded()
        } else if self.backward {
            self.buffer
                .rfind(self.query)
                .map(|s| (s, s + self.query.len()))
        } else {
            self.buffer
                .find(self.query)
                .map(|s| (s, s + self.query.len()))
        };
        if let Some((start, end)) = hit {
            self.found = Some((self.base + start, self.base + end));
            if !self.backward {
                return false;
            }
        }
        // Keep what could start a match that the next chunk finishes.
        let mut keep = self.buffer.len().saturating_sub(self.most_bytes.saturating_sub(1));
        while !self.buffer.is_char_boundary(keep) {
            keep += 1;
        }
        self.buffer.drain(..keep);
        self.base += keep;
        true
    }
    fn scan_folded(&self) -> Option<(usize, usize)> {
        let first = *self.chars.first()?;
        let mut hit = None;
        for (start, c) in self.buffer.char_indices() {
            if !same(c, first) {
                continue;
            }
            let Some(tail) = self.buffer.get(start..) else {
                continue;
            };
            let mut end = start;
            let mut letters = tail.chars();
            let mut good = true;
            for wanted in &self.chars {
                match letters.next() {
                    Some(actual) if same(actual, *wanted) => end += actual.len_utf8(),
                    _ => {
                        good = false;
                        break;
                    }
                }
            }
            if good {
                hit = Some((start, end));
                if !self.backward {
                    break;
                }
            }
        }
        hit
    }
}
