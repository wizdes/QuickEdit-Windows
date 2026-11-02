//! UTF-8 text storage. See `README.md` in this crate for the rules and limits.
//!
//! - `PieceStore` is the editable store: an immutable balanced tree of small text
//!   leaves, so a snapshot is a cheap copy of the root.
//! - `UndoHistory` records edits to a `PieceStore` as swaps of tree slices.
//! - `MappedStore` is the read-only store for very large files. It uses ReadFile
//!   and not a live memory mapping, because a live mapping stops other apps from
//!   truncating the file.
use qe_api::{
    ByteOffset, EditError, LineIndex, TextChange, TextPosition, TextSnapshot, TextStore,
    Utf16Offset,
};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(windows)]
mod mapped;
mod measure;
mod tree;
mod undo;

#[cfg(windows)]
pub use mapped::{IndexTiming, MappedStore, Notify};
pub use tree::TreeShape;
pub use undo::{DEFAULT_UNDO_LIMIT_BYTES, UndoHistory};

use measure::Sum;
#[cfg(windows)]
use measure::measure;
use tree::{Kind, Node, Tree, byte_at, from_text, is_boundary, prefix, splice, split, visit};
use undo::Edit;

static NEXT_STAMP: AtomicU64 = AtomicU64::new(1);

fn fresh_stamp() -> u64 {
    NEXT_STAMP.fetch_add(1, Ordering::Relaxed)
}

/// The editable store. Each real edit makes a new stamp, unique in the process,
/// which is how `UndoHistory` notices an edit it did not record.
#[derive(Clone)]
pub struct PieceStore {
    root: Tree,
    revision: u64,
    stamp: u64,
}

impl Default for PieceStore {
    fn default() -> Self {
        Self::new("")
    }
}

impl PieceStore {
    pub fn new(text: &str) -> Self {
        Self {
            root: from_text(text),
            revision: 0,
            stamp: fresh_stamp(),
        }
    }
    /// Converts UTF-8 and builds the tree on a worker thread. The error means the
    /// OS refused to start the thread; the bytes are lost in that case, so the
    /// caller must still hold the file.
    pub fn build_async(
        bytes: Vec<u8>,
    ) -> std::io::Result<std::thread::JoinHandle<Result<Self, std::string::FromUtf8Error>>> {
        std::thread::Builder::new()
            .name("qe-buffer-build".into())
            .spawn(move || String::from_utf8(bytes).map(|s| Self::new(&s)))
    }
    /// Walks the whole tree, checks every balance and sum rule, and gives its
    /// shape. It takes time in proportion to the tree, so it is for tests.
    pub fn check_tree(&self) -> Result<TreeShape, String> {
        tree::check(&self.root)
    }
    pub(crate) fn stamp(&self) -> u64 {
        self.stamp
    }
    fn sum(&self) -> Sum {
        self.root.as_ref().map_or(Sum::default(), |n| n.sum)
    }
    fn breaks_before(&self, at: usize) -> Option<usize> {
        let s = if at == 0 {
            Sum::default()
        } else {
            prefix(&self.root, at)?
        };
        Some(s.breaks - usize::from(s.last_cr && byte_at(&self.root, at) == Some(b'\n')))
    }
    fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return Some(0);
        }
        if line > self.sum().breaks {
            return None;
        }
        fn find(n: &Node, mut rank: usize, next_lf: bool, base: usize) -> Option<usize> {
            match &n.kind {
                Kind::Leaf(text) => {
                    let mut chars = text.char_indices().peekable();
                    while let Some((at, c)) = chars.next() {
                        let next = chars.peek().is_some_and(|(_, c)| *c == '\n')
                            || (chars.peek().is_none() && next_lf);
                        if matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
                            && !(c == '\r' && next)
                        {
                            rank -= 1;
                            if rank == 0 {
                                return Some(base + at + c.len_utf8());
                            }
                        }
                    }
                    None
                }
                Kind::Branch(l, r) => {
                    let left_breaks = l.sum.breaks - usize::from(l.sum.last_cr && r.sum.first_lf);
                    if rank <= left_breaks {
                        find(l, rank, r.sum.first_lf, base)
                    } else {
                        find(r, rank - left_breaks, next_lf, base + l.sum.bytes)
                    }
                }
            }
        }
        find(self.root.as_ref()?, line, false, 0)
    }
    pub(crate) fn check_range(&self, range: &Range<ByteOffset>) -> Result<(), EditError> {
        if range.start > range.end
            || !is_boundary(&self.root, range.start.0)
            || !is_boundary(&self.root, range.end.0)
        {
            return Err(EditError::InvalidRange);
        }
        Ok(())
    }
    pub(crate) fn no_change(&self, range: Range<ByteOffset>) -> TextChange {
        TextChange {
            replaced: range,
            new_len: 0,
            line_count_delta: 0,
            revision: self.revision,
        }
    }
    /// Makes the edit and gives back the removed text as a tree. The range must
    /// have passed `check_range`.
    pub(crate) fn splice_text(
        &mut self,
        range: Range<ByteOffset>,
        text: &str,
    ) -> (TextChange, Tree) {
        let before = self.line_count();
        let (left, rest) = split(self.root.clone(), range.start.0);
        let (removed, right) = split(rest, range.end.0 - range.start.0);
        self.root = splice(left, from_text(text), right);
        self.revision += 1;
        self.stamp = fresh_stamp();
        let change = TextChange {
            replaced: range,
            new_len: text.len(),
            line_count_delta: self.line_count() as isize - before as isize,
            revision: self.revision,
        };
        (change, removed)
    }
    /// Swaps each edit with the text in the document, on a copy of the root. The
    /// store changes only when every edit worked.
    pub(crate) fn swap_edits(
        &mut self,
        edits: &[Edit],
        backwards: bool,
    ) -> Result<(Vec<Edit>, Vec<TextChange>), EditError> {
        let mut root = self.root.clone();
        let mut revision = self.revision;
        let mut swapped = Vec::with_capacity(edits.len());
        let mut changes = Vec::with_capacity(edits.len());
        let lines = |t: &Tree| t.as_ref().map_or(1, |n| n.sum.breaks + 1) as isize;
        let mut step = |e: &Edit| -> Result<(), EditError> {
            let end = e.start + e.len_in_doc;
            if !is_boundary(&root, e.start) || !is_boundary(&root, end) {
                return Err(EditError::InvalidRange);
            }
            let before = lines(&root);
            let (left, rest) = split(root.clone(), e.start);
            let (taken, right) = split(rest, e.len_in_doc);
            root = splice(left, e.other.clone(), right);
            let new_len = e.other.as_ref().map_or(0, |n| n.sum.bytes);
            revision += 1;
            changes.push(TextChange {
                replaced: ByteOffset(e.start)..ByteOffset(end),
                new_len,
                line_count_delta: lines(&root) - before,
                revision,
            });
            swapped.push(Edit {
                start: e.start,
                len_in_doc: new_len,
                other: taken,
            });
            Ok(())
        };
        if backwards {
            edits.iter().rev().try_for_each(&mut step)?;
            swapped.reverse();
        } else {
            edits.iter().try_for_each(&mut step)?;
        }
        self.root = root;
        self.revision = revision;
        self.stamp = fresh_stamp();
        Ok((swapped, changes))
    }
}

