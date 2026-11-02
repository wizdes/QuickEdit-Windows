//! Which events one input batch should raise. The host calls this once for each batch, so a
//! Replace All and a fast typist both raise at most one event of each kind per batch.
use qe_api::ByteOffset;
use qe_editor::EditorView;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventFlags {
    pub text_changed: bool,
    pub selection_changed: bool,
    pub caret_moved: bool,
}

#[derive(Clone, PartialEq)]
struct Seen {
    owner: (u64, u64),
    revision: u64,
    selection: Range<ByteOffset>,
    caret: ByteOffset,
    index_complete: bool,
}

/// Remembers what the last batch saw. Nothing is kept while no client listens, so a client
/// that arrives later hears about its first batch, even if that batch changed nothing.
#[derive(Default)]
pub struct EventTracker {
    last: Option<Seen>,
}

impl EventTracker {
    /// Forgets the last batch. This costs one write and reads nothing.
    pub fn no_client(&mut self) {
        self.last = None;
    }
    pub fn holds_state(&self) -> bool {
        self.last.is_some()
    }
    pub fn poll(&mut self, view_id: u64, view: &EditorView) -> EventFlags {
        let now = Seen {
            owner: (view_id, view.document_number()),
            revision: view.revision(),
            selection: view.selection(),
            caret: view.caret(),
            index_complete: view.store_status().complete,
        };
        let flags = match &self.last {
            None => EventFlags {
                text_changed: true,
                selection_changed: true,
                caret_moved: true,
            },
            Some(last) => EventFlags {
                text_changed: last.owner != now.owner
                    || last.revision != now.revision
                    || last.index_complete != now.index_complete,
                selection_changed: last.owner != now.owner || last.selection != now.selection,
                caret_moved: last.owner != now.owner || last.caret != now.caret,
            },
        };
        self.last = Some(now);
        flags
    }
}
