//! The COM text store that TSF calls.
//!
//! Rules from TSF that shape this file:
//! - No text call works without a lock. A lock comes only through `RequestLock`.
//! - A sync lock request during a lock is refused. An async one runs when the lock ends.
//! - The sink's change calls must not run during a lock, so they wait for its end.
//!
//! No borrow of `State` is held across a call to TSF or to the host, because either can
//! call back into this object.
use crate::{
    ImeHost,
    overlay::{Overlay, Place},
};
use qe_api::{ByteOffset, LookupError, Rect, Utf16Offset, ViewContext};
use qe_editor::{ChangesSince, EditorView, GeometryError};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, HWND, POINT, RECT},
        System::Com::{FORMATETC, IDataObject},
        UI::TextServices::{
            GXFPF_NEAREST, ITextStoreACP, ITextStoreACP_Impl, ITextStoreACPSink,
            ITfCompositionView, ITfContext, ITfContextOwnerCompositionServices,
            ITfContextOwnerCompositionSink, ITfContextOwnerCompositionSink_Impl, ITfRange,
            ITfRangeACP, TEXT_STORE_LOCK_FLAGS, TEXT_STORE_TEXT_CHANGE_FLAGS, TF_DEFAULT_SELECTION,
            TS_AE_END, TS_AE_START, TS_AS_LAYOUT_CHANGE, TS_AS_SEL_CHANGE, TS_AS_STATUS_CHANGE,
            TS_AS_TEXT_CHANGE, TS_ATTRVAL, TS_E_FORMAT, TS_E_INVALIDPOINT, TS_E_INVALIDPOS,
            TS_E_NOLAYOUT, TS_E_NOLOCK, TS_E_NOSELECTION, TS_E_READONLY, TS_E_SYNCHRONOUS,
            TS_IAS_QUERYONLY, TS_LC_CHANGE, TS_LF_READ, TS_LF_READWRITE, TS_LF_SYNC, TS_RT_PLAIN,
            TS_RUNINFO, TS_S_ASYNC, TS_SD_LOADING, TS_SD_READONLY, TS_SELECTION_ACP,
            TS_SELECTIONSTYLE, TS_SS_NOHIDDENTEXT, TS_STATUS, TS_TEXTCHANGE,
        },
    },
    core::{
        BOOL, ComObject, Error, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result,
        implement,
    },
};

const READ: u32 = TS_LF_READ.0;
const WRITE: u32 = TS_LF_READWRITE.0;
/// This store has one view. TSF asks for it with `GetActiveView`.
const VIEW_COOKIE: u32 = 0;
/// One `GetText` answers at most this much. TSF asks again from `pacpNext`.
const MAX_READ_UNITS: usize = 32 * 1024;
/// A composition can take in at most this much of the text around it.
const MAX_SPAN_UNITS: usize = 64 * 1024;
/// The text is not in memory yet. TSF can ask again.
const E_PENDING: HRESULT = HRESULT(0x8000_000A_u32 as i32);
const CONNECT_E_ADVISELIMIT: HRESULT = HRESULT(0x8004_0201_u32 as i32);
const CONNECT_E_NOCONNECTION: HRESULT = HRESULT(0x8004_0200_u32 as i32);

/// How many times each path ran, for tests and for measuring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreCounts {
    pub locks_granted: u32,
    pub sync_refused: u32,
    pub async_queued: u32,
    pub text_changes_sent: u32,
    pub selection_changes_sent: u32,
    pub layout_changes_sent: u32,
    pub status_changes_sent: u32,
    pub ime_updates: u32,
    pub ime_commits: u32,
    pub ime_cancels: u32,
}

struct Advised {
    sink: ITextStoreACPSink,
    identity: IUnknown,
    mask: u32,
}

#[derive(Default)]
struct State {
    advised: Option<Advised>,
    context: Option<ITfContext>,
    /// The lock held now: 0, `READ` or `WRITE`.
    lock: u32,
    /// Async requests for when the lock ends: 0, `READ` or `WRITE`.
    pending: u32,
    overlay: Option<Overlay>,
    /// The overlay changed since the editor last heard.
    dirty: bool,
    /// The last revision TSF knows.
    seen_revision: u64,
    seen_document: u64,
    /// The editor's length that TSF knows.
    known_len: usize,
    last_selection: Range<usize>,
    last_status: u32,
    /// Revisions of edits that TSF itself made, so they are not told back to it.
    own: Vec<u64>,
    owe_sync: bool,
    owe_cancel: bool,
    owe_complete: bool,
    /// TSF got `TS_E_NOLAYOUT`, so it must hear when the layout comes.
    owe_layout: bool,
    /// The host said that the layout changed during a lock.
    layout_moved_in_lock: bool,
    canceling: bool,
    /// An overlay that ended with no commit. TSF still has to hear that its text went.
    dropped: Option<Overlay>,
    closed: bool,
    counts: StoreCounts,
}

#[implement(ITextStoreACP, ITfContextOwnerCompositionSink)]
pub(crate) struct StoreObject {
    host: Rc<dyn ImeHost>,
    state: RefCell<State>,
    notifying: Cell<bool>,
}

/// The host's side of one text store.
pub struct TextStoreHandle {
    object: ComObject<StoreObject>,
}
impl TextStoreHandle {
    /// Makes a store with no TSF behind it. `TsfThread::open_document` gives it to TSF.
    pub fn new(host: Rc<dyn ImeHost>) -> Self {
        let object = ComObject::new(StoreObject {
            host,
            state: RefCell::default(),
            notifying: Cell::new(false),
        });
        object.remember_current();
        Self { object }
    }
    /// The store's COM interface, as TSF holds it.
    pub fn text_store(&self) -> ITextStoreACP {
        self.object.to_interface()
    }
    /// The composition sink's COM interface, as TSF holds it.
    pub fn composition_sink(&self) -> ITfContextOwnerCompositionSink {
        self.object.to_interface()
    }
    pub fn counts(&self) -> StoreCounts {
        self.object.state.borrow().counts
    }
    /// True while TSF has a composition running in this store.
    pub fn is_composing(&self) -> bool {
        self.object.state.borrow().overlay.is_some()
    }
    /// True while an async lock waits.
    pub fn lock_is_waiting(&self) -> bool {
        self.object.state.borrow().pending != 0
    }
    /// Tells TSF what the editor did since the last call, and runs any lock that had to
    /// wait. During a lock it waits for the lock's end. Each call costs one look at the
    /// change log, and one sink call for each change.
    pub fn sync(&self) {
        self.object.sync();
    }
    /// Tells TSF that a place it asked about has changed. It does nothing when no
    /// composition runs and no `TS_E_NOLAYOUT` answer waits, so typing pays nothing.
    pub fn layout_changed(&self) {
        self.object.layout_changed();
    }
    /// Ends the composition in TSF and drops its text. Asks again at the lock's end when
    /// a lock is held, because TSF refuses during a lock.
    pub fn cancel_composition(&self) {
        self.object.cancel_composition();
    }
    /// Ends the composition in TSF and keeps its text.
    pub fn complete_composition(&self) {
        self.object.complete_composition();
    }
    pub(crate) fn set_context(&self, context: &ITfContext) {
        self.object.state.borrow_mut().context = Some(context.clone());
    }
    pub(crate) fn hwnd(&self) -> HWND {
        self.object.host.hwnd()
    }
    pub(crate) fn report(&self, message: String) {
        self.object.host.problem(message);
    }
    pub(crate) fn close(&self) {
        let mut state = self.object.state.borrow_mut();
        state.closed = true;
        state.advised = None;
        state.context = None;
    }
}

