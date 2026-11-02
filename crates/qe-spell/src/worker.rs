//! The worker thread: reads text from a snapshot, asks the checker, and answers.
use crate::{
    Action, Checker, Latest, MAX_SUGGESTIONS, MakeChecker, Misspelling, Wake, token_is_skipped,
};
use qe_api::{ByteOffset, TextSnapshot};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender, TryRecvError},
    },
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoIncrementMTAUsage, CoInitializeEx, CoUninitialize,
};

/// How far a range looks for the ends of the words at its edges.
const MARGIN_BYTES: usize = 2048;
/// One call to the checker takes about this much text.
const CHUNK_BYTES: usize = 16 * 1024;
const SUGGESTION_CACHE: usize = 1024;

pub(crate) struct Job {
    pub(crate) snapshot: Arc<dyn TextSnapshot>,
    pub(crate) ranges: Vec<Range<usize>>,
    pub(crate) revision: u64,
    pub(crate) document: u64,
    pub(crate) generation: u64,
}
pub(crate) enum Command {
    Check(Job),
    Add(String),
    /// Ignore belongs to one document, so it stays here and never reaches Windows, whose
    /// `Ignore` lasts as long as the checker does.
    Ignore(String),
    /// A new document came, with no words ignored yet.
    ForgetIgnored,
    FindNext {
        snapshot: Arc<dyn TextSnapshot>,
        from: usize,
        generation: u64,
    },
}
pub(crate) enum Answer {
    Checked {
        revision: u64,
        document: u64,
        checked: Vec<Range<usize>>,
        errors: Vec<Misspelling>,
    },
    /// The job was for text that no one wants any more.
    Skipped,
    Found {
        generation: u64,
        found: Option<Misspelling>,
    },
    /// The word stays unknown, and spelling goes on.
    AddFailed {
        word: String,
        message: String,
    },
    /// One thing went wrong, and spelling goes on. The host shows this.
    Problem(String),
    Failed(String),
}

pub(crate) fn run(
    make: MakeChecker,
    jobs: Receiver<Command>,
    answers: Sender<Answer>,
    wake: Wake,
    latest: Arc<Latest>,
) {
    // COM shuts the apartment down when its last thread leaves, and a checker that starts
    // during that shutdown fails with 0x80080008. Tabs come and go, so the apartment stays up
    // from the first time spelling turns on. With spelling off, this never runs.
    static APARTMENT: OnceLock<Result<(), String>> = OnceLock::new();
    let apartment = APARTMENT.get_or_init(|| {
        // SAFETY: takes no pointers. The cookie is left unused, so the hold lasts until exit.
        unsafe { CoIncrementMTAUsage() }
            .map(|_| ())
            .map_err(|e| format!("Windows spelling is not available: {e}"))
    });
    if let Err(message) = apartment {
        if answers.send(Answer::Failed(message.clone())).is_ok() {
            wake();
        }
        return;
    }
    // SAFETY: this thread stays in the apartment until the end of this function.
    let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if let Err(e) = joined.ok() {
        // The UI thread hears this when it polls, and its send fails from here on.
        if answers
            .send(Answer::Failed(format!(
                "Windows spelling is not available: {e}"
            )))
            .is_ok()
        {
            wake();
        }
        return;
    }
    serve(make, &jobs, &answers, &wake, &latest);
    // SAFETY: pairs with the successful call above. The checker has gone by now.
    unsafe { CoUninitialize() };
}

/// A search through the document, one piece at a time, so that the worker can hear other
/// commands between pieces.
struct Scan {
    snapshot: Arc<dyn TextSnapshot>,
    generation: u64,
    /// The rest of the search: what is left of this part, then the part after it.
    now: Range<usize>,
    then: Option<Range<usize>>,
}

