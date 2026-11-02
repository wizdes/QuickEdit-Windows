//! The text store's COM interface, called directly as TSF would call it. `real_tsf.rs`
//! has the same paths through real TSF.
#![allow(hidden_glob_reexports)]
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod support;

use qe_api::{ByteOffset, InputEvent, InputKind, Modifiers, View, ViewContext};
use qe_buffer::{MappedStore, PieceStore};
use qe_editor::{EditorView, LoadedFile};
use std::time::Duration;
use support::{Direct, Heard, READ, SYNC, WRITE, area, laid_out, read, units};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, POINT, RECT},
        UI::TextServices::{
            GXFPF_NEAREST, ITextStoreACPSink, ITfCompositionView, TS_AE_END, TS_AE_START,
            TS_E_INVALIDPOINT, TS_E_INVALIDPOS, TS_E_NOLAYOUT, TS_E_NOLOCK, TS_E_READONLY,
            TS_E_SYNCHRONOUS, TS_IAS_NOQUERY, TS_IAS_QUERYONLY, TS_S_ASYNC, TS_SD_LOADING,
            TS_SD_READONLY, TS_SELECTION_ACP, TS_SELECTIONSTYLE, TS_SS_NOHIDDENTEXT, TS_TEXTCHANGE,
        },
    },
    core::{BOOL, ComObject, GUID, HRESULT, Interface},
};

fn view(text: &str) -> EditorView {
    EditorView::from_text(text).unwrap()
}
fn b(n: usize) -> ByteOffset {
    ByteOffset(n)
}
fn code<T>(result: windows::core::Result<T>) -> HRESULT {
    match result {
        Ok(_) => HRESULT(0),
        Err(e) => e.code(),
    }
}
fn selection(start: i32, end: i32, at_start: bool) -> TS_SELECTION_ACP {
    TS_SELECTION_ACP {
        acpStart: start,
        acpEnd: end,
        style: TS_SELECTIONSTYLE {
            ase: if at_start { TS_AE_START } else { TS_AE_END },
            fInterimChar: BOOL(0),
        },
    }
}
fn get_selection(store: &windows::Win32::UI::TextServices::ITextStoreACP) -> TS_SELECTION_ACP {
    let mut out = [TS_SELECTION_ACP::default()];
    let mut got = 0;
    unsafe { store.GetSelection(0, &mut out, &mut got) }.unwrap();
    assert_eq!(got, 1);
    out[0]
}

// ---- Sinks ----

#[test]
fn advise_takes_one_sink_and_only_the_right_interface() {
    let d = Direct::new(view("abc"));
    let other = ComObject::new(support::FakeSink {
        heard: Default::default(),
        on_lock: Default::default(),
        lock_flags: Default::default(),
        fail_text: Default::default(),
    });
    let other: ITextStoreACPSink = other.to_interface();
    unsafe {
        // The wrong interface.
        assert_eq!(
            code(d.store.AdviseSink(&GUID::zeroed(), &other, 1)),
            E_INVALIDARG
        );
        // A second sink.
        assert!(
            d.store
                .AdviseSink(&ITextStoreACPSink::IID, &other, 1)
                .is_err()
        );
        // Taking out a sink that is not in.
        assert!(d.store.UnadviseSink(&other).is_err());
        // The same sink again only changes its mask: no text change now reaches it.
        let same: ITextStoreACPSink = d.sink.to_interface();
        d.store
            .AdviseSink(&ITextStoreACPSink::IID, &same, 0)
            .unwrap();
        let mut ctx = ViewContext::default();
        d.fixture
            .view
            .borrow_mut()
            .replace_range(b(0)..b(0), "x", &mut ctx);
        d.handle.sync();
        assert_eq!(d.take_heard(), vec![]);
        // Out, and then the other can come in. A lock with no sink fails.
        d.store.UnadviseSink(&same).unwrap();
        assert!(d.store.RequestLock(READ | SYNC).is_err());
        d.store
            .AdviseSink(&ITextStoreACPSink::IID, &other, 1)
            .unwrap();
    }
}

// ---- Locks ----

