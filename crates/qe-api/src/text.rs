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
    /// The store has edits that this undo history did not record.
    HistoryOutOfStep,
}

/// A store failure that stays until the store is opened again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreFailure {
    pub message: String,
    /// Lookups below this offset still answer. `ByteOffset(0)` means that no
    /// part is safe to use, for example because the file changed on disk.
    pub valid_up_to: ByteOffset,
}

/// Why a `try_` query has no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// The answer depends on text that is not indexed yet. Ask again later.
    NotYetKnown { indexed_up_to: ByteOffset },
    /// The offset, line or position is not in the text, or is inside a character.
    OutOfRange,
    /// The store failed, so this answer will never come.
    Failed(StoreFailure),
}

/// What a store knows at this moment. Answers from the plain queries that return
/// a bare value, such as `line_count`, can only be trusted when `complete` is true
/// and `failure` is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreStatus {
    pub known_bytes: ByteOffset,
    pub known_utf16: Utf16Offset,
    pub known_lines: usize,
    pub complete: bool,
    pub failure: Option<StoreFailure>,
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

    /// What the store knows now, including any failure. Never blocks.
    #[must_use]
    fn status(&self) -> StoreStatus {
        StoreStatus {
            known_bytes: self.len_bytes(),
            known_utf16: self.len_utf16(),
            known_lines: self.line_count(),
            complete: true,
            failure: None,
        }
    }
    // The `try_` queries never block and always say why there is no answer.
    // The UI thread uses these when a store can be `is_read_only`.
    fn try_len_bytes(&self) -> Result<ByteOffset, LookupError> {
        Ok(self.len_bytes())
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        Ok(self.len_utf16())
    }
    fn try_line_count(&self) -> Result<usize, LookupError> {
        Ok(self.line_count())
    }
    fn try_byte_to_utf16(&self, byte: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.byte_to_utf16(byte).ok_or(LookupError::OutOfRange)
    }
    fn try_utf16_to_byte(&self, utf16: Utf16Offset) -> Result<ByteOffset, LookupError> {
        self.utf16_to_byte(utf16).ok_or(LookupError::OutOfRange)
    }
    fn try_byte_to_position(&self, byte: ByteOffset) -> Result<TextPosition, LookupError> {
        self.byte_to_position(byte).ok_or(LookupError::OutOfRange)
    }
    fn try_position_to_byte(&self, position: TextPosition) -> Result<ByteOffset, LookupError> {
        self.position_to_byte(position)
            .ok_or(LookupError::OutOfRange)
    }
    fn try_line_range(&self, line: LineIndex) -> Result<Range<ByteOffset>, LookupError> {
        self.line_range(line).ok_or(LookupError::OutOfRange)
    }
    fn try_visit_chunks(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        if self.visit_chunks(range, visitor) {
            Ok(())
        } else {
            Err(LookupError::OutOfRange)
        }
    }
}

/// Mutable edit side. UI code owns this; readers use a separate immutable snapshot.
pub trait TextStore: TextSnapshot {
    /// Returns an immutable view that stays unchanged after later edits.
    fn snapshot(&self) -> Arc<dyn TextSnapshot>;
    fn replace(&mut self, range: Range<ByteOffset>, text: &str) -> Result<TextChange, EditError>;
}
