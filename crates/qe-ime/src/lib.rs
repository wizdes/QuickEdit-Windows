//! Input methods through the Text Services Framework (TSF).
//!
//! # The parts
//! - [`TsfThread`] starts TSF on the UI thread. Call it on the first focus, never at launch.
//! - [`TsfDocument`] is one view's document in TSF. It owns a [`TextStoreHandle`].
//! - [`TextStoreHandle`] holds the real COM text store (`ITextStoreACP` with
//!   `ITfContextOwnerCompositionSink`). TSF calls it, and the host calls `sync` on it.
//! - [`ImeHost`] is what the store asks of the host: the view, and screen coordinates.
//!
//! # What TSF sees while a composition runs
//! TSF writes a composition's text into the document. The editor keeps that text out of
//! its store until the commit. So the store answers TSF as if the text were there, and
//! sends it to the editor with `ime_update`. When TSF ends the composition, the text goes
//! to the editor with `ime_commit`, as one edit.
//!
//! # Threads and locks
//! Everything here runs on the UI thread, in a single-threaded COM apartment. A store call
//! from TSF can arrive whenever the host runs its message loop. The host must not hold the
//! view borrowed across the message loop. If it does, the store refuses and tries later.
#![allow(hidden_glob_reexports)]

// `#[implement]` writes paths that start at `windows_core`. This crate stands in
// for that crate, inside this crate only, so that it needs no extra dependency.
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod overlay;
mod store;

pub use store::{StoreCounts, TextStoreHandle};

use qe_api::{Dip, Point, Rect, ViewContext, ViewRequest};
use qe_editor::EditorView;
use std::{cell::RefCell, fmt, rc::Rc, result::Result};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT, RPC_E_CHANGED_MODE},
    Graphics::Gdi::{ClientToScreen, ScreenToClient},
    System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    },
    UI::TextServices::{CLSID_TF_ThreadMgr, ITfContext, ITfDocumentMgr, ITfThreadMgr, TF_POPF_ALL},
};
use windows::core::Interface;

/// A failure with its reason in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImeError(pub String);
impl fmt::Display for ImeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ImeError {}
fn failed(what: &str, error: impl fmt::Display) -> ImeError {
    ImeError(format!(
        "Input methods are off for this window: {what} failed: {error}"
    ))
}