#[test]
fn no_lock_means_ts_e_nolock_and_a_read_lock_cannot_write() {
    let d = Direct::new(view("abc"));
    let store = &d.store;
    let mut out = [TS_SELECTION_ACP::default()];
    let (mut got, mut buffer) = (0, [0_u16; 8]);
    let mut runs = [Default::default(); 1];
    let (mut run_count, mut next) = (0, 0);
    let (mut start, mut end, mut change) = (0, 0, TS_TEXTCHANGE::default());
    let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
    let at = POINT::default();
    unsafe {
        assert_eq!(code(store.GetSelection(0, &mut out, &mut got)), TS_E_NOLOCK);
        assert_eq!(
            code(store.GetText(
                0,
                1,
                &mut buffer,
                &mut got,
                &mut runs,
                &mut run_count,
                &mut next
            )),
            TS_E_NOLOCK
        );
        assert_eq!(code(store.SetText(0, 0, 0, &units("x"))), TS_E_NOLOCK);
        assert_eq!(
            code(store.SetSelection(&[selection(0, 0, false)])),
            TS_E_NOLOCK
        );
        assert_eq!(
            code(store.InsertTextAtSelection(0, &units("x"), &mut start, &mut end, &mut change)),
            TS_E_NOLOCK
        );
        assert_eq!(code(store.GetEndACP()), TS_E_NOLOCK);
        assert_eq!(code(store.GetACPFromPoint(0, &at, 0)), TS_E_NOLOCK);
        assert_eq!(
            code(store.GetTextExt(0, 0, 1, &mut rect, &mut clipped)),
            TS_E_NOLOCK
        );
        // These need no lock.
        assert!(store.GetStatus().is_ok());
        assert_eq!(store.GetActiveView().unwrap(), 0);
        assert_eq!(store.GetWnd(0).unwrap(), d.fixture.window.0);
    }
    let codes = d.locked(READ, |store| unsafe {
        let mut start = 0;
        let mut end = 0;
        let query = code(store.InsertTextAtSelection(
            TS_IAS_QUERYONLY,
            &[],
            &mut start,
            &mut end,
            std::ptr::null_mut(),
        ));
        (
            code(store.SetText(0, 0, 0, &units("x"))),
            code(store.SetSelection(&[selection(0, 0, false)])),
            query,
        )
    });
    assert_eq!(codes, (TS_E_NOLOCK, TS_E_NOLOCK, HRESULT(0)));
    assert_eq!(d.fixture.text(), "abc");
    d.fixture.no_problems();
}

#[test]
fn a_sync_lock_inside_a_lock_is_refused_and_an_async_one_runs_at_its_end() {
    let d = Direct::new(view("abc"));
    let order: std::rc::Rc<std::cell::RefCell<Vec<u32>>> = Default::default();
    let store = d.store.clone();
    let sink = d.sink.clone();
    let seen = order.clone();
    let answers: std::rc::Rc<std::cell::RefCell<Vec<HRESULT>>> = Default::default();
    let answers_out = answers.clone();
    *d.sink.on_lock.borrow_mut() = Some(Box::new(move |flags| {
        seen.borrow_mut().push(flags);
        let inner_seen = seen.clone();
        let inner_store = store.clone();
        // Runs on the next grant, which must come after this lock ended.
        *sink.on_lock.borrow_mut() = Some(Box::new(move |flags| {
            inner_seen.borrow_mut().push(flags);
            // A write lock came, so a write works.
            unsafe { inner_store.SetText(0, 0, 0, &units("Z")) }.map(|_| ())
        }));
        // A panic cannot cross this COM call, so the answers are checked outside it.
        unsafe {
            *answers_out.borrow_mut() = vec![
                store.RequestLock(READ | SYNC)?,
                store.RequestLock(WRITE | SYNC)?,
                store.RequestLock(READ)?,
                // Two waits become one, and a write beats a read.
                store.RequestLock(WRITE)?,
            ];
        }
        Ok(())
    }));
    let session = unsafe { d.store.RequestLock(READ | SYNC) }.unwrap();
    assert_eq!(session, HRESULT(0));
    assert_eq!(
        answers.borrow().as_slice(),
        &[TS_E_SYNCHRONOUS, TS_E_SYNCHRONOUS, TS_S_ASYNC, TS_S_ASYNC]
    );
    assert_eq!(order.borrow().as_slice(), &[READ, WRITE]);
    assert_eq!(d.fixture.text(), "Zabc");
    let counts = d.handle.counts();
    assert_eq!(counts.sync_refused, 2);
    assert_eq!(counts.async_queued, 2);
    assert_eq!(counts.locks_granted, 2);
    // No lock is left behind.
    assert_eq!(
        code(unsafe { d.store.SetText(0, 0, 0, &units("x")) }),
        TS_E_NOLOCK
    );
    d.fixture.no_problems();
}

#[test]
fn an_async_lock_with_none_held_runs_at_once() {
    let d = Direct::new(view("abc"));
    let session = unsafe { d.store.RequestLock(READ) }.unwrap();
    assert_eq!(session, HRESULT(0));
    assert_eq!(d.sink.lock_flags.borrow().as_slice(), &[READ]);
}

