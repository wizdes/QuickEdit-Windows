//! The text store with real TSF behind it, in a hidden window. The test plays the part of
//! a text service: it asks TSF for edit sessions, and TSF calls the store. No real input
//! method runs here, so root runs that trial by hand.
#![allow(hidden_glob_reexports)]
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod support;

use qe_api::{ByteOffset, InputEvent, InputKind, Modifiers, View, ViewContext, ViewRequest};
use qe_editor::{CompositionClause, EditorCommand, EditorView};
use qe_ime::{TsfDocument, TsfThread};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};
use support::{Fixture, GateHost, laid_out, settle, units};
use windows::{
    Win32::{
        Foundation::{E_FAIL, LPARAM, POINT, RECT, WPARAM},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoGetApartmentType, CoUninitialize},
            Variant::VARIANT,
        },
        UI::TextServices::{
            CLSID_TF_CategoryMgr, GUID_PROP_ATTRIBUTE, GXFPF_NEAREST, ITfCategoryMgr,
            ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext,
            ITfContextComposition, ITfDocumentMgr, ITfEditSession, ITfEditSession_Impl,
            ITfInsertAtSelection, ITfRange, ITfRangeACP, TF_ANCHOR_END, TF_ANCHOR_START,
            TF_DEFAULT_SELECTION, TF_E_COMPOSITION_REJECTED, TF_E_LOCKED, TF_E_NOLAYOUT,
            TF_E_NOLOCK, TF_E_READONLY, TF_E_SYNCHRONOUS, TF_ES_ASYNC, TF_ES_READ, TF_ES_READWRITE,
            TF_ES_SYNC, TF_IAS_QUERYONLY, TF_S_ASYNC, TF_SELECTION, TF_SELECTIONSTYLE, TS_AE_END,
            TS_SD_READONLY,
        },
        UI::WindowsAndMessaging::{
            MSG, PostMessageW, PostQuitMessage, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_USER,
        },
    },
    core::{BOOL, GUID, HRESULT, Interface, Ref, Result, implement},
};

type Body = Box<dyn FnOnce(u32) -> Result<()>>;

#[implement(ITfEditSession)]
struct Session(RefCell<Option<Body>>);
impl ITfEditSession_Impl for Session_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        match self.0.borrow_mut().take() {
            Some(body) => body(ec),
            None => Ok(()),
        }
    }
}

#[implement(ITfCompositionSink)]
struct CompositionSink(Rc<Cell<u32>>);
impl ITfCompositionSink_Impl for CompositionSink_Impl {
    fn OnCompositionTerminated(&self, _ec: u32, _composition: Ref<ITfComposition>) -> Result<()> {
        self.0.set(self.0.get() + 1);
        Ok(())
    }
}

struct Tsf {
    fixture: Fixture,
    doc: TsfDocument,
    thread: TsfThread,
    terminated: Rc<Cell<u32>>,
    _one_at_a_time: MutexGuard<'static, ()>,
}
/// TSF sessions on two threads of one process made `StartComposition` fail now and then with
/// `E_FAIL` (seen in about 1 run in 2 at 8 test threads, and in none of 10 runs at 1). The
/// app has one UI thread, so tests take turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
impl Tsf {
    fn new(view: EditorView) -> Self {
        let one_at_a_time = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let fixture = Fixture::new(view);
        let thread = support::start_tsf();
        let doc = thread.open_document(fixture.host.clone()).unwrap();
        doc.focus().unwrap();
        Self {
            fixture,
            doc,
            thread,
            terminated: Rc::default(),
            _one_at_a_time: one_at_a_time,
        }
    }
    /// A store whose view the test can make busy, with the flag that does it.
    fn gated(view: EditorView) -> (Self, Rc<Cell<bool>>) {
        let one_at_a_time = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let fixture = Fixture::new(view);
        let shut: Rc<Cell<bool>> = Rc::default();
        let host = Rc::new(GateHost {
            inner: fixture.host.clone(),
            shut: shut.clone(),
        });
        let thread = support::start_tsf();
        let doc = thread.open_document(host).unwrap();
        doc.focus().unwrap();
        (
            Self {
                fixture,
                doc,
                thread,
                terminated: Rc::default(),
                _one_at_a_time: one_at_a_time,
            },
            shut,
        )
    }
    fn context(&self) -> ITfContext {
        self.doc.context().clone()
    }
    /// A sync edit session. Returns the session's own result, as TSF hands it back.
    fn session(&self, flags: u32, body: impl FnOnce(u32) -> Result<()> + 'static) -> HRESULT {
        let session: ITfEditSession = Session(RefCell::new(Some(Box::new(body)))).into();
        // The host syncs after its edits, and when the store asks after a lock.
        self.doc.sync();
        let answer = unsafe {
            self.context().RequestEditSession(
                self.thread.client_id(),
                &session,
                windows::Win32::UI::TextServices::TF_CONTEXT_EDIT_CONTEXT_FLAGS(flags),
            )
        }
        .unwrap();
        self.doc.sync();
        answer
    }
    fn read_session(&self, body: impl FnOnce(u32) -> Result<()> + 'static) {
        let answer = self.session(TF_ES_SYNC.0 | TF_ES_READ.0, body);
        assert_eq!(answer, HRESULT(0));
    }
    fn write(&self, body: impl FnOnce(u32) -> Result<()> + 'static) {
        let answer = self.session(TF_ES_SYNC.0 | TF_ES_READWRITE.0, body);
        assert_eq!(answer, HRESULT(0));
    }
    fn read_text(&self) -> String {
        let out: Rc<RefCell<String>> = Rc::default();
        let (slot, context) = (out.clone(), self.context());
        let answer = self.session(TF_ES_SYNC.0 | TF_ES_READ.0, move |ec| unsafe {
            *slot.borrow_mut() = whole_text(&context, ec)?;
            Ok(())
        });
        assert_eq!(answer, HRESULT(0));
        out.borrow().clone()
    }
    /// Starts a composition at the selection, as a text service does.
    fn start_composition(&self) -> Rc<RefCell<Option<(ITfComposition, ITfRange)>>> {
        let out: Rc<RefCell<Option<(ITfComposition, ITfRange)>>> = Rc::default();
        let (slot, context, count) = (out.clone(), self.context(), self.terminated.clone());
        self.write(move |ec| unsafe {
            let insert: ITfInsertAtSelection = context.cast()?;
            let range = insert.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[])?;
            let composing: ITfContextComposition = context.cast()?;
            let sink: ITfCompositionSink = CompositionSink(count).into();
            let composition = composing.StartComposition(ec, &range, &sink)?;
            let range = composition.GetRange()?;
            *slot.borrow_mut() = Some((composition, range));
            Ok(())
        });
        out
    }
    fn set_composition_text(
        &self,
        held: &Rc<RefCell<Option<(ITfComposition, ITfRange)>>>,
        text: &str,
    ) {
        let range = held.borrow().as_ref().unwrap().1.clone();
        let text = units(text);
        self.write(move |ec| unsafe { range.SetText(ec, 0, &text) });
    }
    fn end_composition(&self, held: &Rc<RefCell<Option<(ITfComposition, ITfRange)>>>) {
        let composition = held.borrow().as_ref().unwrap().0.clone();
        self.write(move |ec| unsafe { composition.EndComposition(ec) });
    }
}

unsafe fn whole_text(context: &ITfContext, ec: u32) -> Result<String> {
    unsafe {
        let range = context.GetStart(ec)?;
        let mut moved = 0;
        range.ShiftEnd(ec, i32::MAX / 2, &mut moved, std::ptr::null())?;
        let mut buffer = [0_u16; 512];
        let mut got = 0;
        range.GetText(ec, 0, &mut buffer, &mut got)?;
        Ok(String::from_utf16_lossy(&buffer[..got as usize]))
    }
}

fn esc(view: &mut EditorView) -> Vec<ViewRequest> {
    let mut ctx = ViewContext::default();
    view.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: 0x1b,
                modifiers: Modifiers::default(),
                repeat: false,
            },
        },
        &mut ctx,
    );
    ctx.requests
}

