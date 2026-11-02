//! The find engine, with no UI. It reads a snapshot in bounded pieces. It holds about
//! `BLOCK` bytes of text at a time, so a 1 GiB mapped file costs no more than a small one.
use crate::text::{
    align_down, align_up, fold_char, is_cluster_edge, is_word, read_failure, readable_len,
};
use qe_api::{ByteOffset, TextSnapshot};
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The text scanned at one time. A piece of the store is smaller than this or about the
/// same, so the buffer never holds much more than twice this.
const BLOCK: usize = 64 * 1024;
/// Bytes kept before a candidate, for the character rules and whole words.
const CONTEXT: usize = 64;
/// Bytes read at one time when searching backward.
const BACK_BLOCK: usize = 256 * 1024;
/// Matches waiting for the host. Past this they are counted but not kept.
pub const QUEUE_LIMIT: usize = 64 * 1024;
const WAKE_GAP: Duration = Duration::from_millis(30);

/// A wake for the host. It runs on the worker's thread, so it must only post a message.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The Mac find bar's pop-up: `Contains`, `Starts With`, `Full Word`, `Ends With`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MatchKind {
    #[default]
    Contains,
    StartsWith,
    FullWord,
    EndsWith,
}

/// The Mac find bar's options. `Ignore Case` and `Wrap Around` start on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindOptions {
    pub ignore_case: bool,
    pub wrap: bool,
    pub kind: MatchKind,
}
impl Default for FindOptions {
    fn default() -> Self {
        Self {
            ignore_case: true,
            wrap: true,
            kind: MatchKind::Contains,
        }
    }
}

/// A compiled query.
#[derive(Clone, Debug)]
pub struct Matcher {
    raw: Vec<u8>,
    folded: Vec<char>,
    ignore_case: bool,
    kind: MatchKind,
    first: [bool; 256],
    max_bytes: usize,
}
impl Matcher {
    /// `None` for an empty query, which matches nothing.
    pub fn new(query: &str, options: FindOptions) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        let mut folded = Vec::new();
        let mut first = [false; 256];
        if options.ignore_case {
            for c in query.chars() {
                fold_char(c, &mut |f| folded.push(f));
            }
            let head = *folded.first()?;
            if head.is_ascii() {
                first[head.to_ascii_lowercase() as usize] = true;
                first[head.to_ascii_uppercase() as usize] = true;
            }
            // Many characters beyond ASCII fold to something that starts with an ASCII
            // letter, such as `ß` and the Kelvin sign. Their first bytes all stay in.
            for lead in first.iter_mut().skip(0xc2).take(0xf5 - 0xc2) {
                *lead = true;
            }
        } else {
            first[*query.as_bytes().first()? as usize] = true;
        }
        // A folded character can come from one source character of up to 4 bytes.
        let max_bytes = if options.ignore_case {
            folded.len().saturating_mul(4)
        } else {
            query.len()
        };
        Some(Self {
            raw: query.as_bytes().to_vec(),
            folded,
            ignore_case: options.ignore_case,
            kind: options.kind,
            first,
            max_bytes,
        })
    }
    /// The most bytes one match can cover.
    pub fn max_match_bytes(&self) -> usize {
        self.max_bytes
    }
    /// The end of a match that starts at `at`, before the edge and word rules.
    fn end_of_match(&self, text: &str, at: usize) -> Option<usize> {
        if !self.ignore_case {
            let tail = text.as_bytes().get(at..)?;
            return tail.starts_with(&self.raw).then_some(at + self.raw.len());
        }
        let tail = text.get(at..)?;
        let mut want = 0;
        for (delta, c) in tail.char_indices() {
            let mut same = true;
            fold_char(c, &mut |f| {
                if self.folded.get(want) == Some(&f) {
                    want += 1;
                } else {
                    same = false;
                }
            });
            if !same {
                return None;
            }
            if want == self.folded.len() {
                return Some(at + delta + c.len_utf8());
            }
        }
        None
    }
    /// The match at `at`, with all the rules, or `None`.
    fn match_at(&self, text: &str, at: usize) -> Option<usize> {
        let end = self.end_of_match(text, at)?;
        if !is_cluster_edge(text, at) || !is_cluster_edge(text, end) {
            return None;
        }
        let word_before = || {
            text.get(..at)
                .and_then(|s| s.chars().next_back())
                .is_some_and(is_word)
        };
        let word_after = || {
            text.get(end..)
                .and_then(|s| s.chars().next())
                .is_some_and(is_word)
        };
        let good = match self.kind {
            MatchKind::Contains => true,
            MatchKind::StartsWith => !word_before(),
            MatchKind::EndsWith => !word_after(),
            MatchKind::FullWord => !word_before() && !word_after(),
        };
        good.then_some(end)
    }
    /// Calls `found` for each match that starts in `from..=last_start`, in order. A match
    /// never overlaps the one before. With `overlap`, each start is tried. It returns the
    /// offset to go on from, or `None` once `found` says to stop.
    fn scan_text(
        &self,
        text: &str,
        from: usize,
        last_start: usize,
        overlap: bool,
        found: &mut dyn FnMut(usize, usize) -> bool,
    ) -> Option<usize> {
        let bytes = text.as_bytes();
        let mut at = from;
        let stop = last_start.min(bytes.len().saturating_sub(1));
        while at <= stop && at < bytes.len() {
            let Some(skip) = bytes
                .get(at..=stop)
                .and_then(|window| window.iter().position(|b| self.first[*b as usize]))
            else {
                return Some(stop + 1);
            };
            at += skip;
            match self.match_at(text, at) {
                Some(end) => {
                    if !found(at, end) {
                        return None;
                    }
                    at = if overlap { at + 1 } else { end };
                }
                None => at += 1,
            }
        }
        Some(at)
    }
}

