//! The read-only store for very large files. `README.md` has the rules.
//!
//! Three threads touch this store. The caller's thread only reads memory on the
//! `try_` route. A worker thread indexes the file. A reader thread loads pages and
//! checks the file on request. Each has its own file handle, because reads on one
//! handle wait for each other.
use crate::hash::page_hash;
use crate::{Sum, measure};
use qe_api::{
    ByteOffset, EditError, KnownLine, LineIndex, LookupError, StoreFailure, StoreStatus,
    TextChange, TextPosition, TextSnapshot, TextStore, Utf16Offset,
};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx,
};

const PAGE: usize = 64 * 1024;
const SCAN_BLOCK: usize = 4 * 1024 * 1024;
const CACHED_PAGES: usize = 16;
const CACHED_LINE_TABLES: usize = 4;
/// The most pages `try_visit_chunks` will ask for, half of the cache.
const TRY_VISIT_PAGES: usize = CACHED_PAGES / 2;
/// The file is checked at most this often while queries are running.
const CHECK_INTERVAL: Duration = Duration::from_millis(25);
/// How long another app's lock may last before the store gives up.
const DEFAULT_LOCK_GIVE_UP: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(50);
const WAITING_QUEUE: usize = 64;
const SHARE_ALL: u32 = 1 | 2 | 4;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;

type Result<T> = std::result::Result<T, LookupError>;

/// Why an internal query stopped. A page miss differs from "past the index",
/// because a snapshot can wait for a page and can never wait for the index.
enum Stop {
    Page,
    Error(LookupError),
}

impl From<LookupError> for Stop {
    fn from(e: LookupError) -> Self {
        Stop::Error(e)
    }
}

type Query<T> = std::result::Result<T, Stop>;

/// Runs after the index grows, a page loads or the store fails, on one of the
/// store's own threads. It must only post a message and return. See `README.md`
/// for when it starts and stops.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Fills `buf` from `offset`. A short read loops. Zero bytes means the file ended.
fn read_full(
    mut read_at: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
    buf: &mut [u8],
    offset: u64,
) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        match read_at(&mut buf[done..], offset + done as u64) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the file ended early",
                ));
            }
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Another app holding a lock or an exclusive open. This passes, so it is not a
/// failure until `DEFAULT_LOCK_GIVE_UP` has gone by.
fn is_lock_error(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(ERROR_LOCK_VIOLATION | ERROR_SHARING_VIOLATION)
    )
}

/// What the file looked like at open. `change` is NTFS's ChangeTime, which also
/// moves when an app restores the modified time with SetFileTime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    volume: u32,
    id: u64,
    len: u64,
    created: i64,
    written: i64,
    change: i64,
}

fn os_error(e: windows::core::Error) -> io::Error {
    io::Error::other(e.message())
}

fn file_stamp(file: &File) -> io::Result<FileStamp> {
    let handle = HANDLE(file.as_raw_handle());
    let mut whole = BY_HANDLE_FILE_INFORMATION::default();
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: `handle` stays open for this call because `file` is borrowed, and
    // both out pointers refer to live values of the size each call expects.
    unsafe {
        GetFileInformationByHandle(handle, &mut whole).map_err(os_error)?;
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&raw mut basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
        .map_err(os_error)?;
    }
    Ok(FileStamp {
        volume: whole.dwVolumeSerialNumber,
        id: u64::from(whole.nFileIndexHigh) << 32 | u64::from(whole.nFileIndexLow),
        len: u64::from(whole.nFileSizeHigh) << 32 | u64::from(whole.nFileSizeLow),
        created: basic.CreationTime,
        written: basic.LastWriteTime,
        change: basic.ChangeTime,
    })
}

fn stamp_at_path(path: &Path) -> io::Result<FileStamp> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .open(path)?;
    file_stamp(&file)
}

/// `cumulative` holds totals from the start of the file to this block's end. A
/// block never ends between CR and LF, so its break count is exact.
#[derive(Clone, Copy)]
struct Block {
    cumulative: Sum,
}

/// All three change together under the write lock, so a revision names one state.
struct Index {
    blocks: Vec<Block>,
    total: Sum,
    revision: u64,
}

/// Time the worker spent on each step, in nanoseconds. Measured, for tuning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexTiming {
    pub read: u64,
    pub hash: u64,
    pub validate: u64,
    pub measure: u64,
    pub check: u64,
}

#[derive(Default)]
struct Timers {
    read: AtomicU64,
    hash: AtomicU64,
    validate: AtomicU64,
    measure: AtomicU64,
    check: AtomicU64,
}

fn timed<T>(timer: &AtomicU64, work: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = work();
    timer.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    value
}

/// Work waiting for the reader thread.
#[derive(Default)]
struct ReaderQueue {
    pages: VecDeque<usize>,
    check_wanted: u64,
    check_done: u64,
    stop: bool,
}

struct Inner {
    path: PathBuf,
    original: FileStamp,
    size: usize,
    index: RwLock<Index>,
    indexed: AtomicUsize,
    failed: AtomicBool,
    failure: Mutex<Option<StoreFailure>>,
    /// One hash per page, 0 until something first reads that page.
    page_hashes: Box<[AtomicU64]>,
    pages: Mutex<VecDeque<(usize, Arc<[u8]>)>>,
    line_tables: Mutex<VecDeque<(usize, Arc<[u32]>)>>,
    /// Counts events that a waiting plain query cares about.
    progress: Mutex<u64>,
    wake: Condvar,
    queue: Mutex<ReaderQueue>,
    queue_wake: Condvar,
    stop: AtomicBool,
    notify_on: AtomicBool,
    born: Instant,
    last_check_us: AtomicU64,
    locked_since_us: AtomicU64,
    lock_give_up_us: AtomicU64,
    test_read_delay_ms: AtomicU64,
    disk_reads: AtomicU64,
    file_checks: AtomicU64,
    disk_threads: Mutex<Vec<ThreadId>>,
    timers: Timers,
    notify: Option<Notify>,
}

