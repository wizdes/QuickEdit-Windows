//! Input methods. TSF starts on the first focus, so a run that never takes the keyboard pays
//! nothing. Each document gets its text store when it first has the keyboard.
//!
//! The views live in the store, so this host borrows them through `App`, with `try_borrow_mut`.
//! `SharedViewHost` needs an `Rc<RefCell<EditorView>>`, which the store does not have. This does
//! the same work: a busy view answers false, and the store asks for a `sync` once it is free.
//!
//! A call into TSF can reach the pump and so run any effect, including one that closes a
//! document. So `Ime.docs` is never borrowed across a TSF call: each call works on its own
//! reference, and a document taken out of the list closes when the last reference goes.
use crate::app::{Shared, pump, queue_note};
use qe_api::{Point, Rect, ViewContext};
use qe_docs::{DocId, WindowId};
use qe_editor::EditorView;
use qe_ime::{
    ImeHost, TsfDocument, TsfThread, screen_to_window, window_is_shown, window_rect_to_screen,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use windows::Win32::Foundation::{HWND, POINT, RECT};

#[derive(Default)]
pub struct Ime {
    pub thread: RefCell<Option<Rc<TsfThread>>>,
    docs: RefCell<HashMap<DocId, (Rc<TsfDocument>, HWND)>>,
    failed: Cell<bool>,
    /// A store asked for a sync while the view was busy. The next wake serves it.
    pub owed: Cell<bool>,
    /// The document whose store is being made, which a nested call must not make again.
    opening: Cell<Option<DocId>>,
    /// That document closed while its store was being made.
    opening_closed: Cell<bool>,
}

struct AppImeHost {
    shared: Weak<Shared>,
    doc: DocId,
    hwnd: HWND,
    dpi: Rc<Cell<u32>>,
}
impl ImeHost for AppImeHost {
    fn with_view(&self, f: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let Ok(mut app) = shared.state.try_borrow_mut() else {
            return false;
        };
        let Some(view) = app.store.view_mut(self.doc) else {
            return false;
        };
        let mut ctx = ViewContext::default();
        f(view, &mut ctx);
        if let Some(window) = app.store.window_of(self.doc) {
            app.apply(window, ctx.requests);
            app.after_view_call(window, self.doc);
        }
        let quiet = app.effects.is_empty() && shared.queue.borrow().is_empty();
        drop(app);
        // TSF reads through here many times in a row, and a read leaves nothing to do. Effects
        // and events from a call that changed something wait in the queue. The pump returns at
        // once when an outer call is at work.
        if !quiet {
            pump(&shared);
        }
        true
    }
    fn to_screen(&self, rect: Rect) -> Option<RECT> {
        window_rect_to_screen(self.hwnd, rect, self.dpi.get())
    }
    fn to_window(&self, point: POINT) -> Option<Point> {
        screen_to_window(self.hwnd, point, self.dpi.get())
    }
    fn hwnd(&self) -> HWND {
        self.hwnd
    }
    fn problem(&self, message: String) {
        if let Some(shared) = self.shared.upgrade() {
            queue_note(&shared, message);
        }
    }
    fn sync_needed(&self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        // A document that has gone has nothing to catch up on. Asking again would never end.
        if let Ok(app) = shared.state.try_borrow()
            && app.store.view(self.doc).is_none()
        {
            return;
        }
        // The view is busy now. A posted wake comes back through the message loop, with no
        // borrow held, and then syncs. It never runs in this same pump.
        shared.ime.owed.set(true);
        (shared.wake)();
    }
    fn is_shown(&self) -> bool {
        window_is_shown(self.hwnd)
    }
}

impl Ime {
    pub fn started(&self) -> bool {
        self.thread.borrow().is_some()
    }
    fn document(&self, doc: DocId) -> Option<Rc<TsfDocument>> {
        self.docs.borrow().get(&doc).map(|(found, _)| found.clone())
    }
    fn all(&self) -> Vec<Rc<TsfDocument>> {
        self.docs
            .borrow()
            .values()
            .map(|(found, _)| found.clone())
            .collect()
    }
    pub fn is_composing(&self, doc: DocId) -> bool {
        self.document(doc)
            .is_some_and(|document| document.store().is_composing())
    }
    pub fn any_composing(&self) -> bool {
        self.all()
            .iter()
            .any(|document| document.store().is_composing())
    }
    pub fn any_composing_in(&self, docs: &[DocId]) -> bool {
        docs.iter().any(|doc| self.is_composing(*doc))
    }
    pub fn commit_all(&self) {
        for document in self.all() {
            document.complete_composition();
            document.sync();
        }
    }
    /// Asks TSF to end these compositions with their text, before the text leaves.
    pub fn commit_docs(&self, docs: &[DocId]) {
        for doc in docs {
            if let Some(document) = self.document(*doc) {
                document.complete_composition();
                document.sync();
            }
        }
    }
    /// Gives this document the keyboard, starting TSF and its text store if they have not.
    pub fn focus(&self, shared: &Rc<Shared>, window: WindowId, doc: DocId) {
        if self.failed.get() || self.opening.get().is_some() {
            return;
        }
        if self.thread.borrow().is_none() {
            match TsfThread::start() {
                Ok(thread) => *self.thread.borrow_mut() = Some(Rc::new(thread)),
                Err(e) => {
                    self.failed.set(true);
                    queue_note(shared, e.to_string());
                    return;
                }
            }
        }
        let place = shared.state.try_borrow().ok().and_then(|app| {
            app.store.view(doc)?;
            let win = app.windows.get(&window)?;
            Some((win.hwnd, win.dpi.clone()))
        });
        let Some((hwnd, dpi)) = place else {
            return;
        };
        // A tab that moved to another window needs a store that knows the new window.
        let moved = self
            .docs
            .borrow()
            .get(&doc)
            .is_some_and(|(_, known)| *known != hwnd);
        if moved {
            self.close(doc);
        }
        if !self.docs.borrow().contains_key(&doc) {
            let Some(thread) = self.thread.borrow().clone() else {
                return;
            };
            let host = Rc::new(AppImeHost {
                shared: Rc::downgrade(shared),
                doc,
                hwnd,
                dpi,
            });
            self.opening.set(Some(doc));
            self.opening_closed.set(false);
            let opened = thread.open_document(host);
            self.opening.set(None);
            match opened {
                Ok(document) => {
                    // The call can reach the pump, and the document may have closed there. A
                    // dead entry is never kept.
                    let alive = !self.opening_closed.take()
                        && shared
                            .state
                            .try_borrow()
                            .is_ok_and(|app| app.store.view(doc).is_some());
                    if !alive {
                        return;
                    }
                    self.docs
                        .borrow_mut()
                        .insert(doc, (Rc::new(document), hwnd));
                }
                Err(e) => {
                    queue_note(shared, e.to_string());
                    return;
                }
            }
        }
        if let Some(document) = self.document(doc)
            && let Err(e) = document.focus()
        {
            queue_note(shared, e.to_string());
        }
    }
    /// The keyboard moved to a field beside this document's text.
    pub fn leave(&self, shared: &Rc<Shared>, doc: DocId) {
        if let Some(document) = self.document(doc)
            && let Err(e) = document.leave_focus()
        {
            queue_note(shared, e.to_string());
        }
    }
    /// True while TSF's focus is on this document's store.
    pub fn has_focus(&self, doc: DocId) -> bool {
        self.document(doc)
            .is_some_and(|document| document.has_tsf_focus())
    }
    /// A real composition, as a text service starts one, for the test channel.
    pub fn compose_for_test(&self, doc: DocId, text: &str) -> Result<(), String> {
        let document = self.document(doc).ok_or("this tab has no text store")?;
        document.compose_for_test(text).map_err(|e| e.to_string())
    }
    pub fn sync(&self, doc: DocId) {
        if let Some(document) = self.document(doc) {
            document.sync();
        }
    }
    pub fn cancel(&self, doc: DocId) {
        if let Some(document) = self.document(doc) {
            document.cancel_composition();
            document.sync();
        }
    }
    /// The last reference closes it, with any composition's text going in if the view lives.
    pub fn close(&self, doc: DocId) {
        if self.opening.get() == Some(doc) {
            self.opening_closed.set(true);
        }
        let taken = self.docs.borrow_mut().remove(&doc);
        drop(taken);
    }
    /// Closes every store made for this window, while the window still lives for TSF's calls.
    pub fn close_for_window(&self, hwnd: HWND) {
        let docs: Vec<DocId> = self
            .docs
            .borrow()
            .iter()
            .filter(|(_, (_, known))| *known == hwnd)
            .map(|(doc, _)| *doc)
            .collect();
        for doc in docs {
            self.close(doc);
        }
    }
    pub fn close_all(&self) {
        let all: Vec<DocId> = self.docs.borrow().keys().copied().collect();
        for doc in all {
            self.close(doc);
        }
    }
    /// A wake after a busy moment: every open store catches up.
    pub fn sync_all(&self, shared: &Rc<Shared>) {
        // A store can outlive its document only if something went wrong. It goes now.
        if let Ok(app) = shared.state.try_borrow() {
            let dead: Vec<DocId> = self
                .docs
                .borrow()
                .keys()
                .copied()
                .filter(|doc| app.store.view(*doc).is_none())
                .collect();
            drop(app);
            for doc in dead {
                self.close(doc);
            }
        }
        for document in self.all() {
            document.sync();
        }
    }
}