fn plain(error: &LookupError) -> HRESULT {
    match error {
        LookupError::NotYetKnown { .. } => E_PENDING,
        LookupError::OutOfRange | LookupError::TooLong { .. } => TS_E_INVALIDPOS,
        LookupError::Failed(_) => E_FAIL,
    }
}
fn acp(position: usize) -> i32 {
    i32::try_from(position).unwrap_or(i32::MAX)
}
fn fail<T>(code: HRESULT) -> Result<T> {
    Err(Error::from_hresult(code))
}
fn utf8_len(units: &[u16]) -> usize {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.map_or(3, char::len_utf8))
        .sum()
}
fn union(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    let mut all = rects.into_iter();
    let first = all.next()?;
    let (mut left, mut top) = (first.x.0, first.y.0);
    let (mut right, mut bottom) = (left + first.width.0, top + first.height.0);
    for r in all {
        left = left.min(r.x.0);
        top = top.min(r.y.0);
        right = right.max(r.x.0 + r.width.0);
        bottom = bottom.max(r.y.0 + r.height.0);
    }
    Some(Rect {
        x: qe_api::Dip(left),
        y: qe_api::Dip(top),
        width: qe_api::Dip(right - left),
        height: qe_api::Dip(bottom - top),
    })
}
fn inside(rect: &Rect, area: &Rect) -> bool {
    rect.x.0 >= area.x.0
        && rect.y.0 >= area.y.0
        && rect.x.0 + rect.width.0 <= area.x.0 + area.width.0
        && rect.y.0 + rect.height.0 <= area.y.0 + area.height.0
}

/// The editor's UTF-16 units, which may start or end inside a surrogate pair.
fn read_editor(view: &EditorView, range: Range<usize>) -> std::result::Result<Vec<u16>, HRESULT> {
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let floor = |at: usize| match view.utf16_to_byte(Utf16Offset(at)) {
        Err(LookupError::OutOfRange) if at > 0 => view
            .utf16_to_byte(Utf16Offset(at - 1))
            .map(|byte| (byte, 1))
            .map_err(|e| plain(&e)),
        other => other.map(|byte| (byte, 0)).map_err(|e| plain(&e)),
    };
    let (first, skip) = floor(range.start)?;
    let (last, cut) = floor(range.end)?;
    // An end inside a pair reads the whole pair and cuts it to one unit below.
    let last = if cut == 1 {
        view.utf16_to_byte(Utf16Offset(range.end + 1))
            .map_err(|e| plain(&e))?
    } else {
        last
    };
    let mut text = String::new();
    view.worker_snapshot()
        .try_visit_chunks(first..last, &mut |s| text.push_str(s))
        .map_err(|e| plain(&e))?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let wanted = range.end - range.start;
    Ok(units.into_iter().skip(skip).take(wanted).collect())
}

impl StoreObject {
    fn remember_current(&self) {
        let now = self.with(|v, _| {
            (
                v.revision(),
                v.document_number(),
                v.store_status().known_utf16.0,
                selection_units(v),
                status_flags(v),
            )
        });
        if let Ok((revision, document, len, selection, status)) = now {
            let mut s = self.state.borrow_mut();
            s.seen_revision = revision;
            s.seen_document = document;
            s.known_len = len;
            s.last_selection = selection.unwrap_or(s.last_selection.clone());
            s.last_status = status;
            s.own.clear();
        }
    }
    fn with<R>(
        &self,
        f: impl FnOnce(&mut EditorView, &mut ViewContext) -> R,
    ) -> std::result::Result<R, HRESULT> {
        let mut f = Some(f);
        let mut out = None;
        self.host.with_view(&mut |view, ctx| {
            if let Some(f) = f.take() {
                out = Some(f(view, ctx));
            }
        });
        out.ok_or(E_PENDING)
    }
    fn with_or_err<R>(&self, f: impl FnOnce(&mut EditorView, &mut ViewContext) -> R) -> Result<R> {
        self.with(f).map_err(Error::from_hresult)
    }
    fn lock_kind(&self) -> u32 {
        self.state.borrow().lock
    }
    fn need_read(&self) -> Result<()> {
        if self.lock_kind() & READ == READ {
            Ok(())
        } else {
            fail(TS_E_NOLOCK)
        }
    }
    fn need_write(&self) -> Result<()> {
        if self.lock_kind() & WRITE == WRITE {
            Ok(())
        } else {
            fail(TS_E_NOLOCK)
        }
    }
    fn count(&self, f: impl FnOnce(&mut StoreCounts)) {
        f(&mut self.state.borrow_mut().counts);
    }
    fn sink(&self) -> Option<(ITextStoreACPSink, u32)> {
        let s = self.state.borrow();
        s.advised.as_ref().map(|a| (a.sink.clone(), a.mask))
    }
    /// Whether the view can take a lock of this kind now. A waiting key must run before
    /// an edit from TSF, or the edit would pass it.
    fn can_grant(&self, kind: u32) -> bool {
        self.with(|v, _| kind != WRITE || v.queued_input_count() == 0)
            .unwrap_or(false)
    }

    // ---- Locks ----

    fn request_lock(&self, flags: u32) -> Result<HRESULT> {
        let sync = flags & TS_LF_SYNC != 0;
        let kind = flags & WRITE;
        if kind != READ && kind != WRITE {
            return fail(E_INVALIDARG);
        }
        let held = {
            let s = self.state.borrow();
            if s.closed || s.advised.is_none() {
                return fail(E_FAIL);
            }
            s.lock != 0
        };
        if held || !self.can_grant(kind) {
            let mut s = self.state.borrow_mut();
            if sync {
                s.counts.sync_refused += 1;
                return Ok(TS_E_SYNCHRONOUS);
            }
            s.pending |= kind;
            s.counts.async_queued += 1;
            return Ok(TS_S_ASYNC);
        }
        // TSF must hear what the editor did before it reads the text.
        self.notify();
        let answer = self.grant(kind);
        self.after_locks();
        Ok(answer)
    }
    /// A sink that fails here costs no text: what it wrote stays, and the lock ends.
    fn grant(&self, kind: u32) -> HRESULT {
        let Some((sink, _)) = self.sink() else {
            return E_FAIL;
        };
        {
            let mut s = self.state.borrow_mut();
            s.lock = kind;
            s.counts.locks_granted += 1;
        }
        // SAFETY: a live sink, called on its own thread.
        let answer = unsafe { sink.OnLockGranted(TEXT_STORE_LOCK_FLAGS(kind)) };
        self.state.borrow_mut().lock = 0;
        match answer {
            Ok(()) => HRESULT(0),
            Err(e) => {
                self.host.problem(format!(
                    "An input method failed while it held the text: {e}. Your text is safe."
                ));
                e.code()
            }
        }
    }
    /// Runs when no lock is held: waiting locks, then all that had to wait for no lock.
    fn after_locks(&self) {
        loop {
            let kind = std::mem::take(&mut self.state.borrow_mut().pending);
            if kind == 0 {
                break;
            }
            let kind = if kind & WRITE == WRITE { WRITE } else { READ };
            if !self.can_grant(kind) {
                self.state.borrow_mut().pending = kind;
                break;
            }
            // `grant` shows any failure, and no caller waits for this answer.
            let _ = self.grant(kind);
        }
        self.push_overlay();
        let (cancel, complete) = {
            let mut s = self.state.borrow_mut();
            (
                std::mem::take(&mut s.owe_cancel),
                std::mem::take(&mut s.owe_complete),
            )
        };
        if cancel {
            self.cancel_composition();
        } else if complete {
            self.complete_composition();
        }
        self.tell_dropped();
        self.notify();
    }
    fn sync(&self) {
        let (locked, pending) = {
            let s = self.state.borrow();
            (s.lock != 0, s.pending != 0)
        };
        if locked {
            self.state.borrow_mut().owe_sync = true;
            return;
        }
        if pending {
            self.after_locks();
        } else {
            self.notify();
        }
    }

