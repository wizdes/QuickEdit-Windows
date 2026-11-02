//! Undo and redo as swaps of tree slices.
//!
//! An edit records where the change is, how long the text now in the document
//! is, and a tree holding the text it replaced. Undo swaps those two, so text
//! is never copied and a typed run costs one record however long it is.
use crate::PieceStore;
use crate::tree::{Kind, Node, Tree};
use qe_api::{ByteOffset, EditError, TextChange};
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

/// The default limit on memory charged to undo and redo text: 256 MiB.
pub const DEFAULT_UNDO_LIMIT_BYTES: usize = 256 << 20;
/// How many tree nodes one edit may release. See `Trash`.
const RELEASE_PER_EDIT: usize = 256;
/// A rough cost per leaf in a held tree: the leaf node and the branch above it.
const NODE_COST: usize = 2 * (size_of::<Node>() + 16);

pub(crate) struct Edit {
    pub start: usize,
    /// Bytes of this edit's text now in the document.
    pub len_in_doc: usize,
    /// The text to put back when this edit is swapped.
    pub other: Tree,
}

struct Group {
    edits: Vec<Edit>,
    state_before: u64,
    state_after: u64,
    charge: usize,
}

fn edit_charge(edit: &Edit) -> usize {
    size_of::<Edit>()
        + edit
            .other
            .as_ref()
            .map_or(0, |n| n.sum.bytes + n.leaves * NODE_COST)
}

fn charge_of(edits: &[Edit]) -> usize {
    size_of::<Group>() + edits.iter().map(edit_charge).sum::<usize>()
}

/// History that is no longer needed. Freeing a big tree takes time in
/// proportion to its size, so each edit frees only `RELEASE_PER_EDIT` nodes.
#[derive(Default)]
struct Trash {
    stacks: Vec<VecDeque<Group>>,
    edits: Vec<Edit>,
    nodes: Vec<Arc<Node>>,
}

impl Trash {
    fn release(&mut self, mut budget: usize) {
        while budget > 0 {
            budget -= 1;
            if let Some(node) = self.nodes.pop() {
                // A node that something else holds only loses one count here.
                if let Ok(owned) = Arc::try_unwrap(node)
                    && let Kind::Branch(l, r) = owned.kind
                {
                    self.nodes.push(l);
                    self.nodes.push(r);
                }
            } else if let Some(edit) = self.edits.pop() {
                self.nodes.extend(edit.other);
            } else if let Some(stack) = self.stacks.last_mut() {
                if let Some(group) = stack.pop_back() {
                    self.edits = group.edits;
                } else {
                    self.stacks.pop();
                }
            } else {
                return;
            }
        }
    }
    fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edits.is_empty() && self.stacks.is_empty()
    }
}

