use super::{Sum, measure};
use qe_api::{
    ByteOffset, EditError, LineIndex, TextChange, TextPosition, TextSnapshot, TextStore,
    Utf16Offset,
};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::windows::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

const PAGE: usize = 64 * 1024;
struct Block {
    end: usize,
    cumulative: Sum,
}
struct Index {
    blocks: Vec<Block>,
    total: Sum,
}
struct Inner {
    path: PathBuf,
    file: File,
    original: Metadata,
    size: usize,
    index: RwLock<Index>,
    indexed: AtomicU64,
    error: Mutex<Option<String>>,
}
impl Inner {
    fn fail(&self, message: impl Into<String>) {
        *self.error.lock().unwrap() = Some(message.into());
    }
    fn check_change(&self) -> io::Result<()> {
        let current = std::fs::metadata(&self.path)?;
        let held = self.file.metadata()?;
        if current.creation_time() != self.original.creation_time()
            || current.len() != self.original.len()
            || held.len() != self.original.len()
            || current.modified()? != self.original.modified()?
        {
            return Err(io::Error::other("file changed outside QuickEdit"));
        }
        Ok(())
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Option<usize> {
        if self.error.lock().unwrap().is_some() {
            return None;
        }
        if let Err(e) = self.check_change() {
            self.fail(format!("mapped file changed: {e}"));
            return None;
        }
        match self.file.seek_read(buf, offset as u64) {
            Ok(n) if n == buf.len() => Some(n),
            Ok(_) => {
                self.fail("mapped file shrank during read");
                None
            }
            Err(e) => {
                self.fail(format!("mapped file read failed: {e}"));
                None
            }
        }
    }
    fn index_one(&self) -> Option<()> {
        let start = self.indexed.load(Ordering::Acquire) as usize;
        if start >= self.size {
            return Some(());
        }
        let want = (self.size - start).min(PAGE);
        let mut bytes = vec![0; want + 3.min(self.size - start - want)];
        self.read(start, &mut bytes)?;
        let mut end = want;
        while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
            end -= 1;
        }
        if end == 0 {
            self.fail("invalid UTF-8 in mapped file");
            return None;
        }
        let Ok(text) = std::str::from_utf8(&bytes[..end]) else {
            self.fail("invalid UTF-8 in mapped file");
            return None;
        };
        let part = measure(text);
        let mut index = self.index.write().unwrap();
        if index.total.bytes == start {
            index.total = index.total.add(part);
            let total = index.total;
            index.blocks.push(Block {
                end: start + end,
                cumulative: total,
            });
            self.indexed.store((start + end) as u64, Ordering::Release);
        }
        Some(())
    }
    fn ensure(&self, at: usize) -> Option<()> {
        while (self.indexed.load(Ordering::Acquire) as usize) < at {
            self.index_one()?;
        }
        Some(())
    }
    fn prefix(&self, at: usize) -> Option<Sum> {
        if at > self.size {
            return None;
        }
        if at == 0 {
            return Some(Sum::default());
        }
        self.ensure(at)?;
        let index = self.index.read().unwrap();
        let i = index.blocks.partition_point(|b| b.end < at);
        let before = if i == 0 {
            Sum::default()
        } else {
            index.blocks[i - 1].cumulative
        };
        let start = before.bytes;
        let mut bytes = vec![0; at - start];
        self.read(start, &mut bytes)?;
        Some(before.add(measure(std::str::from_utf8(&bytes).ok()?)))
    }
    fn break_count(&self, at: usize) -> Option<usize> {
        let sum = self.prefix(at)?;
        if !sum.last_cr || at == self.size {
            return Some(sum.breaks);
        }
        let mut next = [0];
        self.read(at, &mut next)?;
        Some(sum.breaks - usize::from(next[0] == b'\n'))
    }
    fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return Some(0);
        }
        loop {
            let indexed = self.indexed.load(Ordering::Acquire) as usize;
            let breaks = self.index.read().unwrap().total.breaks;
            if breaks >= line || indexed == self.size {
                if indexed < self.size && self.break_count(indexed)? < line {
                    self.index_one()?;
                    continue;
                }
                break;
            }
            self.index_one()?;
        }
        let indexed = self.indexed.load(Ordering::Acquire) as usize;
        if line > self.index.read().unwrap().total.breaks {
            return None;
        }
        let mut lo = 0;
        let mut hi = indexed;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mut at = mid;
            while at > 0 && self.prefix(at).is_none() {
                at -= 1;
            }
            if self.break_count(at)? < line {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Some(lo)
    }
}