    // ---- From the editor to TSF ----

    fn notify(&self) {
        if self.state.borrow().lock != 0 {
            self.state.borrow_mut().owe_sync = true;
            return;
        }
        if self.notifying.replace(true) {
            return;
        }
        self.notify_now();
        self.notifying.set(false);
    }
    fn notify_now(&self) {
        self.state.borrow_mut().owe_sync = false;
        self.end_composition_the_editor_ended();
        // One change at a time. TSF can take a lock and edit during any sink call.
        while let Some(change) = self.next_change() {
            let Some(change) = change else { continue };
            if !self.send_text_change(change) {
                return;
            }
        }
        self.send_selection_and_status();
    }
    /// `None` at the end of the list. `Some(None)` for a step with nothing to tell.
    fn next_change(&self) -> Option<Option<PendingChange>> {
        let (seen, document, known) = {
            let s = self.state.borrow();
            (s.seen_revision, s.seen_document, s.known_len)
        };
        let step = self
            .with(|v, _| {
                let revision = v.revision();
                let len = v.store_status().known_utf16.0;
                if v.document_number() != document {
                    return Step::All {
                        revision,
                        len,
                        document: v.document_number(),
                        grown_only: false,
                    };
                }
                match v.changes_since(seen) {
                    ChangesSince::Current if len == known => Step::Done,
                    // A mapped file's index found more text, or the log lost track.
                    ChangesSince::Current | ChangesSince::Rescan => Step::All {
                        revision,
                        len,
                        document,
                        grown_only: v.worker_snapshot().is_read_only(),
                    },
                    ChangesSince::Changes(list) => match list.first() {
                        Some(first) => Step::One {
                            revision: first.change.revision,
                            start: first.utf16.start.0,
                            old_end: first.utf16.old_end.0,
                            new_end: first.utf16.new_end.0,
                        },
                        None => Step::Skip { revision },
                    },
                }
            })
            .ok()?;
        let mut s = self.state.borrow_mut();
        match step {
            Step::Done => {
                s.own.clear();
                None
            }
            Step::Skip { revision } => {
                s.seen_revision = revision;
                Some(None)
            }
            Step::All {
                revision,
                len,
                document,
                grown_only,
            } => {
                let grown = grown_only && document == s.seen_document && len > known;
                s.own.clear();
                Some(Some(PendingChange {
                    revision,
                    document,
                    start: if grown { known } else { 0 },
                    old_end: known,
                    new_end: len,
                    new_len: len,
                }))
            }
            Step::One {
                revision,
                start,
                old_end,
                new_end,
            } => {
                let new_len = (known + new_end).saturating_sub(old_end);
                if let Some(at) = s.own.iter().position(|own| *own == revision) {
                    s.own.swap_remove(at);
                    s.seen_revision = revision;
                    s.known_len = new_len;
                    return Some(None);
                }
                Some(Some(PendingChange {
                    revision,
                    document,
                    start,
                    old_end,
                    new_end,
                    new_len,
                }))
            }
        }
    }
    /// Returns false when the sink failed. The change stays owed for the next `sync`.
    fn send_text_change(&self, change: PendingChange) -> bool {
        if let Some((sink, mask)) = self.sink()
            && mask & TS_AS_TEXT_CHANGE != 0
        {
            let tell = TS_TEXTCHANGE {
                acpStart: acp(change.start),
                acpOldEnd: acp(change.old_end),
                acpNewEnd: acp(change.new_end),
            };
            // SAFETY: a live sink, and `tell` lives for the call.
            let sent = unsafe { sink.OnTextChange(TEXT_STORE_TEXT_CHANGE_FLAGS(0), &tell) };
            if let Err(e) = sent {
                self.host.problem(format!(
                    "An input method did not hear about an edit: {e}. Your text is safe, \
                     and the next edit tries again."
                ));
                return false;
            }
            self.count(|c| c.text_changes_sent += 1);
        }
        let mut s = self.state.borrow_mut();
        s.seen_revision = change.revision;
        s.seen_document = change.document;
        s.known_len = change.new_len;
        true
    }
    fn send_selection_and_status(&self) {
        let composing = self.state.borrow().overlay.is_some();
        let Ok((selection, status)) = self.with(|v, _| (selection_units(v), status_flags(v)))
        else {
            return;
        };
        let advised = self.sink();
        if !composing
            && let Some(selection) = selection
            && selection != self.state.borrow().last_selection
        {
            if let Some((sink, mask)) = &advised
                && mask & TS_AS_SEL_CHANGE != 0
            {
                // SAFETY: a live sink, called on its own thread.
                if let Err(e) = unsafe { sink.OnSelectionChange() } {
                    self.host
                        .problem(format!("An input method missed a caret move: {e}."));
                    return;
                }
                self.count(|c| c.selection_changes_sent += 1);
            }
            self.state.borrow_mut().last_selection = selection;
        }
        if status != self.state.borrow().last_status {
            if let Some((sink, mask)) = &advised
                && mask & TS_AS_STATUS_CHANGE != 0
            {
                // SAFETY: a live sink, called on its own thread.
                if let Err(e) = unsafe { sink.OnStatusChange(status) } {
                    self.host.problem(format!(
                        "An input method missed that the file became read only or finished loading: {e}."
                    ));
                    return;
                }
                self.count(|c| c.status_changes_sent += 1);
            }
            self.state.borrow_mut().last_status = status;
        }
        if std::mem::take(&mut self.state.borrow_mut().layout_moved_in_lock) {
            self.send_layout();
        }
    }
    fn layout_changed(&self) {
        let mut s = self.state.borrow_mut();
        if s.overlay.is_none() && !s.owe_layout {
            return;
        }
        if s.lock != 0 {
            s.layout_moved_in_lock = true;
            s.owe_sync = true;
            return;
        }
        s.owe_layout = false;
        drop(s);
        self.send_layout();
    }
    fn send_layout(&self) {
        if let Some((sink, mask)) = self.sink()
            && mask & TS_AS_LAYOUT_CHANGE != 0
        {
            // SAFETY: a live sink, called on its own thread.
            match unsafe { sink.OnLayoutChange(TS_LC_CHANGE, VIEW_COOKIE) } {
                Ok(()) => self.count(|c| c.layout_changes_sent += 1),
                Err(e) => self.host.problem(format!(
                    "An input method may show its list in the wrong place: {e}."
                )),
            }
        }
    }

    // ---- Ending a composition ----

