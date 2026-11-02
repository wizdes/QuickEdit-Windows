//! The read-only store for very large files. `README.md` has the rules.
use crate::{Sum, measure};
use qe_api::{
    ByteOffset, EditError, LineIndex, LookupError, StoreFailure, StoreStatus, TextChange,
    TextPosition, TextSnapshot, TextStore, Utf16Offset,
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
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx,
};

const PAGE: usize = 64 * 1024;
const SCAN_BLOCK: usize = 4 * 1024 * 1024;
const CACHED_PAGES: usize = 4;
/// Lookups check the file at most this often. See `check_if_due`.
const CHECK_INTERVAL: Duration = Duration::from_millis(25);
const SHARE_ALL: u32 = 1 | 2 | 4;
const FILE_READ_ATTRIBUTES: u32 = 0x80;

type Result<T> = std::result::Result<T, LookupError>;
/// Called from any thread after the index grows or the store fails. It must only
/// post a message and return.
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
struct Block {
    cumulative: Sum,
}

struct Index {
    blocks: Vec<Block>,
    total: Sum,
}

/// Time the worker spent on each step, in nanoseconds. Measured, for tuning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexTiming {
    pub read: u64,
    pub validate: u64,
    pub measure: u64,
    pub check: u64,
}

#[derive(Default)]
struct Timers {
    read: AtomicU64,
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

struct Inner {
    path: PathBuf,
    file: File,
    original: FileStamp,
    size: usize,
    index: RwLock<Index>,
    indexed: AtomicUsize,
    revision: AtomicU64,
    failed: AtomicBool,
    failure: Mutex<Option<StoreFailure>>,
    pages: Mutex<VecDeque<(usize, Arc<[u8]>)>>,
    progress: Mutex<()>,
    wake: Condvar,
    stop: AtomicBool,
    born: Instant,
    last_check_us: AtomicU64,
    disk_reads: AtomicU64,
    file_checks: AtomicU64,
    timers: Timers,
    notify: Option<Notify>,
}

impl Inner {
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
    fn signal(&self) {
        drop(self.progress.lock().unwrap());
        self.wake.notify_all();
        if let Some(notify) = &self.notify {
            notify();
        }
    }
    fn changed_on_disk(&self) -> io::Result<()> {
        self.file_checks.fetch_add(1, Ordering::Relaxed);
        let at_path = stamp_at_path(&self.path)?;
        let held = file_stamp(&self.file)?;
        if at_path != self.original || held != self.original {
            return Err(io::Error::other("the file changed outside QuickEdit"));
        }
        Ok(())
    }
    fn check_now(&self) -> Result<()> {
        self.last_check_us
            .store(self.born.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.changed_on_disk()
            .map_err(|e| LookupError::Failed(self.fail(format!("mapped file changed: {e}"), 0)))
    }
    /// Bounds the cost of the file check. A check opens the path and asks twice
    /// about the file, so a lookup runs it only when `CHECK_INTERVAL` has passed
    /// since the last one. A lookup can therefore answer from the old file for up
    /// to that long. `visit_chunks` does not wait: it always checks.
    fn check_if_due(&self) -> Result<()> {
        let now = self.born.elapsed().as_micros() as u64;
        let last = self.last_check_us.load(Ordering::Relaxed);
        if now.saturating_sub(last) < CHECK_INTERVAL.as_micros() as u64 {
            return Ok(());
        }
        if self
            .last_check_us
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return Ok(());
        }
        self.check_now()
    }
    /// The failure gate that every query passes. It fails when no part is safe or
    /// when the query needs bytes at or past `valid_up_to`.
    fn gate(&self, upto: usize) -> Result<()> {
        if let Some(failure) = self.current_failure()
            && (failure.valid_up_to.0 == 0 || upto > failure.valid_up_to.0)
        {
            return Err(LookupError::Failed(failure));
        }
        self.check_if_due()
    }
    /// Reads one page without holding a lock across the disk read, so a lookup on
    /// another thread never waits for this one's disk read.
    fn page(&self, page_at: usize) -> Result<Arc<[u8]>> {
        {
            let pages = self.pages.lock().unwrap();
            if let Some((_, bytes)) = pages.iter().find(|(start, _)| *start == page_at) {
                return Ok(bytes.clone());
            }
        }
        self.disk_reads.fetch_add(1, Ordering::Relaxed);
        let mut bytes = vec![0; PAGE.min(self.size - page_at)];
        read_full(|b, o| self.file.seek_read(b, o), &mut bytes, page_at as u64).map_err(|e| {
            LookupError::Failed(self.fail(format!("mapped file read failed: {e}"), 0))
        })?;
        // The check comes after the read, so it also covers this read's bytes.
        self.check_if_due()?;
        let bytes: Arc<[u8]> = Arc::from(bytes);
        let mut pages = self.pages.lock().unwrap();
        if !pages.iter().any(|(start, _)| *start == page_at) {
            if pages.len() == CACHED_PAGES {
                pages.pop_front();
            }
            pages.push_back((page_at, bytes.clone()));
        }
        Ok(bytes)
    }
    fn read_range(&self, start: usize, end: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(end - start);
        let mut at = start;
        while at < end {
            let page_at = at / PAGE * PAGE;
            let page = self.page(page_at)?;
            let from = at - page_at;
            let take = (end - at).min(page.len() - from);
            out.extend_from_slice(&page[from..from + take]);
            at += take;
        }
        Ok(out)
    }
    fn mismatch(&self) -> LookupError {
        LookupError::Failed(self.fail("the index does not match the file", 0))
    }
    /// Reads and indexes the next part of the file on the caller's thread: at open
    /// with `PAGE` and on the worker with `SCAN_BLOCK`. No lock is held during the
    /// read or the scan; the write lock covers only adding the finished blocks.
    fn index_next(&self, file: &File, buffer: &mut Vec<u8>, max_bytes: usize) -> bool {
        let start = self.indexed.load(Ordering::Acquire);
        if start >= self.size {
            return true;
        }
        let want = (self.size - start).min(max_bytes);
        buffer.resize(want, 0);
        let bytes = &mut buffer[..want];
        let read = timed(&self.timers.read, || {
            read_full(|b, o| file.seek_read(b, o), bytes, start as u64)
        });
        if let Err(e) = read {
            self.fail(format!("mapped file read failed: {e}"), 0);
            return false;
        }
        if timed(&self.timers.check, || self.check_now()).is_err() {
            return false;
        }
        let bytes = &buffer[..want];
        let at_eof = start + want == self.size;
        let keep = if at_eof {
            want
        } else {
            whole_lines_edge(bytes)
        };
        let (text, bad_at) = match timed(&self.timers.validate, || {
            std::str::from_utf8(&bytes[..keep])
        }) {
            Ok(text) => (text, None),
            Err(e) => {
                let valid = e.valid_up_to();
                match std::str::from_utf8(&bytes[..valid]) {
                    Ok(text) => (text, Some(start + valid)),
                    Err(_) => ("", Some(start)),
                }
            }
        };
        let blocks = timed(&self.timers.measure, || {
            let previous = self.index.read().unwrap().total;
            measure_blocks(text, start, previous)
        });
        if let Some(last) = blocks.last() {
            let total = last.cumulative;
            {
                let mut index = self.index.write().unwrap();
                index.blocks.extend(blocks);
                index.total = total;
            }
            self.indexed.store(total.bytes, Ordering::Release);
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(at) = bad_at {
            self.fail(format!("invalid UTF-8 at byte {at}"), at);
            return false;
        }
        self.signal();
        true
    }
    fn worker(self: Arc<Self>, file: File) {
        struct OnPanic(Arc<Inner>);
        impl Drop for OnPanic {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    self.0.fail("the mapped file worker stopped", 0);
                }
            }
        }
        let guard = OnPanic(self);
        let inner = &guard.0;
        let mut buffer = Vec::with_capacity(SCAN_BLOCK);
        while inner.indexed.load(Ordering::Acquire) < inner.size
            && !inner.stop.load(Ordering::Acquire)
            && !inner.failed.load(Ordering::Acquire)
        {
            if !inner.index_next(&file, &mut buffer, SCAN_BLOCK) {
                break;
            }
        }
    }
}