/// Records edits to one `PieceStore` so that they can be undone.
///
/// - **One writer.** Undo refuses to run after an edit it did not record. Direct
///   calls to `PieceStore::replace` stay possible, so `undo`, `redo` and `replace`
///   check the store's edit stamp and return `EditError::HistoryOutOfStep`, with
///   nothing changed. `reset` starts the history again.
/// - **All or nothing.** A failed undo or redo changes neither the store nor the
///   history. The work happens on a copy of the tree root and commits at the end.
/// - **Size limit.** Undo and redo text is charged against a limit, 256 MiB by
///   default. Past it the oldest undo groups go first, then the furthest redo
///   groups. The newest undo group and the next redo group always stay, even when
///   one alone is over the limit, so the latest edit can always be undone.
///   Charged text can be shared with the document, so the charge can be too high
///   but not too low, apart from the rough per-leaf cost.
/// - **Key press cost.** Clearing redo or dropping old groups moves them to a list
///   and frees a fixed number of nodes per edit. Call `release_memory` when idle
///   to free the rest at once.
pub struct UndoHistory {
    undo: VecDeque<Group>,
    redo: VecDeque<Group>,
    typing_end: Option<usize>,
    group_depth: usize,
    group_started: bool,
    state: u64,
    next_state: u64,
    saved: Option<u64>,
    attached: Option<u64>,
    undo_charge: usize,
    redo_charge: usize,
    limit: usize,
    trash: Trash,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoHistory {
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_UNDO_LIMIT_BYTES)
    }
    /// A history with its own limit, in charged bytes.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            typing_end: None,
            group_depth: 0,
            group_started: false,
            state: 0,
            next_state: 1,
            saved: Some(0),
            attached: None,
            undo_charge: 0,
            redo_charge: 0,
            limit,
            trash: Trash::default(),
        }
    }
    /// Ends the current typed run. The next edit starts a new undo step.
    pub fn break_group(&mut self) {
        self.typing_end = None;
    }
    pub fn cursor_moved(&mut self) {
        self.break_group();
    }
    /// Starts one undo step that holds every edit until the matching `end_group`.
    /// Replace All uses this. Calls can nest; the outermost pair counts. A group
    /// with no real edit adds nothing.
    pub fn begin_group(&mut self) {
        self.group_depth += 1;
        self.typing_end = None;
    }
    /// Closes `begin_group`. Extra calls do nothing.
    pub fn end_group(&mut self) {
        self.group_depth = self.group_depth.saturating_sub(1);
        if self.group_depth == 0 {
            self.close_group();
        }
    }
    fn close_group(&mut self) {
        self.group_depth = 0;
        self.group_started = false;
        self.typing_end = None;
    }
    /// Marks the current text as saved. This also closes any open group.
    pub fn save_point(&mut self) {
        self.close_group();
        self.saved = Some(self.state);
    }
    /// True when `store` holds the same text as at the last `save_point`, reached
    /// only through this history's edits, undo and redo. A new history counts its
    /// start as saved. Typing a letter and deleting it by hand counts as a change,
    /// because the check follows history and does not compare text.
    pub fn is_at_save_point(&self, store: &PieceStore) -> bool {
        self.saved == Some(self.state) && self.attached.is_none_or(|stamp| stamp == store.stamp())
    }
    pub fn undo_groups(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_groups(&self) -> usize {
        self.redo.len()
    }
    /// Charged bytes in undo and redo. Memory waiting in the release list is not
    /// counted. See `release_memory`.
    pub fn memory_bytes(&self) -> usize {
        self.undo_charge + self.redo_charge
    }
    /// Frees all history that is waiting to be released. Its cost grows with the
    /// amount waiting, so call it when idle and not on a key press.
    pub fn release_memory(&mut self) {
        while !self.trash.is_empty() {
            self.trash.release(RELEASE_PER_EDIT);
        }
    }
    /// True when released history is still waiting to be freed.
    pub fn has_memory_to_release(&self) -> bool {
        !self.trash.is_empty()
    }
    /// Forgets all history and treats `store` as the new start. The text counts
    /// as not saved unless `saved` is true.
    pub fn reset(&mut self, store: &PieceStore, saved: bool) {
        self.trash.stacks.push(std::mem::take(&mut self.undo));
        self.trash.stacks.push(std::mem::take(&mut self.redo));
        self.undo_charge = 0;
        self.redo_charge = 0;
        self.close_group();
        self.state = self.fresh_state();
        self.saved = saved.then_some(self.state);
        self.attached = Some(store.stamp());
    }
    fn fresh_state(&mut self) -> u64 {
        self.next_state += 1;
        self.next_state - 1
    }
    fn check_attached(&self, store: &PieceStore) -> Result<(), EditError> {
        match self.attached {
            Some(stamp) if stamp != store.stamp() => Err(EditError::HistoryOutOfStep),
            _ => Ok(()),
        }
    }
    fn evict(&mut self) {
        while self.memory_bytes() > self.limit {
            if self.undo.len() > 1 {
                let group = self.undo.pop_front().expect("checked above");
                self.undo_charge -= group.charge;
                self.trash.add_group(group);
            } else if self.redo.len() > 1 {
                let group = self.redo.pop_front().expect("checked above");
                self.redo_charge -= group.charge;
                self.trash.add_group(group);
            } else {
                return;
            }
        }
    }
    pub fn replace(
        &mut self,
        store: &mut PieceStore,
        range: Range<ByteOffset>,
        text: &str,
        typing: bool,
    ) -> Result<TextChange, EditError> {
        store.check_range(&range)?;
        if range.start == range.end && text.is_empty() {
            return Ok(store.no_change(range));
        }
        self.check_attached(store)?;
        let (change, removed) = store.splice_text(range.clone(), text);
        self.attached = Some(store.stamp());
        let state = self.fresh_state();
        let inserted = text.len();
        let empty_range = range.start == range.end;
        let continues_typing = typing
            && empty_range
            && self.typing_end == Some(range.start.0)
            && !self.undo.is_empty();
        let in_open_group = self.group_depth > 0 && self.group_started;
        if continues_typing || in_open_group {
            let group = self.undo.back_mut().expect("an open group is on the stack");
            let last = group.edits.last_mut();
            match last {
                Some(last) if empty_range && last.start + last.len_in_doc == range.start.0 => {
                    last.len_in_doc += inserted;
                }
                _ => {
                    let edit = Edit {
                        start: range.start.0,
                        len_in_doc: inserted,
                        other: removed,
                    };
                    let charge = edit_charge(&edit);
                    group.edits.push(edit);
                    group.charge += charge;
                    self.undo_charge += charge;
                }
            }
            group.state_after = state;
        } else {
            let edits = vec![Edit {
                start: range.start.0,
                len_in_doc: inserted,
                other: removed,
            }];
            let charge = charge_of(&edits);
            self.undo_charge += charge;
            self.undo.push_back(Group {
                edits,
                state_before: self.state,
                state_after: state,
                charge,
            });
            self.group_started = self.group_depth > 0;
        }
        self.state = state;
        self.typing_end = (typing && empty_range).then_some(range.start.0 + inserted);
        if !self.redo.is_empty() {
            self.redo_charge = 0;
            self.trash.stacks.push(std::mem::take(&mut self.redo));
        }
        self.evict();
        self.trash.release(RELEASE_PER_EDIT);
        Ok(change)
    }
    /// Undoes the newest group. Returns each change in the order it was applied,
    /// each one in the coordinates left by the change before it. An empty list
    /// means there was nothing to undo.
    pub fn undo(&mut self, store: &mut PieceStore) -> Result<Vec<TextChange>, EditError> {
        self.check_attached(store)?;
        let Some(group) = self.undo.back() else {
            self.close_group();
            return Ok(Vec::new());
        };
        let (edits, changes) = store.swap_edits(&group.edits, true)?;
        let group = self.undo.pop_back().expect("checked above");
        self.finish_swap(store, group, edits, true);
        Ok(changes)
    }
    /// Redoes the group undone last. The result reads like `undo`'s.
    pub fn redo(&mut self, store: &mut PieceStore) -> Result<Vec<TextChange>, EditError> {
        self.check_attached(store)?;
        let Some(group) = self.redo.back() else {
            self.close_group();
            return Ok(Vec::new());
        };
        let (edits, changes) = store.swap_edits(&group.edits, false)?;
        let group = self.redo.pop_back().expect("checked above");
        self.finish_swap(store, group, edits, false);
        Ok(changes)
    }
    fn finish_swap(&mut self, store: &PieceStore, group: Group, edits: Vec<Edit>, undoing: bool) {
        self.close_group();
        self.attached = Some(store.stamp());
        let charge = charge_of(&edits);
        if undoing {
            self.undo_charge -= group.charge;
            self.redo_charge += charge;
        } else {
            self.redo_charge -= group.charge;
            self.undo_charge += charge;
        }
        self.state = if undoing {
            group.state_before
        } else {
            group.state_after
        };
        let done = Group {
            edits,
            state_before: group.state_before,
            state_after: group.state_after,
            charge,
        };
        if undoing {
            self.redo.push_back(done);
        } else {
            self.undo.push_back(done);
        }
        self.trash.add_group(group);
        self.evict();
        self.trash.release(RELEASE_PER_EDIT);
    }
}

impl Trash {
    fn add_group(&mut self, group: Group) {
        let mut stack = VecDeque::with_capacity(1);
        stack.push_back(group);
        self.stacks.push(stack);
    }
}
