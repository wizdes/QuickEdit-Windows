//! Editor-facing ACP text store. A view owns this on its UI thread.
use qe_api::{ByteOffset, LookupError, Rect, Utf16Offset, ViewContext};
use qe_editor::{ChangesSince, EditorView, GeometryError};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockKind {
    Read,
    ReadWrite,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReply {
    Granted,
    AsyncQueued,
    SyncRefused,
}
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    NotYetKnown,
    InvalidPosition,
    ReadOnly,
    NotLaidOut,
}
impl From<LookupError> for StoreError {
    fn from(value: LookupError) -> Self {
        match value {
            LookupError::NotYetKnown { .. } => Self::NotYetKnown,
            _ => Self::InvalidPosition,
        }
    }
}
impl From<GeometryError> for StoreError {
    fn from(_: GeometryError) -> Self {
        Self::NotLaidOut
    }
}

pub struct TextStore<'a> {
    view: &'a mut EditorView,
    ctx: &'a mut ViewContext,
    in_tsf_call: bool,
    pending: Option<LockKind>,
    revision: u64,
}
impl<'a> TextStore<'a> {
    pub fn new(view: &'a mut EditorView, ctx: &'a mut ViewContext) -> Self {
        Self {
            revision: view.revision(),
            view,
            ctx,
            in_tsf_call: false,
            pending: None,
        }
    }
    pub fn view(&self) -> &EditorView {
        self.view
    }
    pub fn read_only(&self) -> bool {
        !self.view.can_edit()
    }
    /// The caller runs `on_lock` before returning from the granted request.
    pub fn request_lock(&mut self, kind: LockKind, synchronous: bool) -> LockReply {
        if self.in_tsf_call {
            if synchronous {
                LockReply::SyncRefused
            } else {
                self.pending = Some(kind);
                LockReply::AsyncQueued
            }
        } else {
            self.in_tsf_call = true;
            LockReply::Granted
        }
    }
    pub fn release_lock(&mut self) -> Option<LockKind> {
        self.in_tsf_call = false;
        self.pending.take()
    }
    pub fn selection(&self) -> Result<Range<usize>, StoreError> {
        let r = self.view.selection();
        Ok(self.view.byte_to_utf16(r.start)?.0..self.view.byte_to_utf16(r.end)?.0)
    }
    pub fn read(&self, range: Range<usize>) -> Result<String, StoreError> {
        let start = self.view.utf16_to_byte(Utf16Offset(range.start))?;
        let end = self.view.utf16_to_byte(Utf16Offset(range.end))?;
        if start > end || end.0.saturating_sub(start.0) > 64 * 1024 {
            return Err(StoreError::InvalidPosition);
        }
        let mut text = String::new();
        self.view
            .worker_snapshot()
            .try_visit_chunks(start..end, &mut |s| text.push_str(s))?;
        Ok(text)
    }
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Result<(), StoreError> {
        if self.read_only() {
            return Err(StoreError::ReadOnly);
        }
        let start = self.view.utf16_to_byte(Utf16Offset(range.start))?;
        let end = self.view.utf16_to_byte(Utf16Offset(range.end))?;
        if start > end {
            return Err(StoreError::InvalidPosition);
        }
        self.view.replace_range(start..end, text, self.ctx);
        Ok(())
    }
    pub fn insert_at_selection(&mut self, text: &str) -> Result<(), StoreError> {
        let selection = self.selection()?;
        self.replace(selection, text)
    }
    pub fn start_composition(&mut self) {
        if !self.read_only() {
            self.view.ime_start();
        }
    }
    pub fn update_composition(&mut self, text: &str, cursor_utf16: usize) {
        if !self.read_only() {
            self.view.ime_update(text, cursor_utf16, self.ctx);
        }
    }
    pub fn commit_composition(&mut self, text: &str) {
        if !self.read_only() {
            self.view.ime_commit(text, self.ctx);
        }
    }
    pub fn cancel_composition(&mut self) {
        self.view.ime_cancel(self.ctx);
    }
    pub fn caret_screen_rect(&self, to_screen: impl Fn(Rect) -> Rect) -> Result<Rect, StoreError> {
        Ok(to_screen(self.view.caret_rect()?))
    }
    pub fn composition_screen_rects(
        &self,
        to_screen: impl Fn(Rect) -> Rect,
    ) -> Result<Vec<Rect>, StoreError> {
        Ok(self
            .view
            .composition_rects()?
            .into_iter()
            .map(|r| to_screen(r.rect))
            .collect())
    }
    pub fn changes(&mut self) -> ChangesSince {
        let changes = self.view.changes_since(self.revision);
        self.revision = self.view.revision();
        changes
    }
    pub fn byte_to_acp(&self, byte: ByteOffset) -> Result<usize, StoreError> {
        Ok(self.view.byte_to_utf16(byte)?.0)
    }
}
