use qe_a11y::EventTracker;
use qe_a11y::when_listening;
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorView, HostCommand};
use std::cell::Cell;

#[test]
fn no_client_does_no_event_work() {
    let count = Cell::new(0);
    assert_eq!(
        when_listening(false, || {
            count.set(count.get() + 1);
            42
        }),
        None
    );
    assert_eq!(count.get(), 0);
    assert_eq!(
        when_listening(true, || {
            count.set(count.get() + 1);
            42
        }),
        Some(42)
    );
    assert_eq!(count.get(), 1);
}

#[test]
fn change_and_selection_are_coalesced_per_poll() {
    let mut view = EditorView::from_text("abc").unwrap();
    let mut events = EventTracker::default();
    assert!(!events.poll(&view, false).text_changed);
    assert!(!events.poll(&view, true).text_changed);
    let mut ctx = ViewContext::default();
    view.replace_range(ByteOffset(1)..ByteOffset(2), "xy", &mut ctx);
    view.run_host(
        HostCommand::Select {
            range: ByteOffset(0)..ByteOffset(2),
            reveal: false,
        },
        &mut ctx,
    );
    let one = events.poll(&view, true);
    assert!(one.text_changed);
    assert!(one.selection_changed);
    assert!(!events.poll(&view, true).text_changed);
    assert!(!events.poll(&view, true).selection_changed);
}