#[test]
fn a_view_in_use_refuses_a_sync_lock_and_runs_an_async_one_later() {
    let d = Direct::new(view("abc"));
    {
        let _held = d.fixture.view.borrow_mut();
        let store = &d.store;
        assert_eq!(
            unsafe { store.RequestLock(WRITE | SYNC) }.unwrap(),
            TS_E_SYNCHRONOUS
        );
        assert_eq!(unsafe { store.RequestLock(WRITE) }.unwrap(), TS_S_ASYNC);
        assert!(d.handle.lock_is_waiting());
        // Still busy: `sync` must not run it or lose it.
        d.handle.sync();
        assert!(d.handle.lock_is_waiting());
        assert!(d.sink.lock_flags.borrow().is_empty());
    }
    d.handle.sync();
    assert!(!d.handle.lock_is_waiting());
    assert_eq!(d.sink.lock_flags.borrow().as_slice(), &[WRITE]);
}

#[test]
fn a_waiting_key_holds_back_a_write_lock_but_not_a_read_lock() {
    // Ctrl+End waits for a running index. An edit from TSF must not pass a key like it.
    let (mut v, _remove) = mapped_view("waiting.txt");
    let mut ctx = ViewContext::default();
    v.layout(
        area(600.0, 300.0),
        &qe_layout::DirectWriteLayout::new().unwrap(),
    );
    v.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: 0x23,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
                repeat: false,
            },
        },
        &mut ctx,
    );
    let waiting = v.queued_input_count();
    assert!(waiting > 0, "this test needs a key that waits");
    let d = Direct::new(v);
    assert_eq!(
        unsafe { d.store.RequestLock(WRITE | SYNC) }.unwrap(),
        TS_E_SYNCHRONOUS
    );
    assert_eq!(
        unsafe { d.store.RequestLock(READ | SYNC) }.unwrap(),
        HRESULT(0)
    );
}

#[test]
fn a_sink_that_fails_in_its_lock_keeps_the_text_and_ends_the_lock() {
    let d = Direct::new(view("abc"));
    let store = d.store.clone();
    *d.sink.on_lock.borrow_mut() = Some(Box::new(move |_| {
        unsafe { store.SetText(0, 1, 1, &units("X")) }.unwrap();
        Err(E_FAIL.into())
    }));
    let session = unsafe { d.store.RequestLock(WRITE | SYNC) }.unwrap();
    assert_eq!(session, E_FAIL);
    assert_eq!(d.fixture.text(), "aXbc");
    assert_eq!(d.fixture.problems.borrow().len(), 1);
    assert!(d.fixture.problems.borrow()[0].contains("Your text is safe"));
    assert_eq!(d.read_all(), "aXbc");
}

// ---- Reading ----

#[test]
fn get_text_counts_utf16_through_emoji_combining_marks_and_crlf() {
    // Units: a=0, emoji=1,2, e=3, mark=4, CR=5, LF=6, b=7.
    let d = Direct::new(view("a\u{1f600}e\u{301}\r\nb"));
    let out = d.locked(READ, |store| {
        (
            read(store, 0, -1),
            read(store, 1, 3),
            read(store, 3, 5),
            read(store, 5, 7),
            read(store, 7, 8),
            read(store, 8, 8),
            unsafe { store.GetEndACP() }.unwrap(),
        )
    });
    assert_eq!(out.0, "a\u{1f600}e\u{301}\r\nb");
    assert_eq!(out.1, "\u{1f600}");
    assert_eq!(out.2, "e\u{301}");
    assert_eq!(out.3, "\r\n");
    assert_eq!(out.4, "b");
    assert_eq!(out.5, "");
    assert_eq!(out.6, 8);
}

#[test]
fn get_text_hands_a_pair_over_in_two_calls_when_the_buffer_cuts_it() {
    let d = Direct::new(view("a\u{1f600}b"));
    let out = d.locked(READ, |store| {
        let mut all = Vec::new();
        let mut at = 0;
        loop {
            // Room for one unit at a time, so that each call cuts through the pair.
            let mut one = [0_u16; 1];
            let (mut got, mut run_count, mut next) = (0, 0, 0);
            let mut runs = [Default::default(); 1];
            unsafe {
                store
                    .GetText(
                        at,
                        -1,
                        &mut one,
                        &mut got,
                        &mut runs,
                        &mut run_count,
                        &mut next,
                    )
                    .unwrap();
            }
            if got == 0 {
                break;
            }
            assert_eq!(got, 1);
            assert_eq!(run_count, 1);
            assert_eq!(runs[0].uCount, 1);
            assert_eq!(next, at + 1);
            all.push(one[0]);
            at = next;
        }
        all
    });
    assert_eq!(String::from_utf16(&out).unwrap(), "a\u{1f600}b");
}