// ---- TSF takes the store ----

#[test]
fn real_tsf_accepts_the_store_in_a_hidden_window_and_reads_through_it() {
    let tsf = Tsf::new(EditorView::from_text("a\u{1f600}\r\nb").unwrap());
    assert_eq!(tsf.read_text(), "a\u{1f600}\r\nb");
    let status = unsafe { tsf.context().GetStatus() }.unwrap();
    assert_eq!(status.dwDynamicFlags & TS_SD_READONLY, 0);
    assert!(
        tsf.doc.store().counts().locks_granted >= 1,
        "TSF took a lock on the store"
    );
    let focused = unsafe { tsf.thread.thread_manager().GetFocus() }.unwrap();
    assert_eq!(&focused, tsf.doc.document_manager());
    tsf.fixture.no_problems();
}

#[test]
fn real_tsf_says_tf_e_nolock_with_no_lock_and_after_the_session_ends() {
    let tsf = Tsf::new(EditorView::from_text("abc").unwrap());
    let kept: Rc<RefCell<Option<(ITfRange, u32)>>> = Rc::default();
    let (slot, context) = (kept.clone(), tsf.context());
    tsf.write(move |ec| unsafe {
        *slot.borrow_mut() = Some((context.GetStart(ec)?, ec));
        Ok(())
    });
    let (range, stale) = kept.borrow().clone().unwrap();
    let mut buffer = [0_u16; 8];
    let mut got = 0;
    let answer = unsafe { range.GetText(stale, 0, &mut buffer, &mut got) };
    assert_eq!(answer.unwrap_err().code(), TF_E_NOLOCK);
    // A read session cannot write. TSF stops it before the store sees it.
    let (range_for_read, context) = (range.clone(), tsf.context());
    let code = Rc::new(Cell::new(HRESULT(0)));
    let code_out = code.clone();
    tsf.read_session(move |ec| unsafe {
        let _ = &context;
        if let Err(e) = range_for_read.SetText(ec, 0, &units("x")) {
            code_out.set(e.code());
        }
        Ok(())
    });
    assert_eq!(code.get(), TF_E_NOLOCK);
    assert_eq!(tsf.fixture.text(), "abc");
}

#[test]
fn real_tsf_refuses_a_sync_write_inside_a_read_and_runs_an_async_one_later() {
    let tsf = Rc::new(Tsf::new(EditorView::from_text("abc").unwrap()));
    let order: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let (order_out, inner) = (order.clone(), tsf.clone());
    let nested_sync = Rc::new(Cell::new(HRESULT(0)));
    let nested_async = Rc::new(Cell::new(HRESULT(0)));
    let (sync_out, async_out) = (nested_sync.clone(), nested_async.clone());
    let answer = tsf.session(TF_ES_SYNC.0 | TF_ES_READ.0, move |_| {
        order_out.borrow_mut().push("outer");
        let request = |body: Body, flags: u32| unsafe {
            let session: ITfEditSession = Session(RefCell::new(Some(body))).into();
            let answer = inner.context().RequestEditSession(
                inner.thread.client_id(),
                &session,
                windows::Win32::UI::TextServices::TF_CONTEXT_EDIT_CONTEXT_FLAGS(flags),
            );
            answer.unwrap_or_else(|e| e.code())
        };
        sync_out.set(request(
            Box::new(|_| Ok(())),
            TF_ES_SYNC.0 | TF_ES_READWRITE.0,
        ));
        let later = order_out.clone();
        let context = inner.context();
        async_out.set(request(
            Box::new(move |ec| unsafe {
                later.borrow_mut().push("inner");
                let range = context.GetEnd(ec)?;
                range.SetText(ec, 0, &units("Z"))
            }),
            TF_ES_ASYNC.0 | TF_ES_READWRITE.0,
        ));
        Ok(())
    });
    assert_eq!(answer, HRESULT(0));
    let refused = nested_sync.get();
    assert!(
        refused == TF_E_LOCKED || refused == TF_E_SYNCHRONOUS,
        "a sync write inside a read must be refused: {refused:?}"
    );
    assert_eq!(nested_async.get(), TF_S_ASYNC);
    // TSF runs it once the outer lock has ended. It may post it to the message loop.
    let until = Instant::now() + Duration::from_secs(5);
    while order.borrow().len() < 2 && Instant::now() < until {
        pump();
    }
    assert_eq!(order.borrow().as_slice(), &["outer", "inner"]);
    assert_eq!(tsf.fixture.text(), "abcZ");
    tsf.fixture.no_problems();
}

fn pump() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    unsafe {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    std::thread::sleep(Duration::from_millis(1));
}

#[test]
fn real_tsf_writes_and_selection_reach_the_editor_outside_a_composition() {
    let tsf = Tsf::new(EditorView::from_text("hello").unwrap());
    let context = tsf.context();
    tsf.write(move |ec| unsafe {
        let range = context.GetStart(ec)?;
        let mut moved = 0;
        range.ShiftStart(ec, 1, &mut moved, std::ptr::null())?;
        range.ShiftEnd(ec, 2, &mut moved, std::ptr::null())?;
        range.SetText(ec, 0, &units("\u{1f600}"))
    });
    assert_eq!(tsf.fixture.text(), "h\u{1f600}lo");
    let context = tsf.context();
    tsf.write(move |ec| unsafe {
        let range = context.GetStart(ec)?;
        let mut moved = 0;
        range.ShiftEnd(ec, 3, &mut moved, std::ptr::null())?;
        let selection = TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(range)),
            style: TF_SELECTIONSTYLE {
                ase: windows::Win32::UI::TextServices::TF_AE_END,
                fInterimChar: BOOL(0),
            },
        };
        let answer = context.SetSelection(ec, std::slice::from_ref(&selection));
        drop(std::mem::ManuallyDrop::into_inner(selection.range));
        answer
    });
    assert_eq!(
        tsf.fixture.view.borrow().selection(),
        ByteOffset(0)..ByteOffset(5),
        "three UTF-16 units are one letter and one emoji, which is five bytes"
    );
    let _ = TS_AE_END;
    tsf.fixture.no_problems();
}

// ---- Compositions ----

#[test]
fn a_composition_shows_in_the_editor_and_only_its_commit_edits_the_store() {
    let mut view = laid_out("ab");
    view.select(ByteOffset(1)..ByteOffset(1), &mut ViewContext::default());
    let tsf = Tsf::new(view);
    let revision = tsf.fixture.view.borrow().revision();
    let held = tsf.start_composition();
    assert!(tsf.doc.store().is_composing());
    tsf.set_composition_text(&held, "\u{304b}");
    tsf.set_composition_text(&held, "\u{304b}\u{1f600}");
    // TSF sees its text in the document, at the place it wrote it.
    assert_eq!(tsf.read_text(), "a\u{304b}\u{1f600}b");
    {
        let view = tsf.fixture.view.borrow();
        let composition = view.composition().unwrap();
        assert_eq!(composition.text, "\u{304b}\u{1f600}");
        assert_eq!(
            composition.cursor_utf16, 3,
            "after the emoji, in UTF-16 units"
        );
        assert_eq!(composition.at, ByteOffset(1));
        // The store did not change.
        assert_eq!(view.text().unwrap(), "ab");
        assert_eq!(view.revision(), revision);
    }
    tsf.end_composition(&held);
    assert!(!tsf.doc.store().is_composing());
    assert_eq!(tsf.fixture.text(), "a\u{304b}\u{1f600}b");
    assert!(tsf.fixture.view.borrow().composition().is_none());
    assert_eq!(tsf.read_text(), "a\u{304b}\u{1f600}b");
    let counts = tsf.doc.store().counts();
    assert_eq!(counts.ime_commits, 1);
    assert_eq!(counts.ime_updates, 2, "once per lock");
    assert_eq!(tsf.terminated.get(), 0, "the service ended it itself");
    // One undo takes the whole commit back.
    let mut ctx = ViewContext::default();
    tsf.fixture
        .view
        .borrow_mut()
        .run(EditorCommand::Undo, &mut ctx);
    assert_eq!(tsf.fixture.text(), "ab");
    tsf.fixture.no_problems();
}