impl Inner {
    fn now_us(&self) -> u64 {
        self.born.elapsed().as_micros() as u64
    }
    fn current_failure(&self) -> Option<StoreFailure> {
        if !self.failed.load(Ordering::Acquire) {
            return None;
        }
        self.failure.lock().unwrap().clone()
    }
    /// Records a failure. The smallest `valid_up_to` wins, so a later file change
    /// overrides an earlier report of a bad byte.
    fn fail(&self, message: impl Into<String>, valid_up_to: usize) -> StoreFailure {
        let failure = {
            let mut slot = self.failure.lock().unwrap();
            match &*slot {
                Some(old) if old.valid_up_to.0 <= valid_up_to => {}
                _ => {
                    *slot = Some(StoreFailure {
                        message: message.into(),
                        valid_up_to: ByteOffset(valid_up_to),
                    });
                }
            }
            slot.clone().expect("the slot was just filled")
        };
        self.failed.store(true, Ordering::Release);
        self.signal();
        failure
    }
    fn wake_waiters(&self) {
        *self.progress.lock().unwrap() += 1;
        self.wake.notify_all();
    }
    fn signal(&self) {
        self.wake_waiters();
        if self.notify_on.load(Ordering::Acquire)
            && !self.stop.load(Ordering::Acquire)
            && let Some(notify) = &self.notify
        {
            notify();
        }
    }
    fn note_disk_thread(&self) {
        let id = std::thread::current().id();
        let mut threads = self.disk_threads.lock().unwrap();
        if !threads.contains(&id) {
            threads.push(id);
        }
    }
    /// Keeps track of how long another app's lock has lasted. Past the limit, it
    /// counts as a failure, so that no plain query can wait for it for ever.
    fn note_lock(&self, e: &io::Error) {
        let now = self.now_us().max(1);
        let since =
            match self
                .locked_since_us
                .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => now,
                Err(since) => since,
            };
        if now - since > self.lock_give_up_us.load(Ordering::Relaxed) {
            self.fail(format!("mapped file stayed locked: {e}"), 0);
        }
    }
    fn lock_passed(&self) {
        self.locked_since_us.store(0, Ordering::Release);
    }
    /// Runs on the worker or the reader, with that thread's own handle.
    fn run_file_check(&self, file: &File) -> Result<()> {
        self.note_disk_thread();
        self.file_checks.fetch_add(1, Ordering::Relaxed);
        self.last_check_us.store(self.now_us(), Ordering::Relaxed);
        let result = stamp_at_path(&self.path).and_then(|at_path| {
            let held = file_stamp(file)?;
            if at_path != self.original || held != self.original {
                return Err(io::Error::other("the file changed outside QuickEdit"));
            }
            Ok(())
        });
        match result {
            Ok(()) => Ok(()),
            Err(e) if is_lock_error(&e) => {
                self.note_lock(&e);
                Ok(())
            }
            Err(e) => Err(LookupError::Failed(
                self.fail(format!("mapped file changed: {e}"), 0),
            )),
        }
    }
    /// Asks the reader for a file check. It never waits.
    fn request_check(&self) -> u64 {
        let mut queue = self.queue.lock().unwrap();
        queue.check_wanted += 1;
        let target = queue.check_wanted;
        drop(queue);
        self.queue_wake.notify_one();
        target
    }
    /// Claims a file check when `CHECK_INTERVAL` has passed. True for the winner.
    fn check_is_due(&self) -> bool {
        let now = self.now_us();
        let last = self.last_check_us.load(Ordering::Relaxed);
        now.saturating_sub(last) >= CHECK_INTERVAL.as_micros() as u64
            && self
                .last_check_us
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }
    /// Bounds the cost of the file check, and never runs it on the caller. No query
    /// means no check, so an idle store does no work.
    fn check_if_due(&self) {
        if self.check_is_due() {
            self.request_check();
        }
    }
    /// Runs a check on the reader and waits for it. For plain queries only.
    fn check_and_wait(&self) -> Result<()> {
        let target = self.request_check();
        let mut seen = self.progress.lock().unwrap();
        loop {
            if let Some(failure) = self.current_failure()
                && failure.valid_up_to.0 == 0
            {
                return Err(LookupError::Failed(failure));
            }
            if self.queue.lock().unwrap().check_done >= target {
                return Ok(());
            }
            seen = self
                .wake
                .wait_timeout(seen, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }
    /// The failure gate that every query passes. It fails when no part is safe or
    /// when the query needs bytes past `valid_up_to`.
    fn gate(&self, upto: usize) -> Result<()> {
        if let Some(failure) = self.current_failure()
            && (failure.valid_up_to.0 == 0 || upto > failure.valid_up_to.0)
        {
            return Err(LookupError::Failed(failure));
        }
        self.check_if_due();
        Ok(())
    }
    /// Compares a page with its hash, or records the hash the first time. Every page
    /// read goes through here, on either thread, so changed bytes cannot get into
    /// the index or the cache.
    fn verify_page(&self, page_at: usize, bytes: &[u8]) -> bool {
        let hash = page_hash(bytes);
        match self.page_hashes[page_at / PAGE].compare_exchange(
            0,
            hash,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(old) => old == hash,
        }
    }
    /// Loads one page on the reader thread. No lock is held during the read.
    fn load_page(&self, file: &File, page_at: usize) {
        self.note_disk_thread();
        self.disk_reads.fetch_add(1, Ordering::Relaxed);
        let delay = self.test_read_delay_ms.load(Ordering::Relaxed);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        let mut bytes = vec![0; PAGE.min(self.size - page_at)];
        if let Err(e) = read_full(|b, o| file.seek_read(b, o), &mut bytes, page_at as u64) {
            if is_lock_error(&e) {
                // A waiting query asks again after its own short wait.
                self.note_lock(&e);
            } else {
                self.fail(format!("mapped file read failed: {e}"), 0);
            }
            return;
        }
        self.lock_passed();
        if !self.verify_page(page_at, &bytes) {
            self.fail(
                format!("the file changed outside QuickEdit: page at byte {page_at} differs"),
                0,
            );
            return;
        }
        self.add_page(page_at, Arc::from(bytes));
        self.signal();
    }
    fn add_page(&self, page_at: usize, bytes: Arc<[u8]>) {
        let mut pages = self.pages.lock().unwrap();
        if !pages.iter().any(|(start, _)| *start == page_at) {
            if pages.len() == CACHED_PAGES {
                pages.pop_front();
            }
            pages.push_back((page_at, bytes));
        }
    }
    fn reader(self: Arc<Self>, file: File) {
        let guard = FailOnPanic(self);
        let inner = &guard.0;
        loop {
            let mut queue = inner.queue.lock().unwrap();
            while !queue.stop && queue.check_wanted == queue.check_done && queue.pages.is_empty() {
                queue = inner.queue_wake.wait(queue).unwrap();
            }
            if queue.stop {
                return;
            }
            if queue.check_wanted != queue.check_done {
                let target = queue.check_wanted;
                drop(queue);
                let _ = inner.run_file_check(&file);
                inner.queue.lock().unwrap().check_done = target;
                inner.wake_waiters();
            } else if let Some(page_at) = queue.pages.pop_front() {
                drop(queue);
                let cached = inner
                    .pages
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(start, _)| *start == page_at);
                if !cached {
                    inner.load_page(&file, page_at);
                }
            }
        }
    }
    /// Copies a range from the cache, and never touches the disk. On a miss it asks
    /// the reader for every missing page.
    fn cached_range(&self, start: usize, end: usize) -> Query<Vec<u8>> {
        let first = start / PAGE;
        let last = end.saturating_sub(1).max(start) / PAGE;
        let mut found: Vec<Arc<[u8]>> = Vec::with_capacity(last - first + 1);
        let mut missing: Vec<usize> = Vec::new();
        {
            let mut pages = self.pages.lock().unwrap();
            for page in first..=last {
                let page_at = page * PAGE;
                if let Some(i) = pages.iter().position(|(at, _)| *at == page_at) {
                    // Newest use goes to the back, so the page leaves the cache last.
                    let entry = pages.remove(i).expect("position found it");
                    found.push(entry.1.clone());
                    pages.push_back(entry);
                } else {
                    missing.push(page_at);
                }
            }
        }
        if !missing.is_empty() {
            let mut queue = self.queue.lock().unwrap();
            for page_at in missing {
                if !queue.pages.contains(&page_at) {
                    if queue.pages.len() == WAITING_QUEUE {
                        queue.pages.pop_front();
                    }
                    queue.pages.push_back(page_at);
                }
            }
            drop(queue);
            self.queue_wake.notify_one();
            return Err(Stop::Page);
        }
        let mut out = Vec::with_capacity(end - start);
        let mut at = start;
        for page in &found {
            if at >= end {
                break;
            }
            let from = at % PAGE;
            let take = (end - at).min(page.len() - from);
            out.extend_from_slice(&page[from..from + take]);
            at += take;
        }
        Ok(out)
    }
    /// Takes pages out of the cache, so the next read of them comes from the file and
    /// meets its hash. Only the plain chunk read does this.
    fn forget_pages(&self, start: usize, end: usize) {
        let first = start / PAGE;
        let last = end.saturating_sub(1).max(start) / PAGE;
        self.pages
            .lock()
            .unwrap()
            .retain(|(at, _)| at / PAGE < first || at / PAGE > last);
    }
    fn mismatch(&self) -> LookupError {
        LookupError::Failed(self.fail("the index does not match the file", 0))
    }
    fn should_index(&self) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        let indexed = self.indexed.load(Ordering::Acquire);
        if indexed >= self.size {
            return false;
        }
        // A bad byte that a chunk read found further on does not stop the index
        // short of it. Anything that makes the whole file unsafe does.
        self.current_failure()
            .is_none_or(|f| f.valid_up_to.0 > indexed)
    }
    /// Reads and indexes the next part of the file on the caller's thread: at open
    /// with `PAGE` and on the worker with `SCAN_BLOCK`. Reads start on a page edge, so
    /// each whole page gets its hash. No lock is held during the read or the scan; the
    /// write lock covers only adding the finished blocks.
    fn index_next(&self, file: &File, buffer: &mut Vec<u8>, max_bytes: usize) -> bool {
        let start = self.indexed.load(Ordering::Acquire);
        if start >= self.size {
            return true;
        }
        let read_at = start / PAGE * PAGE;
        let skip = start - read_at;
        let want = (self.size - read_at).min(max_bytes);
        buffer.resize(want, 0);
        let bytes = &mut buffer[..want];
        self.note_disk_thread();
        let read = timed(&self.timers.read, || {
            read_full(|b, o| file.seek_read(b, o), bytes, read_at as u64)
        });
        if let Err(e) = read {
            if is_lock_error(&e) {
                self.note_lock(&e);
                std::thread::sleep(LOCK_RETRY);
                return !self.failed.load(Ordering::Acquire);
            }
            self.fail(format!("mapped file read failed: {e}"), 0);
            return false;
        }
        self.lock_passed();
        if timed(&self.timers.check, || self.run_file_check(file)).is_err() {
            return false;
        }
        let bytes = &buffer[..want];
        let same = timed(&self.timers.hash, || {
            bytes
                .chunks(PAGE)
                .enumerate()
                .position(|(i, page)| !self.verify_page(read_at + i * PAGE, page))
        });
        if let Some(i) = same {
            let page_at = read_at + i * PAGE;
            self.fail(
                format!("the file changed outside QuickEdit: page at byte {page_at} differs"),
                0,
            );
            return false;
        }
        let at_eof = read_at + want == self.size;
        let body = &bytes[skip..];
        let keep = if at_eof {
            body.len()
        } else {
            whole_lines_edge(body)
        };
        let (text, bad_at) =
            match timed(&self.timers.validate, || std::str::from_utf8(&body[..keep])) {
                Ok(text) => (text, None),
                Err(e) => {
                    let valid = e.valid_up_to();
                    match std::str::from_utf8(&body[..valid]) {
                        Ok(text) => (text, Some(start + valid)),
                        Err(_) => ("", Some(start)),
                    }
                }
            };
        let blocks = timed(&self.timers.measure, || {
            let previous = self.index.read().unwrap().total;
            measure_blocks(text, start, previous)
        });
        self.publish(blocks);
        if let Some(at) = bad_at {
            self.fail(format!("invalid UTF-8 at byte {at}"), at);
            return false;
        }
        self.signal();
        true
    }
    /// Adds finished blocks. Growing the Vec copies it, so that happens on the side,
    /// and the write lock is held only to swap and to add.
    fn publish(&self, blocks: Vec<Block>) {
        let Some(total) = blocks.last().map(|b| b.cumulative) else {
            return;
        };
        let bigger = {
            let index = self.index.read().unwrap();
            let need = index.blocks.len() + blocks.len();
            (need > index.blocks.capacity()).then(|| {
                let mut grown = Vec::with_capacity((2 * index.blocks.capacity()).max(need));
                grown.extend_from_slice(&index.blocks);
                grown
            })
        };
        let old = {
            let mut index = self.index.write().unwrap();
            let old = bigger.map(|grown| std::mem::replace(&mut index.blocks, grown));
            index.blocks.extend(blocks);
            index.total = total;
            index.revision += 1;
            self.indexed.store(total.bytes, Ordering::Release);
            old
        };
        drop(old);
    }
    fn worker(self: Arc<Self>, file: File) {
        let guard = FailOnPanic(self);
        let inner = &guard.0;
        let mut buffer = Vec::with_capacity(SCAN_BLOCK);
        while inner.should_index() {
            if !inner.index_next(&file, &mut buffer, SCAN_BLOCK) {
                break;
            }
        }
    }
}