#[test]
fn get_text_refuses_positions_outside_the_text() {
    let d = Direct::new(view("abc"));
    let codes = d.locked(READ, |store| {
        let mut buffer = [0_u16; 8];
        let (mut got, mut run_count, mut next) = (0, 0, 0);
        let mut runs = [Default::default(); 1];
        let mut try_range = |a, b| unsafe {
            code(store.GetText(
                a,
                b,
                &mut buffer,
                &mut got,
                &mut runs,
                &mut run_count,
                &mut next,
            ))
        };
        (
            try_range(4, 5),
            try_range(0, 4),
            try_range(2, 1),
            try_range(-2, 1),
        )
    });
    assert_eq!(
        codes,
        (
            TS_E_INVALIDPOS,
            TS_E_INVALIDPOS,
            TS_E_INVALIDPOS,
            TS_E_INVALIDPOS
        )
    );
}

#[test]
fn an_empty_document_answers_every_read() {
    let d = Direct::new(view(""));
    let out = d.locked(READ, |store| {
        (
            read(store, 0, -1),
            unsafe { store.GetEndACP() }.unwrap(),
            get_selection(store),
        )
    });
    assert_eq!(out.0, "");
    assert_eq!(out.1, 0);
    assert_eq!((out.2.acpStart, out.2.acpEnd), (0, 0));
    let done = d.locked(WRITE, |store| unsafe {
        store.SetText(0, 0, 0, &units("\u{1f600}"))
    });
    assert_eq!(done.unwrap().acpNewEnd, 2);
    assert_eq!(d.fixture.text(), "\u{1f600}");
}

// ---- Writing ----

#[test]
fn set_text_uses_utf16_positions_and_says_what_changed() {
    let d = Direct::new(view("a\u{1f600}\r\nb"));
    // Replaces CRLF (units 3 and 4) with an emoji.
    let change = d
        .locked(WRITE, |store| unsafe {
            store.SetText(0, 3, 5, &units("\u{1f431}"))
        })
        .unwrap();
    assert_eq!(change.acpStart, 3);
    assert_eq!(change.acpOldEnd, 5);
    assert_eq!(change.acpNewEnd, 5);
    assert_eq!(d.fixture.text(), "a\u{1f600}\u{1f431}b");
    assert_eq!(d.read_all(), "a\u{1f600}\u{1f431}b");
    let caret = d.locked(READ, get_selection);
    assert_eq!((caret.acpStart, caret.acpEnd), (5, 5));
    d.fixture.no_problems();
}

#[test]
fn set_text_refuses_the_inside_of_a_pair_and_bad_utf16_and_changes_nothing() {
    let d = Direct::new(view("a\u{1f600}b"));
    let codes = d.locked(WRITE, |store| unsafe {
        (
            code(store.SetText(0, 2, 2, &units("x"))),
            code(store.SetText(0, 1, 2, &units("x"))),
            code(store.SetText(0, 0, 9, &units("x"))),
            code(store.SetText(0, 0, 0, &[0xd83d])),
        )
    });
    assert_eq!(codes.0, TS_E_INVALIDPOS);
    assert_eq!(codes.1, TS_E_INVALIDPOS);
    assert_eq!(codes.2, TS_E_INVALIDPOS);
    assert_eq!(codes.3, E_INVALIDARG);
    assert_eq!(d.fixture.text(), "a\u{1f600}b");
}