#[test]
fn a_composition_over_a_selection_replaces_it_at_the_commit_and_keeps_it_on_cancel() {
    let mut view = laid_out("hello");
    view.select(ByteOffset(1)..ByteOffset(4), &mut ViewContext::default());
    let tsf = Tsf::new(view);
    let held = tsf.start_composition();
    assert_eq!(tsf.read_text(), "hello");
    tsf.set_composition_text(&held, "Z");
    assert_eq!(tsf.read_text(), "hZo");
    assert_eq!(tsf.fixture.text(), "hello");
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "hZo");
    assert_eq!(
        tsf.fixture.view.borrow().selection(),
        ByteOffset(2)..ByteOffset(2)
    );

    let mut ctx = ViewContext::default();
    tsf.fixture
        .view
        .borrow_mut()
        .select(ByteOffset(0)..ByteOffset(2), &mut ctx);
    tsf.doc.sync();
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "Q");
    assert_eq!(tsf.read_text(), "Qo");
    tsf.doc.cancel_composition();
    assert_eq!(tsf.fixture.text(), "hZo");
    assert_eq!(tsf.read_text(), "hZo", "TSF must see the text come back");
    tsf.fixture.no_problems();
}

#[test]
fn a_service_that_empties_its_composition_changes_nothing() {
    let tsf = Tsf::new(laid_out("ab"));
    let revision = tsf.fixture.view.borrow().revision();
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    tsf.set_composition_text(&held, "");
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "ab");
    assert_eq!(tsf.fixture.view.borrow().revision(), revision);
    assert_eq!(tsf.doc.store().counts().ime_cancels, 1);
    tsf.fixture.no_problems();
}

#[test]
fn cancel_from_the_host_reaches_tsf_and_drops_the_text() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "\u{1f600}x");
    assert_eq!(tsf.read_text(), "\u{1f600}xab");
    tsf.doc.cancel_composition();
    assert_eq!(
        tsf.terminated.get(),
        1,
        "TSF told the service that its composition ended"
    );
    assert!(!tsf.doc.store().is_composing());
    assert!(tsf.fixture.view.borrow().composition().is_none());
    assert_eq!(tsf.fixture.text(), "ab");
    assert_eq!(tsf.read_text(), "ab");
    // A second cancel has nothing to do, and no failure to show.
    tsf.doc.cancel_composition();
    assert_eq!(tsf.terminated.get(), 1);
    tsf.fixture.no_problems();
}

#[test]
fn esc_in_the_view_asks_the_host_and_the_host_call_ends_it_in_tsf() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let requests = esc(&mut tsf.fixture.view.borrow_mut());
    assert!(requests.contains(&ViewRequest::CancelComposition));
    assert!(tsf.fixture.view.borrow().composition().is_none());
    tsf.doc.cancel_composition();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "ab");
    assert_eq!(tsf.read_text(), "ab");
    tsf.fixture.no_problems();
}

#[test]
fn an_editor_edit_during_a_composition_ends_it_in_tsf_at_the_next_sync() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let mut ctx = ViewContext::default();
    tsf.fixture
        .view
        .borrow_mut()
        .replace_range(ByteOffset(2)..ByteOffset(2), "c", &mut ctx);
    assert!(ctx.requests.contains(&ViewRequest::CancelComposition));
    tsf.doc.sync();
    assert_eq!(tsf.terminated.get(), 1);
    assert!(!tsf.doc.store().is_composing());
    assert_eq!(
        tsf.fixture.text(),
        "xabc",
        "the composing text went in first"
    );
    assert_eq!(
        tsf.read_text(),
        "xabc",
        "TSF's copy matches the editor again"
    );
    tsf.fixture.no_problems();
}

#[test]
fn complete_keeps_the_text_and_closing_a_document_does_too() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    tsf.doc.complete_composition();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "xab");
    assert_eq!(tsf.read_text(), "xab");

    let mut tsf = tsf;
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "y");
    tsf.doc.close();
    assert_eq!(
        tsf.fixture.text(),
        "xyab",
        "closing must not lose what was typed"
    );
    tsf.fixture.no_problems();
}

#[test]
fn a_cancel_asked_for_during_a_lock_runs_when_the_lock_ends() {
    let tsf = Rc::new(Tsf::new(laid_out("ab")));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let inner = tsf.clone();
    let still_composing = Rc::new(Cell::new(false));
    let out = still_composing.clone();
    tsf.write(move |_| {
        inner.doc.cancel_composition();
        out.set(inner.doc.store().is_composing());
        Ok(())
    });
    assert!(
        still_composing.get(),
        "TSF refuses during a lock, so it must wait"
    );
    assert!(!tsf.doc.store().is_composing());
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "ab");
    assert_eq!(tsf.read_text(), "ab");
    tsf.fixture.no_problems();
}

#[test]
fn a_read_only_view_gets_no_write_session_from_tsf() {
    let mut view = laid_out("abc");
    view.set_prevent_editing(true);
    let tsf = Tsf::new(view);
    let status = unsafe { tsf.context().GetStatus() }.unwrap();
    assert_eq!(status.dwDynamicFlags & TS_SD_READONLY, TS_SD_READONLY);
    let ran = Rc::new(Cell::new(false));
    let out = ran.clone();
    let answer = tsf.session(TF_ES_SYNC.0 | TF_ES_READWRITE.0, move |_| {
        out.set(true);
        Ok(())
    });
    // TSF reads the store's status and stops the session before it starts.
    assert_eq!(answer, TF_E_READONLY);
    assert!(!ran.get());
    assert_eq!(tsf.read_text(), "abc");
    assert_eq!(tsf.fixture.text(), "abc");
    assert!(!tsf.doc.store().is_composing());
}

#[test]
fn a_view_that_turns_read_only_in_a_session_rejects_the_write_and_the_composition() {
    let tsf = Tsf::new(laid_out("abc"));
    let codes = Rc::new(RefCell::new((HRESULT(0), HRESULT(0))));
    let (out, context, count) = (codes.clone(), tsf.context(), tsf.terminated.clone());
    let view = tsf.fixture.view.clone();
    tsf.write(move |ec| unsafe {
        let range = context.GetStart(ec)?;
        view.borrow_mut().set_prevent_editing(true);
        if let Err(e) = range.SetText(ec, 0, &units("x")) {
            out.borrow_mut().0 = e.code();
        }
        let composing: ITfContextComposition = context.cast()?;
        let sink: ITfCompositionSink = CompositionSink(count).into();
        if let Err(e) = composing.StartComposition(ec, &range, &sink) {
            out.borrow_mut().1 = e.code();
        }
        Ok(())
    });
    let (write, composition) = *codes.borrow();
    // TSF wraps a write in a default composition, so the first refusal it meets may be that.
    assert!(
        write == TF_E_READONLY || write == TF_E_COMPOSITION_REJECTED,
        "the store must refuse the write: {write:?}"
    );
    // TSF says "rejected", or S_OK with no composition, which the `windows` crate shows as an
    // error with code 0. The lines below are what count: none is running.
    assert!(
        composition == HRESULT(0) || composition == TF_E_COMPOSITION_REJECTED,
        "{composition:?}"
    );
    assert!(!tsf.doc.store().is_composing());
    assert!(tsf.fixture.view.borrow().composition().is_none());
    assert_eq!(tsf.fixture.text(), "abc");
}

#[test]
fn editor_edits_reach_tsf_so_its_ranges_move() {
    let tsf = Tsf::new(EditorView::from_text("abc").unwrap());
    // A range kept across an edit stays on its text only if TSF heard about the edit.
    let kept: Rc<RefCell<Option<ITfRange>>> = Rc::default();
    let (slot, context) = (kept.clone(), tsf.context());
    tsf.write(move |ec| unsafe {
        let range = context.GetEnd(ec)?;
        *slot.borrow_mut() = Some(range);
        Ok(())
    });
    let mut ctx = ViewContext::default();
    tsf.fixture.view.borrow_mut().replace_range(
        ByteOffset(0)..ByteOffset(0),
        "\u{1f600}",
        &mut ctx,
    );
    tsf.doc.sync();
    assert!(tsf.doc.store().counts().text_changes_sent >= 1);
    assert_eq!(tsf.read_text(), "\u{1f600}abc");
    let range = kept.borrow().clone().unwrap();
    let (out, context) = (Rc::new(Cell::new(false)), tsf.context());
    let done = out.clone();
    tsf.write(move |ec| unsafe {
        let end = context.GetEnd(ec)?;
        done.set(range.IsEqualEnd(ec, &end, TF_ANCHOR_END)?.as_bool());
        Ok(())
    });
    assert!(out.get(), "TSF moved the range with the text");
    tsf.fixture.no_problems();
}