/// Every match in a string, for small text and for tests.
pub fn find_text(text: &str, query: &str, options: FindOptions) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if let Some(matcher) = Matcher::new(query, options)
        && !text.is_empty()
    {
        matcher.scan_text(text, 0, text.len() - 1, false, &mut |start, end| {
            out.push(start..end);
            true
        });
    }
    out
}

/// How a scan ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanEnd {
    /// It reached the end of the range.
    Finished,
    /// The caller's function said to stop.
    Stopped,
    Cancelled,
    Failed(String),
    /// A snapshot of a mapped file reads only what was indexed when it was taken. The scan
    /// used all of that. Go on from `resume_at` with a newer snapshot.
    NotYetIndexed {
        resume_at: ByteOffset,
    },
}

/// Where a forward scan stands in its buffer.
struct Window {
    buf: String,
    /// The document offset of `buf[0]`.
    base: usize,
    /// The offset in `buf` of the next candidate.
    next: usize,
}

/// Finds matches inside `range`, in order, with no overlap. Reads in bounded pieces, so it
/// may wait for disk: run it on a worker. A match that crosses a piece's edge is found.
/// `on_match` returns false to stop.
pub fn scan_forward(
    snapshot: &dyn TextSnapshot,
    matcher: &Matcher,
    range: Range<ByteOffset>,
    cancel: &AtomicBool,
    on_match: &mut dyn FnMut(Range<ByteOffset>) -> bool,
) -> ScanEnd {
    let readable = readable_len(snapshot);
    let start = range.start.0;
    let wanted_end = range.end.0.min(snapshot.len_bytes().0);
    let more_to_come = wanted_end > readable;
    let end = wanted_end.min(readable);
    if start >= end {
        return if more_to_come {
            ScanEnd::NotYetIndexed {
                resume_at: range.start,
            }
        } else {
            ScanEnd::Finished
        };
    }
    let read_from = align_down(snapshot, start.saturating_sub(CONTEXT));
    let read_to = align_up(snapshot, end.saturating_add(CONTEXT));
    let tail = matcher.max_bytes + CONTEXT;
    let block = BLOCK.max(tail * 2);
    let mut window = Window {
        buf: String::with_capacity(block * 2),
        base: read_from,
        next: start - read_from,
    };
    let mut stopped = false;
    let mut process = |window: &mut Window, last: bool| -> bool {
        // A candidate this close to the end of the buffer waits for more text, because
        // its match or the character after its match may be in the next piece.
        let room = if last {
            window.buf.len()
        } else {
            window.buf.len().saturating_sub(tail)
        };
        if room == 0 || window.next >= room {
            return true;
        }
        let doc = window.base;
        let going = matcher.scan_text(&window.buf, window.next, room - 1, false, &mut |s, e| {
            if doc + e > end {
                return true;
            }
            on_match(ByteOffset(doc + s)..ByteOffset(doc + e))
        });
        let Some(place) = going else {
            return false;
        };
        window.next = place;
        if !last {
            let mut cut = window.next.saturating_sub(CONTEXT);
            while cut > 0 && !window.buf.is_char_boundary(cut) {
                cut -= 1;
            }
            window.buf.drain(..cut);
            window.next -= cut;
            window.base += cut;
        }
        true
    };
    let good =
        snapshot.visit_chunks_while(ByteOffset(read_from)..ByteOffset(read_to), &mut |part| {
            if cancel.load(Ordering::Acquire) {
                return false;
            }
            window.buf.push_str(part);
            if window.buf.len() >= block && !process(&mut window, false) {
                stopped = true;
                return false;
            }
            true
        });
    if stopped {
        return ScanEnd::Stopped;
    }
    if cancel.load(Ordering::Acquire) {
        return ScanEnd::Cancelled;
    }
    if !good {
        return ScanEnd::Failed(read_failure(snapshot));
    }
    // With more text to come, the last candidates wait for it, as at any piece's edge.
    if !process(&mut window, !more_to_come) {
        return ScanEnd::Stopped;
    }
    if more_to_come {
        return ScanEnd::NotYetIndexed {
            resume_at: ByteOffset(window.base + window.next),
        };
    }
    ScanEnd::Finished
}