    /// Esc, or an edit or caret move in the editor, ends the composition there first.
    fn end_composition_the_editor_ended(&self) {
        {
            let s = self.state.borrow();
            if s.overlay.is_none() || s.canceling {
                return;
            }
        }
        if self.with(|v, _| v.composition().is_none()).unwrap_or(false) {
            self.cancel_composition();
        }
    }
    fn terminate_in_tsf(&self) {
        let context = self.state.borrow().context.clone();
        let Some(context) = context else { return };
        let ended = context
            .cast::<ITfContextOwnerCompositionServices>()
            // SAFETY: a live interface on its own thread. `None` ends all compositions.
            .and_then(|services| unsafe {
                services.TerminateComposition(None::<&ITfCompositionView>)
            });
        if let Err(e) = ended {
            self.host.problem(format!(
                "Windows would not end the input method's composition: {e}."
            ));
        }
    }
    fn cancel_composition(&self) {
        {
            let mut s = self.state.borrow_mut();
            if s.overlay.is_none() || s.canceling {
                return;
            }
            if s.lock != 0 {
                s.owe_cancel = true;
                return;
            }
            s.canceling = true;
        }
        self.terminate_in_tsf();
        {
            let mut s = self.state.borrow_mut();
            if let Some(overlay) = s.overlay.take() {
                s.dropped = Some(overlay);
            }
            s.canceling = false;
            s.dirty = false;
        }
        let ended = self.with(|v, c| {
            if v.composition().is_some() {
                v.ime_cancel(c);
                true
            } else {
                false
            }
        });
        if ended.unwrap_or(false) {
            self.count(|c| c.ime_cancels += 1);
        }
        self.tell_dropped();
    }
    fn complete_composition(&self) {
        {
            let mut s = self.state.borrow_mut();
            if s.overlay.is_none() {
                return;
            }
            if s.lock != 0 {
                s.owe_complete = true;
                return;
            }
        }
        self.terminate_in_tsf();
        // No TSF, or TSF did not call back: the text still goes to the editor.
        self.commit_overlay();
        self.tell_dropped();
        self.notify();
    }
    /// TSF believes that the overlay's text is in the document. Tells it the text went.
    fn tell_dropped(&self) {
        let Some(overlay) = self.state.borrow_mut().dropped.take() else {
            return;
        };
        self.state.borrow_mut().dirty = false;
        if overlay.text != overlay.original
            && let Some((sink, mask)) = self.sink()
            && mask & TS_AS_TEXT_CHANGE != 0
        {
            let tell = TS_TEXTCHANGE {
                acpStart: acp(overlay.base.start),
                acpOldEnd: acp(overlay.end()),
                acpNewEnd: acp(overlay.base.end),
            };
            // SAFETY: a live sink, and `tell` lives for the call.
            match unsafe { sink.OnTextChange(TEXT_STORE_TEXT_CHANGE_FLAGS(0), &tell) } {
                Ok(()) => self.count(|c| c.text_changes_sent += 1),
                Err(e) => self.host.problem(format!(
                    "An input method did not hear that its text was dropped: {e}."
                )),
            }
        }
        // The selection changed for TSF, even if the editor's stayed.
        if let Some((sink, mask)) = self.sink()
            && mask & TS_AS_SEL_CHANGE != 0
        {
            // SAFETY: a live sink, called on its own thread.
            match unsafe { sink.OnSelectionChange() } {
                Ok(()) => self.count(|c| c.selection_changes_sent += 1),
                Err(e) => self
                    .host
                    .problem(format!("An input method missed a caret move: {e}.")),
            }
        }
    }
    /// Puts the overlay's text in the editor, as one edit.
    fn commit_overlay(&self) {
        let Some(overlay) = self.state.borrow_mut().overlay.take() else {
            return;
        };
        self.state.borrow_mut().dirty = false;
        let done = self.with(|v, c| {
            let start = v.utf16_to_byte(Utf16Offset(overlay.base.start));
            let end = v.utf16_to_byte(Utf16Offset(overlay.base.end));
            let (Ok(start), Ok(end)) = (start, end) else {
                return Commit::Lost;
            };
            let before = v.revision();
            if overlay.text == overlay.original {
                v.ime_cancel(c);
            } else if !v.can_edit() {
                v.ime_cancel(c);
                return Commit::Refused;
            } else {
                let text = String::from_utf16_lossy(&overlay.text);
                if text.is_empty() {
                    v.ime_cancel(c);
                    v.replace_range(start..end, "", c);
                } else {
                    if v.selection() != (start..end) {
                        v.ime_cancel(c);
                        v.select(start..end, c);
                    }
                    v.ime_commit(&text, c);
                }
            }
            if let (Ok(a), Ok(b)) = (
                v.utf16_to_byte(Utf16Offset(overlay.selection.start)),
                v.utf16_to_byte(Utf16Offset(overlay.selection.end)),
            ) && v.selection() != (a..b)
            {
                v.select(a..b, c);
            }
            Commit::Done {
                before,
                after: v.revision(),
                selection: selection_units(v),
            }
        });
        match done {
            Ok(Commit::Done {
                before,
                after,
                selection,
            }) => {
                let mut s = self.state.borrow_mut();
                if after != before {
                    s.own.push(after);
                    s.counts.ime_commits += 1;
                } else {
                    s.counts.ime_cancels += 1;
                }
                if let Some(selection) = selection {
                    s.last_selection = selection;
                }
            }
            Ok(Commit::Refused) => {
                let text = String::from_utf16_lossy(&overlay.text);
                self.host.problem(format!(
                    "This file became read only, so this input was not added: {text}"
                ));
                self.state.borrow_mut().dropped = Some(overlay);
            }
            Ok(Commit::Lost) | Err(_) => {
                let text = String::from_utf16_lossy(&overlay.text);
                self.host.problem(format!(
                    "The text moved while this input was on its way, so it was not added: {text}"
                ));
                self.state.borrow_mut().dropped = Some(overlay);
            }
        }
    }
    /// Sends the overlay's text to the editor once per lock, not once per TSF write.
    fn push_overlay(&self) {
        let update = {
            let mut s = self.state.borrow_mut();
            if !std::mem::take(&mut s.dirty) {
                return;
            }
            s.overlay
                .as_ref()
                .map(|o| (String::from_utf16_lossy(&o.text), o.cursor()))
        };
        if let Some((text, cursor)) = update {
            if self.with(|v, c| v.ime_update(&text, cursor, c)).is_ok() {
                self.count(|c| c.ime_updates += 1);
            } else {
                self.state.borrow_mut().dirty = true;
            }
        }
    }

    // ---- Positions ----

