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
    /// Undo or redo was asked for while an edit group is open.
    GroupOpen,
}

/// A change in UTF-16 units, which an input method needs. All three offsets are in
/// the text as it stood just before this change, except `new_end`, which is in the
/// text just after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utf16Change {
    pub start: Utf16Offset,
    pub old_end: Utf16Offset,
    pub new_end: Utf16Offset,
}

/// A change in bytes and in UTF-16 units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedChange {
    pub change: TextChange,
    pub utf16: Utf16Change,
}

/// The known part of a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownLine {
    pub range: Range<ByteOffset>,
    /// False when the index has not found this line's end yet.
    pub complete: bool,
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
    /// A `try_visit_chunks` range is longer than a store can give without a wait.
    TooLong { max_bytes: usize },
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
///
/// The plain queries that return `Option` or `bool` may wait for disk on a store
/// where `is_read_only` is true, so they must not run on the UI thread for that
/// store. The `try_` queries and `status` never touch the disk on the calling thread.
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
    /// The known part of a line. It answers on a line whose end is still unknown,
    /// so a very long line can paint before the index finishes.
    fn try_known_line(&self, line: LineIndex) -> Result<KnownLine, LookupError> {
        self.try_line_range(line).map(|range| KnownLine {
            range,
            complete: true,
        })
    }
    /// Bytes of the line ending at the end of this line: 0 on a last line with no
    /// ending, 2 for CRLF and for U+0085, 3 for U+2028 and U+2029, otherwise 1.
    fn try_line_ending_len(&self, line: LineIndex) -> Result<usize, LookupError> {
        let range = self.try_line_range(line)?;
        let longest = (range.end.0 - range.start.0).min(3);
        for back in (1..=longest).rev() {
            let mut tail = String::new();
            let start = ByteOffset(range.end.0 - back);
            match self.try_visit_chunks(start..range.end, &mut |s| tail.push_str(s)) {
                Ok(()) => {
                    return Ok(if tail.ends_with("\r\n") {
                        2
                    } else if tail.ends_with(['\r', '\n']) {
                        1
                    } else if tail.ends_with('\u{85}') {
                        2
                    } else if tail.ends_with(['\u{2028}', '\u{2029}']) {
                        3
                    } else {
                        0
                    });
                }
                Err(LookupError::OutOfRange) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(0)
    }
    /// Like `visit_chunks`, and stops as soon as `visitor` returns false. The result
    /// says whether the range was good, as in `visit_chunks`, not whether it stopped.
    fn visit_chunks_while(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str) -> bool,
    ) -> bool {
        let mut going = true;
        self.visit_chunks(range, &mut |s| {
            if going {
                going = visitor(s);
            }
        })
    }
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
