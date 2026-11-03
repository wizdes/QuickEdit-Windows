//! One test for each finding of the second review, and for each break it found that the other
//! tests let through.
mod support;
use qe_a11y::{
    marshal::crossed_calls,
    search::FIND_PATIENCE,
    text::{Answer, Mover, TextDoc, Unit, byte_of, utf16_of},
    widgets::fail_next_raises_for_tests,
};
use qe_api::{
    ByteOffset, Dip, LineIndex, Rect, TextSnapshot, ViewContext, WidgetId, WidgetRange, WidgetRole,
    testing::StringStore,
};
use qe_editor::{EditorView, HostCommand};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use support::*;
use windows::{
    Win32::{
        Foundation::{E_POINTER, POINT},
        Graphics::Gdi::ClientToScreen,
        System::{
            Com::SAFEARRAY,
            Ole::SafeArrayGetElement,
            Variant::{VARENUM, VARIANT},
        },
        UI::{Accessibility::*, WindowsAndMessaging::DestroyWindow},
    },
    core::{BOOL, BSTR, IUnknown, Interface, Result as WinResult, implement, w},
};

// The `implement` macro names `windows_core` directly.
extern crate qe_a11y as windows_core;

const START: TextPatternRangeEndpoint = TextPatternRangeEndpoint_Start;
const END: TextPatternRangeEndpoint = TextPatternRangeEndpoint_End;
const INVALID_OPERATION: i32 = UIA_E_INVALIDOPERATION as i32;
const NOT_ENABLED: i32 = UIA_E_ELEMENTNOTENABLED as i32;

// ---- Finding 1: FindAttribute and the caller's value ----

