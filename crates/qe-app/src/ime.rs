//! Input methods. TSF starts on the first focus, so a run that never takes the keyboard pays
//! nothing. Each document gets its text store when it first has the keyboard.
//!
//! The views live in the store, so this host borrows them through `App`, with `try_borrow_mut`.
//! `SharedViewHost` needs an `Rc<RefCell<EditorView>>`, which the store does not have. This does
//! the same work: a busy view answers false, and the store asks for a `sync` once it is free.
use crate::app::{Incoming, Shared, pump, queue_note};
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
    docs: RefCell<HashMap<DocId, (TsfDocument, HWND)>>,
    failed: Cell<bool>,
    /// A store asked for a sync while the view was busy.
    pub owed: Cell<bool>,
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
        drop(app);
        // Effects and events from the call wait in the queue, and the pump reaches them. It
        // returns at once when an outer call is at work.
        pump(&shared);
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
        // The view is busy now. A posted wake comes back with no borrow held, and then syncs.
        shared
            .queue
            .borrow_mut()
            .push_back(Incoming::Note(String::new()));
        shared.ime.owed.set(true);
    }
    fn is_shown(&self) -> bool {
        window_is_shown(self.hwnd)
    }
}

impl Ime {
    pub fn started(&self) -> bool {
        self.thread.borrow().is_some()
    }
    pub fn is_composing(&self, doc: DocId) -> bool {
        self.docs
            .borrow()
            .get(&doc)
            .is_some_and(|(document, _)| document.store().is_composing())
    }
    pub fn any_composing(&self) -> bool {
        self.docs
            .borrow()
            .values()
            .any(|(document, _)| document.store().is_composing())
    }
    pub fn commit_all(&self) {
        let docs = self.docs.borrow();
        for (document, _) in docs.values() {
            document.complete_composition();
            document.sync();
        }
    }
    /// Gives this document the keyboard, starting TSF and its text store if they have not.
    pub fn focus(&self, shared: &Rc<Shared>, window: WindowId, doc: DocId) {
        if self.failed.get() {
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
            match thread.open_document(host) {
                Ok(document) => {
                    self.docs.borrow_mut().insert(doc, (document, hwnd));
                }
                Err(e) => {
                    queue_note(shared, e.to_string());
                    return;
                }
            }
        }
        let focus = self
            .docs
            .borrow()
            .get(&doc)
            .map(|(document, _)| document.focus());
        if let Some(Err(e)) = focus {
            queue_note(shared, e.to_string());
        }
    }
    pub fn sync(&self, doc: DocId) {
        let docs = self.docs.borrow();
        if let Some((document, _)) = docs.get(&doc) {
            document.sync();
        }
    }
    pub fn cancel(&self, doc: DocId) {
        let docs = self.docs.borrow();
        if let Some((document, _)) = docs.get(&doc) {
            document.cancel_composition();
            document.sync();
        }
    }
    /// Asks TSF to end a composition with its text, before the text leaves for a file.
    pub fn commit(&self, doc: DocId) {
        let docs = self.docs.borrow();
        if let Some((document, _)) = docs.get(&doc) {
            document.complete_composition();
            document.sync();
        }
    }
    pub fn close(&self, doc: DocId) {
        let taken = self.docs.borrow_mut().remove(&doc);
        if let Some((mut document, _)) = taken {
            document.close();
        }
    }
    pub fn close_all(&self) {
        let all: Vec<DocId> = self.docs.borrow().keys().copied().collect();
        for doc in all {
            self.close(doc);
        }
    }
    /// A wake after a busy moment: every open store catches up.
    pub fn sync_all(&self) {
        let docs = self.docs.borrow();
        for (document, _) in docs.values() {
            document.sync();
        }
    }
}