/// Turns a panic on one of the store's threads into a failure that callers see.
struct FailOnPanic(Arc<Inner>);

impl Drop for FailOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.fail("a mapped file thread stopped", 0);
        }
    }
}

/// Where to end a scan that is not at the end of the file, so that no character
/// and no CRLF is cut. `bytes` holds several bytes at least.
fn whole_lines_edge(bytes: &[u8]) -> usize {
    let len = bytes.len();
    let mut lead = len - 1;
    while lead > 0 && len - lead < 4 && bytes[lead] & 0xC0 == 0x80 {
        lead -= 1;
    }
    if lead == len - 1 && bytes[lead] < 0x80 && bytes[lead] != b'\r' {
        return len;
    }
    lead
}

/// Cuts valid text into blocks that end near page edges, and totals them.
fn measure_blocks(text: &str, file_start: usize, mut total: Sum) -> Vec<Block> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::with_capacity(text.len() / PAGE + 2);
    let mut start = 0;
    while start < text.len() {
        // A start just short of a page edge aims for the next edge, so a block that
        // backed off from an edge is not followed by a tiny block.
        let page_edge = ((file_start + start + 4) / PAGE + 1) * PAGE - file_start;
        let mut end = page_edge.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end <= start {
            end = page_edge.min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
        }
        if end < text.len() && bytes[end - 1] == b'\r' && bytes[end] == b'\n' {
            end += 1;
        }
        total = total.add(measure(&text[start..end]));
        blocks.push(Block { cumulative: total });
        start = end;
    }
    blocks
}