/// The first match in `range`.
pub fn first_match(
    snapshot: &dyn TextSnapshot,
    matcher: &Matcher,
    range: Range<ByteOffset>,
    cancel: &AtomicBool,
) -> Result<Option<Range<ByteOffset>>, ScanEnd> {
    let mut hit = None;
    match scan_forward(snapshot, matcher, range, cancel, &mut |found| {
        hit = Some(found);
        false
    }) {
        ScanEnd::Finished | ScanEnd::Stopped | ScanEnd::NotYetIndexed { .. } => Ok(hit),
        other => Err(other),
    }
}

/// The last match that ends at or before `end` and starts at or after `floor`. As in a
/// backward search on the Mac, each start is tried, so `aa` in `aaa` gives 1..3.
pub fn last_match(
    snapshot: &dyn TextSnapshot,
    matcher: &Matcher,
    floor: ByteOffset,
    end: ByteOffset,
    cancel: &AtomicBool,
) -> Result<Option<Range<ByteOffset>>, ScanEnd> {
    let overlap = matcher.max_bytes + CONTEXT;
    // A match is often near, so the first read is small, and each miss reads more.
    let most = BACK_BLOCK.max(overlap * 4);
    let mut block = (16 * 1024).max(overlap * 4);
    let limit = end.0.min(readable_len(snapshot));
    let floor = floor.0;
    let mut high = limit;
    let mut buf = String::new();
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(ScanEnd::Cancelled);
        }
        if high <= floor {
            return Ok(None);
        }
        let wanted = high
            .saturating_sub(block)
            .max(floor.saturating_sub(CONTEXT));
        let low = align_down(snapshot, wanted);
        let read_to = align_up(snapshot, high.saturating_add(overlap));
        buf.clear();
        if !snapshot.visit_chunks(ByteOffset(low)..ByteOffset(read_to), &mut |part| {
            buf.push_str(part)
        }) {
            return Err(ScanEnd::Failed(read_failure(snapshot)));
        }
        // The first bytes of a block that is not the first have no context, so the block
        // before covers them.
        let from = if low == 0 { 0 } else { CONTEXT }.max(floor.saturating_sub(low));
        let mut best = None;
        if !buf.is_empty() {
            matcher.scan_text(&buf, from, buf.len() - 1, true, &mut |s, e| {
                if low + e <= limit {
                    best = Some(low + s..low + e);
                }
                true
            });
        }
        if let Some(found) = best {
            return Ok(Some(ByteOffset(found.start)..ByteOffset(found.end)));
        }
        if low == 0 || low + from <= floor {
            return Ok(None);
        }
        // The next block ends far enough into this one to hold any match on the edge.
        let next_high = low + CONTEXT + matcher.max_bytes;
        if next_high >= high {
            return Err(ScanEnd::Failed("The search could not move on.".to_string()));
        }
        high = next_high;
        block = (block * 2).min(most);
    }
}