impl TextSnapshot for PieceStore {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.sum().bytes)
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.sum().units)
    }
    fn line_count(&self) -> usize {
        self.sum().breaks + 1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        if byte.0 == 0 {
            return Some(Utf16Offset(0));
        }
        prefix(&self.root, byte.0).map(|s| Utf16Offset(s.units))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        if utf16.0 > self.sum().units {
            return None;
        }
        let Some(mut n) = self.root.as_ref() else {
            return Some(ByteOffset(0));
        };
        let mut units = utf16.0;
        let mut base = 0;
        loop {
            match &n.kind {
                Kind::Leaf(text) => {
                    let mut count = 0;
                    for (i, c) in text.char_indices() {
                        if count == units {
                            return Some(ByteOffset(base + i));
                        }
                        count += c.len_utf16();
                        if count > units {
                            return None;
                        }
                    }
                    return (count == units).then_some(ByteOffset(base + text.len()));
                }
                Kind::Branch(l, r) => {
                    if units <= l.sum.units {
                        n = l;
                    } else {
                        units -= l.sum.units;
                        base += l.sum.bytes;
                        n = r;
                    }
                }
            }
        }
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        let units = self.byte_to_utf16(byte)?.0;
        let line = self.breaks_before(byte.0)?;
        let start = self.line_start(line)?;
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
        let byte = self.utf16_to_byte(Utf16Offset(units))?;
        (byte <= range.end).then_some(byte)
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        let start = self.line_start(line.0)?;
        let end = self.line_start(line.0 + 1).unwrap_or(self.sum().bytes);
        Some(ByteOffset(start)..ByteOffset(end))
    }
    /// Gives no callback and returns false for a range that is backwards, past the
    /// end, or has an end inside a character. An empty valid range gives no callback
    /// and returns true. `MappedStore` follows the same rule.
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        if self.check_range(&range).is_err() {
            return false;
        }
        if let Some(n) = &self.root {
            visit(n, range.start.0, range.end.0, f);
        }
        true
    }
    fn is_read_only(&self) -> bool {
        false
    }
}

impl TextStore for PieceStore {
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        Arc::new(self.clone())
    }
    /// An empty edit changes nothing: no new revision and no new stamp. This call
    /// frees removed text on the spot. An editor goes through `UndoHistory`, which
    /// keeps that text and frees it a little at a time.
    fn replace(&mut self, range: Range<ByteOffset>, text: &str) -> Result<TextChange, EditError> {
        self.check_range(&range)?;
        if range.start == range.end && text.is_empty() {
            return Ok(self.no_change(range));
        }
        Ok(self.splice_text(range, text).0)
    }
}