/// Gives the end of each line break in a block, in order.
fn break_ends(bytes: &[u8]) -> Vec<u32> {
    let mut ends = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = match bytes[at] {
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => at + 2,
            b'\r' | b'\n' => at + 1,
            0xC2 if bytes.get(at + 1) == Some(&0x85) => at + 2,
            0xE2 if bytes.get(at + 1) == Some(&0x80)
                && matches!(bytes.get(at + 2), Some(0xA8 | 0xA9)) =>
            {
                at + 3
            }
            _ => {
                at += 1;
                continue;
            }
        };
        ends.push(end as u32);
        at = end;
    }
    ends
}

/// The index as it stood when a snapshot was made.
#[derive(Clone, Copy)]
struct Pin {
    total: Sum,
    blocks: usize,
    revision: u64,
}

/// Stops the worker when the last live store for a file goes away.
struct WorkerStop(Arc<Inner>);

impl Drop for WorkerStop {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
    }
}

/// Stops the reader when the last store or snapshot for a file goes away.
struct ReaderStop(Arc<Inner>);

impl Drop for ReaderStop {
    fn drop(&mut self) {
        self.0.queue.lock().unwrap().stop = true;
        self.0.queue_wake.notify_all();
    }
}

/// A read-only UTF-8 file. `open` reads the first 64 KiB, and two threads do all
/// later disk work.
///
/// **On the UI thread, use only the `try_` queries and `status`.** They read memory
/// only: a page that is not in memory gives `NotYetKnown`, a background read starts,
/// and `Notify` runs when it is done. The plain queries that return `Option` or `bool`
/// wait for disk and for the index, so **they must not run on the UI thread**. `open`
/// also reads the disk. `TextStore::snapshot` gives a view that never moves. See
/// `README.md`.
#[derive(Clone)]
pub struct MappedStore {
    inner: Arc<Inner>,
    /// Only a live store keeps the worker running.
    _worker_stop: Option<Arc<WorkerStop>>,
    _reader_stop: Arc<ReaderStop>,
    pin: Option<Pin>,
}

type Spawn = fn(&str, Box<dyn FnOnce() + Send>) -> io::Result<()>;

fn spawn_thread(name: &str, work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(work)
        .map(|_| ())
}

