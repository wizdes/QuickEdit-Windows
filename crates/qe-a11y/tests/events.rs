//! Events are coalesced, and with no client nothing runs.
mod support;
use qe_a11y::{EventFlags, EventTracker, UiaSlot};
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorView, HostCommand};
use std::sync::atomic::Ordering;
use support::*;
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    UI::{Accessibility::UiaClientsAreListening, WindowsAndMessaging::WM_GETOBJECT},
};

#[test]
fn no_client_reads_nothing_on_the_input_path() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    for _ in 0..100 {
        assert_eq!(ui.after_input_when(false).unwrap(), EventFlags::default());
        ui.widgets_changed_when(false).unwrap();
    }
    assert_eq!(window.counts.editor_calls.load(Ordering::SeqCst), 0);
    assert_eq!(window.counts.widget_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn the_real_listening_check_gates_the_path() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let flags = ui.after_input().unwrap();
    // Any UIA client on this PC can make the system say yes. That is outside this test, so
    // this only checks the case the system reports now.
    if !unsafe { UiaClientsAreListening() }.as_bool() {
        assert_eq!(flags, EventFlags::default());
        assert_eq!(window.counts.editor_calls.load(Ordering::SeqCst), 0);
    } else {
        assert!(flags.text_changed, "a listener hears the first batch");
        assert!(window.counts.editor_calls.load(Ordering::SeqCst) > 0);
    }
}

#[test]
fn a_listening_client_gets_one_event_of_each_kind_per_batch() {
    let window = TestWindow::from_text("abcdef");
    let ui = window.accessibility();
    // The first batch after a client arrives always speaks, so it cannot miss a change.
    let first = ui.after_input_when(true).unwrap();
    assert!(first.text_changed && first.selection_changed && first.caret_moved);
    assert_eq!(ui.after_input_when(true).unwrap(), EventFlags::default());

    let mut ctx = ViewContext::default();
    {
        let mut view = window.ui.view.borrow_mut();
        for i in 0..1000 {
            view.replace_range(ByteOffset(i)..ByteOffset(i), "x", &mut ctx);
        }
    }
    let typed = ui.after_input_when(true).unwrap();
    assert!(typed.text_changed);
    assert_eq!(ui.after_input_when(true).unwrap(), EventFlags::default());

    window.ui.view.borrow_mut().run_host(
        HostCommand::Select {
            range: ByteOffset(1)..ByteOffset(2),
            reveal: false,
        },
        &mut ctx,
    );
    let selected = ui.after_input_when(true).unwrap();
    assert!(!selected.text_changed);
    assert!(selected.selection_changed);
    assert_eq!(ui.after_input_when(true).unwrap(), EventFlags::default());
}

#[test]
fn a_new_document_counts_as_a_text_change() {
    let mut tracker = EventTracker::default();
    let view = EditorView::from_text("abc").unwrap();
    assert!(tracker.poll(1, &view).text_changed);
    assert_eq!(tracker.poll(1, &view), EventFlags::default());
    // Another tab comes forward.
    let moved = tracker.poll(2, &view);
    assert!(moved.text_changed && moved.selection_changed && moved.caret_moved);
}

#[test]
fn while_no_client_listens_the_tracker_keeps_nothing() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    ui.after_input_when(true).unwrap();
    ui.after_input_when(false).unwrap();
    // After a gap, the next listener hears about its first batch again.
    let again = ui.after_input_when(true).unwrap();
    assert!(again.text_changed);
}

const GET_OBJECT: u32 = WM_GETOBJECT;

#[test]
fn wm_getobject_makes_nothing_for_other_ids() {
    let window = TestWindow::from_text("abc");
    let slot = UiaSlot::new();
    let mut made = 0;
    // OBJID_CLIENT, OBJID_WINDOW, and a plain zero.
    for id in [-4isize, 0, -1] {
        let answer = slot.on_window_message(window.hwnd, GET_OBJECT, WPARAM(0), LPARAM(id), || {
            made += 1;
            window.host.clone()
        });
        assert!(answer.is_none());
    }
    assert_eq!(made, 0);
    assert!(slot.current().is_none());
}

#[test]
fn other_messages_make_nothing_and_are_not_taken() {
    let window = TestWindow::from_text("abc");
    let slot = UiaSlot::new();
    let answer = slot.on_window_message(window.hwnd, 0x0400, WPARAM(1), LPARAM(-25), || {
        panic!("this message must not make a provider")
    });
    assert!(answer.is_none());
    assert!(slot.current().is_none());
    // With no provider, the reports read one Option and touch nothing.
    assert_eq!(slot.after_input().unwrap(), EventFlags::default());
    slot.widgets_changed().unwrap();
    assert_eq!(window.counts.editor_calls.load(Ordering::SeqCst), 0);
    assert_eq!(window.counts.widget_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn wm_getobject_accepts_the_id_with_or_without_sign_extension() {
    let window = TestWindow::from_text("abc");
    let mut made = 0;
    for id in [-25isize, 0xFFFF_FFE7] {
        let slot = UiaSlot::new();
        let answer = slot.on_window_message(window.hwnd, GET_OBJECT, WPARAM(0), LPARAM(id), || {
            made += 1;
            window.host.clone()
        });
        assert!(answer.is_some(), "{id:#x}");
        assert!(slot.current().is_some());
        slot.shutdown().unwrap();
    }
    assert_eq!(made, 2);
}

#[test]
fn wm_getobject_makes_one_provider_for_many_requests_and_none_after_close() {
    let window = TestWindow::from_text("abc");
    let slot = UiaSlot::new();
    let mut made = 0;
    let mut roots = Vec::new();
    for _ in 0..3 {
        slot.on_window_message(window.hwnd, GET_OBJECT, WPARAM(0), LPARAM(-25), || {
            made += 1;
            window.host.clone()
        });
        roots.push(slot.current().unwrap().root());
    }
    assert_eq!(made, 1);
    assert_eq!(roots[0], roots[2]);
    slot.shutdown().unwrap();
    slot.shutdown().unwrap();
    let late = slot.on_window_message(window.hwnd, GET_OBJECT, WPARAM(0), LPARAM(-25), || {
        panic!("a closed window must not make a provider")
    });
    assert!(late.is_none());
}