/// Which way a step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// The next match after `selection`, going round the end of the text if `wrap` is set.
/// Returns the match, and whether it wrapped.
pub fn find_step(
    snapshot: &dyn TextSnapshot,
    matcher: &Matcher,
    selection: Range<ByteOffset>,
    direction: Direction,
    wrap: bool,
    cancel: &AtomicBool,
) -> Result<Option<(Range<ByteOffset>, bool)>, ScanEnd> {
    let len = snapshot.len_bytes();
    match direction {
        Direction::Forward => {
            if let Some(found) = first_match(snapshot, matcher, selection.end..len, cancel)? {
                return Ok(Some((found, false)));
            }
            // Going round waits until the whole text is known, or it would skip matches.
            if !wrap || !snapshot.status().complete {
                return Ok(None);
            }
            let reach = ByteOffset(selection.end.0.saturating_add(matcher.max_bytes).min(len.0));
            Ok(first_match(snapshot, matcher, ByteOffset(0)..reach, cancel)?.map(|f| (f, true)))
        }
        Direction::Backward => {
            if let Some(found) =
                last_match(snapshot, matcher, ByteOffset(0), selection.start, cancel)?
            {
                return Ok(Some((found, false)));
            }
            if !wrap || !snapshot.status().complete {
                return Ok(None);
            }
            let floor = ByteOffset(selection.start.0.saturating_sub(matcher.max_bytes));
            Ok(last_match(snapshot, matcher, floor, len, cancel)?.map(|f| (f, true)))
        }
    }
}

/// What a `FindJob` is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Finished,
    Cancelled,
    Failed(String),
}

struct JobShared {
    cancel: AtomicBool,
    count: AtomicU64,
    dropped: AtomicU64,
    wake_armed: AtomicBool,
    queue: Mutex<VecDeque<Range<ByteOffset>>>,
    state: Mutex<JobState>,
    /// A newer snapshot from the host, while the worker waits for one.
    newer: Mutex<Option<Arc<dyn TextSnapshot>>>,
    newer_came: Condvar,
    /// The bytes the worker's snapshot could read, while it waits for more. Zero otherwise.
    waiting_past: AtomicU64,
}