fn serve(
    make: MakeChecker,
    jobs: &Receiver<Command>,
    answers: &Sender<Answer>,
    wake: &Wake,
    latest: &Latest,
) {
    let say = |answer: Answer| {
        // No one to hear means that the service has gone, so this thread ends soon too.
        if answers.send(answer).is_ok() {
            wake();
        }
    };
    let mut checker = match make() {
        Ok(checker) => checker,
        Err(message) => {
            say(Answer::Failed(message));
            return;
        }
    };
    let mut memory = Memory::default();
    let mut scan: Option<Scan> = None;
    loop {
        if let Some(message) = memory.problem.take() {
            say(Answer::Problem(message));
        }
        // With a search running, a command must not wait for the search to end.
        let command = if scan.is_some() {
            match jobs.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            match jobs.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        };
        let Some(command) = command else {
            let Some(running) = scan.as_mut() else {
                continue;
            };
            if running.generation < latest.find.load(Ordering::Relaxed) {
                scan = None;
                continue;
            }
            match scan_step(checker.as_mut(), &mut memory, running) {
                Ok(None) => {}
                Ok(Some(found)) => {
                    say(Answer::Found {
                        generation: running.generation,
                        found,
                    });
                    scan = None;
                }
                Err(message) => {
                    say(Answer::Failed(message));
                    return;
                }
            }
            continue;
        };
        match command {
            Command::Check(job) => {
                if job.generation < latest.check.load(Ordering::Relaxed) {
                    say(Answer::Skipped);
                    continue;
                }
                match check_job(checker.as_mut(), &mut memory, &job, &latest.check) {
                    Ok(Some((checked, errors))) => say(Answer::Checked {
                        revision: job.revision,
                        document: job.document,
                        checked,
                        errors,
                    }),
                    Ok(None) => say(Answer::Skipped),
                    Err(message) => {
                        say(Answer::Failed(message));
                        return;
                    }
                }
            }
            Command::Add(word) => match checker.add(&word) {
                Ok(()) => {
                    memory.cache.remove(&word);
                }
                Err(message) => say(Answer::AddFailed { word, message }),
            },
            Command::Ignore(word) => {
                memory.ignored.insert(word);
            }
            Command::ForgetIgnored => memory.ignored.clear(),
            Command::FindNext {
                snapshot,
                from,
                generation,
            } => {
                let len = snapshot.len_bytes().0;
                let from = floor_edge(snapshot.as_ref(), from.min(len));
                scan = Some(Scan {
                    snapshot,
                    generation,
                    now: from..len,
                    then: Some(0..from),
                });
            }
        }
    }
}

/// What the worker keeps between commands.
#[derive(Default)]
struct Memory {
    /// Suggestions by word.
    cache: HashMap<String, Vec<String>>,
    /// Words ignored in this document.
    ignored: HashSet<String>,
    /// A failure for the next answer to carry.
    problem: Option<String>,
    /// Suggestions have failed once, so the host has heard.
    suggest_failed: bool,
}
/// What a job covered, and the errors in it.
type Covered = (Vec<Range<usize>>, Vec<Misspelling>);

fn check_job(
    checker: &mut dyn Checker,
    memory: &mut Memory,
    job: &Job,
    latest: &AtomicU64,
) -> Result<Option<Covered>, String> {
    let mut checked = Vec::new();
    let mut errors = Vec::new();
    for range in &job.ranges {
        let mut at = range.start;
        while at < range.end {
            if job.generation < latest.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let piece = at..range.end.min(at + CHUNK_BYTES);
            let (done, found) = check_range(checker, memory, job.snapshot.as_ref(), &piece)?;
            at = done.end.max(piece.end);
            checked.push(done);
            errors.extend(found);
        }
    }
    Ok(Some((checked, errors)))
}

fn floor_edge(snapshot: &dyn TextSnapshot, at: usize) -> usize {
    let mut at = at;
    while at > 0 && snapshot.byte_to_utf16(ByteOffset(at)).is_none() {
        at -= 1;
    }
    at
}