/// What the text store asks of the host. All calls come on the UI thread.
pub trait ImeHost {
    /// Runs `f` with the view and a context, then applies the context's requests. Returns
    /// false, without running `f`, when the view is in use further up the stack.
    fn with_view(&self, f: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool;
    /// A rectangle in window coordinates, in screen pixels. `None` when the window is gone.
    fn to_screen(&self, rect: Rect) -> Option<RECT>;
    /// A screen point in window coordinates. `None` when the window is gone.
    fn to_window(&self, point: POINT) -> Option<Point>;
    /// The window that shows the view.
    fn hwnd(&self) -> HWND;
    /// Shows a problem to the user. It comes at once, and the text stays safe.
    fn problem(&self, message: String);
}

/// A window rectangle in device-independent pixels, in screen pixels.
pub fn window_rect_to_screen(hwnd: HWND, rect: Rect, dpi: u32) -> Option<RECT> {
    let scale = dpi as f32 / 96.0;
    let mut top_left = POINT {
        x: (rect.x.0 * scale).floor() as i32,
        y: (rect.y.0 * scale).floor() as i32,
    };
    let mut bottom_right = POINT {
        x: ((rect.x.0 + rect.width.0) * scale).ceil() as i32,
        y: ((rect.y.0 + rect.height.0) * scale).ceil() as i32,
    };
    // SAFETY: both points live for the calls, and a bad window returns false.
    let ok = unsafe {
        ClientToScreen(hwnd, &mut top_left).as_bool()
            && ClientToScreen(hwnd, &mut bottom_right).as_bool()
    };
    ok.then_some(RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

/// A screen point, in the window's device-independent pixels.
pub fn screen_to_window(hwnd: HWND, point: POINT, dpi: u32) -> Option<Point> {
    let mut at = point;
    // SAFETY: the point lives for the call, and a bad window returns false.
    let ok = unsafe { ScreenToClient(hwnd, &mut at).as_bool() };
    let scale = dpi as f32 / 96.0;
    ok.then_some(Point {
        x: Dip(at.x as f32 / scale),
        y: Dip(at.y as f32 / scale),
    })
}

/// An [`ImeHost`] for a view in an `Rc<RefCell<_>>`.
pub struct SharedViewHost {
    pub view: Rc<RefCell<EditorView>>,
    pub hwnd: HWND,
    pub dpi: Rc<std::cell::Cell<u32>>,
    /// Gets each batch of requests from a store call, after the view's borrow has ended.
    pub on_requests: Box<dyn Fn(Vec<ViewRequest>)>,
    pub on_problem: Box<dyn Fn(String)>,
}
impl ImeHost for SharedViewHost {
    fn with_view(&self, f: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool {
        let Ok(mut view) = self.view.try_borrow_mut() else {
            return false;
        };
        let mut ctx = ViewContext::default();
        f(&mut view, &mut ctx);
        drop(view);
        if !ctx.requests.is_empty() {
            (self.on_requests)(ctx.requests);
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
        (self.on_problem)(message);
    }
}

/// TSF on one UI thread. Start it on the first focus. Keep it for the thread's life.
pub struct TsfThread {
    thread: ITfThreadMgr,
    client: u32,
}
impl TsfThread {
    /// Loads TSF and activates it for this thread.
    pub fn start() -> Result<Self, ImeError> {
        // SAFETY: called on the thread that uses the result. COM types keep it there.
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if init == RPC_E_CHANGED_MODE {
            return Err(ImeError(
                "Input methods are off for this window: this thread is not in a \
                 single-threaded COM apartment, which TSF needs."
                    .into(),
            ));
        }
        init.ok().map_err(|e| failed("CoInitializeEx", e))?;
        let started = (|| {
            // SAFETY: plain COM creation and activation on this apartment's thread.
            let thread: ITfThreadMgr =
                unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| failed("creating the TSF thread manager", e))?;
            let client = unsafe { thread.Activate() }.map_err(|e| failed("activating TSF", e))?;
            Ok((thread, client))
        })();
        match started {
            Ok((thread, client)) => Ok(Self { thread, client }),
            Err(e) => {
                // SAFETY: pairs with the successful `CoInitializeEx` above.
                unsafe { CoUninitialize() };
                Err(e)
            }
        }
    }
    /// The client id TSF gave this thread.
    pub fn client_id(&self) -> u32 {
        self.client
    }
    pub fn thread_manager(&self) -> &ITfThreadMgr {
        &self.thread
    }
    /// Makes a document for one view: document manager, context and text store. TSF
    /// advises its sink on the store during this call.
    pub fn open_document(&self, host: Rc<dyn ImeHost>) -> Result<TsfDocument, ImeError> {
        // SAFETY: plain COM calls on this apartment's thread, with live interfaces.
        unsafe {
            let doc = self
                .thread
                .CreateDocumentMgr()
                .map_err(|e| failed("CreateDocumentMgr", e))?;
            let store = TextStoreHandle::new(host);
            let mut context = None;
            let mut cookie = 0;
            doc.CreateContext(
                self.client,
                0,
                &store.text_store(),
                &mut context,
                &mut cookie,
            )
            .map_err(|e| failed("CreateContext", e))?;
            let context: ITfContext =
                context.ok_or_else(|| ImeError("TSF gave no context for this window.".into()))?;
            doc.Push(&context)
                .map_err(|e| failed("pushing the context", e))?;
            store.set_context(&context);
            Ok(TsfDocument {
                thread: self.thread.clone(),
                doc,
                context,
                store,
                closed: false,
            })
        }
    }
}
impl Drop for TsfThread {
    fn drop(&mut self) {
        // SAFETY: pairs with `Activate` and `CoInitializeEx` in `start`.
        unsafe {
            if let Err(e) = self.thread.Deactivate() {
                eprintln!("TSF would not stop for this thread: {e}");
            }
            CoUninitialize();
        }
    }
}

/// Links a document to the window. The `windows` crate's wrapper treats the normal answer
/// "no document was linked before" as an error, so this calls the interface directly.
fn associate_focus(
    thread: &ITfThreadMgr,
    hwnd: HWND,
    doc: Option<&ITfDocumentMgr>,
) -> windows::core::Result<()> {
    let mut previous: *mut std::ffi::c_void = std::ptr::null_mut();
    let new = doc.map_or(std::ptr::null_mut(), Interface::as_raw);
    // SAFETY: a live interface on its own thread, with a matching vtable call. A previous
    // document comes with a reference, which `from_raw` takes over and releases.
    unsafe {
        (Interface::vtable(thread).AssociateFocus)(
            Interface::as_raw(thread),
            hwnd,
            new,
            &mut previous,
        )
        .ok()?;
        if !previous.is_null() {
            drop(ITfDocumentMgr::from_raw(previous));
        }
    }
    Ok(())
}

/// One view's document in TSF.
pub struct TsfDocument {
    thread: ITfThreadMgr,
    doc: ITfDocumentMgr,
    context: ITfContext,
    store: TextStoreHandle,
    closed: bool,
}
impl TsfDocument {
    /// Call when this view gets the keyboard focus or its tab comes to the front.
    pub fn focus(&self) -> Result<(), ImeError> {
        let hwnd = self.store.hwnd();
        // SAFETY: plain COM calls on this apartment's thread, with live interfaces.
        unsafe {
            associate_focus(&self.thread, hwnd, Some(&self.doc))
                .map_err(|e| failed("AssociateFocus", e))?;
            self.thread
                .SetFocus(&self.doc)
                .map_err(|e| failed("setting TSF's focus", e))
        }
    }
    pub fn store(&self) -> &TextStoreHandle {
        &self.store
    }
    /// Call after any editor call that may have changed the text, the selection or the
    /// editable state. See [`TextStoreHandle::sync`].
    pub fn sync(&self) {
        self.store.sync();
    }
    /// Call when the layout, the scroll position or the window's place changed.
    pub fn layout_changed(&self) {
        self.store.layout_changed();
    }
    /// The answer to `ViewRequest::CancelComposition`. Ends it in TSF and drops its text.
    pub fn cancel_composition(&self) {
        self.store.cancel_composition();
    }
    /// Ends the composition in TSF and keeps its text.
    pub fn complete_composition(&self) {
        self.store.complete_composition();
    }
    pub fn context(&self) -> &ITfContext {
        &self.context
    }
    pub fn document_manager(&self) -> &ITfDocumentMgr {
        &self.doc
    }
    /// Ends any composition, keeping its text, and takes the document out of TSF.
    pub fn close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        self.store.complete_composition();
        // SAFETY: plain COM calls on this apartment's thread, with live interfaces.
        unsafe {
            if let Err(e) = associate_focus(&self.thread, self.store.hwnd(), None) {
                self.store.report(format!("TSF kept this tab's focus: {e}"));
            }
            if let Err(e) = self.doc.Pop(TF_POPF_ALL) {
                self.store
                    .report(format!("TSF kept this tab's document: {e}"));
            }
        }
        self.store.close();
    }
}
impl Drop for TsfDocument {
    fn drop(&mut self) {
        self.close();
    }
}