impl MappedStore {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_inner(path.as_ref(), None, spawn_thread)
    }
    /// Like `open`. `notify` runs after the index grows, a page loads or the store
    /// fails, so a window can post itself a message. This is how a failure reaches a
    /// UI that is not asking at that moment. No call comes from the work `open` does
    /// itself. Calls can start as soon as the threads start, which can be just before
    /// `open` returns. No new call starts after the last live store is dropped, and
    /// one already running may still finish.
    pub fn open_with_notify(path: impl AsRef<Path>, notify: Notify) -> io::Result<Self> {
        Self::open_inner(path.as_ref(), Some(notify), spawn_thread)
    }
    fn open_inner(path: &Path, notify: Option<Notify>, spawn: Spawn) -> io::Result<Self> {
        // The process can change its folder later, and the file check opens this path.
        let path = std::path::absolute(path)?;
        let open_read = || {
            OpenOptions::new()
                .read(true)
                .share_mode(SHARE_ALL)
                .open(&path)
        };
        let reader_file = open_read()?;
        let worker_file = open_read()?;
        let original = file_stamp(&reader_file)?;
        if file_stamp(&worker_file)? != original {
            return Err(io::Error::other("the file changed during open"));
        }
        let size =
            usize::try_from(original.len).map_err(|_| io::Error::other("file is too large"))?;
        if size == 0 {
            return Err(io::Error::other("cannot map an empty file"));
        }
        let page_count = size.div_ceil(PAGE);
        // One block per page, one more for each scan that ends short of a page edge.
        // Past 2^20 blocks (32 MiB of index, or about 64 GiB of file), it grows.
        let block_room = (page_count + size / SCAN_BLOCK + 3).min(1 << 20);
        let inner = Arc::new(Inner {
            path,
            original,
            size,
            index: RwLock::new(Index {
                blocks: Vec::with_capacity(block_room),
                total: Sum::default(),
                revision: 0,
            }),
            indexed: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            failure: Mutex::new(None),
            page_hashes: (0..page_count).map(|_| AtomicU64::new(0)).collect(),
            pages: Mutex::new(VecDeque::new()),
            line_tables: Mutex::new(VecDeque::new()),
            progress: Mutex::new(0),
            wake: Condvar::new(),
            queue: Mutex::new(ReaderQueue::default()),
            queue_wake: Condvar::new(),
            stop: AtomicBool::new(false),
            notify_on: AtomicBool::new(false),
            born: Instant::now(),
            last_check_us: AtomicU64::new(0),
            locked_since_us: AtomicU64::new(0),
            lock_give_up_us: AtomicU64::new(DEFAULT_LOCK_GIVE_UP.as_micros() as u64),
            test_read_delay_ms: AtomicU64::new(0),
            disk_reads: AtomicU64::new(0),
            file_checks: AtomicU64::new(0),
            disk_threads: Mutex::new(Vec::new()),
            timers: Timers::default(),
            notify,
        });
        let mut buffer = Vec::with_capacity(PAGE);
        inner.index_next(&worker_file, &mut buffer, PAGE);
        // A bad byte in the first page means this is not UTF-8 text to show as is.
        if let Some(failure) = inner.current_failure() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, failure.message));
        }
        // The first page is in memory from the start, so a first screen never waits.
        inner.add_page(0, Arc::from(&buffer[..]));
        drop(buffer);
        inner.notify_on.store(true, Ordering::Release);
        let reader = inner.clone();
        spawn(
            "qe-buffer-read",
            Box::new(move || reader.reader(reader_file)),
        )?;
        let reader_stop = Arc::new(ReaderStop(inner.clone()));
        let worker = inner.clone();
        spawn(
            "qe-buffer-index",
            Box::new(move || worker.worker(worker_file)),
        )?;
        Ok(Self {
            _worker_stop: Some(Arc::new(WorkerStop(inner.clone()))),
            _reader_stop: reader_stop,
            inner,
            pin: None,
        })
    }
    pub fn indexed_up_to(&self) -> ByteOffset {
        ByteOffset(self.known().bytes)
    }
    /// The failure message, if any. `TextSnapshot::status` has the full failure.
    pub fn error_state(&self) -> Option<String> {
        self.inner.current_failure().map(|f| f.message)
    }
    pub fn is_index_complete(&self) -> bool {
        self.known().bytes == self.inner.size
    }
    /// Checks the file now, on the reader thread, and waits for the answer, so it
    /// must not run on the UI thread. Any failure stored before or found now is the
    /// result. `request_file_check` is the route that does not wait.
    pub fn check_file(&self) -> Result<()> {
        self.inner.check_and_wait()?;
        match self.inner.current_failure() {
            Some(failure) => Err(LookupError::Failed(failure)),
            None => Ok(()),
        }
    }
    /// Asks for a file check now and does not wait. A failure then reaches `Notify`
    /// and every later query.
    pub fn request_file_check(&self) {
        self.inner.request_check();
    }
    /// Bytes reserved for sparse checkpoints and page hashes, excluding the bounded
    /// scan buffer and page cache.
    pub fn index_memory_bytes(&self) -> usize {
        self.inner.index.read().unwrap().blocks.capacity() * size_of::<Block>()
            + self.inner.page_hashes.len() * size_of::<AtomicU64>()
    }
    /// Read and scan time so far, in nanoseconds. `index_timing` splits the scan.
    pub fn index_timing_ns(&self) -> (u64, u64) {
        let t = self.index_timing();
        (t.read, t.validate + t.measure)
    }
    pub fn index_timing(&self) -> IndexTiming {
        let t = &self.inner.timers;
        IndexTiming {
            read: t.read.load(Ordering::Relaxed),
            hash: t.hash.load(Ordering::Relaxed),
            validate: t.validate.load(Ordering::Relaxed),
            measure: t.measure.load(Ordering::Relaxed),
            check: t.check.load(Ordering::Relaxed),
        }
    }
    /// Pages read from the file for lookups so far.
    pub fn lookup_disk_reads(&self) -> u64 {
        self.inner.disk_reads.load(Ordering::Relaxed)
    }
    /// File checks run so far, by lookups and by the worker.
    pub fn file_checks(&self) -> u64 {
        self.inner.file_checks.load(Ordering::Relaxed)
    }
    /// Each thread that has read the file or checked it, for tests. `open` counts,
    /// so a test compares this with what it saw after `open`.
    #[doc(hidden)]
    pub fn disk_threads(&self) -> Vec<ThreadId> {
        self.inner.disk_threads.lock().unwrap().clone()
    }
    /// Forgets the thread list, so that `open`'s own reads do not count.
    #[doc(hidden)]
    pub fn clear_disk_threads(&self) {
        self.inner.disk_threads.lock().unwrap().clear();
    }
    /// Makes each page read slow, so a test can see what happens during one.
    #[doc(hidden)]
    pub fn set_test_read_delay(&self, delay: Duration) {
        self.inner
            .test_read_delay_ms
            .store(delay.as_millis() as u64, Ordering::Relaxed);
    }
    /// Shortens how long another app's lock may last, for a test.
    #[doc(hidden)]
    pub fn set_test_lock_give_up(&self, limit: Duration) {
        self.inner
            .lock_give_up_us
            .store(limit.as_micros() as u64, Ordering::Relaxed);
    }

    fn known(&self) -> Sum {
        match self.pin {
            Some(pin) => pin.total,
            None => self.inner.index.read().unwrap().total,
        }
    }
    /// What to say when the answer is past what is known. While the worker can still
    /// reach it, the answer is "not yet", even after a bad byte further on.
    fn missing(&self, known: Sum) -> LookupError {
        match self.inner.current_failure() {
            Some(failure) if self.pin.is_some() || known.bytes >= failure.valid_up_to.0 => {
                LookupError::Failed(failure)
            }
            _ => LookupError::NotYetKnown {
                indexed_up_to: ByteOffset(known.bytes),
            },
        }
    }
    /// Finds a block with binary search on the checkpoints, then lets go of the
    /// lock before anything else. Gives the totals before it and its end.
    fn find_block(&self, is_before: impl Fn(&Sum) -> bool) -> Option<(Sum, usize)> {
        let index = self.inner.index.read().unwrap();
        let count = self.pin.map_or(index.blocks.len(), |pin| pin.blocks);
        let blocks = &index.blocks[..count];
        let i = blocks.partition_point(|b| is_before(&b.cumulative));
        let end = blocks.get(i)?.cumulative.bytes;
        let before = if i == 0 {
            Sum::default()
        } else {
            blocks[i - 1].cumulative
        };
        Some((before, end))
    }
    /// Totals for the text before `at`.
    fn prefix(&self, at: usize) -> Query<Sum> {
        if at > self.inner.size {
            return Err(LookupError::OutOfRange.into());
        }
        self.inner.gate(at)?;
        if at == 0 {
            return Ok(Sum::default());
        }
        let known = self.known();
        if at > known.bytes {
            return Err(self.missing(known).into());
        }
        let (before, _) = self
            .find_block(|sum| sum.bytes < at)
            .ok_or_else(|| self.inner.mismatch())?;
        let bytes = self.inner.cached_range(before.bytes, at)?;
        match std::str::from_utf8(&bytes) {
            Ok(text) => Ok(before.add(measure(text))),
            Err(e) if e.error_len().is_none() => Err(LookupError::OutOfRange.into()),
            Err(_) => Err(self.inner.mismatch().into()),
        }
    }
    /// The line that holds byte `at`.
    fn line_of(&self, at: usize) -> Query<usize> {
        let sum = self.prefix(at)?;
        if !sum.last_cr || at == self.inner.size {
            return Ok(sum.breaks);
        }
        let next = self.inner.cached_range(at, at + 1)?;
        Ok(sum.breaks - usize::from(next[0] == b'\n'))
    }
    /// The break ends of one block, kept so that a frame of nearby lines scans once.
    fn line_table(&self, block_at: usize, block_end: usize) -> Query<Arc<[u32]>> {
        {
            let tables = self.inner.line_tables.lock().unwrap();
            if let Some((_, table)) = tables.iter().find(|(at, _)| *at == block_at) {
                return Ok(table.clone());
            }
        }
        let bytes = self.inner.cached_range(block_at, block_end)?;
        let table: Arc<[u32]> = break_ends(&bytes).into();
        let mut tables = self.inner.line_tables.lock().unwrap();
        if tables.len() == CACHED_LINE_TABLES {
            tables.pop_front();
        }
        tables.push_back((block_at, table.clone()));
        Ok(table)
    }
    fn line_start(&self, line: usize) -> Query<usize> {
        let known = self.known();
        self.inner.gate(known.bytes)?;
        if line == 0 {
            return Ok(0);
        }
        if line > known.breaks {
            return Err(if known.bytes == self.inner.size {
                LookupError::OutOfRange
            } else {
                self.missing(known)
            }
            .into());
        }
        let (before, end) = self
            .find_block(|sum| sum.breaks < line)
            .ok_or_else(|| self.inner.mismatch())?;
        let table = self.line_table(before.bytes, end)?;
        table
            .get(line - before.breaks - 1)
            .map(|offset| before.bytes + *offset as usize)
            .ok_or_else(|| self.inner.mismatch().into())
    }
    fn range_of_line(&self, line: usize) -> Query<Range<ByteOffset>> {
        let start = self.line_start(line)?;
        let end = match self.line_start(line + 1) {
            Ok(end) => end,
            Err(Stop::Error(LookupError::OutOfRange)) => self.inner.size,
            Err(e) => return Err(e),
        };
        Ok(ByteOffset(start)..ByteOffset(end))
    }
    /// The known part of a line, which may stop short of its end.
    fn known_line(&self, line: usize) -> Query<KnownLine> {
        let start = self.line_start(line)?;
        let (end, complete) = match self.line_start(line + 1) {
            Ok(end) => (end, true),
            Err(Stop::Error(LookupError::OutOfRange)) => (self.inner.size, true),
            Err(Stop::Error(LookupError::NotYetKnown { .. })) => (self.known().bytes, false),
            // A bad byte ended the index, and the part before it is still good.
            Err(Stop::Error(LookupError::Failed(f))) if f.valid_up_to.0 > 0 => {
                (self.known().bytes, false)
            }
            Err(e) => return Err(e),
        };
        Ok(KnownLine {
            range: ByteOffset(start)..ByteOffset(end),
            complete,
        })
    }
    fn byte_of_unit(&self, utf16: usize) -> Query<usize> {
        let known = self.known();
        self.inner.gate(known.bytes)?;
        if utf16 > known.units {
            return Err(if known.bytes == self.inner.size {
                LookupError::OutOfRange
            } else {
                self.missing(known)
            }
            .into());
        }
        if utf16 == 0 {
            return Ok(0);
        }
        let (before, end) = self
            .find_block(|sum| sum.units < utf16)
            .ok_or_else(|| self.inner.mismatch())?;
        let bytes = self.inner.cached_range(before.bytes, end)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| self.inner.mismatch())?;
        let mut units = before.units;
        for (at, c) in text.char_indices() {
            if units == utf16 {
                return Ok(before.bytes + at);
            }
            units += c.len_utf16();
            if units > utf16 {
                return Err(LookupError::OutOfRange.into());
            }
        }
        if units == utf16 {
            Ok(end)
        } else {
            Err(self.inner.mismatch().into())
        }
    }
    fn position_of(&self, byte: usize) -> Query<TextPosition> {
        let units = self.prefix(byte)?.units;
        let line = self.line_of(byte)?;
        let start = self.line_start(line)?;
        Ok(TextPosition {
            line: LineIndex(line),
            column: Utf16Offset(units - self.prefix(start)?.units),
        })
    }
    /// Needs only the part of the line up to the column, so it answers on a line whose
    /// end is still unknown.
    fn byte_of_position(&self, pos: TextPosition) -> Query<usize> {
        let line = self.known_line(pos.line.0)?;
        let units = self
            .prefix(line.range.start.0)?
            .units
            .checked_add(pos.column.0)
            .ok_or(LookupError::OutOfRange)?;
        if units > self.prefix(line.range.end.0)?.units {
            return Err(if line.complete {
                LookupError::OutOfRange
            } else {
                self.missing(self.known())
            }
            .into());
        }
        self.byte_of_unit(units)
    }
    /// The checks that every chunk read shares. A range must stay inside a snapshot's
    /// pinned part, so that the snapshot gives the same answer later as it does now.
    fn check_visit(&self, start: usize, end: usize) -> Result<()> {
        if start > end || end > self.inner.size {
            return Err(LookupError::OutOfRange);
        }
        if let Some(failure) = self.inner.current_failure()
            && (failure.valid_up_to.0 == 0 || end > failure.valid_up_to.0)
        {
            return Err(LookupError::Failed(failure));
        }
        if let Some(pin) = self.pin
            && end > pin.total.bytes
        {
            return Err(LookupError::NotYetKnown {
                indexed_up_to: ByteOffset(pin.total.bytes),
            });
        }
        Ok(())
    }
    /// Gives a range from memory in one callback, or nothing at all.
    fn visit_cached(&self, start: usize, end: usize, f: &mut dyn FnMut(&str)) -> Query<()> {
        self.check_visit(start, end)?;
        self.inner.check_if_due();
        let reach = (end + 1).min(self.inner.size);
        if reach.div_ceil(PAGE) - start / PAGE > TRY_VISIT_PAGES {
            return Err(LookupError::TooLong {
                max_bytes: (TRY_VISIT_PAGES - 1) * PAGE,
            }
            .into());
        }
        let bytes = self.inner.cached_range(start, reach)?;
        let edge = |b: Option<&u8>| b.is_none_or(|b| b & 0xC0 != 0x80);
        if !edge(bytes.first()) || !edge(bytes.get(end - start)) {
            return Err(LookupError::OutOfRange.into());
        }
        match std::str::from_utf8(&bytes[..end - start]) {
            Ok(text) => {
                if !text.is_empty() {
                    f(text);
                }
                Ok(())
            }
            Err(e) => {
                let at = start + e.valid_up_to();
                Err(
                    LookupError::Failed(self.inner.fail(format!("invalid UTF-8 at byte {at}"), at))
                        .into(),
                )
            }
        }
    }
    /// Waits for each page in turn. For plain queries only. A page in a long read
    /// leaves the cache as later pages arrive, so this holds only one chunk at a time.
    fn fetch(&self, start: usize, end: usize) -> Result<Vec<u8>> {
        self.wait_for(|| self.inner.cached_range(start, end))
    }
    /// The plain chunk read. `f` returns false to stop.
    fn visit_waiting(
        &self,
        range: Range<ByteOffset>,
        f: &mut dyn FnMut(&str) -> bool,
    ) -> Result<()> {
        let (start, end) = (range.start.0, range.end.0);
        self.check_visit(start, end)?;
        self.inner.check_and_wait()?;
        let is_edge = |at: usize| -> Result<bool> {
            if at == 0 || at == self.inner.size {
                return Ok(true);
            }
            self.inner.forget_pages(at, at + 1);
            Ok(self.fetch(at, at + 1)?[0] & 0xC0 != 0x80)
        };
        if !is_edge(start)? || !is_edge(end)? {
            return Err(LookupError::OutOfRange);
        }
        let mut at = start;
        while at < end {
            let want = (end - at).min(PAGE);
            // Text that leaves through this route is always fresh from the file.
            self.inner.forget_pages(at, at + want);
            let bytes = self.fetch(at, at + want)?;
            // A chunk that stops short of `end` ends on a character edge, so it is
            // validated once. Only a bad byte costs a second pass.
            let cut = if at + want < end {
                whole_lines_edge(&bytes)
            } else {
                want
            };
            let going = match std::str::from_utf8(&bytes[..cut]) {
                Ok(text) => {
                    at += cut;
                    f(text)
                }
                Err(e) if e.valid_up_to() > 0 => {
                    let text = std::str::from_utf8(&bytes[..e.valid_up_to()])
                        .map_err(|_| self.inner.mismatch())?;
                    at += e.valid_up_to();
                    f(text)
                }
                Err(_) => {
                    return Err(LookupError::Failed(
                        self.inner.fail(format!("invalid UTF-8 at byte {at}"), at),
                    ));
                }
            };
            if !going {
                return Ok(());
            }
        }
        // Text may have gone out already; this makes the caller drop it.
        self.inner.check_and_wait()
    }
    /// Runs a query until it stops needing the disk or the worker. A snapshot waits
    /// for pages, and never for the index. The wait does no work on this thread and
    /// ends on any failure. A page held back by another app's lock waits here too,
    /// until the lock passes or the store gives up on it.
    fn wait_for<T>(&self, query: impl Fn() -> Query<T>) -> Result<T> {
        loop {
            let seen = *self.inner.progress.lock().unwrap();
            let result = query();
            let waits = match &result {
                Err(Stop::Page) => true,
                Err(Stop::Error(LookupError::NotYetKnown { .. })) => self.pin.is_none(),
                _ => false,
            };
            match result {
                Ok(value) => return Ok(value),
                Err(Stop::Error(e)) if !waits => return Err(e),
                Err(_) => {}
            }
            // A page that failed to load never arrives, so a failure ends the wait.
            if let Some(failure) = self.inner.current_failure()
                && failure.valid_up_to.0 == 0
            {
                return Err(LookupError::Failed(failure));
            }
            let guard = self.inner.progress.lock().unwrap();
            if *guard == seen {
                drop(self.inner.wake.wait_timeout(guard, LOCK_RETRY).unwrap());
            }
        }
    }
    /// The route of every plain query: a file check first if one is due, then the wait.
    fn answer<T>(&self, query: impl Fn() -> Query<T>) -> Option<T> {
        if self.inner.check_is_due() && self.inner.check_and_wait().is_err() {
            return None;
        }
        self.wait_for(query).ok()
    }
    /// The route of every `try_` query: memory only, and a miss is "not yet".
    fn now<T>(&self, query: Query<T>) -> Result<T> {
        query.map_err(|stop| match stop {
            Stop::Error(e) => e,
            Stop::Page => LookupError::NotYetKnown {
                indexed_up_to: ByteOffset(self.known().bytes),
            },
        })
    }
    fn whole_total<T>(&self, value: impl Fn(Sum) -> T) -> Result<T> {
        let known = self.known();
        self.inner.gate(known.bytes)?;
        if let Some(failure) = self.inner.current_failure() {
            return Err(LookupError::Failed(failure));
        }
        if known.bytes < self.inner.size {
            return Err(self.missing(known));
        }
        Ok(value(known))
    }
}