/// Checks a range, grown out to whole words. Returns what it covered, and the errors in it.
/// What it covered always holds all of `range`, so a gap asked about can never stay open,
/// even when an end of `range` is inside a character of more than one byte.
fn check_range(
    checker: &mut dyn Checker,
    memory: &mut Memory,
    snapshot: &dyn TextSnapshot,
    range: &Range<usize>,
) -> Result<(Range<usize>, Vec<Misspelling>), String> {
    let len = snapshot.len_bytes().0;
    let read_start = floor_edge(snapshot, range.start.saturating_sub(MARGIN_BYTES));
    let read_end = floor_edge(snapshot, range.end.saturating_add(MARGIN_BYTES).min(len));
    let mut text = String::new();
    if !snapshot.visit_chunks(ByteOffset(read_start)..ByteOffset(read_end), &mut |s| {
        text.push_str(s);
    }) {
        return Err("The spell checker could not read this text.".into());
    }
    let inner_start = floor_edge(snapshot, range.start).saturating_sub(read_start);
    let inner_end = floor_edge(snapshot, range.end).saturating_sub(read_start);
    let head = text.get(..inner_start).unwrap_or("");
    let tail = text.get(inner_end..).unwrap_or("");
    let starts_clean = text
        .get(inner_start..)
        .is_some_and(|t| t.starts_with(char::is_whitespace));
    let (start, cut_left) = match head.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        // The range starts on a word's edge, so the word before it needs no new look.
        _ if starts_clean => (inner_start, false),
        Some((i, c)) => (i + c.len_utf8(), false),
        None => (0, read_start > 0),
    };
    let (end, cut_right) = match tail.find(char::is_whitespace) {
        Some(i) => (inner_end + i, false),
        None => (text.len(), read_end < len),
    };
    let body = text.get(start..end).unwrap_or("");
    let base = read_start + start;
    let body_end = base + body.len();
    let mut errors = errors_in(checker, memory, body, base)?;
    // The margin cut the token at this side, so its whole is not known. It may not be a whole
    // word, and it may be a web address whose `://` the margin cut off.
    let first_token_end = base + body.find(char::is_whitespace).unwrap_or(body.len());
    let last_token_start = base
        + body
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
    errors.retain(|e| {
        !(cut_left && e.range.start.0 < first_token_end)
            && !(cut_right && e.range.end.0 > last_token_start)
    });
    Ok((base.min(range.start)..body_end.max(range.end), errors))
}

/// Asks the checker about `body`, which starts at byte `base`.
fn errors_in(
    checker: &mut dyn Checker,
    memory: &mut Memory,
    body: &str,
    base: usize,
) -> Result<Vec<Misspelling>, String> {
    if body.is_empty() {
        return Ok(Vec::new());
    }
    // A 0 unit in the text would end the checker's string early. A space has the same size.
    let wide: Vec<u16> = body
        .encode_utf16()
        .map(|u| if u == 0 { 0x20 } else { u })
        .chain(std::iter::once(0))
        .collect();
    let mut raw = checker.check(&wide)?;
    raw.sort_by_key(|e| e.start);
    let mut out = Vec::new();
    let mut walker = Walker::new(body);
    for error in raw {
        let suggestions = match &error.action {
            Action::Suggest => None,
            Action::Replace(text) => Some(vec![text.clone()]),
            Action::Delete | Action::None => continue,
        };
        let (Some(a), Some(b)) = (
            walker.byte_at(error.start),
            walker.byte_at(error.start + error.length),
        ) else {
            continue;
        };
        let Some(word) = body.get(a..b) else { continue };
        if !word.chars().any(char::is_alphabetic)
            || token_is_skipped(token_around(body, a, b))
            || memory.ignored.contains(word)
        {
            continue;
        }
        let suggestions = match suggestions {
            Some(list) => list,
            None => match memory.cache.get(word) {
                Some(list) => list.clone(),
                None => match checker.suggest(word, MAX_SUGGESTIONS) {
                    Ok(list) => {
                        if memory.cache.len() >= SUGGESTION_CACHE {
                            memory.cache.clear();
                        }
                        memory.cache.insert(word.to_owned(), list.clone());
                        list
                    }
                    // The word is still wrong, and the menu says it has no suggestions.
                    Err(message) => {
                        if !std::mem::replace(&mut memory.suggest_failed, true) {
                            memory.problem = Some(message);
                        }
                        Vec::new()
                    }
                },
            },
        };
        out.push(Misspelling {
            range: ByteOffset(base + a)..ByteOffset(base + b),
            word: word.to_owned(),
            suggestions,
        });
    }
    Ok(out)
}