/// A read-only UTF-8 file. `line_count` reports indexed lines so far; use
/// `indexed_up_to` to see the known byte prefix. Other lookups may index ahead.
/// Reads use ReadFile through FileExt. A live Windows mapping prevents another
/// handle from truncating the file, so this store holds no mapping object.
#[derive(Clone)]
pub struct MappedStore {
    inner: Arc<Inner>,
}
impl MappedStore {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .share_mode(1 | 2 | 4)
            .open(&path)?;
        let original = file.metadata()?;
        let size =
            usize::try_from(original.len()).map_err(|_| io::Error::other("file is too large"))?;
        if size == 0 {
            return Err(io::Error::other("cannot map an empty file"));
        }
        let inner = Arc::new(Inner {
            path,
            file,
            original,
            size,
            index: RwLock::new(Index {
                blocks: Vec::new(),
                total: Sum::default(),
            }),
            indexed: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let worker = inner.clone();
        std::thread::spawn(move || {
            while worker.indexed.load(Ordering::Acquire) < worker.size as u64 {
                if worker.index_one().is_none() {
                    break;
                }
            }
        });
        Ok(Self { inner })
    }
    pub fn indexed_up_to(&self) -> ByteOffset {
        ByteOffset(self.inner.indexed.load(Ordering::Acquire) as usize)
    }
    pub fn error_state(&self) -> Option<String> {
        self.inner.error.lock().unwrap().clone()
    }
    pub fn is_index_complete(&self) -> bool {
        self.indexed_up_to().0 == self.inner.size
    }
}
impl TextSnapshot for MappedStore {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.inner.size)
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.inner
            .prefix(self.inner.size)
            .map_or(Utf16Offset(0), |s| Utf16Offset(s.units))
    }
    fn line_count(&self) -> usize {
        self.inner.index.read().unwrap().total.breaks + 1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        Some(Utf16Offset(self.inner.prefix(byte.0)?.units))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        self.inner.ensure(self.inner.size)?;
        let index = self.inner.index.read().unwrap();
        if utf16.0 > index.total.units {
            return None;
        }
        let i = index
            .blocks
            .partition_point(|b| b.cumulative.units < utf16.0);
        let before = if i == 0 {
            Sum::default()
        } else {
            index.blocks[i - 1].cumulative
        };
        let end = index.blocks.get(i).map_or(self.inner.size, |b| b.end);
        drop(index);
        let mut bytes = vec![0; end - before.bytes];
        self.inner.read(before.bytes, &mut bytes)?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let mut units = before.units;
        for (at, c) in text.char_indices() {
            if units == utf16.0 {
                return Some(ByteOffset(before.bytes + at));
            }
            units += c.len_utf16();
            if units > utf16.0 {
                return None;
            }
        }
        (units == utf16.0).then_some(ByteOffset(end))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        let units = self.byte_to_utf16(byte)?.0;
        let line = self.inner.break_count(byte.0)?;
        let start = self.inner.line_start(line)?;
        Some(TextPosition {
            line: LineIndex(line),
            column: Utf16Offset(units - self.byte_to_utf16(ByteOffset(start))?.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        let range = self.line_range(pos.line)?;
        let units = self
            .byte_to_utf16(range.start)?
            .0
            .checked_add(pos.column.0)?;
        let at = self.utf16_to_byte(Utf16Offset(units))?;
        (at <= range.end).then_some(at)
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        let start = self.inner.line_start(line.0)?;
        let end = self.inner.line_start(line.0 + 1).unwrap_or(self.inner.size);
        Some(ByteOffset(start)..ByteOffset(end))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        if range.start > range.end || range.end.0 > self.inner.size {
            return false;
        }
        let mut at = range.start.0;
        while at < range.end.0 {
            let want = (range.end.0 - at).min(PAGE);
            let mut bytes = vec![0; want];
            if self.inner.read(at, &mut bytes).is_none() {
                return false;
            }
            let mut end = want;
            while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
                end -= 1;
            }
            if end == 0 {
                return false;
            }
            let Ok(text) = std::str::from_utf8(&bytes[..end]) else {
                return false;
            };
            f(text);
            at += end;
        }
        true
    }
    fn is_read_only(&self) -> bool {
        true
    }
}
impl TextStore for MappedStore {
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        Arc::new(self.clone())
    }
    fn replace(&mut self, _: Range<ByteOffset>, _: &str) -> Result<TextChange, EditError> {
        Err(EditError::ReadOnly)
    }
}