#[test]
fn insert_at_selection_replaces_it_and_query_only_changes_nothing() {
    let d = Direct::new(view("hello"));
    d.locked(WRITE, |store| unsafe {
        store.SetSelection(&[selection(1, 3, false)])
    })
    .unwrap();
    let query = d.locked(READ, |store| unsafe {
        let (mut start, mut end) = (-1, -1);
        store
            .InsertTextAtSelection(
                TS_IAS_QUERYONLY,
                &[],
                &mut start,
                &mut end,
                std::ptr::null_mut(),
            )
            .unwrap();
        (start, end)
    });
    assert_eq!(query, (1, 3));
    assert_eq!(d.fixture.text(), "hello");
    let done = d.locked(WRITE, |store| unsafe {
        let (mut start, mut end, mut change) = (0, 0, TS_TEXTCHANGE::default());
        store
            .InsertTextAtSelection(0, &units("\u{1f600}Z"), &mut start, &mut end, &mut change)
            .unwrap();
        (start, end, change, get_selection(store))
    });
    assert_eq!((done.0, done.1), (1, 4));
    assert_eq!(
        (done.2.acpStart, done.2.acpOldEnd, done.2.acpNewEnd),
        (1, 3, 4)
    );
    assert_eq!((done.3.acpStart, done.3.acpEnd), (4, 4));
    assert_eq!(d.fixture.text(), "h\u{1f600}Zlo");
    // NOQUERY may pass no out pointers at all.
    d.locked(WRITE, |store| unsafe {
        store.InsertTextAtSelection(
            TS_IAS_NOQUERY,
            &units("!"),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    })
    .unwrap();
    assert_eq!(d.fixture.text(), "h\u{1f600}Z!lo");
}

#[test]
fn query_insert_moves_out_of_a_pair_and_refuses_bad_ranges() {
    let d = Direct::new(view("a\u{1f600}b"));
    let (mut start, mut end) = (0, 0);
    unsafe {
        d.store.QueryInsert(1, 1, 3, &mut start, &mut end).unwrap();
        assert_eq!((start, end), (1, 4));
        d.store.QueryInsert(2, 3, 1, &mut start, &mut end).unwrap();
        assert_eq!(start, 1);
        assert_eq!(
            code(d.store.QueryInsert(0, 9, 1, &mut start, &mut end)),
            E_INVALIDARG
        );
        assert_eq!(
            code(d.store.QueryInsert(3, 1, 1, &mut start, &mut end)),
            E_INVALIDARG
        );
    }
}

#[test]
fn selection_round_trips_in_utf16_and_moves_out_of_a_pair() {
    let d = Direct::new(view("a\u{1f600}\r\nb"));
    let out = d.locked(WRITE, |store| unsafe {
        store.SetSelection(&[selection(1, 5, false)]).unwrap();
        let one = get_selection(store);
        // Inside the pair: out to the pair's edges.
        store.SetSelection(&[selection(2, 2, false)]).unwrap();
        let two = get_selection(store);
        let bad = code(store.SetSelection(&[selection(0, 9, false)]));
        let none = code(store.SetSelection(&[]));
        (one, two, bad, none)
    });
    assert_eq!((out.0.acpStart, out.0.acpEnd), (1, 5));
    assert_eq!(
        d.fixture.view.borrow().selection(),
        b(1)..b(5),
        "the pair, in bytes"
    );
    assert_eq!((out.1.acpStart, out.1.acpEnd), (1, 3));
    assert_eq!(out.2, TS_E_INVALIDPOS);
    assert_eq!(out.3, E_INVALIDARG);
}

// ---- Read only ----

#[test]
fn a_read_only_view_says_so_and_refuses_every_write() {
    let mut v = view("abc");
    v.set_prevent_editing(true);
    let d = Direct::new(v);
    let status = unsafe { d.store.GetStatus() }.unwrap();
    assert_eq!(status.dwDynamicFlags & TS_SD_READONLY, TS_SD_READONLY);
    assert_eq!(
        status.dwStaticFlags & TS_SS_NOHIDDENTEXT,
        TS_SS_NOHIDDENTEXT
    );
    let (mut start, mut end) = (0, 0);
    assert_eq!(
        code(unsafe { d.store.QueryInsert(0, 0, 1, &mut start, &mut end) }),
        TS_E_READONLY
    );
    let codes = d.locked(WRITE, |store| unsafe {
        let (mut start, mut end, mut change) = (0, 0, TS_TEXTCHANGE::default());
        (
            code(store.SetText(0, 0, 0, &units("x"))),
            code(store.InsertTextAtSelection(0, &units("x"), &mut start, &mut end, &mut change)),
            read(store, 0, -1),
        )
    });
    assert_eq!(codes.0, TS_E_READONLY);
    assert_eq!(codes.1, TS_E_READONLY);
    assert_eq!(codes.2, "abc");
    assert_eq!(d.fixture.text(), "abc");
    assert_eq!(
        d.fixture.view.borrow().refused_edits(),
        0,
        "the store stopped it first"
    );
}

#[test]
fn a_read_only_view_says_no_to_a_composition_and_shows_none() {
    // Real TSF stops this earlier, on the store's status. This guard is for a status that
    // TSF has not read yet. It answers before it looks at the composition, so none is needed.
    let mut v = view("abc");
    v.set_prevent_editing(true);
    let d = Direct::new(v);
    let sink = d.handle.composition_sink();
    let allowed = unsafe { sink.OnStartComposition(None::<&ITfCompositionView>) }.unwrap();
    assert!(!allowed.as_bool());
    assert!(!d.handle.is_composing());
    assert!(d.fixture.view.borrow().composition().is_none());
}

#[test]
fn turning_prevent_editing_on_reaches_tsf_as_a_status_change() {
    let d = Direct::new(view("abc"));
    d.fixture.view.borrow_mut().set_prevent_editing(true);
    d.handle.sync();
    assert_eq!(d.take_heard(), vec![Heard::Status(TS_SD_READONLY)]);
    d.handle.sync();
    assert_eq!(d.take_heard(), vec![], "it must be told once");
}

struct Remove(std::path::PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        // The index thread can hold the file for a moment after the view has gone.
        for _ in 0..200 {
            match std::fs::remove_file(&self.0) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        eprintln!("could not remove a test file: {}", self.0.display());
    }
}

fn mapped_view(name: &str) -> (EditorView, Remove) {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("qe-ime-{}-{name}", std::process::id()));
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let line = "some plain text for an index to walk over\n";
    for _ in 0..400_000 {
        file.write_all(line.as_bytes()).unwrap();
    }
    file.flush().unwrap();
    let store = MappedStore::open(&path).unwrap();
    // Slow enough that the index cannot finish before the test has read its status.
    store.set_test_read_delay(Duration::from_millis(40));
    (EditorView::from_mapped(store).unwrap(), Remove(path))
}