// ---- Geometry through TSF ----

#[test]
fn a_composition_with_no_layout_yet_says_so_and_tsf_hears_when_it_comes() {
    let mut view = laid_out("ab");
    view.test_pause_workers(true);
    let tsf = Tsf::new(view);
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    assert!(!tsf.fixture.view.borrow().composition().unwrap().showing);
    let range = held.borrow().as_ref().unwrap().1.clone();
    let (answer, context) = (Rc::new(Cell::new(HRESULT(0))), tsf.context());
    let out = answer.clone();
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        if let Err(e) = view.GetTextExt(ec, &range, &mut rect, &mut clipped) {
            out.set(e.code());
        }
        Ok(())
    });
    assert_eq!(answer.get(), TF_E_NOLAYOUT);
    let before = tsf.doc.store().counts().layout_changes_sent;
    tsf.fixture.view.borrow_mut().test_pause_workers(false);
    settle(&mut tsf.fixture.view.borrow_mut());
    tsf.doc.layout_changed();
    assert_eq!(tsf.doc.store().counts().layout_changes_sent, before + 1);
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

#[test]
fn line_breaks_and_combining_marks_in_a_commit_stay_as_they_came() {
    let text = "x\r\ne\u{301}";
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, text);
    assert_eq!(tsf.read_text(), format!("{text}ab"));
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), format!("{text}ab"));
    assert_eq!(tsf.read_text(), format!("{text}ab"));
    tsf.fixture.no_problems();
}

#[test]
fn tsf_asks_for_places_through_its_view_and_gets_no_layout_then_screen_rects() {
    let tsf = Tsf::new(EditorView::from_text("hello").unwrap());
    let answers: Rc<RefCell<Vec<HRESULT>>> = Rc::default();
    let (out, context) = (answers.clone(), tsf.context());
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        let range = context.GetStart(ec)?;
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        out.borrow_mut()
            .push(match view.GetTextExt(ec, &range, &mut rect, &mut clipped) {
                Ok(()) => HRESULT(0),
                Err(e) => e.code(),
            });
        Ok(())
    });
    assert_eq!(answers.borrow().as_slice(), &[TF_E_NOLAYOUT]);
    support::lay_out(&mut tsf.fixture.view.borrow_mut(), 600.0, 300.0);
    tsf.doc.layout_changed();
    assert_eq!(tsf.doc.store().counts().layout_changes_sent, 1);

    let rects: Rc<RefCell<Option<(RECT, RECT, i32)>>> = Rc::default();
    let (out, context) = (rects.clone(), tsf.context());
    let hwnd = tsf.fixture.window.0;
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        assert_eq!(view.GetWnd()?, hwnd);
        let range = context.GetStart(ec)?;
        let mut moved = 0;
        range.ShiftEnd(ec, 5, &mut moved, std::ptr::null())?;
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        view.GetTextExt(ec, &range, &mut rect, &mut clipped)?;
        let screen = view.GetScreenExt()?;
        let at = POINT {
            x: screen.left + 1,
            y: screen.top + 2,
        };
        let from_point = view.GetRangeFromPoint(ec, &at, 0)?;
        let start = context.GetStart(ec)?;
        let offset = from_point.CompareStart(ec, &start, TF_ANCHOR_START)?;
        *out.borrow_mut() = Some((rect, screen, offset));
        Ok(())
    });
    let (rect, screen, offset) = rects.borrow().unwrap();
    assert!(rect.right > rect.left, "{rect:?}");
    assert!(
        rect.left >= screen.left && rect.right <= screen.right,
        "{rect:?} in {screen:?}"
    );
    assert_eq!(
        offset, 0,
        "a point at the view's left is the first character"
    );
    tsf.fixture.no_problems();
}

#[test]
fn the_candidate_place_is_the_composition_once_it_is_laid_out() {
    let tsf = Tsf::new(laid_out("first line\nsecond"));
    let mut ctx = ViewContext::default();
    tsf.fixture
        .view
        .borrow_mut()
        .select(ByteOffset(11)..ByteOffset(11), &mut ctx);
    tsf.doc.sync();
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "\u{304b}\u{306a}");
    settle(&mut tsf.fixture.view.borrow_mut());
    let range = held.borrow().as_ref().unwrap().1.clone();
    let rects: Rc<RefCell<Option<(RECT, RECT)>>> = Rc::default();
    let (out, context) = (rects.clone(), tsf.context());
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        view.GetTextExt(ec, &range, &mut rect, &mut clipped)?;
        let line_one = context.GetStart(ec)?;
        let mut moved = 0;
        line_one.ShiftEnd(ec, 5, &mut moved, std::ptr::null())?;
        let (mut first, mut clipped) = (RECT::default(), BOOL(0));
        view.GetTextExt(ec, &line_one, &mut first, &mut clipped)?;
        *out.borrow_mut() = Some((rect, first));
        Ok(())
    });
    let (rect, first) = rects.borrow().unwrap();
    assert!(
        rect.top >= first.top + 10,
        "on the second line: {rect:?} under {first:?}"
    );
    assert!(rect.right - rect.left > 8, "two wide characters: {rect:?}");
    tsf.doc.layout_changed();
    assert!(tsf.doc.store().counts().layout_changes_sent >= 1);
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

// ---- A store that fails ----

#[test]
fn a_session_that_fails_after_writing_keeps_the_composition_and_the_text() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    let range = held.borrow().as_ref().unwrap().1.clone();
    let answer = tsf.session(TF_ES_SYNC.0 | TF_ES_READWRITE.0, move |ec| unsafe {
        range.SetText(ec, 0, &units("kept"))?;
        Err(E_FAIL.into())
    });
    assert_eq!(answer, E_FAIL);
    assert_eq!(tsf.read_text(), "keptab");
    assert_eq!(
        tsf.fixture.view.borrow().composition().unwrap().text,
        "kept",
        "the editor still got the text"
    );
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "keptab");
    let problems = tsf.fixture.problems.borrow();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("Your text is safe"), "{problems:?}");
}

#[test]
fn selection_read_through_tsf_in_a_composition_is_at_its_cursor() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "xyz");
    let (out, context) = (Rc::new(Cell::new((0_i32, 0_i32))), tsf.context());
    let slot = out.clone();
    let range = held.borrow().as_ref().unwrap().1.clone();
    tsf.write(move |ec| unsafe {
        // Moves the cursor to after the first character, as a service does.
        let caret = range.Clone()?;
        let mut moved = 0;
        caret.Collapse(ec, TF_ANCHOR_START)?;
        caret.ShiftStart(ec, 1, &mut moved, std::ptr::null())?;
        let selection = TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(caret)),
            style: TF_SELECTIONSTYLE {
                ase: windows::Win32::UI::TextServices::TF_AE_END,
                fInterimChar: BOOL(0),
            },
        };
        let result = context.SetSelection(ec, std::slice::from_ref(&selection));
        drop(std::mem::ManuallyDrop::into_inner(selection.range));
        result?;
        let mut read = [TF_SELECTION::default()];
        let mut fetched = 0;
        context.GetSelection(ec, TF_DEFAULT_SELECTION, &mut read, &mut fetched)?;
        let got = std::mem::ManuallyDrop::into_inner(read[0].range.clone());
        if let Some(got) = got {
            let start = context.GetStart(ec)?;
            slot.set((
                got.CompareStart(ec, &start, TF_ANCHOR_START)?,
                fetched as i32,
            ));
        }
        Ok(())
    });
    assert_eq!(out.get().1, 1);
    assert!(
        out.get().0 > 0,
        "the selection is past the document's start"
    );
    assert_eq!(
        tsf.fixture
            .view
            .borrow()
            .composition()
            .unwrap()
            .cursor_utf16,
        1,
        "the editor's composition cursor followed"
    );
    assert_eq!(tsf.fixture.text(), "ab");
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "xyzab");
    assert_eq!(
        tsf.fixture.view.borrow().selection(),
        ByteOffset(1)..ByteOffset(1),
        "the cursor stays where the service put it"
    );
    tsf.fixture.no_problems();
}