/// A search of a range on its own thread. It counts every match and gives them as it finds
/// them. A new query cancels the old job, and dropping a job cancels it.
pub struct FindJob {
    shared: Arc<JobShared>,
}
impl FindJob {
    /// `limit` is the most matches that wait in its queue. Past it they are only counted,
    /// so a query that matches every byte of 1 GiB holds a bounded list.
    pub fn start(
        snapshot: Arc<dyn TextSnapshot>,
        matcher: Matcher,
        range: Range<ByteOffset>,
        limit: usize,
        wake: Option<Wake>,
    ) -> Self {
        let shared = Arc::new(JobShared {
            cancel: AtomicBool::new(false),
            count: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            wake_armed: AtomicBool::new(true),
            queue: Mutex::new(VecDeque::new()),
            state: Mutex::new(JobState::Running),
            newer: Mutex::new(None),
            newer_came: Condvar::new(),
            waiting_past: AtomicU64::new(0),
        });
        let inner = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("qe-tools-find".into())
            .spawn(move || {
                let mut last_wake = Instant::now();
                let mut snapshot = snapshot;
                let mut from = range.start;
                let end = loop {
                    let end = scan_forward(
                        snapshot.as_ref(),
                        &matcher,
                        from..range.end,
                        &inner.cancel,
                        &mut |found| {
                            let count = inner.count.fetch_add(1, Ordering::Relaxed) + 1;
                            let kept = match inner.queue.lock() {
                                Ok(mut queue) if queue.len() < limit => {
                                    queue.push_back(found);
                                    true
                                }
                                _ => false,
                            };
                            if !kept {
                                inner.dropped.fetch_add(1, Ordering::Relaxed);
                            }
                            // The first match wakes the host at once, and the rest at a pace
                            // it can keep. A host that has not read yet gets no more wakes.
                            if let Some(wake) = &wake
                                && inner.wake_armed.load(Ordering::Acquire)
                                && (count == 1 || last_wake.elapsed() >= WAKE_GAP)
                            {
                                inner.wake_armed.store(false, Ordering::Release);
                                last_wake = Instant::now();
                                wake();
                            }
                            !inner.cancel.load(Ordering::Acquire)
                        },
                    );
                    let ScanEnd::NotYetIndexed { resume_at } = end else {
                        break end;
                    };
                    from = resume_at;
                    // Only the UI thread can take a newer snapshot, so ask for one and sleep.
                    inner.waiting_past.store(
                        (readable_len(snapshot.as_ref()) as u64).max(1),
                        Ordering::Release,
                    );
                    if let Some(wake) = &wake {
                        wake();
                    }
                    let Ok(mut slot) = inner.newer.lock() else {
                        break ScanEnd::Failed("The search worker stopped.".to_string());
                    };
                    let next = loop {
                        if inner.cancel.load(Ordering::Acquire) {
                            break None;
                        }
                        if let Some(next) = slot.take() {
                            break Some(next);
                        }
                        slot = match inner.newer_came.wait(slot) {
                            Ok(slot) => slot,
                            Err(_) => return,
                        };
                    };
                    drop(slot);
                    inner.waiting_past.store(0, Ordering::Release);
                    match next {
                        Some(next) => snapshot = next,
                        None => break ScanEnd::Cancelled,
                    }
                };
                drop(snapshot);
                let state = match end {
                    ScanEnd::Finished => JobState::Finished,
                    ScanEnd::Stopped | ScanEnd::Cancelled | ScanEnd::NotYetIndexed { .. } => {
                        JobState::Cancelled
                    }
                    ScanEnd::Failed(message) => JobState::Failed(message),
                };
                if let Ok(mut slot) = inner.state.lock() {
                    *slot = state;
                }
                if let Some(wake) = &wake {
                    wake();
                }
            });
        if let Err(error) = spawned
            && let Ok(mut slot) = shared.state.lock()
        {
            *slot = JobState::Failed(format!("The search could not start: {error}"));
        }
        Self { shared }
    }
    /// Stops the worker soon. Its results so far stay readable.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Release);
        // Taking the lock first means that a worker about to sleep hears this.
        drop(self.shared.newer.lock());
        self.shared.newer_came.notify_all();
    }
    /// While the worker waits for a mapped file's index, the bytes it has read up to. The host
    /// passes a newer snapshot to `offer_snapshot` once `store_status` knows more.
    pub fn waiting_past(&self) -> Option<u64> {
        match self.shared.waiting_past.load(Ordering::Acquire) {
            0 => None,
            bytes => Some(bytes),
        }
    }
    /// Gives a waiting worker a snapshot that reads further.
    pub fn offer_snapshot(&self, snapshot: Arc<dyn TextSnapshot>) {
        if let Ok(mut slot) = self.shared.newer.lock() {
            *slot = Some(snapshot);
        }
        self.shared.newer_came.notify_all();
    }
    /// Calls `offer_snapshot` if the worker waits and the store knows more than it read.
    pub fn feed(&self, view: &qe_editor::EditorView) {
        if let Some(past) = self.waiting_past() {
            let status = view.store_status();
            if status.complete || status.known_bytes.0 as u64 > past {
                self.offer_snapshot(view.worker_snapshot());
            }
        }
    }
    /// All matches counted so far, also those past the queue's limit.
    pub fn count(&self) -> u64 {
        self.shared.count.load(Ordering::Relaxed)
    }
    /// Matches counted but not kept.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
    pub fn state(&self) -> JobState {
        match self.shared.state.lock() {
            Ok(state) => state.clone(),
            Err(_) => JobState::Failed("The search worker stopped.".to_string()),
        }
    }
    /// Takes the matches found since the last call, and lets the worker wake the host again.
    pub fn drain(&self) -> Vec<Range<ByteOffset>> {
        self.shared.wake_armed.store(true, Ordering::Release);
        match self.shared.queue.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
}
impl Drop for FindJob {
    fn drop(&mut self) {
        self.cancel();
    }
}