#[test]
fn a_mapped_file_is_read_only_and_says_loading_and_not_yet_known_is_an_answer() {
    let (v, _remove) = mapped_view("loading.txt");
    assert!(!v.store_status().complete, "the index must still run");
    let d = Direct::new(v);
    let status = unsafe { d.store.GetStatus() }.unwrap();
    assert_eq!(status.dwDynamicFlags & TS_SD_READONLY, TS_SD_READONLY);
    // Under heavy load the index can finish while the window for this test is made. A status
    // with no loading flag must then mean that it has finished.
    let finished = d.fixture.view.borrow().store_status().complete;
    assert!(status.dwDynamicFlags & TS_SD_LOADING == TS_SD_LOADING || finished);
    let out = d.locked(WRITE, |store| unsafe {
        let end = store.GetEndACP().unwrap();
        let mut buffer = [0_u16; 4];
        let (mut got, mut run_count, mut next) = (0, 0, 0);
        let mut runs = [Default::default(); 1];
        let beyond = code(store.GetText(
            end + 1_000_000,
            end + 1_000_001,
            &mut buffer,
            &mut got,
            &mut runs,
            &mut run_count,
            &mut next,
        ));
        let write = code(store.SetText(0, 0, 0, &units("x")));
        (beyond, write)
    });
    assert_eq!(
        out.0, TS_E_INVALIDPOS,
        "past what is known is not an error to wait on"
    );
    assert_eq!(out.1, TS_E_READONLY);
    if d.fixture.view.borrow().store_status().complete {
        eprintln!("The index finished early, so this run did not see it grow.");
        return;
    }
    // The growth of the index reaches TSF as text added at the end, once a sync looks.
    let known_before = d.fixture.view.borrow().store_status().known_utf16.0;
    let mut waited = 0;
    while d.fixture.view.borrow().store_status().known_utf16.0 == known_before && waited < 2_000 {
        std::thread::sleep(Duration::from_millis(5));
        waited += 1;
    }
    let known_now = d.fixture.view.borrow().store_status().known_utf16.0;
    assert!(known_now > known_before, "the index did not grow");
    d.take_heard();
    d.handle.sync();
    let heard = d.take_heard();
    let Some(Heard::Text(start, old_end, new_end)) = heard.first().cloned() else {
        panic!("no text change from an index that grew: {heard:?}");
    };
    assert_eq!(start, old_end, "text was added, and none replaced");
    assert!(new_end >= known_now as i32);
}

// ---- Editor changes reach TSF ----

#[test]
fn editor_edits_reach_the_sink_in_utf16_and_are_told_once() {
    let d = Direct::new(view("a\u{1f600}b"));
    let mut ctx = ViewContext::default();
    // Replaces b (byte 5, unit 3) with two characters.
    d.fixture
        .view
        .borrow_mut()
        .replace_range(b(5)..b(6), "xy", &mut ctx);
    d.handle.sync();
    assert_eq!(
        d.take_heard(),
        vec![Heard::Text(3, 4, 5), Heard::Selection],
        "the change, then the caret that it moved"
    );
    d.handle.sync();
    assert_eq!(d.take_heard(), vec![], "nothing new");
    // Two edits between syncs arrive in order, each in the text just before it.
    {
        let mut v = d.fixture.view.borrow_mut();
        v.replace_range(b(0)..b(0), "12", &mut ctx);
        v.replace_range(b(2)..b(3), "", &mut ctx);
    }
    d.handle.sync();
    let heard = d.take_heard();
    assert_eq!(heard[..2], [Heard::Text(0, 0, 2), Heard::Text(2, 3, 2)]);
    d.fixture.no_problems();
}