// ---- Composing text is never dropped unless the user cancels ----

#[test]
fn a_caret_move_commits_the_composition_and_the_host_call_keeps_the_text() {
    let tsf = Tsf::new(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let mut ctx = ViewContext::default();
    tsf.fixture
        .view
        .borrow_mut()
        .select(ByteOffset(2)..ByteOffset(2), &mut ctx);
    assert!(ctx.requests.contains(&ViewRequest::CancelComposition));
    let sent = tsf.doc.store().counts().text_changes_sent;
    tsf.doc.cancel_composition();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "xabc");
    assert_eq!(tsf.read_text(), "xabc");
    assert_eq!(
        tsf.doc.store().counts().text_changes_sent,
        sent,
        "TSF wrote this text, so it must not hear of it as an edit"
    );
    assert!(!tsf.doc.store().is_composing());
    tsf.fixture.no_problems();
}

#[test]
fn a_click_commits_the_composition_even_when_only_a_sync_follows() {
    let tsf = Tsf::new(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let sent = tsf.doc.store().counts().text_changes_sent;
    let mut ctx = ViewContext::default();
    tsf.fixture.view.borrow_mut().handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseDown {
                at: qe_api::Point {
                    x: qe_api::Dip(300.0),
                    y: qe_api::Dip(5.0),
                },
                button: qe_api::MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        },
        &mut ctx,
    );
    tsf.doc.sync();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "xabc");
    assert_eq!(
        tsf.doc.store().counts().text_changes_sent,
        sent,
        "TSF wrote this text, so it must not hear that the text went or came"
    );
    assert_eq!(tsf.read_text(), "xabc");
    tsf.fixture.no_problems();
}

#[test]
fn undo_during_a_composition_commits_it_first_and_then_undoes_it() {
    let tsf = Tsf::new(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let mut ctx = ViewContext::default();
    tsf.fixture
        .view
        .borrow_mut()
        .run(EditorCommand::Undo, &mut ctx);
    tsf.doc.sync();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "abc");
    assert_eq!(tsf.read_text(), "abc", "TSF hears of the undo");
    tsf.fixture.no_problems();
}

// ---- A view in use costs no text and leaves no ghost ----

#[test]
fn tsf_ending_a_composition_while_the_view_is_busy_still_adds_the_text_later() {
    let (tsf, shut) = Tsf::gated(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let composition = held.borrow().as_ref().unwrap().0.clone();
    let shut_now = shut.clone();
    tsf.write(move |ec| unsafe {
        shut_now.set(true);
        composition.EndComposition(ec)
    });
    assert_eq!(tsf.fixture.text(), "ab", "the view is busy");
    assert!(
        tsf.fixture.busy.get() > 0,
        "the host was asked to come back"
    );
    assert!(tsf.doc.store().is_composing(), "the text is still held");
    tsf.doc.sync();
    assert_eq!(tsf.fixture.text(), "ab", "still busy");

    shut.set(false);
    tsf.doc.sync();
    assert_eq!(tsf.fixture.text(), "xab");
    assert_eq!(tsf.read_text(), "xab");
    assert!(
        tsf.fixture.view.borrow().composition().is_none(),
        "no ghost"
    );
    assert!(!tsf.doc.store().is_composing());
    tsf.fixture.no_problems();
}

#[test]
fn complete_while_the_view_is_busy_finishes_at_the_next_sync() {
    let (tsf, shut) = Tsf::gated(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    shut.set(true);
    tsf.doc.complete_composition();
    assert_eq!(
        tsf.terminated.get(),
        0,
        "TSF keeps it until the text can go in"
    );
    assert!(tsf.fixture.busy.get() > 0);
    assert!(tsf.doc.store().is_composing());
    shut.set(false);
    tsf.doc.sync();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "xab");
    assert_eq!(tsf.read_text(), "xab");
    assert!(
        tsf.fixture.view.borrow().composition().is_none(),
        "no ghost"
    );
    tsf.fixture.no_problems();
}

#[test]
fn cancel_while_the_view_is_busy_finishes_at_the_next_sync() {
    let (tsf, shut) = Tsf::gated(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    shut.set(true);
    tsf.doc.cancel_composition();
    assert_eq!(tsf.terminated.get(), 0);
    assert!(tsf.fixture.busy.get() > 0);
    shut.set(false);
    tsf.doc.sync();
    assert_eq!(tsf.terminated.get(), 1);
    assert_eq!(tsf.fixture.text(), "ab");
    assert_eq!(tsf.read_text(), "ab");
    assert!(
        tsf.fixture.view.borrow().composition().is_none(),
        "no ghost"
    );
    tsf.fixture.no_problems();
}

#[test]
fn closing_while_the_view_is_busy_shows_the_text_and_does_not_lose_it_unseen() {
    let (mut tsf, shut) = Tsf::gated(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "lost?");
    shut.set(true);
    tsf.doc.close();
    let problems = tsf.fixture.problems.borrow().join(" | ");
    assert!(
        problems.contains("lost?"),
        "the user sees the text: {problems}"
    );
    shut.set(false);
}

#[test]
fn a_composition_update_while_the_view_is_busy_reaches_the_view_later() {
    let (tsf, shut) = Tsf::gated(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let range = held.borrow().as_ref().unwrap().1.clone();
    let shut_now = shut.clone();
    tsf.write(move |ec| unsafe {
        range.SetText(ec, 0, &units("xy"))?;
        shut_now.set(true);
        Ok(())
    });
    assert_eq!(
        tsf.fixture
            .view
            .borrow()
            .composition()
            .map(|c| c.text.clone()),
        Some("x".to_owned()),
        "the view was busy"
    );
    shut.set(false);
    tsf.doc.sync();
    assert_eq!(
        tsf.fixture
            .view
            .borrow()
            .composition()
            .map(|c| c.text.clone()),
        Some("xy".to_owned())
    );
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "xyab");
    tsf.fixture.no_problems();
}

// ---- Which composition ended ----

#[test]
fn a_second_composition_keeps_going_when_the_first_one_ends() {
    let tsf = Tsf::new(laid_out("ab"));
    let first = tsf.start_composition();
    tsf.set_composition_text(&first, "x");
    let second = tsf.start_composition();
    assert_eq!(tsf.fixture.text(), "xab", "the first one's text went in");
    tsf.set_composition_text(&second, "y");
    assert!(tsf.doc.store().is_composing());

    tsf.end_composition(&first);
    assert!(
        tsf.doc.store().is_composing(),
        "the first one's end must not end the second"
    );
    assert_eq!(tsf.fixture.text(), "xab", "the second is still composing");

    tsf.end_composition(&second);
    assert!(!tsf.doc.store().is_composing());
    assert_eq!(tsf.fixture.text(), "xyab");
    assert_eq!(tsf.read_text(), "xyab");
    tsf.fixture.no_problems();
}

// ---- Closing tabs and threads ----

fn linked_document(tsf: &Tsf) -> Option<ITfDocumentMgr> {
    let doc = tsf.doc.document_manager();
    // The wrapper turns "nothing was linked" into an error, with code 0.
    unsafe {
        tsf.thread
            .thread_manager()
            .AssociateFocus(tsf.fixture.window.0, doc)
    }
    .ok()
}

#[test]
fn closing_a_background_tab_leaves_the_front_tab_linked() {
    let tsf = Tsf::new(laid_out("front"));
    let mut behind = tsf.thread.open_document(tsf.fixture.host.clone()).unwrap();
    behind.focus().unwrap();
    tsf.doc.focus().unwrap();
    behind.close();
    let linked = linked_document(&tsf);
    assert_eq!(
        linked.as_ref(),
        Some(tsf.doc.document_manager()),
        "the front tab must stay linked to the window"
    );
    tsf.fixture.no_problems();
}

#[test]
fn closing_the_front_tab_takes_its_link_away() {
    let mut tsf = Tsf::new(laid_out("front"));
    tsf.doc.close();
    assert!(linked_document(&tsf).is_none());
    tsf.fixture.no_problems();
}

#[test]
fn a_document_outlives_its_thread_handle_and_com_goes_last() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    // A fresh thread, so that nothing else holds this thread's apartment open.
    std::thread::spawn(|| unsafe {
        assert!(CoGetApartmentType(&mut Default::default(), &mut Default::default()).is_err());
        let fixture = Fixture::new(laid_out("ab"));
        let thread = support::start_tsf();
        let doc = thread.open_document(fixture.host.clone()).unwrap();
        // `Fixture::new` joined the apartment too. Only TSF's hold on it is left after this.
        CoUninitialize();
        drop(thread);
        doc.focus().unwrap();
        doc.sync();
        assert!(
            CoGetApartmentType(&mut Default::default(), &mut Default::default()).is_ok(),
            "the document holds COM open"
        );
        drop(doc);
        assert!(
            CoGetApartmentType(&mut Default::default(), &mut Default::default()).is_err(),
            "COM goes when the last holder has gone"
        );
        fixture.no_problems();
    })
    .join()
    .unwrap();
}

// ---- Writes and geometry through a running composition ----

#[test]
fn a_commit_is_not_told_back_to_tsf() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let sent = tsf.doc.store().counts().text_changes_sent;
    tsf.end_composition(&held);
    tsf.doc.sync();
    assert_eq!(tsf.doc.store().counts().text_changes_sent, sent);
    assert_eq!(tsf.fixture.text(), "xab");
}

