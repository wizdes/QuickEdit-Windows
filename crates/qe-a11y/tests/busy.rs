//! A window that does not answer. Every call says to ask again. None says "no document", "no
//! focus" or "not supported", and work the window never reached never runs later.
mod support;
use std::{sync::atomic::Ordering, sync::mpsc, time::Duration};
use support::*;
use windows::{
    Win32::{System::Variant::VARIANT, UI::Accessibility::*},
    core::{BOOL, Interface},
};

type Check = Box<dyn FnOnce() -> Vec<(&'static str, i32)> + Send>;

fn code<T>(answer: windows::core::Result<T>) -> i32 {
    match answer {
        // Success here means the call made up an answer for a window that never spoke.
        Ok(_) => 1,
        Err(error) => error.code().0,
    }
}

#[test]
fn a_busy_window_is_never_reported_as_something_else() {
    qe_a11y::marshal::set_wait_for_tests(50);
    let mut view = qe_editor::EditorView::from_text("hello").unwrap();
    view.set_prevent_editing(true);
    let window = TestWindow::with_tree(
        view,
        root_with(vec![widget(
            5,
            qe_api::WidgetRole::Button,
            "Do",
            0.0,
            Vec::new(),
        )]),
    );
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let text = text_of(&document);
    let root = ui.root();
    let fragment: IRawElementProviderFragment = document.cast().unwrap();
    let root_fragment: IRawElementProviderFragment = root.cast().unwrap();
    let root_face: IRawElementProviderFragmentRoot = root.cast().unwrap();
    let button: IRawElementProviderFragment =
        unsafe { root_fragment.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let button_simple: IRawElementProviderSimple = button.cast().unwrap();
    let invoke: IInvokeProvider = unsafe { button_simple.GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let value: IValueProvider = unsafe { document.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let before = window.counts.editor_calls.load(Ordering::SeqCst);

    let objects = SendIt((
        doc,
        text,
        document.clone(),
        fragment,
        root_face,
        button,
        invoke,
        value,
    ));
    let (say, heard) = mpsc::channel();
    let check: Check = Box::new(move || {
        let objects = objects;
        let (doc, text, document, fragment, root_face, button, invoke, value) = objects.0;
        let mut active = BOOL(0);
        unsafe {
            vec![
                (
                    "Navigate to the document",
                    code(fragment.Navigate(NavigateDirection_Parent)),
                ),
                (
                    "Navigate from a widget",
                    code(button.Navigate(NavigateDirection_NextSibling)),
                ),
                ("GetFocus", code(root_face.GetFocus())),
                (
                    "hit test",
                    code(root_face.ElementProviderFromPoint(1.0, 1.0)),
                ),
                (
                    "the Value pattern",
                    code(document.GetPatternProvider(UIA_ValuePatternId)),
                ),
                (
                    "the Text pattern",
                    code(document.GetPatternProvider(UIA_TextPatternId)),
                ),
                (
                    "a property",
                    code(document.GetPropertyValue(UIA_NamePropertyId)),
                ),
                (
                    "FontName",
                    code(doc.GetAttributeValue(UIA_FontNameAttributeId)),
                ),
                (
                    "IsReadOnly",
                    code(doc.GetAttributeValue(UIA_IsReadOnlyAttributeId)),
                ),
                (
                    "FindAttribute",
                    code(doc.FindAttribute(UIA_IsReadOnlyAttributeId, &VARIANT::from(true), false)),
                ),
                ("GetText", code(doc.GetText(-1))),
                ("Move", code(doc.Move(TextUnit_Word, 1))),
                ("Clone", code(doc.Clone())),
                ("DocumentRange", code(text.DocumentRange())),
                ("GetCaretRange", code(text.GetCaretRange(&mut active))),
                ("Value's IsReadOnly", code(value.IsReadOnly())),
                ("Value", code(value.Value())),
                ("BoundingRectangle", code(fragment.BoundingRectangle())),
                ("GetRuntimeId", code(fragment.GetRuntimeId())),
                ("Invoke's check of the control", code(invoke.Invoke())),
            ]
        }
    });
    std::thread::spawn(move || say.send(check()).expect("report"));
    // This thread does not pump, as a busy window would not.
    let answers = heard
        .recv_timeout(Duration::from_secs(60))
        .expect("the calls end by themselves");
    for (name, answer) in answers {
        assert_eq!(answer, NOT_YET, "{name} said 0x{:08x}", answer as u32);
    }
    // The work those calls gave up on must not run when the window wakes.
    pump();
    assert_eq!(
        window.counts.editor_calls.load(Ordering::SeqCst),
        before,
        "abandoned work ran later"
    );
    assert!(
        window.actions_after_pump().is_empty(),
        "an abandoned action ran later"
    );
    // Windows drops a sent message when its sender gives up, so the crate must drop the work.
    assert_eq!(
        qe_a11y::marshal::waiting_work(),
        0,
        "abandoned work stayed in the table"
    );
}
