use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteOffset(pub usize);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Utf16Offset(pub usize);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineIndex(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPosition {
    pub line: LineIndex,
    pub column: Utf16Offset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChange {
    pub replaced: Range<ByteOffset>,
    pub new_len: usize,
    pub line_count_delta: isize,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    ReadOnly,
    InvalidRange,
}

/// Immutable read side. Implementations are `Send + Sync`, so a snapshot may be
/// shared across worker threads. Production lookups should be O(log n) or better.
pub trait TextSnapshot: Send + Sync {
    fn revision(&self) -> u64;
    fn len_bytes(&self) -> ByteOffset;
    fn len_utf16(&self) -> Utf16Offset;
    fn line_count(&self) -> usize;
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset>;
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset>;
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition>;
    fn position_to_byte(&self, position: TextPosition) -> Option<ByteOffset>;
    /// Includes the line ending when present.
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>>;
    /// Calls `visitor` for borrowed UTF-8 chunks without copying the whole document.
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool;
    fn is_read_only(&self) -> bool;
}

/// Mutable edit side. UI code owns this; readers use a separate immutable snapshot.
pub trait TextStore: TextSnapshot {
    /// Returns an immutable view that stays unchanged after later edits.
    fn snapshot(&self) -> Arc<dyn TextSnapshot>;
    fn replace(&mut self, range: Range<ByteOffset>, text: &str) -> Result<TextChange, EditError>;
}
