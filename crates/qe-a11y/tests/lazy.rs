use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

#[test]
fn wm_getobject_does_not_create_provider_for_other_object_ids() {
    let made = Cell::new(0);
    let text = qe_a11y::provider::wm_getobject(HWND::default(), WPARAM(0), LPARAM(0), || {
        made.set(made.get() + 1);
        let view = qe_editor::EditorView::from_text("abc").unwrap();
        qe_a11y::provider::DocumentProvider::new(
            qe_a11y::TextModel::new(view.worker_snapshot()),
            qe_a11y::TextRange::new(0, 0),
            false,
        )
    });
    assert!(text.is_none());
    assert_eq!(made.get(), 0);
    let widgets = qe_a11y::widgets::wm_getobject(HWND::default(), WPARAM(0), LPARAM(0), || {
        made.set(made.get() + 1);
        panic!("a provider was created for another object ID");
    });
    assert!(widgets.is_none());
    assert_eq!(made.get(), 0);
}