impl TextSnapshot for MappedStore {
    /// A live store moves to a new revision each time its index grows. The index and
    /// its revision change together, so one revision always means one state.
    fn revision(&self) -> u64 {
        self.pin.map_or_else(
            || self.inner.index.read().unwrap().revision,
            |pin| pin.revision,
        )
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.inner.size)
    }
    /// The UTF-16 units known so far. Never blocks. `try_len_utf16` says when this
    /// is not the whole answer.
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.known().units)
    }
    /// The lines known so far. Never blocks. `try_line_count` says when this is not
    /// the whole answer.
    fn line_count(&self) -> usize {
        self.known().breaks + 1
    }
    /// Waits for disk and for the index. Must not run on the UI thread.
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.answer(|| Ok(Utf16Offset(self.prefix(byte.0)?.units)))
    }
    /// Waits for disk and for the index. Must not run on the UI thread.
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        self.answer(|| Ok(ByteOffset(self.byte_of_unit(utf16.0)?)))
    }
    /// Waits for disk and for the index. Must not run on the UI thread.
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.answer(|| self.position_of(byte.0))
    }
    /// Waits for disk and for the index. Must not run on the UI thread.
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        self.answer(|| Ok(ByteOffset(self.byte_of_position(pos)?)))
    }
    /// Waits for disk and for the end of the line. Must not run on the UI thread.
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        self.answer(|| self.range_of_line(line.0))
    }
    /// Waits for disk. Must not run on the UI thread. Gives no callback for a bad
    /// range, the same as `PieceStore`. If the file fails part way, text may already
    /// have gone out and the result is false.
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        self.visit_waiting(range, &mut |s| {
            f(s);
            true
        })
        .is_ok()
    }
    /// Waits for disk. Must not run on the UI thread.
    fn visit_chunks_while(
        &self,
        range: Range<ByteOffset>,
        f: &mut dyn FnMut(&str) -> bool,
    ) -> bool {
        self.visit_waiting(range, f).is_ok()
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        let known = self.known();
        StoreStatus {
            known_bytes: ByteOffset(known.bytes),
            known_utf16: Utf16Offset(known.units),
            known_lines: known.breaks + 1,
            complete: known.bytes == self.inner.size,
            failure: self.inner.current_failure(),
        }
    }
    fn try_known_line(&self, line: LineIndex) -> Result<KnownLine> {
        self.now(self.known_line(line.0))
    }
    fn try_len_bytes(&self) -> Result<ByteOffset> {
        self.inner.gate(0)?;
        Ok(ByteOffset(self.inner.size))
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset> {
        self.whole_total(|sum| Utf16Offset(sum.units))
    }
    fn try_line_count(&self) -> Result<usize> {
        self.whole_total(|sum| sum.breaks + 1)
    }
    fn try_byte_to_utf16(&self, byte: ByteOffset) -> Result<Utf16Offset> {
        self.now(self.prefix(byte.0).map(|sum| Utf16Offset(sum.units)))
    }
    fn try_utf16_to_byte(&self, utf16: Utf16Offset) -> Result<ByteOffset> {
        self.now(self.byte_of_unit(utf16.0).map(ByteOffset))
    }
    fn try_byte_to_position(&self, byte: ByteOffset) -> Result<TextPosition> {
        self.now(self.position_of(byte.0))
    }
    fn try_position_to_byte(&self, pos: TextPosition) -> Result<ByteOffset> {
        self.now(self.byte_of_position(pos).map(ByteOffset))
    }
    fn try_line_range(&self, line: LineIndex) -> Result<Range<ByteOffset>> {
        self.now(self.range_of_line(line.0))
    }
    /// Gives the whole range in one callback from memory, or no callback at all. A
    /// page that is not in memory gives `NotYetKnown` and starts a background read. A
    /// range over 448 KiB gives `TooLong`, so read those off the UI thread with
    /// `visit_chunks`.
    fn try_visit_chunks(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str),
    ) -> Result<()> {
        self.now(self.visit_cached(range.start.0, range.end.0, visitor))
    }
}