    fn editor_len(&self) -> Result<usize> {
        self.with_or_err(|v, _| v.store_status().known_utf16.0)
    }
    /// The whole document's length as TSF sees it.
    fn virtual_len(&self) -> Result<usize> {
        let len = self.editor_len()?;
        Ok(match &self.state.borrow().overlay {
            Some(o) => o.len(len),
            None => len,
        })
    }
    /// Checks a TSF range. Reading allows an end of -1, for the document's end.
    fn checked(&self, start: i32, end: i32) -> Result<Range<usize>> {
        let len = self.virtual_len()?;
        let start = usize::try_from(start).map_err(|_| Error::from_hresult(TS_E_INVALIDPOS))?;
        let end = if end == -1 {
            len
        } else {
            usize::try_from(end).map_err(|_| Error::from_hresult(TS_E_INVALIDPOS))?
        };
        if start > end || end > len {
            return fail(TS_E_INVALIDPOS);
        }
        Ok(start..end)
    }
    /// Whether a TSF position is between characters.
    fn on_edge(&self, position: usize) -> Result<bool> {
        let place = self
            .state
            .borrow()
            .overlay
            .as_ref()
            .map(|o| (o.place(position), o.text.clone()));
        let at = match place {
            Some((Place::Inside(unit), text)) => {
                return Ok(text.get(unit).is_none_or(|u| !(0xdc00..0xe000).contains(u)));
            }
            Some((Place::Before(at) | Place::After(at), _)) => at,
            None => position,
        };
        match self.with_or_err(|v, _| v.utf16_to_byte(Utf16Offset(at)))? {
            Ok(_) => Ok(true),
            Err(LookupError::OutOfRange) => Ok(false),
            Err(other) => fail(plain(&other)),
        }
    }
    /// Reads TSF positions, through the overlay.
    fn read_virtual(&self, range: Range<usize>) -> Result<Vec<u16>> {
        let overlay = self.state.borrow().overlay.clone();
        let read = |r: Range<usize>| -> Result<Vec<u16>> {
            self.with_or_err(|v, _| read_editor(v, r))?
                .map_err(Error::from_hresult)
        };
        let Some(o) = overlay else {
            return read(range);
        };
        let mut out = Vec::with_capacity(range.len());
        if range.start < o.base.start {
            out.extend(read(range.start..range.end.min(o.base.start))?);
        }
        let (a, b) = (range.start.max(o.base.start), range.end.min(o.end()));
        if a < b {
            out.extend_from_slice(
                o.text
                    .get(a - o.base.start..b - o.base.start)
                    .unwrap_or(&[]),
            );
        }
        if range.end > o.end() {
            let from = range.start.max(o.end()) - o.end() + o.base.end;
            let to = range.end - o.end() + o.base.end;
            out.extend(read(from..to)?);
        }
        Ok(out)
    }
    fn editor_bytes(&self, range: &Range<usize>) -> Result<Range<ByteOffset>> {
        let (start, end) = self.with_or_err(|v, _| {
            (
                v.utf16_to_byte(Utf16Offset(range.start)),
                v.utf16_to_byte(Utf16Offset(range.end)),
            )
        })?;
        Ok(start.map_err(|e| Error::from_hresult(plain(&e)))?
            ..end.map_err(|e| Error::from_hresult(plain(&e)))?)
    }
    fn current_selection(&self) -> Result<(Range<usize>, bool, bool)> {
        if let Some(o) = &self.state.borrow().overlay {
            return Ok((o.selection.clone(), o.active_start, o.interim));
        }
        let (selection, at_start) = self.with_or_err(|v, _| {
            let selection = v.selection();
            let at_start = v.caret() == selection.start && selection.start != selection.end;
            (selection_units(v), at_start)
        })?;
        let selection = selection.ok_or_else(|| Error::from_hresult(TS_E_NOSELECTION))?;
        Ok((selection, at_start, false))
    }

    // ---- Writing ----

    fn write(&self, range: Range<usize>, text: &[u16]) -> Result<()> {
        if self.state.borrow().overlay.is_some() {
            self.grow_overlay(&range)?;
            let mut s = self.state.borrow_mut();
            if let Some(o) = s.overlay.as_mut() {
                o.replace(range, text);
            }
            s.dirty = true;
            return Ok(());
        }
        let new = String::from_utf16(text).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        let bytes = self.editor_bytes(&range)?;
        let (before, after, selection) = self.with_or_err(|v, c| {
            let before = v.revision();
            v.replace_range(bytes.clone(), &new, c);
            (before, v.revision(), selection_units(v))
        })?;
        let mut s = self.state.borrow_mut();
        if before != after {
            s.own.push(after);
        }
        if let Some(selection) = selection {
            s.last_selection = selection;
        }
        Ok(())
    }
    /// A write that reaches past the span takes that text into the span. An edit in the
    /// editor itself would end the composition.
    fn grow_overlay(&self, range: &Range<usize>) -> Result<()> {
        let Some(o) = self.state.borrow().overlay.clone() else {
            return Ok(());
        };
        let left = o.base.start.saturating_sub(range.start);
        let right = match o.place(range.end) {
            Place::After(at) => at - o.base.end,
            _ => 0,
        };
        if left == 0 && right == 0 {
            return Ok(());
        }
        if o.text.len() + left + right > MAX_SPAN_UNITS {
            return fail(TS_E_INVALIDPOS);
        }
        let (before, after) = self.with_or_err(|v, _| {
            (
                read_editor(v, o.base.start - left..o.base.start),
                read_editor(v, o.base.end..o.base.end + right),
            )
        })?;
        let before = before.map_err(Error::from_hresult)?;
        let after = after.map_err(Error::from_hresult)?;
        let base = o.base.start - left..o.base.end + right;
        let bytes = self.editor_bytes(&base)?;
        // The composition moves to the span's new start, so that nothing shows twice.
        self.with_or_err(|v, c| {
            v.ime_cancel(c);
            v.select(bytes.clone(), c);
            v.ime_start();
        })?;
        let mut s = self.state.borrow_mut();
        if let Some(o) = s.overlay.as_mut() {
            o.absorb(&before, &after);
        }
        s.dirty = true;
        Ok(())
    }

    // ---- Geometry ----

    fn no_layout<T>(&self) -> Result<T> {
        self.state.borrow_mut().owe_layout = true;
        fail(TS_E_NOLAYOUT)
    }
    fn geometry_error<T>(&self, error: &GeometryError) -> Result<T> {
        match error {
            GeometryError::NotLaidOut
            | GeometryError::TooManyLines { .. }
            | GeometryError::Lookup(LookupError::NotYetKnown { .. }) => self.no_layout(),
            GeometryError::Lookup(other) => fail(plain(other)),
        }
    }
    fn text_rect(&self, range: Range<usize>) -> Result<(Rect, bool)> {
        let overlay = self.state.borrow().overlay.clone();
        if let Some(o) = overlay
            && range.start <= o.end()
            && range.end >= o.base.start
        {
            return self.composition_rect(&o, range);
        }
        // Past a composition, the same text in the editor's positions.
        let overlay = self.state.borrow().overlay.clone();
        let to_editor = |p: usize| {
            overlay.as_ref().map_or(p, |o| match o.place(p) {
                Place::Before(at) | Place::After(at) => at,
                Place::Inside(_) => o.base.start,
            })
        };
        let range = to_editor(range.start)..to_editor(range.end);
        let bytes = self.editor_bytes(&range)?;
        let (rects, area) =
            self.with_or_err(|v, _| (v.range_rects(bytes.clone()), v.text_area()))?;
        let rects = match rects {
            Ok(rects) => rects,
            // Over the limit: the start of the range, which TSF is told is cut short.
            Err(GeometryError::TooManyLines { .. }) => {
                let at = bytes.start..bytes.start;
                let rects = self.with_or_err(|v, _| v.range_rects(at))?;
                return match rects.map(|r| union(r.into_iter().map(|r| r.rect))) {
                    Ok(Some(rect)) => Ok((rect, true)),
                    Ok(None) => self.no_layout(),
                    Err(e) => self.geometry_error(&e),
                };
            }
            Err(e) => return self.geometry_error(&e),
        };
        let Some(rect) = union(rects.into_iter().map(|r| r.rect)) else {
            return self.no_layout();
        };
        let clipped = !inside(&rect, &area);
        Ok((rect, clipped))
    }
    /// Answers from `composition_rects`, which has one rectangle a row. A hook for part of
    /// a row does not exist yet, so an ask for part of a row gets the whole row.
    fn composition_rect(&self, o: &Overlay, range: Range<usize>) -> Result<(Rect, bool)> {
        let (rects, caret, area) =
            self.with_or_err(|v, _| (v.composition_rects(), v.caret_rect(), v.text_area()))?;
        let rects = match rects {
            Ok(rects) => rects,
            Err(e) => return self.geometry_error(&e),
        };
        let Some(origin) = rects.first().map(|r| r.range.start.0) else {
            return self.no_layout();
        };
        if range.is_empty()
            && range.start == o.base.start + o.cursor()
            && let Ok(caret) = caret
        {
            return Ok((caret, !inside(&caret, &area)));
        }
        let unit = |p: usize| p.saturating_sub(o.base.start).min(o.text.len());
        let (a, b) = (unit(range.start), unit(range.end));
        let first = utf8_len(o.text.get(..a).unwrap_or(&[]));
        let last = utf8_len(o.text.get(..b).unwrap_or(&[]));
        let touching = rects
            .iter()
            .filter(|r| {
                let (s, e) = (
                    r.range.start.0.saturating_sub(origin),
                    r.range.end.0.saturating_sub(origin),
                );
                s <= last && first <= e
            })
            .map(|r| r.rect);
        let Some(rect) = union(touching).or_else(|| union(rects.iter().map(|r| r.rect))) else {
            return self.no_layout();
        };
        Ok((rect, !inside(&rect, &area)))
    }
}