#[test]
fn a_caret_move_alone_is_a_selection_change() {
    let d = Direct::new(view("abc"));
    let mut ctx = ViewContext::default();
    d.fixture.view.borrow_mut().select(b(1)..b(2), &mut ctx);
    d.handle.sync();
    assert_eq!(d.take_heard(), vec![Heard::Selection]);
}

#[test]
fn edits_that_tsf_made_are_not_told_back_to_it() {
    let d = Direct::new(view("abc"));
    d.locked(WRITE, |store| unsafe {
        store.SetText(0, 1, 2, &units("XY"))
    })
    .unwrap();
    d.locked(WRITE, |store| unsafe {
        store.SetSelection(&[selection(0, 1, false)])
    })
    .unwrap();
    d.handle.sync();
    assert_eq!(d.take_heard(), vec![]);
    assert_eq!(d.fixture.text(), "aXYc");
    // An edit from the editor after them still arrives, at the right place.
    let mut ctx = ViewContext::default();
    d.fixture
        .view
        .borrow_mut()
        .replace_range(b(4)..b(4), "!", &mut ctx);
    d.handle.sync();
    assert_eq!(d.take_heard()[0], Heard::Text(4, 4, 5));
}

#[test]
fn an_edit_during_a_lock_waits_for_the_lock_to_end() {
    let d = Direct::new(view("abc"));
    let fixture_view = d.fixture.view.clone();
    let handle_store = d.store.clone();
    let heard_during: std::rc::Rc<std::cell::RefCell<Vec<Heard>>> = Default::default();
    let sink = d.sink.clone();
    let during = heard_during.clone();
    let doc = std::rc::Rc::new(d);
    let doc2 = doc.clone();
    *sink.on_lock.borrow_mut() = Some(Box::new(move |_| {
        let mut ctx = ViewContext::default();
        fixture_view
            .borrow_mut()
            .replace_range(ByteOffset(0)..ByteOffset(0), "x", &mut ctx);
        doc2.handle.sync();
        *during.borrow_mut() = doc2.sink.heard.borrow().clone();
        Ok(())
    }));
    let session = unsafe { handle_store.RequestLock(READ | SYNC) }.unwrap();
    assert_eq!(session, HRESULT(0));
    assert_eq!(
        heard_during.borrow().as_slice(),
        &[],
        "no sink call during a lock"
    );
    assert_eq!(
        doc.take_heard()[0],
        Heard::Text(0, 0, 1),
        "it came at the lock's end"
    );
}

#[test]
fn a_lock_tells_tsf_about_editor_edits_before_it_can_read() {
    let d = Direct::new(view("abc"));
    let mut ctx = ViewContext::default();
    d.fixture
        .view
        .borrow_mut()
        .replace_range(b(0)..b(0), "x", &mut ctx);
    // No sync call from the host this time.
    let heard_before = {
        let sink = d.sink.clone();
        d.locked(READ, move |_| sink.heard.borrow().clone())
    };
    assert_eq!(heard_before[0], Heard::Text(0, 0, 1));
}

#[test]
fn a_new_document_and_a_lost_log_tell_tsf_the_whole_text_changed() {
    let d = Direct::new(view("abc"));
    d.fixture
        .view
        .borrow_mut()
        .adopt(LoadedFile::Editable {
            store: PieceStore::new("longer text"),
            ending: "\n",
        })
        .unwrap();
    d.handle.sync();
    assert_eq!(d.take_heard()[0], Heard::Text(0, 3, 11));
    assert_eq!(d.read_all(), "longer text");

    // More edits than the log holds.
    let mut ctx = ViewContext::default();
    {
        let mut v = d.fixture.view.borrow_mut();
        for _ in 0..5_000 {
            v.replace_range(b(0)..b(0), "z", &mut ctx);
        }
    }
    d.handle.sync();
    let heard = d.take_heard();
    assert_eq!(heard[0], Heard::Text(0, 11, 5_011));
    assert_eq!(
        heard
            .iter()
            .filter(|h| matches!(h, Heard::Text(..)))
            .count(),
        1,
        "one change stands for them all"
    );
    d.fixture.no_problems();
}

#[test]
fn a_sink_that_fails_on_a_change_loses_no_text_and_hears_it_again() {
    let d = Direct::new(view("abc"));
    d.sink.fail_text.set(1);
    let mut ctx = ViewContext::default();
    d.fixture
        .view
        .borrow_mut()
        .replace_range(b(3)..b(3), "d", &mut ctx);
    d.handle.sync();
    assert_eq!(d.take_heard(), vec![]);
    assert_eq!(d.fixture.problems.borrow().len(), 1);
    assert!(d.fixture.problems.borrow()[0].contains("Your text is safe"));
    assert_eq!(d.fixture.text(), "abcd");
    d.handle.sync();
    assert_eq!(d.take_heard()[0], Heard::Text(3, 3, 4));
    assert_eq!(d.read_all(), "abcd");
}