fn token_around(text: &str, start: usize, end: usize) -> &str {
    let before = text.get(..start).unwrap_or("");
    let after = text.get(end..).unwrap_or("");
    let first = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let last = end + after.find(char::is_whitespace).unwrap_or(after.len());
    text.get(first..last).unwrap_or("")
}

/// Turns rising UTF-16 positions into bytes, in one pass over the text.
struct Walker<'a> {
    chars: std::str::CharIndices<'a>,
    units: usize,
    byte: usize,
    len: usize,
}
impl<'a> Walker<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.char_indices(),
            units: 0,
            byte: 0,
            len: text.len(),
        }
    }
    /// `None` for a position inside a surrogate pair or past the end, or behind the walker.
    fn byte_at(&mut self, unit: usize) -> Option<usize> {
        if unit == self.units {
            return Some(self.byte);
        }
        if unit < self.units {
            return None;
        }
        for (_, c) in self.chars.by_ref() {
            self.units += c.len_utf16();
            self.byte += c.len_utf8();
            if self.units >= unit {
                break;
            }
        }
        debug_assert!(self.byte <= self.len);
        (self.units == unit).then_some(self.byte)
    }
}

/// One piece of a search. `Some(answer)` ends it, and `None` means there is more to do.
fn scan_step(
    checker: &mut dyn Checker,
    memory: &mut Memory,
    scan: &mut Scan,
) -> Result<Option<Option<Misspelling>>, String> {
    if scan.now.is_empty() {
        return Ok(match scan.then.take() {
            Some(next) => {
                scan.now = next;
                None
            }
            None => Some(None),
        });
    }
    let piece = scan.now.start..scan.now.end.min(scan.now.start + CHUNK_BYTES);
    let (done, errors) = check_range(checker, memory, scan.snapshot.as_ref(), &piece)?;
    if let Some(first) = errors
        .into_iter()
        .find(|e| e.range.start.0 >= scan.now.start)
    {
        return Ok(Some(Some(first)));
    }
    scan.now.start = done.end.max(piece.end).min(scan.now.end);
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walker_maps_utf16_to_bytes_and_refuses_the_inside_of_a_pair() {
        let text = "a\u{1f600}e\u{301}b";
        let mut w = Walker::new(text);
        assert_eq!(w.byte_at(0), Some(0));
        assert_eq!(w.byte_at(1), Some(1));
        assert_eq!(w.byte_at(3), Some(5));
        assert_eq!(w.byte_at(4), Some(6));
        assert_eq!(w.byte_at(5), Some(8));
        assert_eq!(w.byte_at(6), Some(9));
        assert_eq!(w.byte_at(7), None);
        let mut w = Walker::new(text);
        assert_eq!(w.byte_at(2), None, "inside the emoji");
    }

    #[test]
    fn the_token_is_what_whitespace_bounds() {
        assert_eq!(
            token_around("see http://x.yz/speling now", 15, 22),
            "http://x.yz/speling"
        );
        assert_eq!(token_around("speling", 0, 7), "speling");
        assert_eq!(token_around("a\r\nspeling\r\nb", 3, 10), "speling");
    }
}