impl TextStore for MappedStore {
    /// A stable view: its line count, totals, lookups and chunk reads stay as they
    /// were at this moment. A lookup past that point gives `NotYetKnown` from a `try_`
    /// query and `None` from a plain one, so take a new snapshot to see more. A
    /// snapshot holds the file open but does not keep the worker running. A file
    /// failure still reaches it, because its bytes come from the same file.
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        let index = self.inner.index.read().unwrap();
        let pin = self.pin.unwrap_or(Pin {
            total: index.total,
            blocks: index.blocks.len(),
            revision: index.revision,
        });
        drop(index);
        Arc::new(Self {
            inner: self.inner.clone(),
            _worker_stop: None,
            _reader_stop: self._reader_stop.clone(),
            pin: Some(pin),
        })
    }
    fn replace(
        &mut self,
        _: Range<ByteOffset>,
        _: &str,
    ) -> std::result::Result<TextChange, EditError> {
        Err(EditError::ReadOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_short_read_loops_until_the_buffer_is_full() {
        let source = b"abcdefghij";
        let mut calls = 0;
        let mut buf = [0; 10];
        read_full(
            |b, o| {
                calls += 1;
                b[0] = source[o as usize];
                Ok(1)
            },
            &mut buf,
            0,
        )
        .unwrap();
        assert_eq!(&buf, source);
        assert_eq!(calls, 10);
    }

    #[test]
    fn an_interrupted_read_retries_and_the_end_of_file_is_an_error() {
        let mut first = true;
        let mut buf = [0; 2];
        read_full(
            |b, _| {
                if first {
                    first = false;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                b.fill(7);
                Ok(b.len())
            },
            &mut buf,
            0,
        )
        .unwrap();
        assert_eq!(buf, [7, 7]);
        let error = read_full(|_, _| Ok(0), &mut buf, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_refused_thread_is_an_error_and_not_a_panic() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let refuse: Spawn = |_, _| Err(io::Error::other("no threads left"));
        let error = match MappedStore::open_inner(&path, None, refuse) {
            Ok(_) => panic!("open worked with no thread"),
            Err(e) => e,
        };
        assert_eq!(error.to_string(), "no threads left");
    }

    #[test]
    fn a_refused_second_thread_stops_the_first_and_closes_the_file() {
        static STARTED: AtomicUsize = AtomicUsize::new(0);
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("qe-unit-{}-refuse.txt", std::process::id()));
        std::fs::write(&path, "text").unwrap();
        let second_refused: Spawn = |name, work| {
            if STARTED.fetch_add(1, Ordering::SeqCst) == 0 {
                spawn_thread(name, work)
            } else {
                Err(io::Error::other("no threads left"))
            }
        };
        assert!(MappedStore::open_inner(&path, None, second_refused).is_err());
        let start = Instant::now();
        while OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .is_err()
        {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the file stayed open"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn lock_and_sharing_errors_are_the_try_again_kind() {
        assert!(is_lock_error(&io::Error::from_raw_os_error(32)));
        assert!(is_lock_error(&io::Error::from_raw_os_error(33)));
        assert!(!is_lock_error(&io::Error::from_raw_os_error(2)));
        assert!(!is_lock_error(&io::Error::other("no code")));
    }

    #[test]
    fn a_scan_edge_never_cuts_a_character_or_a_crlf() {
        assert_eq!(whole_lines_edge(b"abcd"), 4);
        assert_eq!(whole_lines_edge(b"abc\r"), 3);
        assert_eq!(whole_lines_edge(b"abc\n"), 4);
        // Whole three-byte character at the end: it moves to the next scan.
        assert_eq!(whole_lines_edge("ab語".as_bytes()), 2);
        // Each way of cutting a three-byte character.
        assert_eq!(whole_lines_edge(&"ab語".as_bytes()[..3]), 2);
        assert_eq!(whole_lines_edge(&"ab語".as_bytes()[..4]), 2);
    }

    #[test]
    fn blocks_end_near_page_edges_on_character_edges_and_never_inside_crlf() {
        let mut text = "語".repeat(PAGE / 3 * 8);
        text.insert_str(PAGE - 1, "\r\n");
        let blocks = measure_blocks(&text, 0, Sum::default());
        let mut start = 0;
        for block in &blocks {
            let end = block.cumulative.bytes;
            assert!(text.is_char_boundary(end), "cut a character at {end}");
            assert!(end - start <= PAGE + 4);
            assert!(
                !(end < text.len() && text[..end].ends_with('\r') && text[end..].starts_with('\n'))
            );
            start = end;
        }
        assert_eq!(start, text.len());
        assert_eq!(blocks.last().unwrap().cumulative, measure(&text));
        assert!(
            blocks.len() <= text.len() / PAGE + 2,
            "{} blocks for {} bytes",
            blocks.len(),
            text.len()
        );
    }

    #[test]
    fn break_ends_finds_each_kind_of_break() {
        let text = "a\r\nb\rc\nd\u{85}e\u{2028}f\u{2029}g";
        assert_eq!(break_ends(text.as_bytes()), [3, 5, 7, 10, 14, 18]);
    }
}