#[test]
fn a_composition_that_grows_over_the_text_after_it_takes_that_text_in() {
    let tsf = Tsf::new(laid_out("abcd"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let (composition, range) = held.borrow().as_ref().unwrap().clone();
    tsf.write(move |ec| unsafe {
        let longer = range.Clone()?;
        let mut moved = 0;
        longer.ShiftEnd(ec, 2, &mut moved, std::ptr::null())?;
        composition.ShiftEnd(ec, &longer)
    });
    assert_eq!(tsf.read_text(), "xabcd", "nothing changes for TSF");
    assert_eq!(tsf.fixture.text(), "abcd", "the store is not edited yet");
    assert_eq!(
        tsf.fixture
            .view
            .borrow()
            .composition()
            .map(|c| c.text.clone()),
        Some("xab".to_owned()),
        "the view shows the text taken in once"
    );
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "xabcd");
    assert_eq!(tsf.read_text(), "xabcd");
    tsf.fixture.no_problems();
}

#[test]
fn a_write_that_reaches_past_the_composition_grows_it_with_no_commit() {
    let tsf = Tsf::new(laid_out("abcd"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    // TSF would turn this write into a composition of its own and refuse it, so the test
    // makes the call that a text service's write comes down to, with TSF's lock held.
    let store = tsf.doc.store().text_store();
    let seen = Rc::new(RefCell::new(String::new()));
    let out = seen.clone();
    tsf.write(move |_| unsafe {
        store.SetText(0, 0, 2, &units("Q"))?;
        *out.borrow_mut() = support::read(&store, 0, -1);
        Ok(())
    });
    assert_eq!(seen.borrow().as_str(), "Qbcd");
    assert_eq!(tsf.fixture.text(), "abcd", "no commit yet");
    assert_eq!(
        tsf.fixture
            .view
            .borrow()
            .composition()
            .map(|c| c.text.clone()),
        Some("Q".to_owned()),
        "the view shows the new text once"
    );
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "Qbcd");
    tsf.fixture.no_problems();
}

#[test]
fn half_a_surrogate_pair_is_refused_inside_a_composition_too() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let range = held.borrow().as_ref().unwrap().1.clone();
    let answer = tsf.session(TF_ES_SYNC.0 | TF_ES_READWRITE.0, move |ec| unsafe {
        range.SetText(ec, 0, &[0xd83d])
    });
    assert_ne!(answer, HRESULT(0), "the write must fail");
    assert_eq!(tsf.read_text(), "xab");
    tsf.end_composition(&held);
    assert_eq!(tsf.fixture.text(), "xab");
}

/// The screen place of positions `start..end` as TSF asks for them, and for TSF's start.
fn text_rect(tsf: &Tsf, start: i32, end: i32) -> RECT {
    let out: Rc<Cell<RECT>> = Rc::default();
    let (slot, context) = (out.clone(), tsf.context());
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        let range = context.GetStart(ec)?;
        range.cast::<ITfRangeACP>()?.SetExtent(start, end - start)?;
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        view.GetTextExt(ec, &range, &mut rect, &mut clipped)?;
        slot.set(rect);
        Ok(())
    });
    out.get()
}

#[test]
fn a_place_on_a_later_line_answers_through_a_composition() {
    let tsf = Tsf::new(laid_out("ab\ncdef"));
    let before = text_rect(&tsf, 3, 5);
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "xyz");
    settle(&mut tsf.fixture.view.borrow_mut());
    // Three more units in front of it for TSF, and the same place on the screen.
    let after = text_rect(&tsf, 6, 8);
    assert_eq!(after, before);
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

#[test]
fn the_candidate_place_for_a_clause_is_that_clause_and_not_the_whole_composition() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "wwwwwwww");
    settle(&mut tsf.fixture.view.borrow_mut());
    let whole = text_rect(&tsf, 0, 8);
    let head = text_rect(&tsf, 0, 4);
    let tail = text_rect(&tsf, 4, 8);
    assert_eq!(head.left, whole.left);
    assert_eq!(tail.right, whole.right);
    assert!(head.right <= tail.left + 1, "{head:?} then {tail:?}");
    assert!(head.right < whole.right);
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

#[test]
fn a_point_answers_through_a_composition_on_its_line_and_on_later_lines() {
    let tsf = Tsf::new(laid_out("ab\ncdef"));
    let on_e = text_rect(&tsf, 4, 5);
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "wwww");
    settle(&mut tsf.fixture.view.borrow_mut());
    let third = text_rect(&tsf, 2, 3);
    let answers: Rc<Cell<(i32, i32)>> = Rc::default();
    let (slot, context) = (answers.clone(), tsf.context());
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        let inside = view.GetRangeFromPoint(
            ec,
            &POINT {
                x: third.left + 1,
                y: third.top + 2,
            },
            GXFPF_NEAREST,
        )?;
        let below = view.GetRangeFromPoint(
            ec,
            &POINT {
                x: on_e.left + 1,
                y: on_e.top + 2,
            },
            GXFPF_NEAREST,
        )?;
        let (mut a, mut b, mut count) = (0, 0, 0);
        inside
            .cast::<ITfRangeACP>()?
            .GetExtent(&mut a, &mut count)?;
        below.cast::<ITfRangeACP>()?.GetExtent(&mut b, &mut count)?;
        slot.set((a, b));
        Ok(())
    });
    let (inside, below) = answers.get();
    assert_eq!(inside, 2, "the third character of the composition");
    assert_eq!(below, 8, "e, after four more units in front of it");
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

// ---- Clauses ----