/// Where to end a scan that is not at the end of the file, so that no character
/// and no CRLF is cut. `bytes` holds at least `PAGE` bytes.
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
        let page_edge = ((file_start + start) / PAGE + 1) * PAGE - file_start;
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

/// Finds the end of the `rank`th line break in a block, counting from 1.
fn nth_break_end(bytes: &[u8], mut rank: usize) -> Option<usize> {
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
        rank -= 1;
        if rank == 0 {
            return Some(end);
        }
        at = end;
    }
    None
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

/// A read-only UTF-8 file, indexed by a worker thread after the first 64 KiB.
///
/// Use the `try_` queries from the UI thread: they never block. The plain queries
/// that return `Option` block on a live store until the worker reaches the answer.
/// `TextStore::snapshot` gives a view that never moves and never blocks. See
/// `README.md`.
#[derive(Clone)]
pub struct MappedStore {
    inner: Arc<Inner>,
    /// Only a live store keeps the worker running.
    _worker_stop: Option<Arc<WorkerStop>>,
    pin: Option<Pin>,
}

type Spawn = fn(Box<dyn FnOnce() + Send>) -> io::Result<()>;

fn spawn_thread(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("qe-buffer-index".into())
        .spawn(work)
        .map(|_| ())
}

impl MappedStore {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_inner(path.as_ref(), None, spawn_thread)
    }
    /// Like `open`. `notify` runs after the index grows and when the store fails,
    /// on whichever thread saw it, so a window can post itself a message. This is
    /// how a worker failure reaches a UI that is not asking at that moment.
    pub fn open_with_notify(path: impl AsRef<Path>, notify: Notify) -> io::Result<Self> {
        Self::open_inner(path.as_ref(), Some(notify), spawn_thread)
    }
    fn open_inner(path: &Path, notify: Option<Notify>, spawn: Spawn) -> io::Result<Self> {
        let path = path.to_path_buf();
        let open_read = || {
            OpenOptions::new()
                .read(true)
                .share_mode(SHARE_ALL)
                .open(&path)
        };
        let file = open_read()?;
        // Reads on one handle wait for each other, so the worker has its own.
        let worker_file = open_read()?;
        let original = file_stamp(&file)?;
        if file_stamp(&worker_file)? != original {
            return Err(io::Error::other("the file changed during open"));
        }
        let size =
            usize::try_from(original.len).map_err(|_| io::Error::other("file is too large"))?;
        if size == 0 {
            return Err(io::Error::other("cannot map an empty file"));
        }
        let inner = Arc::new(Inner {
            path,
            file,
            original,
            size,
            index: RwLock::new(Index {
                // Growing this Vec under the write lock would copy it, so room for
                // the whole file is set aside now, up to 48 MiB for 64 GiB.
                blocks: Vec::with_capacity((size / PAGE + 2).min(1 << 20)),
                total: Sum::default(),
            }),
            indexed: AtomicUsize::new(0),
            revision: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            failure: Mutex::new(None),
            pages: Mutex::new(VecDeque::new()),
            progress: Mutex::new(()),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            born: Instant::now(),
            last_check_us: AtomicU64::new(0),
            disk_reads: AtomicU64::new(0),
            file_checks: AtomicU64::new(0),
            timers: Timers::default(),
            notify,
        });
        let mut buffer = Vec::with_capacity(PAGE);
        inner.index_next(&worker_file, &mut buffer, PAGE);
        // A bad byte in the first page means this is not UTF-8 text to show as is.
        if let Some(failure) = inner.current_failure() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, failure.message));
        }
        drop(buffer);
        let worker = inner.clone();
        spawn(Box::new(move || worker.worker(worker_file)))?;
        Ok(Self {
            _worker_stop: Some(Arc::new(WorkerStop(inner.clone()))),
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
    /// Checks the file at once instead of at the next due time.
    pub fn check_file(&self) -> std::result::Result<(), LookupError> {
        self.inner.check_now()
    }
    /// Bytes reserved for sparse checkpoints, excluding the bounded scan buffer.
    pub fn index_memory_bytes(&self) -> usize {
        self.inner.index.read().unwrap().blocks.capacity() * size_of::<Block>()
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
            validate: t.validate.load(Ordering::Relaxed),
            measure: t.measure.load(Ordering::Relaxed),
            check: t.check.load(Ordering::Relaxed),
        }
    }
    /// Pages read from the file by lookups so far.
    pub fn lookup_disk_reads(&self) -> u64 {
        self.inner.disk_reads.load(Ordering::Relaxed)
    }
    /// File checks run so far, by lookups and by the worker.
    pub fn file_checks(&self) -> u64 {
        self.inner.file_checks.load(Ordering::Relaxed)
    }

    fn known(&self) -> Sum {
        match self.pin {
            Some(pin) => pin.total,
            None => self.inner.index.read().unwrap().total,
        }
    }
    fn missing(&self, known: Sum) -> LookupError {
        match self.inner.current_failure() {
            Some(failure) => LookupError::Failed(failure),
            None => LookupError::NotYetKnown {
                indexed_up_to: ByteOffset(known.bytes),
            },
        }
    }
    /// Finds a block with binary search on the checkpoints, then lets go of the
    /// lock before any file read. Gives the totals before it and its end.
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
    fn text_in<'a>(&self, bytes: &'a [u8]) -> Result<&'a str> {
        std::str::from_utf8(bytes).map_err(|_| self.inner.mismatch())
    }
    /// Totals for the text before `at`.
    fn prefix(&self, at: usize) -> Result<Sum> {
        if at > self.inner.size {
            return Err(LookupError::OutOfRange);
        }
        self.inner.gate(at)?;
        if at == 0 {
            return Ok(Sum::default());
        }
        let known = self.known();
        if at > known.bytes {
            return Err(self.missing(known));
        }
        let (before, _) = self
            .find_block(|sum| sum.bytes < at)
            .ok_or_else(|| self.inner.mismatch())?;
        let bytes = self.inner.read_range(before.bytes, at)?;
        match std::str::from_utf8(&bytes) {
            Ok(text) => Ok(before.add(measure(text))),
            Err(e) if e.error_len().is_none() => Err(LookupError::OutOfRange),
            Err(_) => Err(self.inner.mismatch()),
        }
    }
    /// The line that holds byte `at`.
    fn line_of(&self, at: usize) -> Result<usize> {
        let sum = self.prefix(at)?;
        if !sum.last_cr || at == self.inner.size {
            return Ok(sum.breaks);
        }
        let next = self.inner.read_range(at, at + 1)?;
        Ok(sum.breaks - usize::from(next[0] == b'\n'))
    }
    fn line_start(&self, line: usize) -> Result<usize> {
        let known = self.known();
        self.inner.gate(known.bytes)?;
        if line == 0 {
            return Ok(0);
        }
        if line > known.breaks {
            return if known.bytes == self.inner.size {
                Err(LookupError::OutOfRange)
            } else {
                Err(self.missing(known))
            };
        }
        let (before, end) = self
            .find_block(|sum| sum.breaks < line)
            .ok_or_else(|| self.inner.mismatch())?;
        let bytes = self.inner.read_range(before.bytes, end)?;
        nth_break_end(&bytes, line - before.breaks)
            .map(|offset| before.bytes + offset)
            .ok_or_else(|| self.inner.mismatch())
    }
    fn range_of_line(&self, line: usize) -> Result<Range<ByteOffset>> {
        let start = self.line_start(line)?;
        let end = match self.line_start(line + 1) {
            Ok(end) => end,
            Err(LookupError::OutOfRange) => self.inner.size,
            Err(e) => return Err(e),
        };
        Ok(ByteOffset(start)..ByteOffset(end))
    }
    fn byte_of_unit(&self, utf16: usize) -> Result<usize> {
        let known = self.known();
        self.inner.gate(known.bytes)?;
        if utf16 > known.units {
            return if known.bytes == self.inner.size {
                Err(LookupError::OutOfRange)
            } else {
                Err(self.missing(known))
            };
        }
        if utf16 == 0 {
            return Ok(0);
        }
        let (before, end) = self
            .find_block(|sum| sum.units < utf16)
            .ok_or_else(|| self.inner.mismatch())?;
        let bytes = self.inner.read_range(before.bytes, end)?;
        let text = self.text_in(&bytes)?;
        let mut units = before.units;
        for (at, c) in text.char_indices() {
            if units == utf16 {
                return Ok(before.bytes + at);
            }
            units += c.len_utf16();
            if units > utf16 {
                return Err(LookupError::OutOfRange);
            }
        }
        if units == utf16 {
            Ok(end)
        } else {
            Err(self.inner.mismatch())
        }
    }
    fn position_of(&self, byte: usize) -> Result<TextPosition> {
        let units = self.prefix(byte)?.units;
        let line = self.line_of(byte)?;
        let start = self.line_start(line)?;
        Ok(TextPosition {
            line: LineIndex(line),
            column: Utf16Offset(units - self.prefix(start)?.units),
        })
    }
    fn byte_of_position(&self, pos: TextPosition) -> Result<usize> {
        let range = self.range_of_line(pos.line.0)?;
        let units = self
            .prefix(range.start.0)?
            .units
            .checked_add(pos.column.0)
            .ok_or(LookupError::OutOfRange)?;
        if units > self.prefix(range.end.0)?.units {
            return Err(LookupError::OutOfRange);
        }
        self.byte_of_unit(units)
    }
    fn is_char_edge(&self, at: usize) -> Result<bool> {
        if at == 0 || at == self.inner.size {
            return Ok(true);
        }
        Ok(self.inner.read_range(at, at + 1)?[0] & 0xC0 != 0x80)
    }
    fn visit(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> Result<()> {
        let (start, end) = (range.start.0, range.end.0);
        if start > end || end > self.inner.size {
            return Err(LookupError::OutOfRange);
        }
        if let Some(failure) = self.inner.current_failure()
            && (failure.valid_up_to.0 == 0 || end > failure.valid_up_to.0)
        {
            return Err(LookupError::Failed(failure));
        }
        self.inner.check_now()?;
        if !self.is_char_edge(start)? || !self.is_char_edge(end)? {
            return Err(LookupError::OutOfRange);
        }
        let mut at = start;
        while at < end {
            let want = (end - at).min(PAGE);
            let bytes = self.inner.read_range(at, at + want)?;
            // A chunk that stops short of `end` ends on a character edge, so it is
            // validated once. Only a bad byte costs a second pass.
            let cut = if at + want < end {
                whole_lines_edge(&bytes)
            } else {
                want
            };
            match std::str::from_utf8(&bytes[..cut]) {
                Ok(text) => {
                    f(text);
                    at += cut;
                }
                Err(e) if e.valid_up_to() > 0 => {
                    f(self.text_in(&bytes[..e.valid_up_to()])?);
                    at += e.valid_up_to();
                }
                Err(_) => {
                    return Err(LookupError::Failed(
                        self.inner.fail(format!("invalid UTF-8 at byte {at}"), at),
                    ));
                }
            }
        }
        // Text may have gone out already; this makes the caller drop it.
        self.inner.check_now()
    }
    /// Runs a query. A live store waits for the worker, and a snapshot does not.
    /// The wait does no indexing on this thread and ends on any failure.
    fn answer<T>(&self, query: impl Fn() -> Result<T>) -> Option<T> {
        loop {
            match query() {
                Ok(value) => return Some(value),
                Err(LookupError::NotYetKnown { indexed_up_to }) if self.pin.is_none() => {
                    let guard = self.inner.progress.lock().unwrap();
                    if self.inner.indexed.load(Ordering::Acquire) == indexed_up_to.0
                        && !self.inner.failed.load(Ordering::Acquire)
                    {
                        drop(
                            self.inner
                                .wake
                                .wait_timeout(guard, Duration::from_millis(50))
                                .unwrap(),
                        );
                    }
                }
                Err(_) => return None,
            }
        }
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
    /// A live store moves to a new revision each time its index grows.
    fn revision(&self) -> u64 {
        self.pin.map_or_else(
            || self.inner.revision.load(Ordering::Acquire),
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
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.answer(|| self.try_byte_to_utf16(byte))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        self.answer(|| self.try_utf16_to_byte(utf16))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.answer(|| self.try_byte_to_position(byte))
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        self.answer(|| self.try_position_to_byte(pos))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        self.answer(|| self.try_line_range(line))
    }
    /// Gives no callback for a bad range, the same as `PieceStore`. If the file
    /// fails part way, text may already have gone out and the result is false.
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        self.visit(range, f).is_ok()
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
        Ok(Utf16Offset(self.prefix(byte.0)?.units))
    }
    fn try_utf16_to_byte(&self, utf16: Utf16Offset) -> Result<ByteOffset> {
        self.byte_of_unit(utf16.0).map(ByteOffset)
    }
    fn try_byte_to_position(&self, byte: ByteOffset) -> Result<TextPosition> {
        self.position_of(byte.0)
    }
    fn try_position_to_byte(&self, pos: TextPosition) -> Result<ByteOffset> {
        self.byte_of_position(pos).map(ByteOffset)
    }
    fn try_line_range(&self, line: LineIndex) -> Result<Range<ByteOffset>> {
        self.range_of_line(line.0)
    }
    fn try_visit_chunks(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str),
    ) -> Result<()> {
        self.visit(range, visitor)
    }
}

