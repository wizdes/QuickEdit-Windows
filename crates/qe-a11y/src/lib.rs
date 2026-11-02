//! UI Automation text data. The shell must marshal calls that touch `EditorView`
//! to its UI thread. Snapshot reads here use only nonblocking `try_` calls.
use qe_api::{ByteOffset, LookupError, TextSnapshot, Utf16Offset};
use std::sync::Arc;

pub mod provider;
pub mod widgets;

/// Run an event calculation only while a UI Automation client is listening.
/// The host calls this after input or a document change, never from a timer.
pub fn when_listening<T>(listening: bool, collect: impl FnOnce() -> T) -> Option<T> {
    listening.then(collect)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventFlags {
    pub text_changed: bool,
    pub selection_changed: bool,
    pub caret_moved: bool,
}

/// Keep one marker per window. Call after an input batch and raise at most one
/// event of each kind for that batch. No timer or idle work is needed.
#[derive(Default)]
pub struct EventTracker {
    last: Option<(
        u64,
        u64,
        std::ops::Range<qe_api::ByteOffset>,
        qe_api::ByteOffset,
    )>,
}

impl EventTracker {
    pub fn poll(&mut self, view: &qe_editor::EditorView, listening: bool) -> EventFlags {
        if !listening {
            return EventFlags::default();
        }
        let current = (
            view.document_number(),
            view.revision(),
            view.selection(),
            view.caret(),
        );
        let Some((document, revision, selection, caret)) = self.last.replace(current.clone())
        else {
            return EventFlags::default();
        };
        let text_changed = document != current.0
            || (revision != current.1
                && !matches!(
                    view.changes_since(revision),
                    qe_editor::ChangesSince::Current
                ));
        EventFlags {
            text_changed,
            selection_changed: selection != current.2,
            caret_moved: caret != current.3,
        }
    }
}

// The `implement` macro names `windows_core` directly. Re-export the exact
// version bundled by `windows` so this crate needs no lock-file edit.
extern crate self as windows_core;
pub use windows::core::*;

/// Largest answer to one unrestricted GetText call, in UTF-16 code units.
/// Clients can ask for the next range. This keeps a whole-file request bounded.
pub const GET_TEXT_CAP: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    NotYetKnown,
    OutOfRange,
    TooLong,
    Failed(String),
}

impl From<LookupError> for ReadError {
    fn from(error: LookupError) -> Self {
        match error {
            LookupError::NotYetKnown { .. } => Self::NotYetKnown,
            LookupError::OutOfRange => Self::OutOfRange,
            LookupError::TooLong { .. } => Self::TooLong,
            LookupError::Failed(failure) => Self::Failed(failure.message),
        }
    }
}

#[derive(Clone)]
pub struct TextModel {
    snapshot: Arc<dyn TextSnapshot>,
}

impl TextModel {
    pub fn new(snapshot: Arc<dyn TextSnapshot>) -> Self {
        Self { snapshot }
    }

    pub fn len_utf16(&self) -> std::result::Result<usize, ReadError> {
        self.snapshot
            .try_len_utf16()
            .map(|n| n.0)
            .map_err(Into::into)
    }

    /// Read at most `limit` UTF-16 units. A negative limit uses GET_TEXT_CAP.
    /// A limit ending inside a surrogate pair stops before that pair.
    pub fn read(&self, range: TextRange, limit: i32) -> std::result::Result<String, ReadError> {
        if range.start > range.end || range.end > self.len_utf16()? {
            return Err(ReadError::OutOfRange);
        }
        let max = if limit < 0 {
            GET_TEXT_CAP
        } else {
            limit as usize
        };
        let end = range.start.saturating_add(max).min(range.end);
        let start_byte = self.snapshot.try_utf16_to_byte(Utf16Offset(range.start))?;
        let end_byte = self.byte_at_or_before(end, range.start)?;
        let mut result = String::new();
        self.snapshot
            .try_visit_chunks(start_byte..end_byte, &mut |part| result.push_str(part))?;
        Ok(result)
    }

    /// Search at most 65,536 UTF-16 units. A larger request returns TooLong,
    /// so absence of a match is never reported for text that was not searched.
    pub fn find_text(
        &self,
        range: TextRange,
        query: &str,
        backward: bool,
        ignore_case: bool,
    ) -> std::result::Result<Option<TextRange>, ReadError> {
        if query.is_empty() {
            return Err(ReadError::OutOfRange);
        }
        if range.end.saturating_sub(range.start) > GET_TEXT_CAP
            || query.encode_utf16().count() > 256
        {
            return Err(ReadError::TooLong);
        }
        let body = self.read(range, -1)?;
        let mut at_utf16 = range.start;
        let mut found = None;
        for (byte, character) in body.char_indices() {
            if let Some(end) = match_text_at(&body, byte, query, ignore_case)
                && let Some(matched) = body.get(byte..end)
            {
                let answer = TextRange::new(at_utf16, at_utf16 + matched.encode_utf16().count());
                if !backward {
                    return Ok(Some(answer));
                }
                found = Some(answer);
            }
            at_utf16 += character.len_utf16();
        }
        Ok(found)
    }

    fn byte_at_or_before(
        &self,
        position: usize,
        floor: usize,
    ) -> std::result::Result<ByteOffset, ReadError> {
        let mut at = position;
        loop {
            match self.snapshot.try_utf16_to_byte(Utf16Offset(at)) {
                Ok(byte) => return Ok(byte),
                Err(LookupError::OutOfRange) if at > floor => at -= 1,
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn match_text_at(body: &str, start: usize, query: &str, ignore_case: bool) -> Option<usize> {
    if !ignore_case {
        return body
            .get(start..)
            .filter(|tail| tail.starts_with(query))
            .map(|_| start + query.len());
    }
    let mut end = start;
    for wanted in query.chars() {
        let actual = body.get(end..)?.chars().next()?;
        if !actual.to_lowercase().eq(wanted.to_lowercase()) {
            return None;
        }
        end = end.checked_add(actual.len_utf8())?;
    }
    Some(end)
}