// ---- Geometry ----

#[test]
fn before_layout_the_answer_is_no_layout_and_tsf_hears_when_it_arrives() {
    let d = Direct::new(view("hello"));
    let code_before = d.locked(READ, |store| unsafe {
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        code(store.GetTextExt(0, 0, 1, &mut rect, &mut clipped))
    });
    assert_eq!(code_before, TS_E_NOLAYOUT);
    assert_eq!(d.take_heard(), vec![]);
    // With no answer owed, a layout change costs nothing and says nothing. Here one is owed.
    support::lay_out(&mut d.fixture.view.borrow_mut(), 600.0, 300.0);
    d.handle.layout_changed();
    assert_eq!(d.take_heard(), vec![Heard::Layout]);
    d.handle.layout_changed();
    assert_eq!(d.take_heard(), vec![], "told once");
    let works = d.locked(READ, |store| unsafe {
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        code(store.GetTextExt(0, 0, 1, &mut rect, &mut clipped))
    });
    assert_eq!(works, HRESULT(0));
}

#[test]
fn text_ext_and_screen_ext_answer_in_screen_pixels() {
    let d = Direct::new(laid_out("hello\nsecond line"));
    let out = d.locked(READ, |store| unsafe {
        let (mut rect, mut clipped) = (RECT::default(), BOOL(1));
        store.GetTextExt(0, 0, 5, &mut rect, &mut clipped).unwrap();
        let (mut low, mut low_clipped) = (RECT::default(), BOOL(0));
        store
            .GetTextExt(0, 6, 12, &mut low, &mut low_clipped)
            .unwrap();
        (rect, clipped, low)
    });
    let screen = unsafe { d.store.GetScreenExt(0) }.unwrap();
    // The hidden window sits away from the corner, and the client area is inside it.
    assert!(screen.left >= 100 && screen.top >= 200, "{screen:?}");
    assert_eq!(screen.right - screen.left, 600);
    assert_eq!(screen.bottom - screen.top, 300);
    let (rect, clipped, low) = out;
    assert!(!clipped.as_bool());
    assert!(
        rect.left >= screen.left && rect.right <= screen.right,
        "{rect:?}"
    );
    assert!(rect.right > rect.left && rect.bottom > rect.top, "{rect:?}");
    assert!(
        low.top >= rect.top + 10,
        "the second line is below: {low:?} {rect:?}"
    );
    d.fixture.no_problems();
}

#[test]
fn text_ext_follows_the_window_dpi_and_says_when_clipped() {
    let d = Direct::new(laid_out("hello"));
    let at_96 = unsafe { d.store.GetScreenExt(0) }.unwrap();
    d.fixture.host.dpi.set(192);
    let at_192 = unsafe { d.store.GetScreenExt(0) }.unwrap();
    assert_eq!(at_192.right - at_192.left, (at_96.right - at_96.left) * 2);
    let mut v = d.fixture.view.borrow_mut();
    support::lay_out(&mut v, 600.0, 5.0);
    drop(v);
    let clipped = d.locked(READ, |store| unsafe {
        let (mut rect, mut clipped) = (RECT::default(), BOOL(0));
        store.GetTextExt(0, 0, 5, &mut rect, &mut clipped).unwrap();
        clipped.as_bool()
    });
    assert!(clipped, "the line is taller than the 5 DIP view");
}

#[test]
fn a_point_maps_to_a_position_and_outside_points_follow_the_flags() {
    let d = Direct::new(laid_out("a\u{1f600}bcdefgh\nsecond"));
    let screen = unsafe { d.store.GetScreenExt(0) }.unwrap();
    let out = d.locked(READ, move |store| unsafe {
        let start = POINT {
            x: screen.left + 1,
            y: screen.top + 2,
        };
        let far = POINT {
            x: screen.right + 500,
            y: screen.top + 2,
        };
        (
            store.GetACPFromPoint(0, &start, 0).unwrap(),
            code(store.GetACPFromPoint(0, &far, 0)),
            code(store.GetACPFromPoint(0, &far, GXFPF_NEAREST)),
            code(store.GetACPFromPoint(7, &start, 0)),
        )
    });
    assert_eq!(out.0, 0);
    assert_eq!(out.1, TS_E_INVALIDPOINT);
    assert_eq!(out.2, HRESULT(0));
    assert_eq!(out.3, E_INVALIDARG);
}