#[test]
fn clause_marks_from_a_service_reach_the_view() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "abcdef");
    let (context, range) = (tsf.context(), held.borrow().as_ref().unwrap().1.clone());
    tsf.write(move |ec| unsafe {
        let manager: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        let property = context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
        let mut marks = [
            GUID::from_u128(0x8f3a_0c11_6a2b_4c9e_9d55_0000_0000_0001),
            GUID::from_u128(0x8f3a_0c11_6a2b_4c9e_9d55_0000_0000_0002),
        ]
        .into_iter();
        let (first, second) = (range.Clone()?, range.Clone()?);
        let mut moved = 0;
        first.ShiftEnd(ec, -4, &mut moved, std::ptr::null())?;
        second.ShiftStart(ec, 2, &mut moved, std::ptr::null())?;
        for part in [first, second] {
            let mark = marks.next().unwrap_or_default();
            let atom = manager.RegisterGUID(&mark)?;
            property.SetValue(ec, &part, &VARIANT::from(atom as i32))?;
        }
        Ok(())
    });
    let clauses = tsf.fixture.view.borrow().composition_clauses();
    assert_eq!(
        clauses,
        vec![
            CompositionClause {
                range_utf16: 0..2,
                target: false
            },
            CompositionClause {
                range_utf16: 2..6,
                target: false
            },
        ]
    );
    tsf.doc.complete_composition();
    assert_eq!(tsf.fixture.text(), "abcdefab");
    tsf.fixture.no_problems();
}

// ---- Keys ----

fn key(message: u32) -> MSG {
    MSG {
        message,
        wParam: WPARAM(0x41),
        lParam: LPARAM(0x001e_0001),
        ..Default::default()
    }
}

#[test]
fn with_no_service_listening_a_key_is_not_taken_and_other_messages_never_are() {
    let tsf = Tsf::new(laid_out("ab"));
    assert!(!tsf.thread.key_was_taken(&key(WM_KEYDOWN)));
    assert!(!tsf.thread.key_was_taken(&key(WM_KEYUP)));
    assert!(!tsf.thread.key_was_taken(&key(WM_CHAR)));
}

#[test]
fn the_pump_hands_out_messages_and_says_when_the_loop_should_stop() {
    let tsf = Tsf::new(laid_out("ab"));
    unsafe {
        PostMessageW(
            Some(tsf.fixture.window.0),
            WM_USER + 7,
            WPARAM(0),
            LPARAM(0),
        )
    }
    .unwrap();
    let mut message = MSG::default();
    // Windows has its own messages for a new window, so this looks past them.
    while message.message != WM_USER + 7 {
        assert!(tsf.thread.get_message(&mut message).unwrap());
    }
    unsafe { PostQuitMessage(0) };
    while tsf.thread.get_message(&mut message).unwrap() {
        assert_ne!(message.message, WM_USER + 7, "the message came only once");
    }
    assert_eq!(message.message, WM_QUIT);
}

// ---- Second review ----

#[test]
fn a_hidden_or_minimized_window_gives_all_zero_rectangles_and_says_clipped() {
    let tsf = Tsf::new(laid_out("abc"));
    assert_ne!(text_rect(&tsf, 0, 1), RECT::default());
    tsf.fixture.shown.set(false);
    assert_eq!(text_rect(&tsf, 0, 1), RECT::default());
    tsf.fixture.shown.set(true);
    assert_ne!(text_rect(&tsf, 0, 1), RECT::default());
}

#[test]
fn window_is_shown_is_false_for_a_window_that_was_never_shown() {
    let tsf = Tsf::new(laid_out("abc"));
    assert!(!qe_ime::window_is_shown(tsf.fixture.window.0));
}

#[test]
fn a_range_across_a_composition_and_the_text_after_it_covers_both() {
    let tsf = Tsf::new(laid_out("abcdef"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "xy");
    settle(&mut tsf.fixture.view.borrow_mut());
    let composition = text_rect(&tsf, 0, 2);
    let after = text_rect(&tsf, 2, 6);
    let both = text_rect(&tsf, 0, 6);
    assert_eq!(both.left, composition.left);
    assert_eq!(both.right, after.right);
    assert!(both.right > composition.right);
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

#[test]
fn a_closed_document_refuses_focus_and_keeps_nothing_owed() {
    let (mut tsf, shut) = Tsf::gated(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    shut.set(true);
    tsf.doc.cancel_composition();
    tsf.doc.close();
    shut.set(false);
    tsf.doc.sync();
    assert!(!tsf.doc.store().is_composing());
    assert!(!tsf.doc.store().lock_is_waiting());
    assert_eq!(tsf.fixture.text(), "ab");
    assert!(tsf.doc.focus().is_err());
}

// ---- Third review: the view ended the composition first ----

fn caret_to(tsf: &Tsf, byte: usize) {
    tsf.fixture.view.borrow_mut().select(
        ByteOffset(byte)..ByteOffset(byte),
        &mut ViewContext::default(),
    );
}

/// `x` composing in front of `abc`.
fn composing_x(tsf: &Tsf) {
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
}

fn finished_as(tsf: &Tsf, text: &str) {
    assert_eq!(tsf.fixture.text(), text);
    assert_eq!(tsf.read_text(), text, "TSF's copy");
    assert!(!tsf.doc.store().is_composing());
    assert!(
        tsf.fixture.view.borrow().composition().is_none(),
        "no ghost"
    );
    assert_eq!(tsf.terminated.get(), 1);
    tsf.fixture.no_problems();
}

#[test]
fn complete_after_the_view_committed_does_not_add_the_text_twice() {
    let tsf = Tsf::new(laid_out("abc"));
    composing_x(&tsf);
    caret_to(&tsf, 2);
    tsf.doc.complete_composition();
    finished_as(&tsf, "xabc");
}

#[test]
fn close_after_the_view_committed_does_not_add_the_text_twice() {
    let mut tsf = Tsf::new(laid_out("abc"));
    composing_x(&tsf);
    caret_to(&tsf, 2);
    tsf.doc.close();
    assert_eq!(tsf.fixture.text(), "xabc");
    assert!(tsf.fixture.view.borrow().composition().is_none());
    assert_eq!(tsf.terminated.get(), 1);
    tsf.fixture.no_problems();
}

#[test]
fn complete_after_esc_in_the_view_does_not_bring_the_text_back() {
    let tsf = Tsf::new(laid_out("abc"));
    composing_x(&tsf);
    esc(&mut tsf.fixture.view.borrow_mut());
    tsf.doc.complete_composition();
    finished_as(&tsf, "abc");
}

#[test]
fn an_owed_complete_after_the_view_committed_does_not_add_the_text_twice() {
    let (tsf, shut) = Tsf::gated(laid_out("abc"));
    composing_x(&tsf);
    shut.set(true);
    tsf.doc.complete_composition();
    caret_to(&tsf, 2);
    shut.set(false);
    tsf.doc.sync();
    finished_as(&tsf, "xabc");
}

#[test]
fn an_owed_complete_after_undo_does_not_bring_the_text_back() {
    let (tsf, shut) = Tsf::gated(laid_out("abc"));
    composing_x(&tsf);
    shut.set(true);
    tsf.doc.complete_composition();
    tsf.fixture
        .view
        .borrow_mut()
        .run(EditorCommand::Undo, &mut ViewContext::default());
    shut.set(false);
    tsf.doc.sync();
    finished_as(&tsf, "abc");
}

#[test]
fn an_owed_cancel_after_the_view_committed_keeps_the_text_once() {
    let (tsf, shut) = Tsf::gated(laid_out("abc"));
    composing_x(&tsf);
    shut.set(true);
    tsf.doc.cancel_composition();
    caret_to(&tsf, 2);
    shut.set(false);
    tsf.doc.sync();
    finished_as(&tsf, "xabc");
}

#[test]
fn tsf_ending_it_while_busy_and_then_the_view_committing_adds_the_text_once() {
    let (tsf, shut) = Tsf::gated(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let composition = held.borrow().as_ref().unwrap().0.clone();
    let shut_now = shut.clone();
    tsf.write(move |ec| unsafe {
        shut_now.set(true);
        composition.EndComposition(ec)
    });
    caret_to(&tsf, 2);
    shut.set(false);
    tsf.doc.sync();
    assert_eq!(tsf.fixture.text(), "xabc");
    assert_eq!(tsf.read_text(), "xabc");
    assert!(!tsf.doc.store().is_composing());
    tsf.fixture.no_problems();
}

#[test]
fn a_waiting_key_owes_the_complete_and_a_commit_from_running_it_adds_the_text_once() {
    let tsf = Tsf::new(laid_out(&"\u{4e2d}\u{6587} text\n".repeat(200)));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    settle(&mut tsf.fixture.view.borrow_mut());
    {
        let mut v = tsf.fixture.view.borrow_mut();
        v.test_pause_workers(true);
        support::lay_out_without_waiting(&mut v, 300.0, 200.0);
        let mut ctx = ViewContext::default();
        v.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::KeyDown {
                    virtual_key: 0x23, // End, which waits for the new layout.
                    modifiers: Modifiers::default(),
                    repeat: false,
                },
            },
            &mut ctx,
        );
        assert!(
            v.queued_input_count() > 0,
            "this test needs a key that waits"
        );
    }
    tsf.doc.complete_composition();
    assert!(tsf.doc.store().is_composing(), "the key goes first");
    {
        let mut v = tsf.fixture.view.borrow_mut();
        v.test_pause_workers(false);
        settle(&mut v);
        v.flush_input(&mut ViewContext::default());
    }
    tsf.doc.sync();
    let text = tsf.fixture.text();
    assert_eq!(text.matches('x').count(), 201, "one x and the 200 in text");
    assert!(text.starts_with('x'));
    assert!(!tsf.doc.store().is_composing());
    tsf.fixture.no_problems();
}

#[test]
fn opening_a_file_during_a_composition_commits_the_text_and_refuses() {
    let tsf = Tsf::new(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "typed");
    let answer = tsf
        .fixture
        .view
        .borrow_mut()
        .adopt(qe_editor::LoadedFile::Editable {
            store: qe_buffer::PieceStore::new("other doc"),
            ending: "\n",
        });
    assert!(answer.is_err(), "the host must hear");
    tsf.doc.sync();
    assert_eq!(tsf.fixture.text(), "typedabc");
    assert_eq!(tsf.read_text(), "typedabc");
    assert!(!tsf.doc.store().is_composing());
    tsf.fixture.no_problems();
}

#[test]
fn a_write_inside_a_composition_away_from_its_cursor_leaves_the_cursor_with_its_text() {
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "wxyz");
    let (range, context) = (held.borrow().as_ref().unwrap().1.clone(), tsf.context());
    tsf.write(move |ec| unsafe {
        let head = range.Clone()?;
        head.Collapse(ec, TF_ANCHOR_START)?;
        let mut moved = 0;
        head.ShiftStart(ec, 1, &mut moved, std::ptr::null())?;
        head.Collapse(ec, TF_ANCHOR_START)?;
        let selection = TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(head)),
            style: TF_SELECTIONSTYLE {
                ase: windows::Win32::UI::TextServices::TF_AE_END,
                fInterimChar: BOOL(0),
            },
        };
        let answer = context.SetSelection(ec, std::slice::from_ref(&selection));
        drop(std::mem::ManuallyDrop::into_inner(selection.range));
        answer?;
        let tail = range.Clone()?;
        tail.ShiftStart(ec, 3, &mut moved, std::ptr::null())?;
        tail.SetText(ec, 0, &units("ZZ"))
    });
    assert_eq!(tsf.read_text(), "wxyZZab");
    assert_eq!(
        tsf.fixture
            .view
            .borrow()
            .composition()
            .map(|c| c.cursor_utf16),
        Some(1),
        "the cursor stays after the w"
    );
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

