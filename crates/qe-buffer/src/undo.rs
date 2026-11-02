//! Undo and redo as swaps of tree slices.
//!
//! An edit records where the change is, how long the text now in the document
//! is, and a tree holding the text it replaced. Undo swaps those two, so text
//! is never copied and a typed run costs one record however long it is.
use crate::tree::{CHUNK, Kind, Node, Tree, join, split};
use crate::{BuiltText, PieceStore};
use qe_api::{ByteOffset, EditError, TextChange, TrackedChange};
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

/// The default limit on memory charged to undo and redo text: 256 MiB.
pub const DEFAULT_UNDO_LIMIT_BYTES: usize = 256 << 20;
/// The fewest tree nodes one edit releases. See `Trash`.
const RELEASE_PER_EDIT: usize = 256;
/// The most groups one edit moves out of history. See `evict`.
const EVICT_PER_EDIT: usize = 64;
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

fn tree_charge(tree: &Tree) -> usize {
    tree.as_ref()
        .map_or(0, |n| n.sum.bytes + n.leaves * NODE_COST)
}

fn edit_charge(edit: &Edit) -> usize {
    size_of::<Edit>() + tree_charge(&edit.other)
}

fn charge_of(edits: &[Edit]) -> usize {
    size_of::<Group>() + edits.iter().map(edit_charge).sum::<usize>()
}

/// History that is no longer needed. Freeing a big tree takes time in proportion
/// to its size, so each edit frees a bounded part of this list. `pending` is a
/// running estimate of the bytes it holds.
#[derive(Default)]
struct Trash {
    stacks: Vec<VecDeque<Group>>,
    groups: VecDeque<Group>,
    edits: Vec<Edit>,
    nodes: Vec<Arc<Node>>,
    pending: usize,
}

