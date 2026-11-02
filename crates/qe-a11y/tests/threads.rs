//! Calls from other threads reach the window's thread, or say to ask again.
mod support;
use qe_a11y::UiThreadOnly;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use support::*;

struct SendIt<T>(T);
// SAFETY: these tests use COM objects from this crate, whose counts are atomic and whose calls
// move to the window's thread. That move is what the tests check.
unsafe impl<T> Send for SendIt<T> {}

#[test]
fn a_call_from_another_thread_runs_on_the_window_thread() {
    let window = TestWindow::from_text("hello");
    let ui = window.accessibility();
    let range = SendIt(document_range(&ui.document().unwrap()));
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let range = range;
        let answer = unsafe { range.0.GetText(-1) }.map(|s| s.to_string());
        say.send(answer).expect("report");
    });
    let mut got = None;
    window.pump_until("the other thread", 30, || {
        if let Ok(answer) = heard.try_recv() {
            got = Some(answer);
        }
        got.is_some()
    });
    assert_eq!(got.unwrap().unwrap(), "hello");
}

#[test]
fn a_window_that_never_answers_makes_a_call_say_ask_again() {
    let window = TestWindow::from_text("hello");
    let ui = window.accessibility();
    let range = SendIt(document_range(&ui.document().unwrap()));
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let range = range;
        let began = Instant::now();
        let answer = unsafe { range.0.GetText(-1) }.map(|s| s.to_string());
        say.send((answer, began.elapsed())).expect("report");
    });
    // This thread does not pump, as a busy window would not.
    let (answer, took) = heard
        .recv_timeout(Duration::from_secs(30))
        .expect("an answer");
    assert_code(answer, NOT_YET, "a busy window");
    assert!(took >= Duration::from_millis(1900), "it waited {took:?}");
    // The late message finds nothing to run, so the stack it once meant is safe.
    pump();
    assert_eq!(
        unsafe { document_range(&ui.document().unwrap()).GetText(-1) }.unwrap(),
        "hello"
    );
}

#[test]
fn a_view_already_borrowed_says_ask_again_not_a_crash() {
    let window = TestWindow::from_text("hello");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let held = window.ui.view.borrow_mut();
    assert_code(unsafe { doc.GetText(-1) }, NOT_YET, "a reentered call");
    drop(held);
    assert_eq!(unsafe { doc.GetText(-1) }.unwrap(), "hello");
}

#[test]
fn ui_thread_only_hands_out_the_value_only_on_its_thread() {
    let cell = UiThreadOnly::new(std::rc::Rc::new(5));
    assert_eq!(cell.get().map(|v| **v), Some(5));
    let moved = SendIt(cell);
    std::thread::spawn(move || {
        let moved = moved;
        assert!(moved.0.get().is_none());
        // Dropped here: it leaks instead of touching the Rc on the wrong thread.
    })
    .join()
    .unwrap();
}