/// A COM object that says when it ends.
#[implement(IInvokeProvider)]
struct Flagged(Arc<AtomicBool>);
impl IInvokeProvider_Impl for Flagged_Impl {
    fn Invoke(&self) -> WinResult<()> {
        Ok(())
    }
}
impl Drop for Flagged {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn find_attribute_leaves_the_callers_value_alone() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let ended = Arc::new(AtomicBool::new(false));
    let object: IUnknown = Flagged(ended.clone()).into();
    let value = VARIANT::from(object);
    // An attribute with no value, then attributes with a value of another type.
    for id in [
        UIA_BackgroundColorAttributeId,
        UIA_FontNameAttributeId,
        UIA_FontSizeAttributeId,
        UIA_IsReadOnlyAttributeId,
    ] {
        assert_null(
            unsafe { doc.FindAttribute(id, &value, false) },
            "another value",
        );
        assert!(
            !ended.load(Ordering::SeqCst),
            "the call ended the caller's value"
        );
    }
    // The path that returns early, with no place for the answer.
    let code = unsafe {
        (Interface::vtable(&doc).FindAttribute)(
            doc.as_raw(),
            UIA_FontNameAttributeId,
            std::mem::transmute_copy(&value),
            BOOL(0),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(code, E_POINTER);
    assert!(
        !ended.load(Ordering::SeqCst),
        "the early return ended the caller's value"
    );
    assert!(!value.is_empty(), "the value's type stayed");
    drop(value);
    assert!(
        ended.load(Ordering::SeqCst),
        "the value held one more reference than it should"
    );
}

#[test]
fn find_attribute_with_a_matching_string_finds_it_and_a_bad_value_is_an_error() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let value = VARIANT::from("Cascadia Mono");
    let found = unsafe { doc.FindAttribute(UIA_FontNameAttributeId, &value, false) }.unwrap();
    assert_eq!(unsafe { found.GetText(-1) }.unwrap(), "abc");
    assert_eq!(
        BSTR::try_from(&value).unwrap(),
        "Cascadia Mono",
        "the caller's string stays good"
    );
    // A type that VariantCopy refuses. It gets an error, and no answer.
    let mut bad = VARIANT::default();
    unsafe { (*bad.Anonymous.Anonymous).vt = VARENUM(0x7FFF) };
    let answer = unsafe { doc.FindAttribute(UIA_FontNameAttributeId, &bad, false) };
    let error = answer.expect_err("a refusal");
    assert_ne!(error.code().0, 0, "a real error, not a null answer");
}

// ---- Finding 2: a line on screen is its real row ----

fn wrapped_window(text: &str, width: f32, height: f32) -> TestWindow {
    let mut view = EditorView::from_text(text).expect("a view");
    view.set_wrap(true).expect("wrap");
    let window = TestWindow::new(view);
    window.lay_out(width, height);
    window
}

fn scroll_to(window: &TestWindow, y: f64) {
    window
        .ui
        .view
        .borrow_mut()
        .set_scroll_position(y, &mut ViewContext::default());
    window.settle();
}

/// A range standing at a position, as a client gets from its caret.
fn caret_at(
    window: &TestWindow,
    document: &IRawElementProviderSimple,
    unit: usize,
) -> ITextRangeProvider {
    let byte = {
        let view = window.ui.view.borrow();
        let snapshot = view.worker_snapshot();
        byte_of(snapshot.as_ref(), unit).unwrap()
    };
    window.ui.view.borrow_mut().run_host(
        HostCommand::Select {
            range: ByteOffset(byte)..ByteOffset(byte),
            reveal: false,
        },
        &mut ViewContext::default(),
    );
    let mut active = BOOL(0);
    unsafe { text_of(document).GetCaretRange(&mut active) }.unwrap()
}

fn ends(range: &ITextRangeProvider, doc: &ITextRangeProvider) -> (i32, i32) {
    unsafe {
        (
            range.CompareEndpoints(START, doc, START).unwrap(),
            range.CompareEndpoints(END, doc, START).unwrap(),
        )
    }
}

fn line_at(window: &TestWindow, document: &IRawElementProviderSimple, unit: usize) -> (i32, i32) {
    let doc = document_range(document);
    let range = caret_at(window, document, unit);
    unsafe { range.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
    ends(&range, &doc)
}

/// The height of a row on screen.
fn row_height(window: &TestWindow) -> f64 {
    f64::from(window.ui.view.borrow().visible_rows().rows[0].height.0)
}

/// The first unit of each row on screen.
fn rows_on_screen(window: &TestWindow) -> Vec<usize> {
    let view = window.ui.view.borrow();
    let snapshot = view.worker_snapshot();
    view.visible_rows()
        .rows
        .iter()
        .map(|row| utf16_of(snapshot.as_ref(), row.first_byte.0).unwrap())
        .collect()
}

#[test]
fn each_row_on_screen_is_its_own_line_at_both_edges_of_the_view() {
    // 2,000 units, 20 to a row, in a view 5 rows high.
    let window = wrapped_window(&"x".repeat(2000), 160.0, 80.0);
    let width = 20;
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let height = row_height(&window);
    for scroll in [0.0, 10.0, 10.5, 95.0].map(|rows| rows * height) {
        scroll_to(&window, scroll);
        let rows = rows_on_screen(&window);
        assert!(rows.len() >= 2, "rows on screen at {scroll}: {rows:?}");
        assert_eq!(rows[1] - rows[0], width, "the test's row width");
        for first in &rows {
            let row = (*first as i32, (*first + width) as i32);
            assert_eq!(
                line_at(&window, &document, *first),
                row,
                "row start {first}"
            );
            assert_eq!(
                line_at(&window, &document, *first + width - 1),
                row,
                "last character of the row at {first}, scroll {scroll}"
            );
        }
    }
    // Part of a row shows at each edge here, and each still counts as its own row.
    scroll_to(&window, 10.5 * height);
    let rows = rows_on_screen(&window);
    assert_eq!(rows[0], 200, "half of this row is above the view");
    let last = rows[rows.len() - 1];
    {
        let view = window.ui.view.borrow();
        let area = view.text_area();
        let bottom = view.visible_rows().rows[rows.len() - 1].y.0 + height as f32;
        assert!(
            bottom > area.y.0 + area.height.0,
            "part of the last row is below"
        );
    }
    assert_eq!(
        line_at(&window, &document, last + 5),
        (last as i32, last as i32 + 20)
    );
    assert_eq!(line_at(&window, &document, 205), (200, 220));
}

#[test]
fn a_view_one_row_high_still_gives_that_row() {
    let window = wrapped_window(&"x".repeat(2000), 160.0, 8.0);
    scroll_to(&window, 10.25 * row_height(&window));
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let rows = rows_on_screen(&window);
    assert_eq!(rows, [200], "one row");
    assert_eq!(line_at(&window, &document, 205), (200, 220));
}

#[test]
fn moving_by_line_steps_through_the_rows_and_past_the_last_one() {
    let window = wrapped_window(&"x".repeat(2000), 160.0, 80.0);
    scroll_to(&window, 10.25 * row_height(&window));
    let rows = rows_on_screen(&window);
    assert_eq!(rows[0], 200);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let range = caret_at(&window, &document, 205);
    unsafe { range.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
    assert_eq!(ends(&range, &doc), (200, 220));
    // Each later row on screen, and then the text below the view, which has no rows known, so
    // it counts as the rest of its logical line. The last row on screen ends where it really ends.
    let last = rows[rows.len() - 1] as i32 + 20;
    let mut expected: Vec<(i32, i32)> = rows[1..]
        .iter()
        .map(|first| (*first as i32, *first as i32 + 20))
        .collect();
    expected.push((last, 2000));
    let mut seen = Vec::new();
    for _ in 0..expected.len() {
        assert_eq!(unsafe { range.Move(TextUnit_Line, 1) }.unwrap(), 1);
        seen.push(ends(&range, &doc));
    }
    assert_eq!(seen, expected);
}

#[test]
fn the_last_row_of_a_line_keeps_its_ending_and_no_row_holds_only_the_ending() {
    let text = format!("{}\r\nnext", "x".repeat(35));
    let window = wrapped_window(&text, 160.0, 80.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    assert_eq!(line_at(&window, &document, 25), (20, 37));
    assert_eq!(line_at(&window, &document, 35), (20, 37), "on the ending");
    assert_eq!(line_at(&window, &document, 37), (37, 41));
}

#[test]
fn with_wrap_off_a_long_line_is_one_line_wherever_the_view_is() {
    // Long enough for the editor to lay it out in parts.
    let body = format!("{}\nnext", "x".repeat(300_000));
    let mut view = EditorView::from_text(&body).expect("a view");
    view.set_wrap(false).expect("wrap off");
    let window = TestWindow::new(view);
    window.lay_out(160.0, 80.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    assert!(!window.ui.view.borrow().wraps());
    assert_eq!(line_at(&window, &document, 50), (0, 300_001));
}

#[test]
fn a_very_long_wrapped_line_laid_out_in_parts_still_gives_real_rows() {
    let window = wrapped_window(&"x".repeat(300_000), 160.0, 80.0);
    let height = row_height(&window);
    scroll_to(&window, height * 7_000.5);
    let rows = rows_on_screen(&window);
    assert!(rows.len() >= 2, "rows: {rows:?}");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let last = rows[rows.len() - 1];
    assert_eq!(rows[1] - rows[0], 20);
    assert_eq!(
        line_at(&window, &document, last + 3),
        (last as i32, last as i32 + 20)
    );
    assert_eq!(
        line_at(&window, &document, rows[0] + 3),
        (rows[0] as i32, rows[0] as i32 + 20)
    );
}

// ---- Finding 3: the search limit ----

#[test]
fn a_search_stops_at_five_seconds() {
    assert_eq!(FIND_PATIENCE, Duration::from_secs(5));
}

// ---- Finding 6: SetValue on the document ----

#[test]
fn set_value_on_a_document_is_refused_even_through_a_pattern_kept_from_read_only() {
    let mut view = EditorView::from_text("keep me").unwrap();
    view.set_prevent_editing(true);
    let window = TestWindow::new(view);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let value: IValueProvider = unsafe { document.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert_code(
        unsafe { value.SetValue(w!("ALL NEW TEXT")) },
        INVALID_OPERATION,
        "read-only",
    );
    window.ui.view.borrow_mut().set_prevent_editing(false);
    assert_null(
        unsafe { document.GetPatternProvider(UIA_ValuePatternId) },
        "an editable document offers no Value",
    );
    assert_code(
        unsafe { value.SetValue(w!("ALL NEW TEXT")) },
        INVALID_OPERATION,
        "editable, through the kept pattern",
    );
    assert!(window.actions_after_pump().is_empty(), "the host heard it");
    assert_eq!(window.ui.view.borrow().text().unwrap(), "keep me");
}

// ---- Finding 7: values returned ----

fn tree_window(children: Vec<qe_api::Widget>) -> TestWindow {
    TestWindow::with_tree(EditorView::from_text("abc").unwrap(), root_with(children))
}

fn node_named(root: &IRawElementProviderFragment, wanted: &str) -> IRawElementProviderSimple {
    fn visit(node: IRawElementProviderFragment, wanted: &str) -> Option<IRawElementProviderSimple> {
        let simple: IRawElementProviderSimple = node.cast().unwrap();
        let name = unsafe { simple.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
        if BSTR::try_from(&name).is_ok_and(|n| n == wanted) {
            return Some(simple);
        }
        let mut child = unsafe { node.Navigate(NavigateDirection_FirstChild) }.ok();
        while let Some(found) = child {
            if let Some(hit) = visit(found.clone(), wanted) {
                return Some(hit);
            }
            child = unsafe { found.Navigate(NavigateDirection_NextSibling) }.ok();
        }
        None
    }
    visit(root.clone(), wanted).unwrap_or_else(|| panic!("no node named {wanted}"))
}

fn offscreen(node: &IRawElementProviderSimple) -> bool {
    bool::try_from(&unsafe { node.GetPropertyValue(UIA_IsOffscreenPropertyId) }.unwrap()).unwrap()
}

#[test]
fn offscreen_says_what_cannot_be_seen() {
    let shown = widget(2, WidgetRole::Button, "Shown", 0.0, Vec::new());
    let mut empty = widget(3, WidgetRole::Button, "Empty", 50.0, Vec::new());
    empty.bounds.width = Dip(0.0);
    let far = widget(4, WidgetRole::Button, "Far", 5000.0, Vec::new());
    let in_view = widget(6, WidgetRole::ListItem, "In view", 100.0, Vec::new());
    let mut scrolled = widget(7, WidgetRole::ListItem, "Scrolled out", 100.0, Vec::new());
    scrolled.bounds.y = Dip(400.0);
    let mut list = widget(5, WidgetRole::List, "List", 100.0, vec![in_view, scrolled]);
    list.bounds.height = Dip(100.0);
    // A popup paints outside the item that owns it, and it is still on screen.
    let popup_item = widget(9, WidgetRole::MenuItem, "Popup item", 300.0, Vec::new());
    let mut owner = widget(8, WidgetRole::MenuItem, "Owner", 200.0, vec![popup_item]);
    owner.states.has_popup = true;
    let window = tree_window(vec![shown, empty, far, list, owner]);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    for (name, expected) in [
        ("Shown", false),
        ("Empty", true),
        ("Far", true),
        ("In view", false),
        ("Scrolled out", true),
        ("Popup item", false),
    ] {
        assert_eq!(offscreen(&node_named(&root, name)), expected, "{name}");
    }
    let document = ui.document().unwrap();
    assert!(!offscreen(&document));
    window.ui.document.borrow_mut().as_mut().unwrap().bounds = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(0.0),
        height: Dip(0.0),
    };
    assert!(offscreen(&document), "a document with no size");
}

fn focusable(node: &IRawElementProviderSimple) -> bool {
    bool::try_from(&unsafe { node.GetPropertyValue(UIA_IsKeyboardFocusablePropertyId) }.unwrap())
        .unwrap()
}

#[test]
fn a_control_that_is_off_is_not_keyboard_focusable() {
    let on = widget(2, WidgetRole::Button, "On", 0.0, Vec::new());
    let mut off = widget(3, WidgetRole::Button, "Off", 50.0, Vec::new());
    off.states.enabled = false;
    let window = tree_window(vec![on, off]);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    assert!(focusable(&node_named(&root, "On")));
    assert!(!focusable(&node_named(&root, "Off")));
    let document = ui.document().unwrap();
    assert!(focusable(&document));
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .enabled = false;
    assert!(!focusable(&document));
}

#[test]
fn a_read_only_control_says_invalid_operation_and_one_that_is_off_says_not_enabled() {
    let mut field = widget(2, WidgetRole::TextField, "Field", 0.0, Vec::new());
    field.states.read_only = true;
    field.value = Some("x".into());
    let mut off_field = widget(3, WidgetRole::TextField, "Off field", 50.0, Vec::new());
    off_field.states.enabled = false;
    let range = Some(WidgetRange {
        step: None,
        minimum: 0.0,
        maximum: 10.0,
        value: 3.0,
    });
    let mut bar = widget(4, WidgetRole::ScrollBar, "Bar", 100.0, Vec::new());
    bar.states.read_only = true;
    bar.range_value = range;
    let mut off_bar = widget(5, WidgetRole::ScrollBar, "Off bar", 150.0, Vec::new());
    off_bar.states.enabled = false;
    off_bar.range_value = range;
    let window = tree_window(vec![field, off_field, bar, off_bar]);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let value = |name: &str| -> IValueProvider {
        unsafe { node_named(&root, name).GetPatternProvider(UIA_ValuePatternId) }
            .unwrap()
            .cast()
            .unwrap()
    };
    let range = |name: &str| -> IRangeValueProvider {
        unsafe { node_named(&root, name).GetPatternProvider(UIA_RangeValuePatternId) }
            .unwrap()
            .cast()
            .unwrap()
    };
    assert_code(
        unsafe { value("Field").SetValue(w!("y")) },
        INVALID_OPERATION,
        "a read-only field",
    );
    assert_code(
        unsafe { value("Off field").SetValue(w!("y")) },
        NOT_ENABLED,
        "a field that is off",
    );
    assert_code(
        unsafe { range("Bar").SetValue(1.0) },
        INVALID_OPERATION,
        "a read-only bar",
    );
    assert_code(
        unsafe { range("Off bar").SetValue(1.0) },
        NOT_ENABLED,
        "a bar that is off",
    );
    assert!(window.actions_after_pump().is_empty());
}

#[test]
fn a_posted_action_on_a_control_turned_off_meanwhile_goes_to_action_failed() {
    let button = widget(2, WidgetRole::Button, "Button", 0.0, Vec::new());
    let leaver = widget(3, WidgetRole::Button, "Leaver", 50.0, Vec::new());
    let window = tree_window(vec![button, leaver]);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let invoke = |name: &str| -> IInvokeProvider {
        unsafe { node_named(&root, name).GetPatternProvider(UIA_InvokePatternId) }
            .unwrap()
            .cast()
            .unwrap()
    };
    unsafe { invoke("Button").Invoke() }.unwrap();
    unsafe { invoke("Leaver").Invoke() }.unwrap();
    // Both have said yes. Now, before their messages run, one turns off and one leaves.
    {
        let mut tree = window.ui.tree.borrow_mut();
        tree.children[0].states.enabled = false;
        tree.children.remove(1);
    }
    assert!(
        window.actions_after_pump().is_empty(),
        "an action ran on a control that cannot take it"
    );
    let failed = window.counts.failed.lock().unwrap().clone();
    assert_eq!(
        failed,
        vec![
            (WidgetId(2), qe_a11y::WidgetAction::Invoke, NOT_ENABLED),
            (WidgetId(3), qe_a11y::WidgetAction::Invoke, GONE),
        ]
    );
}

#[test]
fn a_window_destroyed_with_no_shutdown_says_gone_and_not_ask_again() {
    let _one_at_a_time = one_ui_thread();
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let doc = SendIt(document_range(&ui.document().unwrap()));
    unsafe { DestroyWindow(window.hwnd) }.expect("DestroyWindow");
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let doc = doc;
        let before = crossed_calls();
        let code = unsafe { doc.0.GetText(-1) }
            .map(|_| ())
            .map_err(|e| e.code().0);
        say.send((code, crossed_calls() > before)).expect("report");
    });
    let mut got = None;
    window.pump_until("the call", 20, || {
        got = heard.try_recv().ok();
        got.is_some()
    });
    let (code, crossed) = got.unwrap();
    assert!(crossed, "the call must have tried the window");
    assert_eq!(code, Err(GONE));
}

#[test]
fn a_failed_raise_loses_nothing_because_the_next_report_raises_it_again() {
    let check = widget(2, WidgetRole::Checkbox, "Check", 0.0, Vec::new());
    let other = widget(3, WidgetRole::Checkbox, "Other", 50.0, Vec::new());
    let window = tree_window(vec![check, other]);
    let ui = window.accessibility();
    ui.widgets_changed_when(true).expect("the baseline");
    {
        let mut tree = window.ui.tree.borrow_mut();
        tree.children[0].states.checked = true;
        tree.children[1].states.checked = true;
    }
    fail_next_raises_for_tests(1);
    assert!(ui.widgets_changed_when(true).is_err(), "the failure shows");
    let again = ui.widgets_changed_when(true).expect("the second report");
    assert_eq!(again.properties, 2, "both changes come through");
    assert_eq!(
        ui.widgets_changed_when(true).expect("the third").properties,
        0,
        "and then they are done"
    );
}

#[test]
fn a_failed_raise_for_input_is_heard_on_the_next_batch() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    ui.after_input_when(true).expect("the baseline");
    window.ui.view.borrow_mut().replace_range(
        ByteOffset(0)..ByteOffset(0),
        "x",
        &mut ViewContext::default(),
    );
    fail_next_raises_for_tests(1);
    assert!(ui.after_input_when(true).is_err(), "the failure shows");
    assert!(
        ui.after_input_when(true)
            .expect("the next batch")
            .text_changed,
        "the change was lost"
    );
}

// ---- Finding 9: a long cluster ends on a character edge ----

struct Plain(StringStore);
impl TextDoc for Plain {
    fn snapshot(&self) -> &dyn TextSnapshot {
        &self.0
    }
    fn word_at(&mut self, byte: usize) -> Answer<Range<usize>> {
        Ok(byte..byte + 1)
    }
    fn row_stops(&mut self, _: usize) -> Vec<usize> {
        Vec::new()
    }
}

fn assert_on_an_edge(text: &dyn TextSnapshot, unit: usize) {
    let byte = byte_of(text, unit).unwrap();
    assert_eq!(
        utf16_of(text, byte).unwrap(),
        unit,
        "{unit} is inside a pair"
    );
}

#[test]
fn a_very_long_cluster_never_stops_inside_a_surrogate_pair() {
    const TONE: &str = "\u{1F3FB}";
    // Forward: pairs start at odd units, and the window's end lands on an even one.
    let forward = format!("a{}", TONE.repeat(2000));
    let check = StringStore::new(forward.clone());
    let mut doc = Plain(StringStore::new(forward));
    let mut mover = Mover { doc: &mut doc };
    let stop = mover.next(Unit::Character, 0).unwrap().unwrap();
    assert!(stop > 1000, "the test's cluster stopped early, at {stop}");
    assert_on_an_edge(&check, stop);

    // Backward: one mark changes which units start pairs, so the window's start lands in one.
    let backward = format!("a{}\u{301}{}", TONE.repeat(600), TONE.repeat(1500));
    let check = StringStore::new(backward.clone());
    let mut doc = Plain(StringStore::new(backward));
    let mut mover = Mover { doc: &mut doc };
    assert_on_an_edge(&check, 3000);
    let stop = mover.previous(Unit::Character, 3000).unwrap().unwrap();
    assert!(stop > 100 && stop < 2998, "the test's cluster gave {stop}");
    assert_on_an_edge(&check, stop);
}

// ---- Finding 10: breaks that survived ----

fn doubles(array: *mut SAFEARRAY) -> Vec<f64> {
    let count = unsafe { (*array).rgsabound[0].cElements } as i32;
    (0..count)
        .map(|index| {
            let mut value = 0f64;
            unsafe { SafeArrayGetElement(array, &index, (&raw mut value).cast()) }
                .expect("element");
            value
        })
        .collect()
}

fn numbered_lines(count: usize) -> String {
    (0..count).map(|i| format!("line {i}\n")).collect()
}

#[test]
fn scroll_into_view_at_the_bottom_shows_the_ranges_last_line_and_not_the_next() {
    let window = TestWindow::from_text(&numbered_lines(200));
    window.lay_out(400.0, 160.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let start = {
        let view = window.ui.view.borrow();
        view.byte_to_utf16(view.line_range(LineIndex(30)).unwrap().start)
            .unwrap()
            .0
    };
    let range = caret_at(&window, &document, start);
    unsafe { range.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
    unsafe { range.ScrollIntoView(false) }.unwrap();
    // The range's end is the first unit of line 31, and line 30 must end at the view's bottom.
    let height = window.ui.view.borrow().total_height() / 201.0;
    let close = |actual: f64, expected: f64| {
        assert!(
            (actual - expected).abs() < 0.01,
            "scrolled to {actual}, not {expected}"
        );
    };
    close(
        window.ui.view.borrow().scroll_offset(),
        31.0 * height - 160.0,
    );
    unsafe { range.ScrollIntoView(true) }.unwrap();
    close(window.ui.view.borrow().scroll_offset(), 30.0 * height);
}

#[test]
fn a_rectangle_is_clipped_at_the_top_of_the_text_area() {
    let window = TestWindow::from_text(&numbered_lines(50));
    window.ui.dpi.set(Some(96));
    window.lay_out(400.0, 160.0);
    let height = row_height(&window);
    scroll_to(&window, height / 2.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let rects = |unit: usize| {
        let range = caret_at(&window, &document, unit);
        unsafe { range.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
        doubles(unsafe { range.GetBoundingRectangles() }.unwrap())
    };
    let first = rects(0);
    let second = rects(7);
    assert_eq!(first.len(), 4);
    assert_eq!(second.len(), 4);
    assert!(
        (first[3] - height / 2.0).abs() < 0.01,
        "only half of the first row shows, not {}",
        first[3]
    );
    assert!(
        (first[1] + first[3] - second[1]).abs() < 0.01,
        "it ends where the next begins: {first:?} {second:?}"
    );
}

fn screen_origin(window: &TestWindow) -> (f64, f64) {
    let mut origin = POINT::default();
    assert!(unsafe { ClientToScreen(window.hwnd, &mut origin) }.as_bool());
    (f64::from(origin.x), f64::from(origin.y))
}

#[test]
fn a_later_child_covers_an_earlier_one_for_a_point() {
    let under = widget(2, WidgetRole::Button, "Under", 0.0, Vec::new());
    let over = widget(3, WidgetRole::Button, "Over", 0.0, Vec::new());
    let window = tree_window(vec![under, over]);
    window.ui.dpi.set(Some(96));
    let ui = window.accessibility();
    let face: IRawElementProviderFragmentRoot = ui.root().cast().unwrap();
    let (x, y) = screen_origin(&window);
    let found = unsafe { face.ElementProviderFromPoint(x + 5.0, y + 5.0) }.unwrap();
    let simple: IRawElementProviderSimple = found.cast().unwrap();
    let name = unsafe { simple.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
    assert_eq!(BSTR::try_from(&name).unwrap(), "Over");
}

#[test]
fn a_tab_list_requires_a_selection_and_a_list_does_not() {
    let tabs = widget(2, WidgetRole::TabList, "Tabs", 0.0, Vec::new());
    let list = widget(3, WidgetRole::List, "List", 50.0, Vec::new());
    let window = tree_window(vec![tabs, list]);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let required = |name: &str| {
        let selection: ISelectionProvider =
            unsafe { node_named(&root, name).GetPatternProvider(UIA_SelectionPatternId) }
                .unwrap()
                .cast()
                .unwrap();
        unsafe { selection.IsSelectionRequired() }
            .unwrap()
            .as_bool()
    };
    assert!(required("Tabs"));
    assert!(!required("List"));
}

// ---- Uncertain concern 6: a host that is busy ----

/// The README's rule: a host answers from `try_borrow`, so a sent message that arrives during
/// a borrow gets an error and no panic. A panic would end the app and lose the user's text.
#[test]
fn a_tree_borrowed_while_a_client_calls_gets_an_error_and_no_panic() {
    let _one_at_a_time = one_ui_thread();
    let button = widget(2, WidgetRole::Button, "Button", 0.0, Vec::new());
    let window = tree_window(vec![button]);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let node = SendIt(node_named(&root, "Button"));
    let (say, heard) = mpsc::channel();
    let held = window.ui.tree.borrow_mut();
    std::thread::spawn(move || {
        let node = node;
        let code = unsafe { node.0.GetPropertyValue(UIA_NamePropertyId) }
            .map(|_| ())
            .map_err(|e| e.code().0);
        say.send(code).expect("report");
    });
    let mut got = None;
    window.pump_until("the call", 20, || {
        got = heard.try_recv().ok();
        got.is_some()
    });
    drop(held);
    assert_eq!(got.unwrap(), Err(GONE));
}
