//! The text store with real TSF behind it, in a hidden window. The test plays the part of
//! a text service: it asks TSF for edit sessions, and TSF calls the store. No real input
//! method runs here, so root runs that trial by hand.
#![allow(hidden_glob_reexports)]
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod support;

use qe_api::{ByteOffset, InputEvent, InputKind, Modifiers, View, ViewContext, ViewRequest};
use qe_editor::{EditorCommand, EditorView};
use qe_ime::{TsfDocument, TsfThread};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};
use support::{Fixture, laid_out, settle, units};
use windows::{
    Win32::{
        Foundation::{E_FAIL, POINT, RECT},
        UI::TextServices::{
            ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext,
            ITfContextComposition, ITfEditSession, ITfEditSession_Impl, ITfInsertAtSelection,
            ITfRange, TF_ANCHOR_END, TF_ANCHOR_START, TF_DEFAULT_SELECTION,
            TF_E_COMPOSITION_REJECTED, TF_E_LOCKED, TF_E_NOLAYOUT, TF_E_NOLOCK, TF_E_READONLY,
            TF_E_SYNCHRONOUS, TF_ES_ASYNC, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC,
            TF_IAS_QUERYONLY, TF_S_ASYNC, TF_SELECTION, TF_SELECTIONSTYLE, TS_AE_END,
            TS_SD_READONLY,
        },
    },
    core::{BOOL, HRESULT, Interface, Ref, Result, implement},
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
        let thread = TsfThread::start().unwrap();
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
    fn context(&self) -> ITfContext {
        self.doc.context().clone()
    }
    /// A sync edit session. Returns the session's own result, as TSF hands it back.
    fn session(&self, flags: u32, body: impl FnOnce(u32) -> Result<()> + 'static) -> HRESULT {
        let session: ITfEditSession = Session(RefCell::new(Some(Box::new(body)))).into();
        unsafe {
            self.context().RequestEditSession(
                self.thread.client_id(),
                &session,
                windows::Win32::UI::TextServices::TF_CONTEXT_EDIT_CONTEXT_FLAGS(flags),
            )
        }
        .unwrap()
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
    assert_eq!(tsf.fixture.text(), "abc");
    assert_eq!(
        tsf.read_text(),
        "abc",
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
    let tsf = Tsf::new(laid_out("ab"));
    let held = tsf.start_composition();
    tsf.set_composition_text(
        &held, "x
e{301}",
    );
    assert_eq!(
        tsf.read_text(),
        "x
e{301}ab"
    );
    tsf.end_composition(&held);
    assert_eq!(
        tsf.fixture.text(),
        "x
e{301}ab"
    );
    assert_eq!(
        tsf.read_text(),
        "x
e{301}ab"
    );
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