impl Trash {
    fn add_group(&mut self, group: Group) {
        self.pending += group.charge;
        self.groups.push_back(group);
    }
    fn add_stack(&mut self, stack: VecDeque<Group>, charge: usize) {
        self.pending += charge;
        self.stacks.push(stack);
    }
    fn add_tree(&mut self, tree: Tree) {
        self.pending += tree_charge(&tree);
        self.nodes.extend(tree);
    }
    fn release(&mut self, mut budget: usize) {
        while budget > 0 {
            budget -= 1;
            if let Some(node) = self.nodes.pop() {
                let cost = node.sum.bytes + node.leaves * NODE_COST;
                match Arc::try_unwrap(node) {
                    Ok(owned) => match owned.kind {
                        Kind::Branch(l, r) => {
                            self.nodes.push(l);
                            self.nodes.push(r);
                        }
                        Kind::Leaf(_) => self.pending = self.pending.saturating_sub(cost),
                    },
                    // Something else holds it, so this list has no more to free.
                    Err(_) => self.pending = self.pending.saturating_sub(cost),
                }
            } else if let Some(edit) = self.edits.pop() {
                self.pending = self.pending.saturating_sub(size_of::<Edit>());
                self.nodes.extend(edit.other);
            } else if let Some(group) = self.groups.pop_front() {
                self.pending = self.pending.saturating_sub(size_of::<Group>());
                self.edits = group.edits;
            } else if let Some(stack) = self.stacks.last_mut() {
                if let Some(group) = stack.pop_back() {
                    self.pending = self.pending.saturating_sub(size_of::<Group>());
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
        self.nodes.is_empty()
            && self.edits.is_empty()
            && self.groups.is_empty()
            && self.stacks.is_empty()
    }
}

/// History that `UndoHistory::take_released` took out. Dropping this frees it, so it can
/// go to a worker, and its size does not then matter to the thread that holds the view.
pub struct ReleasedHistory(#[allow(dead_code)] Trash);

/// Records edits to one `PieceStore` so that they can be undone.
///
/// - **One writer.** Undo refuses to run after an edit it did not record. Direct
///   calls to `PieceStore::replace` stay possible, so `undo`, `redo` and `replace`
///   check the store's edit stamp first and return `EditError::HistoryOutOfStep`,
///   with nothing changed. `reset` starts the history again.
/// - **Binding.** `for_store` binds the history to a store from the start, and the
///   editor uses it. `new` binds at the first recorded edit. Until then there is no
///   store to compare with, so `is_at_save_point` is false and `save_point` marks
///   nothing.
/// - **All or nothing.** A failed undo or redo changes neither the store nor the
///   history. The work happens on a copy of the tree root and commits at the end.
/// - **Size limit.** Undo and redo text is charged against a limit, 256 MiB by
///   default. Past it the oldest undo groups go first, then the furthest redo
///   groups. The newest undo group and the next redo group always stay, even when
///   one alone is over the limit, so the latest edit can always be undone. One edit
///   drops at most 64 groups, so the charge can stay over the limit for a few edits.
/// - **Key press cost.** Clearing redo or dropping old groups moves them to a list
///   in constant time. Each edit then frees the larger of 256 nodes and 4 nodes per
///   leaf of text it added. Every node was made by some edit, so the list cannot grow
///   without limit even when `release_memory` is never called. `replace_built` is the
///   one exception: it frees only 256 nodes, because it must not take time that follows
///   the text's size. So a caller of it must call `take_released` or `release_memory`.
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
    /// A history that binds to a store at its first recorded edit. See `for_store`.
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_UNDO_LIMIT_BYTES)
    }
    /// A history with its own limit, in charged bytes. It binds like `new`.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            typing_end: None,
            group_depth: 0,
            group_started: false,
            state: 0,
            next_state: 1,
            saved: None,
            attached: None,
            undo_charge: 0,
            redo_charge: 0,
            limit,
            trash: Trash::default(),
        }
    }
    /// A history bound to `store` now. The store's text as it stands counts as saved.
    pub fn for_store(store: &PieceStore) -> Self {
        let mut history = Self::new();
        history.attached = Some(store.stamp());
        history.saved = Some(history.state);
        history
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
    /// with no real edit adds nothing. `undo` and `redo` refuse to run while a group
    /// is open and return `EditError::GroupOpen`.
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
    /// Marks the current text as saved. This also closes any open group. With no
    /// bound store yet it marks nothing, since there is no text to name.
    pub fn save_point(&mut self) {
        self.close_group();
        self.saved = self.attached.map(|_| self.state);
    }
    /// True when `store` is the bound store and holds the text of the last
    /// `save_point`, reached only through this history's edits, undo and redo. Typing
    /// a letter and deleting it by hand counts as a change, because the check follows
    /// history and does not compare text.
    pub fn is_at_save_point(&self, store: &PieceStore) -> bool {
        self.saved == Some(self.state) && self.attached == Some(store.stamp())
    }
    pub fn undo_groups(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_groups(&self) -> usize {
        self.redo.len()
    }
    /// Charged bytes in undo and redo. Memory waiting in the release list is not
    /// counted here. See `memory_to_release_bytes`.
    pub fn memory_bytes(&self) -> usize {
        self.undo_charge + self.redo_charge
    }
    /// An estimate of the bytes waiting in the release list.
    pub fn memory_to_release_bytes(&self) -> usize {
        self.trash.pending
    }
    /// Frees all history that is waiting to be released. Its cost grows with the
    /// amount waiting, so call it when idle and not on a key press.
    pub fn release_memory(&mut self) {
        while !self.trash.is_empty() {
            self.trash.release(RELEASE_PER_EDIT);
        }
    }
    /// Takes out all history that is waiting to be released, in constant time, for another
    /// thread to free.
    pub fn take_released(&mut self) -> ReleasedHistory {
        ReleasedHistory(std::mem::take(&mut self.trash))
    }
    /// True when released history is still waiting to be freed.
    pub fn has_memory_to_release(&self) -> bool {
        !self.trash.is_empty()
    }
    /// Forgets all history and treats `store` as the new start. The text counts
    /// as not saved unless `saved` is true.
    pub fn reset(&mut self, store: &PieceStore, saved: bool) {
        let (undo_charge, redo_charge) = (self.undo_charge, self.redo_charge);
        self.trash
            .add_stack(std::mem::take(&mut self.undo), undo_charge);
        self.trash
            .add_stack(std::mem::take(&mut self.redo), redo_charge);
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
    /// Drops old groups while over the limit, at most `EVICT_PER_EDIT` per call.
    fn evict(&mut self) {
        for _ in 0..EVICT_PER_EDIT {
            if self.memory_bytes() <= self.limit {
                return;
            }
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
    /// Like `replace`, and also gives the change in UTF-16 units. See `TrackedChange`.
    ///
    /// With `typing` true, these keep one undo step going while the caret stays put:
    /// an insert at the caret, and a delete that ends at the caret (Backspace). Typing
    /// over a selection starts a step that typing then continues.
    pub fn replace_tracked(
        &mut self,
        store: &mut PieceStore,
        range: Range<ByteOffset>,
        text: &str,
        typing: bool,
    ) -> Result<TrackedChange, EditError> {
        self.check_attached(store)?;
        store.check_range(&range)?;
        if range.start == range.end && text.is_empty() {
            let start = store.utf16_at(range.start.0);
            return Ok(TrackedChange {
                change: store.no_change(range),
                utf16: qe_api::Utf16Change {
                    start,
                    old_end: start,
                    new_end: start,
                },
            });
        }
        let (tracked, removed) = store.splice_text(range.clone(), text);
        let inserted = text.len();
        // Freeing in step with what this edit made keeps the list from growing.
        let release = RELEASE_PER_EDIT.max(4 * (inserted / CHUNK + 1));
        self.record(store, range, inserted, removed, typing, release);
        Ok(tracked)
    }
    /// Like `replace_tracked` for one undo step of its own, with text a worker already made.
    /// Its cost does not follow the size of the text, so it frees only the fewest nodes. The
    /// rest of the release list waits for `take_released` or `release_memory`.
    pub fn replace_built(
        &mut self,
        store: &mut PieceStore,
        range: Range<ByteOffset>,
        text: &BuiltText,
    ) -> Result<TrackedChange, EditError> {
        self.check_attached(store)?;
        store.check_range(&range)?;
        if range.start == range.end && text.is_empty() {
            let start = store.utf16_at(range.start.0);
            return Ok(TrackedChange {
                change: store.no_change(range),
                utf16: qe_api::Utf16Change {
                    start,
                    old_end: start,
                    new_end: start,
                },
            });
        }
        let (tracked, removed) = store.splice_built(range.clone(), text);
        self.record(store, range, text.len(), removed, false, RELEASE_PER_EDIT);
        Ok(tracked)
    }
    /// Writes down an edit that the store has just made.
    fn record(
        &mut self,
        store: &PieceStore,
        range: Range<ByteOffset>,
        inserted: usize,
        removed: Tree,
        typing: bool,
        release: usize,
    ) {
        let empty_range = range.start == range.end;
        self.attached = Some(store.stamp());
        let state = self.fresh_state();
        let at_caret = typing && !self.undo.is_empty();
        let continues_insert = at_caret && empty_range && self.typing_end == Some(range.start.0);
        let continues_delete =
            at_caret && !empty_range && inserted == 0 && self.typing_end == Some(range.end.0);
        let in_open_group = self.group_depth > 0 && self.group_started;
        if continues_delete {
            self.absorb_delete(removed, range.end.0 - range.start.0);
            self.undo.back_mut().expect("checked above").state_after = state;
        } else if continues_insert || in_open_group {
            let group = self.undo.back_mut().expect("an open group is on the stack");
            match group.edits.last_mut() {
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
        self.typing_end = typing.then_some(range.start.0 + inserted);
        if !self.redo.is_empty() {
            let charge = self.redo_charge;
            self.redo_charge = 0;
            self.trash.add_stack(std::mem::take(&mut self.redo), charge);
        }
        self.evict();
        self.trash.release(release);
    }
    /// A Backspace at the end of the newest edit. It takes back typed text first and
    /// then adds what it removed from before the edit.
    fn absorb_delete(&mut self, removed: Tree, deleted: usize) {
        let group = self.undo.back_mut().expect("the caller checked");
        let last = group.edits.last_mut().expect("a group has an edit");
        if deleted <= last.len_in_doc {
            last.len_in_doc -= deleted;
            self.trash.add_tree(removed);
            return;
        }
        let before = deleted - last.len_in_doc;
        let (older, typed) = split(removed, before);
        let old_charge = edit_charge(last);
        last.other = join(older, last.other.take());
        last.start -= before;
        last.len_in_doc = 0;
        let new_charge = edit_charge(last);
        group.charge = group.charge - old_charge + new_charge;
        self.undo_charge = self.undo_charge - old_charge + new_charge;
        self.trash.add_tree(typed);
    }
    pub fn replace(
        &mut self,
        store: &mut PieceStore,
        range: Range<ByteOffset>,
        text: &str,
        typing: bool,
    ) -> Result<TextChange, EditError> {
        self.replace_tracked(store, range, text, typing)
            .map(|tracked| tracked.change)
    }
    /// Undoes the newest group. Returns each change in the order it was applied,
    /// each one in the coordinates left by the change before it. An empty list
    /// means there was nothing to undo.
    pub fn undo(&mut self, store: &mut PieceStore) -> Result<Vec<TextChange>, EditError> {
        self.undo_tracked(store).map(plain_changes)
    }
    /// Redoes the group undone last. The result reads like `undo`'s.
    pub fn redo(&mut self, store: &mut PieceStore) -> Result<Vec<TextChange>, EditError> {
        self.redo_tracked(store).map(plain_changes)
    }
    /// Like `undo`, and each change also gives its UTF-16 place.
    pub fn undo_tracked(
        &mut self,
        store: &mut PieceStore,
    ) -> Result<Vec<TrackedChange>, EditError> {
        self.check_can_swap(store)?;
        let Some(group) = self.undo.back() else {
            return Ok(Vec::new());
        };
        let (edits, changes) = store.swap_edits(&group.edits, true)?;
        let group = self.undo.pop_back().expect("checked above");
        self.finish_swap(store, group, edits, true);
        Ok(changes)
    }
    /// Like `redo`, and each change also gives its UTF-16 place.
    pub fn redo_tracked(
        &mut self,
        store: &mut PieceStore,
    ) -> Result<Vec<TrackedChange>, EditError> {
        self.check_can_swap(store)?;
        let Some(group) = self.redo.back() else {
            return Ok(Vec::new());
        };
        let (edits, changes) = store.swap_edits(&group.edits, false)?;
        let group = self.redo.pop_back().expect("checked above");
        self.finish_swap(store, group, edits, false);
        Ok(changes)
    }
    fn check_can_swap(&self, store: &PieceStore) -> Result<(), EditError> {
        self.check_attached(store)?;
        if self.group_depth > 0 {
            return Err(EditError::GroupOpen);
        }
        Ok(())
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

fn plain_changes(tracked: Vec<TrackedChange>) -> Vec<TextChange> {
    tracked.into_iter().map(|t| t.change).collect()
}