impl TextStore for MappedStore {
    /// A stable view: its line count, totals and lookups stay as they were at this
    /// moment and never block. A lookup past that point gives `NotYetKnown` from a
    /// `try_` query and `None` from a plain one, so take a new snapshot to see more.
    /// A snapshot holds the file open but does not keep the worker running. A file
    /// failure still reaches it, because its bytes are read from the same file.
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        let index = self.inner.index.read().unwrap();
        let pin = self.pin.unwrap_or(Pin {
            total: index.total,
            blocks: index.blocks.len(),
            revision: self.inner.revision.load(Ordering::Acquire),
        });
        drop(index);
        Arc::new(Self {
            inner: self.inner.clone(),
            _worker_stop: None,
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
        let refuse: Spawn = |_| Err(io::Error::other("no threads left"));
        let error = match MappedStore::open_inner(&path, None, refuse) {
            Ok(_) => panic!("open worked with no thread"),
            Err(e) => e,
        };
        assert_eq!(error.to_string(), "no threads left");
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
        let mut text = "語".repeat(PAGE / 3 * 2);
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
    }

    #[test]
    fn nth_break_finds_each_kind_of_break() {
        let text = "a\r\nb\rc\nd\u{85}e\u{2028}f\u{2029}g";
        let ends: Vec<usize> = (1..=6)
            .map(|rank| nth_break_end(text.as_bytes(), rank).unwrap())
            .collect();
        assert_eq!(ends, [3, 5, 7, 10, 14, 18]);
        assert_eq!(nth_break_end(text.as_bytes(), 7), None);
    }
}