struct PendingChange {
    revision: u64,
    document: u64,
    start: usize,
    old_end: usize,
    new_end: usize,
    new_len: usize,
}
enum Step {
    Done,
    Skip {
        revision: u64,
    },
    All {
        revision: u64,
        len: usize,
        document: u64,
        /// A mapped file: its text never changes, and its index only finds more of it.
        grown_only: bool,
    },
    One {
        revision: u64,
        start: usize,
        old_end: usize,
        new_end: usize,
    },
}
enum Commit {
    Done {
        before: u64,
        after: u64,
        selection: Option<Range<usize>>,
    },
    Refused,
    Lost,
}

fn selection_units(view: &EditorView) -> Option<Range<usize>> {
    let selection = view.selection();
    let start = view.byte_to_utf16(selection.start).ok()?;
    let end = view.byte_to_utf16(selection.end).ok()?;
    Some(start.0..end.0)
}
fn status_flags(view: &EditorView) -> u32 {
    let mut flags = 0;
    if !view.can_edit() {
        flags |= TS_SD_READONLY;
    }
    if !view.store_status().complete {
        flags |= TS_SD_LOADING;
    }
    flags
}

#[allow(non_snake_case)]
impl ITextStoreACP_Impl for StoreObject_Impl {
    fn AdviseSink(&self, riid: *const GUID, punk: Ref<IUnknown>, dwmask: u32) -> Result<()> {
        if riid.is_null() {
            return fail(E_INVALIDARG);
        }
        // SAFETY: not null, and TSF passes a GUID that lives for the call.
        if unsafe { *riid } != ITextStoreACPSink::IID {
            return fail(E_INVALIDARG);
        }
        let punk = punk.ok()?;
        let identity = punk.cast::<IUnknown>()?;
        let mut s = self.state.borrow_mut();
        if let Some(advised) = s.advised.as_mut() {
            if advised.identity == identity {
                advised.mask = dwmask;
                return Ok(());
            }
            return fail(CONNECT_E_ADVISELIMIT);
        }
        s.advised = Some(Advised {
            sink: punk.cast()?,
            identity,
            mask: dwmask,
        });
        drop(s);
        // TSF reads the text from here on, so it needs to hear only what comes after.
        self.remember_current();
        Ok(())
    }
    fn UnadviseSink(&self, punk: Ref<IUnknown>) -> Result<()> {
        let identity = punk.ok()?.cast::<IUnknown>()?;
        let mut s = self.state.borrow_mut();
        if s.advised.as_ref().is_some_and(|a| a.identity == identity) {
            s.advised = None;
            Ok(())
        } else {
            fail(CONNECT_E_NOCONNECTION)
        }
    }
    fn RequestLock(&self, dwlockflags: u32) -> Result<HRESULT> {
        self.request_lock(dwlockflags)
    }
    fn GetStatus(&self) -> Result<TS_STATUS> {
        let dynamic = self
            .with(|v, _| status_flags(v))
            .unwrap_or_else(|_| self.state.borrow().last_status);
        Ok(TS_STATUS {
            dwDynamicFlags: dynamic,
            dwStaticFlags: TS_SS_NOHIDDENTEXT,
        })
    }
    fn QueryInsert(
        &self,
        acpteststart: i32,
        acptestend: i32,
        cch: u32,
        pacpresultstart: *mut i32,
        pacpresultend: *mut i32,
    ) -> Result<()> {
        if pacpresultstart.is_null() || pacpresultend.is_null() {
            return fail(E_INVALIDARG);
        }
        if self.with_or_err(|v, _| !v.can_edit())? {
            return fail(TS_E_READONLY);
        }
        let range = self
            .checked(acpteststart, acptestend)
            .map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        // A position inside a surrogate pair moves out to the whole pair.
        let start = if self.on_edge(range.start)? {
            range.start
        } else {
            range.start - 1
        };
        // SAFETY: both are checked above, and TSF passes out pointers that live for the call.
        unsafe {
            *pacpresultstart = acp(start);
            *pacpresultend = acp(start).saturating_add(i32::try_from(cch).unwrap_or(i32::MAX));
        }
        Ok(())
    }
    fn GetSelection(
        &self,
        ulindex: u32,
        ulcount: u32,
        pselection: *mut TS_SELECTION_ACP,
        pcfetched: *mut u32,
    ) -> Result<()> {
        self.need_read()?;
        if pcfetched.is_null() || (ulcount > 0 && pselection.is_null()) {
            return fail(E_INVALIDARG);
        }
        // SAFETY: checked above.
        unsafe { *pcfetched = 0 };
        if ulindex != 0 && ulindex != TF_DEFAULT_SELECTION {
            return fail(TS_E_NOSELECTION);
        }
        if ulcount == 0 {
            return Ok(());
        }
        let (selection, at_start, interim) = self.current_selection()?;
        // SAFETY: checked above, and `ulcount` is at least 1.
        unsafe {
            *pselection = TS_SELECTION_ACP {
                acpStart: acp(selection.start),
                acpEnd: acp(selection.end),
                style: TS_SELECTIONSTYLE {
                    ase: if at_start { TS_AE_START } else { TS_AE_END },
                    fInterimChar: BOOL::from(interim),
                },
            };
            *pcfetched = 1;
        }
        Ok(())
    }
    fn SetSelection(&self, ulcount: u32, pselection: *const TS_SELECTION_ACP) -> Result<()> {
        self.need_write()?;
        if ulcount == 0 || pselection.is_null() {
            return fail(E_INVALIDARG);
        }
        // SAFETY: checked above.
        let asked = unsafe { *pselection };
        if asked.acpEnd == -1 {
            return fail(TS_E_INVALIDPOS);
        }
        let range = self.checked(asked.acpStart, asked.acpEnd)?;
        // A position inside a surrogate pair moves out to the whole pair.
        let start = if self.on_edge(range.start)? {
            range.start
        } else {
            range.start - 1
        };
        let end = if self.on_edge(range.end)? {
            range.end
        } else {
            range.end + 1
        };
        {
            let mut s = self.state.borrow_mut();
            if let Some(o) = s.overlay.as_mut() {
                o.selection = start..end;
                o.active_start = asked.style.ase == TS_AE_START;
                o.interim = asked.style.fInterimChar.as_bool();
                s.dirty = true;
                return Ok(());
            }
        }
        let bytes = self.editor_bytes(&(start..end))?;
        let selection = self.with_or_err(|v, c| {
            v.select(bytes.clone(), c);
            selection_units(v)
        })?;
        if let Some(selection) = selection {
            self.state.borrow_mut().last_selection = selection;
        }
        Ok(())
    }
    fn GetText(
        &self,
        acpstart: i32,
        acpend: i32,
        pchplain: PWSTR,
        cchplainreq: u32,
        pcchplainret: *mut u32,
        prgruninfo: *mut TS_RUNINFO,
        cruninforeq: u32,
        pcruninforet: *mut u32,
        pacpnext: *mut i32,
    ) -> Result<()> {
        self.need_read()?;
        if pcchplainret.is_null() || pcruninforet.is_null() || pacpnext.is_null() {
            return fail(E_INVALIDARG);
        }
        let range = self.checked(acpstart, acpend)?;
        let room = if pchplain.is_null() {
            0
        } else {
            cchplainreq as usize
        };
        let count = range.len().min(room).min(MAX_READ_UNITS);
        let units = self.read_virtual(range.start..range.start + count)?;
        // SAFETY: TSF gives room for `cchplainreq` units and `cruninforeq` runs, and the
        // out pointers are checked above. `units` is at most `count`, so it fits.
        unsafe {
            if !units.is_empty() {
                std::ptr::copy_nonoverlapping(units.as_ptr(), pchplain.0, units.len());
            }
            *pcchplainret = units.len() as u32;
            *pacpnext = acp(range.start + units.len());
            if cruninforeq > 0 && !prgruninfo.is_null() && !units.is_empty() {
                *prgruninfo = TS_RUNINFO {
                    uCount: units.len() as u32,
                    r#type: TS_RT_PLAIN,
                };
                *pcruninforet = 1;
            } else {
                *pcruninforet = 0;
            }
        }
        Ok(())
    }
    fn SetText(
        &self,
        _dwflags: u32,
        acpstart: i32,
        acpend: i32,
        pchtext: &PCWSTR,
        cch: u32,
    ) -> Result<TS_TEXTCHANGE> {
        self.need_write()?;
        if self.with_or_err(|v, _| !v.can_edit())? {
            return fail(TS_E_READONLY);
        }
        if acpend == -1 {
            return fail(TS_E_INVALIDPOS);
        }
        let range = self.checked(acpstart, acpend)?;
        if !self.on_edge(range.start)? || !self.on_edge(range.end)? {
            return fail(TS_E_INVALIDPOS);
        }
        let text = units_from(pchtext, cch)?;
        self.write(range.clone(), text)?;
        Ok(TS_TEXTCHANGE {
            acpStart: acp(range.start),
            acpOldEnd: acp(range.end),
            acpNewEnd: acp(range.start + text.len()),
        })
    }
    fn GetFormattedText(&self, _acpstart: i32, _acpend: i32) -> Result<IDataObject> {
        fail(E_NOTIMPL)
    }
    fn GetEmbedded(
        &self,
        _acppos: i32,
        _service: *const GUID,
        _riid: *const GUID,
    ) -> Result<IUnknown> {
        fail(E_NOTIMPL)
    }
    fn QueryInsertEmbedded(
        &self,
        _service: *const GUID,
        _format: *const FORMATETC,
    ) -> Result<BOOL> {
        Ok(BOOL(0))
    }
    fn InsertEmbedded(
        &self,
        _dwflags: u32,
        _acpstart: i32,
        _acpend: i32,
        _data: Ref<IDataObject>,
    ) -> Result<TS_TEXTCHANGE> {
        fail(TS_E_FORMAT)
    }
    fn InsertTextAtSelection(
        &self,
        dwflags: u32,
        pchtext: &PCWSTR,
        cch: u32,
        pacpstart: *mut i32,
        pacpend: *mut i32,
        pchange: *mut TS_TEXTCHANGE,
    ) -> Result<()> {
        let query = dwflags & TS_IAS_QUERYONLY != 0;
        if query {
            self.need_read()?;
        } else {
            self.need_write()?;
        }
        if self.with_or_err(|v, _| !v.can_edit())? {
            return fail(TS_E_READONLY);
        }
        let (selection, _, _) = self.current_selection()?;
        if query {
            // SAFETY: each is checked, and TSF's out pointers live for the call.
            unsafe {
                if !pacpstart.is_null() {
                    *pacpstart = acp(selection.start);
                }
                if !pacpend.is_null() {
                    *pacpend = acp(selection.end);
                }
            }
            return Ok(());
        }
        let text = units_from(pchtext, cch)?;
        self.write(selection.clone(), text)?;
        let new_end = selection.start + text.len();
        // SAFETY: each is checked, and TSF's out pointers live for the call.
        unsafe {
            if !pacpstart.is_null() {
                *pacpstart = acp(selection.start);
            }
            if !pacpend.is_null() {
                *pacpend = acp(new_end);
            }
            if !pchange.is_null() {
                *pchange = TS_TEXTCHANGE {
                    acpStart: acp(selection.start),
                    acpOldEnd: acp(selection.end),
                    acpNewEnd: acp(new_end),
                };
            }
        }
        Ok(())
    }
    fn InsertEmbeddedAtSelection(
        &self,
        _dwflags: u32,
        _data: Ref<IDataObject>,
        _start: *mut i32,
        _end: *mut i32,
        _change: *mut TS_TEXTCHANGE,
    ) -> Result<()> {
        fail(TS_E_FORMAT)
    }
    fn RequestSupportedAttrs(&self, _flags: u32, _count: u32, _ids: *const GUID) -> Result<()> {
        Ok(())
    }
    fn RequestAttrsAtPosition(
        &self,
        _at: i32,
        _n: u32,
        _ids: *const GUID,
        _flags: u32,
    ) -> Result<()> {
        Ok(())
    }
    fn RequestAttrsTransitioningAtPosition(
        &self,
        _at: i32,
        _n: u32,
        _ids: *const GUID,
        _flags: u32,
    ) -> Result<()> {
        Ok(())
    }
    fn FindNextAttrTransition(
        &self,
        _acpstart: i32,
        acphalt: i32,
        _n: u32,
        _ids: *const GUID,
        _flags: u32,
        pacpnext: *mut i32,
        pffound: *mut BOOL,
        plfoundoffset: *mut i32,
    ) -> Result<()> {
        self.need_read()?;
        if pacpnext.is_null() || pffound.is_null() || plfoundoffset.is_null() {
            return fail(E_INVALIDARG);
        }
        // SAFETY: checked above.
        unsafe {
            *pacpnext = acphalt;
            *pffound = BOOL(0);
            *plfoundoffset = 0;
        }
        Ok(())
    }
    fn RetrieveRequestedAttrs(
        &self,
        _count: u32,
        _values: *mut TS_ATTRVAL,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if pcfetched.is_null() {
            return fail(E_INVALIDARG);
        }
        // SAFETY: checked above.
        unsafe { *pcfetched = 0 };
        Ok(())
    }
    fn GetEndACP(&self) -> Result<i32> {
        self.need_read()?;
        Ok(acp(self.virtual_len()?))
    }
    fn GetActiveView(&self) -> Result<u32> {
        Ok(VIEW_COOKIE)
    }
    fn GetACPFromPoint(&self, vcview: u32, ptscreen: *const POINT, dwflags: u32) -> Result<i32> {
        self.need_read()?;
        if vcview != VIEW_COOKIE || ptscreen.is_null() {
            return fail(E_INVALIDARG);
        }
        // SAFETY: checked above.
        let screen = unsafe { *ptscreen };
        let Some(at) = self.host.to_window(screen) else {
            return self.no_layout();
        };
        let (hit, area, rects) =
            self.with_or_err(|v, _| (v.byte_at_point(at), v.text_area(), v.composition_rects()))?;
        let in_area = at.x.0 >= area.x.0
            && at.y.0 >= area.y.0
            && at.x.0 < area.x.0 + area.width.0
            && at.y.0 < area.y.0 + area.height.0;
        if !in_area && dwflags & GXFPF_NEAREST == 0 {
            return fail(TS_E_INVALIDPOINT);
        }
        let overlay = self.state.borrow().overlay.clone();
        // A point on the composition: its start. A hook for inside it does not exist yet.
        if let (Some(o), Ok(rects)) = (&overlay, &rects)
            && rects.iter().any(|r| {
                at.x.0 >= r.rect.x.0
                    && at.x.0 < r.rect.x.0 + r.rect.width.0
                    && at.y.0 >= r.rect.y.0
                    && at.y.0 < r.rect.y.0 + r.rect.height.0
            })
        {
            return Ok(acp(o.base.start));
        }
        let hit = match hit {
            Ok(hit) => hit,
            Err(e) => return self.geometry_error(&e),
        };
        let unit = self
            .with_or_err(|v, _| v.byte_to_utf16(hit.byte))?
            .map_err(|e| Error::from_hresult(plain(&e)))?;
        Ok(acp(match &overlay {
            Some(o) => o.editor_to_tsf(unit.0),
            None => unit.0,
        }))
    }
    fn GetTextExt(
        &self,
        vcview: u32,
        acpstart: i32,
        acpend: i32,
        prc: *mut RECT,
        pfclipped: *mut BOOL,
    ) -> Result<()> {
        self.need_read()?;
        if vcview != VIEW_COOKIE || prc.is_null() || pfclipped.is_null() {
            return fail(E_INVALIDARG);
        }
        let range = self.checked(acpstart, acpend)?;
        let (rect, clipped) = self.text_rect(range)?;
        let Some(screen) = self.host.to_screen(rect) else {
            return self.no_layout();
        };
        // SAFETY: checked above.
        unsafe {
            *prc = screen;
            *pfclipped = BOOL::from(clipped);
        }
        Ok(())
    }
    fn GetScreenExt(&self, vcview: u32) -> Result<RECT> {
        if vcview != VIEW_COOKIE {
            return fail(E_INVALIDARG);
        }
        let area = self.with_or_err(|v, _| v.text_area())?;
        self.host
            .to_screen(area)
            .ok_or_else(|| Error::from_hresult(TS_E_NOLAYOUT))
    }
    fn GetWnd(&self, vcview: u32) -> Result<HWND> {
        if vcview != VIEW_COOKIE {
            return fail(E_INVALIDARG);
        }
        Ok(self.host.hwnd())
    }
}