#[test]
fn a_composition_cannot_start_between_cr_and_lf() {
    let tsf = Tsf::new(laid_out("a\r\nb"));
    let context = tsf.context();
    let answer = tsf.session(TF_ES_SYNC.0 | TF_ES_READWRITE.0, move |ec| unsafe {
        let range = context.GetStart(ec)?;
        range.cast::<ITfRangeACP>()?.SetExtent(2, 0)?;
        let composing: ITfContextComposition = context.cast()?;
        let sink: ITfCompositionSink = CompositionSink(Rc::default()).into();
        composing.StartComposition(ec, &range, &sink).map(|_| ())
    });
    // A refused start gives no composition, which the crate's wrapper sees as an error with
    // code 0, so the session's answer says nothing here.
    let _ = answer;
    assert!(!tsf.doc.store().is_composing(), "the start is refused");
    assert_eq!(tsf.fixture.text(), "a\r\nb");
}

// ---- Third review: answers with a composition running ----

#[test]
fn the_end_of_the_document_counts_the_composition() {
    let tsf = Tsf::new(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "xy");
    let store = tsf.doc.store().text_store();
    let end: Rc<Cell<i32>> = Rc::default();
    let out = end.clone();
    tsf.read_session(move |_| unsafe {
        out.set(store.GetEndACP()?);
        Ok(())
    });
    assert_eq!(end.get(), 5);
    tsf.doc.cancel_composition();
}

#[test]
fn a_point_on_a_composition_that_does_not_start_at_zero_counts_what_is_before_it() {
    let tsf = Tsf::new(laid_out("abcd"));
    caret_to(&tsf, 2);
    tsf.doc.sync();
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "wwww");
    settle(&mut tsf.fixture.view.borrow_mut());
    let third = text_rect(&tsf, 4, 5);
    let at: Rc<Cell<i32>> = Rc::default();
    let (out, context) = (at.clone(), tsf.context());
    tsf.read_session(move |ec| unsafe {
        let view = context.GetActiveView()?;
        let hit = view.GetRangeFromPoint(
            ec,
            &POINT {
                x: third.left + 1,
                y: third.top + 2,
            },
            GXFPF_NEAREST,
        )?;
        let (mut start, mut count) = (0, 0);
        hit.cast::<ITfRangeACP>()?
            .GetExtent(&mut start, &mut count)?;
        out.set(start);
        Ok(())
    });
    assert_eq!(at.get(), 4);
    tsf.doc.cancel_composition();
    tsf.fixture.no_problems();
}

#[test]
fn a_cancel_over_selected_text_tells_tsf_the_right_new_end() {
    let tsf = Tsf::new(laid_out("abcdef"));
    tsf.fixture
        .view
        .borrow_mut()
        .select(ByteOffset(1)..ByteOffset(3), &mut ViewContext::default());
    tsf.doc.sync();
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "wxyz");
    assert_eq!(tsf.read_text(), "awxyzdef");
    // A range that TSF moves as it hears of changes.
    let tail: Rc<RefCell<Option<ITfRange>>> = Rc::default();
    let (slot, context) = (tail.clone(), tsf.context());
    tsf.read_session(move |ec| unsafe {
        let range = context.GetStart(ec)?;
        range.cast::<ITfRangeACP>()?.SetExtent(6, 2)?;
        *slot.borrow_mut() = Some(range);
        Ok(())
    });
    tsf.doc.cancel_composition();
    assert_eq!(tsf.fixture.text(), "abcdef");
    let range = tail.borrow().clone().unwrap();
    let extent: Rc<Cell<(i32, i32)>> = Rc::default();
    let out = extent.clone();
    tsf.read_session(move |_| unsafe {
        let (mut start, mut count) = (0, 0);
        range
            .cast::<ITfRangeACP>()?
            .GetExtent(&mut start, &mut count)?;
        out.set((start, count));
        Ok(())
    });
    assert_eq!(extent.get(), (4, 2), "`ef` is still `ef` for TSF");
}

#[test]
fn text_the_view_never_heard_of_still_goes_in_when_the_view_commits() {
    let (tsf, shut) = Tsf::gated(laid_out("abc"));
    let held = tsf.start_composition();
    tsf.set_composition_text(&held, "x");
    let range = held.borrow().as_ref().unwrap().1.clone();
    let shut_now = shut.clone();
    tsf.write(move |ec| unsafe {
        range.SetText(ec, 0, &units("xy"))?;
        shut_now.set(true);
        Ok(())
    });
    // The view shows only `x`, and it commits that.
    caret_to(&tsf, 2);
    shut.set(false);
    tsf.doc.sync();
    assert_eq!(tsf.fixture.text(), "xyabc", "the y must not be lost");
    assert_eq!(tsf.read_text(), "xyabc");
    assert!(!tsf.doc.store().is_composing());
    assert!(
        tsf.fixture.view.borrow().composition().is_none(),
        "no ghost"
    );
    tsf.fixture.no_problems();
}