fn units_from(text: &PCWSTR, count: u32) -> Result<&[u16]> {
    if count == 0 {
        return Ok(&[]);
    }
    if text.is_null() {
        return fail(E_INVALIDARG);
    }
    // SAFETY: TSF passes `count` units at this pointer, which live for the call.
    Ok(unsafe { std::slice::from_raw_parts(text.0, count as usize) })
}

#[allow(non_snake_case)]
impl ITfContextOwnerCompositionSink_Impl for StoreObject_Impl {
    fn OnStartComposition(&self, pcomposition: Ref<ITfCompositionView>) -> Result<BOOL> {
        if self.with_or_err(|v, _| !v.can_edit())? {
            return Ok(BOOL(0));
        }
        // A second composition: the first one's text goes in, so that none of it is lost.
        if self.state.borrow().overlay.is_some() {
            self.commit_overlay();
        }
        let extent = range_extent(pcomposition.ok()?)?;
        let base = usize::try_from(extent.0).unwrap_or(0)
            ..usize::try_from(extent.0.saturating_add(extent.1)).unwrap_or(0);
        if base.len() > MAX_SPAN_UNITS {
            return Ok(BOOL(0));
        }
        let Ok(bytes) = self.editor_bytes(&base) else {
            return Ok(BOOL(0));
        };
        let original = self.with_or_err(|v, c| {
            let original = read_editor(v, base.clone());
            if v.selection() != bytes {
                v.select(bytes.clone(), c);
            }
            v.ime_start();
            original
        })?;
        let Ok(original) = original else {
            return Ok(BOOL(0));
        };
        self.state.borrow_mut().overlay = Some(Overlay::new(base, original));
        Ok(BOOL(1))
    }
    fn OnUpdateComposition(
        &self,
        _pcomposition: Ref<ITfCompositionView>,
        prangenew: Ref<ITfRange>,
    ) -> Result<()> {
        if let Some(range) = prangenew.as_ref()
            && let Ok(acp_range) = range.cast::<ITfRangeACP>()
        {
            let (mut start, mut count) = (0, 0);
            // SAFETY: both out values live for the call.
            unsafe { acp_range.GetExtent(&mut start, &mut count)? };
            if let (Ok(a), Ok(b)) = (
                usize::try_from(start),
                usize::try_from(start.saturating_add(count)),
            ) {
                self.grow_overlay(&(a..b))?;
            }
        }
        self.state.borrow_mut().dirty = true;
        if self.lock_kind() == 0 {
            self.push_overlay();
        }
        Ok(())
    }
    fn OnEndComposition(&self, _pcomposition: Ref<ITfCompositionView>) -> Result<()> {
        let canceling = {
            let mut s = self.state.borrow_mut();
            if s.canceling {
                if let Some(overlay) = s.overlay.take() {
                    s.dropped = Some(overlay);
                }
                s.dirty = false;
                true
            } else {
                false
            }
        };
        if canceling {
            return Ok(());
        }
        // The editor ended it already, with Esc or an edit of its own: nothing goes in.
        if self.with_or_err(|v, _| v.composition().is_none())? {
            let mut s = self.state.borrow_mut();
            if let Some(overlay) = s.overlay.take() {
                s.dropped = Some(overlay);
            }
            s.dirty = false;
            return Ok(());
        }
        self.commit_overlay();
        Ok(())
    }
}

fn range_extent(view: &ITfCompositionView) -> Result<(i32, i32)> {
    // SAFETY: live interfaces on their own thread, and the out values live for the call.
    unsafe {
        let range = view.GetRange()?.cast::<ITfRangeACP>()?;
        let (mut start, mut count) = (0, 0);
        range.GetExtent(&mut start, &mut count)?;
        Ok((start, count))
    }
}
