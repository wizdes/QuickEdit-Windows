//! One test for each failure the second review reproduced. Tests of an out value start it at a
//! bad pointer, because a real caller may pass it uninitialized.
mod support;
use qe_a11y::{ReadError, TextRange, WidgetAction, text::read_text};
use qe_api::{
    ByteOffset, LineIndex, LookupError, TextPosition, TextSnapshot, Utf16Offset, ViewContext,
    WidgetRole,
};
use qe_editor::EditorView;
use std::{
    cell::RefCell,
    ffi::c_void,
    ops::Range,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use support::*;
use windows::{
    Win32::{
        Foundation::{LPARAM, WPARAM},
        System::{Com::SAFEARRAY, Ole::SafeArrayGetElement, Variant::VARIANT},
        UI::{Accessibility::*, WindowsAndMessaging::WM_GETOBJECT},
    },
    core::{BOOL, BSTR, HRESULT, IUnknown, Interface, w},
};

const BAD: *mut c_void = std::ptr::without_provenance_mut(1);

fn assert_null_out(what: &str, call: impl FnOnce(*mut *mut c_void) -> HRESULT) {
    let mut out = BAD;
    let status = call(&raw mut out);
    assert_eq!(status.0, 0, "{what}: expected S_OK");
    assert!(out.is_null(), "{what}: the out value was left at {out:?}");
}

fn tree() -> qe_api::Widget {
    let mut field = widget(30, WidgetRole::TextField, "Find", 100.0, Vec::new());
    field.value = Some("a\u{1f600} word".into());
    root_with(vec![
        widget(5, WidgetRole::Button, "Do", 0.0, Vec::new()),
        field,
    ])
}

fn window() -> TestWindow {
    TestWindow::with_tree(EditorView::from_text("one two three").unwrap(), tree())
}

#[test]
fn every_null_answer_writes_null_over_a_bad_out_value() {
    let window = window();
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let query = BSTR::from("not there");
    let raw = doc.as_raw();
    let table = Interface::vtable(&doc);
    assert_null_out("FindText with no match", |out| unsafe {
        (table.FindText)(raw, std::mem::transmute_copy(&query), BOOL(0), BOOL(0), out)
    });
    let other_font = VARIANT::from("Arial");
    assert_null_out("FindAttribute with no match", |out| unsafe {
        (table.FindAttribute)(
            raw,
            UIA_FontNameAttributeId,
            std::mem::transmute_copy(&other_font),
            BOOL(0),
            out,
        )
    });
    let raw = document.as_raw();
    let table = Interface::vtable(&document);
    assert_null_out("a pattern the document lacks", |out| unsafe {
        (table.GetPatternProvider)(raw, UIA_InvokePatternId, out)
    });
    assert_null_out("the document's host provider", |out| unsafe {
        (table.HostRawElementProvider)(raw, out)
    });
    let fragment: IRawElementProviderFragment = document.cast().unwrap();
    let table = Interface::vtable(&fragment);
    assert_null_out("nothing after the document", |out| unsafe {
        (table.Navigate)(fragment.as_raw(), NavigateDirection_NextSibling, out)
    });
    let root = ui.root();
    let table = Interface::vtable(&root);
    assert_null_out("a pattern the root lacks", |out| unsafe {
        (table.GetPatternProvider)(root.as_raw(), UIA_TextPatternId, out)
    });
    let root_fragment: IRawElementProviderFragment = root.cast().unwrap();
    let table = Interface::vtable(&root_fragment);
    assert_null_out("the root's parent", |out| unsafe {
        (table.Navigate)(root_fragment.as_raw(), NavigateDirection_Parent, out)
    });
    let face: IRawElementProviderFragmentRoot = root.cast().unwrap();
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = false;
    let table = Interface::vtable(&face);
    assert_null_out("no focus", |out| unsafe {
        (table.GetFocus)(face.as_raw(), out)
    });
    let tab = widget(9, WidgetRole::Tab, "One", 0.0, Vec::new());
    window.ui.tree.borrow_mut().children.push(tab);
    let last = unsafe { root_fragment.Navigate(NavigateDirection_LastChild) }.unwrap();
    let previous = unsafe { last.Navigate(NavigateDirection_PreviousSibling) }.unwrap();
    let simple: IRawElementProviderSimple = previous.cast().unwrap();
    let table = Interface::vtable(&simple);
    assert_null_out("a widget's host provider", |out| unsafe {
        (table.HostRawElementProvider)(simple.as_raw(), out)
    });
    let item: ISelectionItemProvider =
        unsafe { simple.GetPatternProvider(UIA_SelectionItemPatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert!(unsafe { item.SelectionContainer() }.is_ok());
}

/// Makes the window's own provider, as a client's first WM_GETOBJECT does.
fn provider_of(window: &TestWindow) -> Rc<qe_a11y::Accessibility> {
    let slot = window.slot();
    let host = window.host.clone();
    slot.on_window_message(window.hwnd, WM_GETOBJECT, WPARAM(0), LPARAM(-25), || host)
        .expect("an answer");
    slot.current().expect("the provider")
}

fn field_value_of(ui: &qe_a11y::Accessibility) -> IValueProvider {
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let first = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let second = unsafe { first.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let simple: IRawElementProviderSimple = second.cast().unwrap();
    unsafe { simple.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap()
}

/// Runs SetValue on another thread, which reaches the host inside a sent message.
fn set_value_from_another_thread(window: &TestWindow, value: IValueProvider) -> i32 {
    let moved = SendIt(value);
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let moved = moved;
        let answer = unsafe { moved.0.SetValue(w!("x")) };
        say.send(answer.map_or_else(|e| e.code().0, |()| 0))
            .expect("report");
    });
    let mut got = None;
    window.pump_until("SetValue", 30, || {
        if let Ok(answer) = heard.try_recv() {
            got = Some(answer);
        }
        got.is_some()
    });
    got.expect("an answer")
}

#[test]
fn shutdown_inside_a_sent_message_still_disconnects() {
    let _one_at_a_time = one_ui_thread();
    let window = window();
    let ui = provider_of(&window);
    let document = ui.document().unwrap();
    let held = document_range(&document);
    let value = field_value_of(&ui);
    let answer: Rc<RefCell<Option<windows::core::Result<()>>>> = Rc::default();
    let (slot, said) = (window.slot(), answer.clone());
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if matches!(action, WidgetAction::SetValue(_)) {
            *said.borrow_mut() = Some(slot.shutdown());
        }
    })));
    set_value_from_another_thread(&window, value);
    let said = answer.borrow_mut().take().expect("the action ran");
    said.expect("shutdown inside a sent message");
    assert_code(
        unsafe { held.GetText(-1) },
        GONE,
        "a held range after shutdown",
    );
    assert!(window.slot().current().is_none(), "the slot let go of it");
}

#[test]
fn events_raised_inside_a_sent_message_do_not_fail() {
    let window = window();
    let ui = provider_of(&window);
    let value = field_value_of(&ui);
    let answer: Rc<RefCell<Option<String>>> = Rc::default();
    let (inner, said) = (ui.clone(), answer.clone());
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if matches!(action, WidgetAction::SetValue(_)) {
            let raised = inner.after_input_when(true).map(drop);
            let changed = inner.widgets_changed_when(true).map(drop);
            *said.borrow_mut() = Some(format!("{raised:?} {changed:?}"));
        }
    })));
    set_value_from_another_thread(&window, value);
    assert_eq!(answer.borrow().as_deref(), Some("Ok(()) Ok(())"));
}

#[test]
fn a_sent_call_that_starts_and_runs_long_still_returns_at_its_limit() {
    let window = window();
    let ui = window.accessibility();
    let value = field_value_of(&ui);
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if matches!(action, WidgetAction::SetValue(_)) {
            std::thread::sleep(Duration::from_millis(3200));
        }
    })));
    let moved = SendIt(value);
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let moved = moved;
        let began = Instant::now();
        let answer = unsafe { moved.0.SetValue(w!("x")) };
        say.send((answer.map_or_else(|e| e.code().0, |()| 0), began.elapsed()))
            .expect("report");
    });
    let mut got = None;
    window.pump_until("SetValue", 30, || {
        if let Ok(answer) = heard.try_recv() {
            got = Some(answer);
        }
        got.is_some()
    });
    let (code, took) = got.expect("an answer");
    assert_eq!(code, NOT_YET);
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_millis(3000),
        "it took {took:?}, and the action takes 3.2 s"
    );
}

/// Each read past the first slice fails, as a page out of memory does.
struct FirstSliceOnly {
    text: String,
}
impl TextSnapshot for FirstSliceOnly {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.text.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.text.len())
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        Some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        Some(ByteOffset(utf16.0))
    }
    fn byte_to_position(&self, _: ByteOffset) -> Option<TextPosition> {
        None
    }
    fn position_to_byte(&self, _: TextPosition) -> Option<ByteOffset> {
        None
    }
    fn line_range(&self, _: LineIndex) -> Option<Range<ByteOffset>> {
        None
    }
    fn visit_chunks(&self, _: Range<ByteOffset>, _: &mut dyn FnMut(&str)) -> bool {
        false
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn try_visit_chunks(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        if range.start.0 > 0 {
            return Err(LookupError::NotYetKnown {
                indexed_up_to: range.start,
            });
        }
        visitor(self.text.get(range.start.0..range.end.0).unwrap_or(""));
        Ok(())
    }
}

#[test]
fn a_read_error_after_the_first_slice_is_an_error_not_a_short_answer() {
    let text = FirstSliceOnly {
        text: "x".repeat(300_000),
    };
    assert_eq!(
        read_text(&text, TextRange::new(0, 300_000), -1),
        Err(ReadError::NotYetKnown)
    );
    // A limit inside the first slice never touches the part that fails.
    assert_eq!(
        read_text(&text, TextRange::new(0, 300_000), 5).unwrap(),
        "xxxxx"
    );
}

#[test]
fn move_starts_from_the_start_of_the_unit_a_range_is_in() {
    let window = TestWindow::from_text("one two three\nsecond line\nthird");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let text = |range: &ITextRangeProvider| unsafe { range.GetText(-1) }.unwrap().to_string();
    let range = range_at(&document, 5, 6);
    assert_eq!(unsafe { range.Move(TextUnit_Word, -1) }.unwrap(), -1);
    assert_eq!(text(&range), "one ");
    let range = range_at(&document, 5, 6);
    assert_eq!(unsafe { range.Move(TextUnit_Word, 1) }.unwrap(), 1);
    assert_eq!(text(&range), "three\n");
    let range = range_at(&document, 16, 17);
    assert_eq!(unsafe { range.Move(TextUnit_Line, -1) }.unwrap(), -1);
    assert_eq!(text(&range), "one two three\n");
    let range = range_at(&document, 16, 17);
    assert_eq!(unsafe { range.Move(TextUnit_Paragraph, -1) }.unwrap(), -1);
    assert_eq!(text(&range), "one two three\n");
    let range = range_at(&document, 16, 17);
    assert_eq!(unsafe { range.Move(TextUnit_Line, 1) }.unwrap(), 1);
    assert_eq!(text(&range), "third");
    // In the first unit there is nothing before it, and the range stays as it was.
    let range = range_at(&document, 1, 2);
    assert_eq!(unsafe { range.Move(TextUnit_Word, -1) }.unwrap(), 0);
    assert_eq!(ends(&range, &doc), (1, 2));
}

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

fn long_window() -> TestWindow {
    let lines: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let window = TestWindow::from_text(&lines);
    window.ui.dpi.set(Some(96));
    window.lay_out(400.0, 200.0);
    window
}

fn line_range(doc: &ITextRangeProvider, line: i32) -> ITextRangeProvider {
    let range = unsafe { doc.Clone() }.unwrap();
    unsafe { range.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
    let mut moved = 0;
    while moved < line {
        moved += unsafe { range.Move(TextUnit_Line, line - moved) }.unwrap();
    }
    range
}

fn top_and_bottom(range: &ITextRangeProvider) -> (f64, f64) {
    let rects = doubles(unsafe { range.GetBoundingRectangles() }.unwrap());
    assert_eq!(rects.len(), 4, "one row on screen");
    (rects[1], rects[1] + rects[3])
}

#[test]
fn scroll_into_view_aligns_to_the_top_or_the_bottom() {
    let window = long_window();
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let fragment: IRawElementProviderFragment = document.cast().unwrap();
    let area = unsafe { fragment.BoundingRectangle() }.unwrap();
    let area_top = area.top;
    let selection = window.ui.view.borrow().selection();
    let height = |range: &ITextRangeProvider| {
        let (top, bottom) = top_and_bottom(range);
        bottom - top
    };

    // Text already on screen still moves to the top.
    let visible = line_range(&doc, 5);
    let row = height(&visible);
    unsafe { visible.ScrollIntoView(true) }.unwrap();
    window.settle();
    let (top, _) = top_and_bottom(&visible);
    assert!((top - area_top).abs() < 1.0, "top {top}, area {area_top}");

    // Far text, aligned to the top.
    let far = line_range(&doc, 300);
    unsafe { far.ScrollIntoView(true) }.unwrap();
    window.settle();
    let (top, _) = top_and_bottom(&far);
    assert!((top - area_top).abs() < row, "top {top}, area {area_top}");

    // Far text, aligned to the bottom: its last row ends at the bottom of the text area.
    let area_bottom = area_top + 200.0;
    let other = line_range(&doc, 100);
    unsafe { other.ScrollIntoView(false) }.unwrap();
    window.settle();
    let (_, bottom) = top_and_bottom(&other);
    assert!(
        (bottom - area_bottom).abs() < row,
        "bottom {bottom}, area bottom {area_bottom}"
    );
    assert_eq!(window.ui.view.borrow().selection(), selection);
}

#[test]
fn a_point_anywhere_gets_the_nearest_position() {
    let window = TestWindow::from_text("hello\nworld");
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let text = text_of(&document);
    let at = |x: f64, y: f64| {
        let found = unsafe { text.RangeFromPoint(UiaPoint { x, y }) }.expect("a point");
        ends(&found, &doc)
    };
    assert_eq!(at(-5000.0, -5000.0), (0, 0), "far above and to the left");
    assert_eq!(at(50000.0, 50000.0), (11, 11), "far below and to the right");
    assert_code(
        unsafe {
            text.RangeFromPoint(UiaPoint {
                x: f64::NAN,
                y: 0.0,
            })
        },
        -2147024809,
        "not a number",
    );
}

#[test]
fn a_point_on_a_field_gets_a_position_in_its_text() {
    let window = window();
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let first = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let field = unsafe { first.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let simple: IRawElementProviderSimple = field.cast().unwrap();
    let text = text_of(&simple);
    let doc = unsafe { text.DocumentRange() }.unwrap();
    let area = unsafe { field.BoundingRectangle() }.unwrap();
    let at = |x: f64| {
        let found = unsafe { text.RangeFromPoint(UiaPoint { x, y: area.top }) }.expect("a point");
        ends(&found, &doc)
    };
    assert_eq!(at(area.left - 500.0), (0, 0));
    assert_eq!(at(area.left + area.width + 500.0), (8, 8));
    let (middle, _) = at(area.left + area.width / 2.0);
    assert!((1..8).contains(&middle), "got {middle}");
    assert_ne!(middle, 2, "a position never lands inside the emoji");
}

#[test]
fn an_empty_range_has_no_rectangles() {
    let window = TestWindow::with_tree(EditorView::from_text("hello").unwrap(), tree());
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let caret = range_at(&document, 1, 1);
    assert!(doubles(unsafe { caret.GetBoundingRectangles() }.unwrap()).is_empty());
    let whole = range_at(&document, 0, 5);
    assert_eq!(
        doubles(unsafe { whole.GetBoundingRectangles() }.unwrap()).len(),
        4
    );

    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let first = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let field: IRawElementProviderSimple = unsafe { first.Navigate(NavigateDirection_NextSibling) }
        .unwrap()
        .cast()
        .unwrap();
    let doc = unsafe { text_of(&field).DocumentRange() }.unwrap();
    assert_eq!(
        doubles(unsafe { doc.GetBoundingRectangles() }.unwrap()).len(),
        4
    );
    let empty = unsafe { doc.Clone() }.unwrap();
    unsafe {
        empty.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &doc,
            TextPatternRangeEndpoint_Start,
        )
    }
    .unwrap();
    assert!(doubles(unsafe { empty.GetBoundingRectangles() }.unwrap()).is_empty());
}

#[test]
fn every_call_on_a_stale_range_says_gone() {
    let window = TestWindow::from_text("first");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let other = unsafe { doc.Clone() }.unwrap();
    window.ui.view_id.set(Some(8));
    assert_code(
        unsafe { doc.GetEnclosingElement() },
        GONE,
        "GetEnclosingElement",
    );
    assert_code(unsafe { doc.GetChildren() }, GONE, "GetChildren");
    assert_code(
        unsafe { doc.GetAttributeValue(UIA_BackgroundColorAttributeId) },
        GONE,
        "an attribute with no value",
    );
    assert_code(unsafe { doc.AddToSelection() }, GONE, "AddToSelection");
    assert_code(
        unsafe { doc.RemoveFromSelection() },
        GONE,
        "RemoveFromSelection",
    );
    assert_code(
        unsafe { doc.FindAttribute(UIA_BackgroundColorAttributeId, &VARIANT::from(1i32), false) },
        GONE,
        "FindAttribute",
    );
    assert_code(unsafe { doc.Compare(&other) }, GONE, "Compare");
    let lone = BSTR::from_wide(&[0xd83d]);
    assert_code(
        unsafe { doc.FindText(&lone, false, false) },
        GONE,
        "FindText",
    );
}

fn runtime_id(node: &IRawElementProviderFragment) -> Vec<i32> {
    let array = unsafe { node.GetRuntimeId() }.unwrap();
    let count = unsafe { (*array).rgsabound[0].cElements } as i32;
    (0..count)
        .map(|index| {
            let mut value = 0i32;
            unsafe { SafeArrayGetElement(array, &index, (&raw mut value).cast()) }
                .expect("element");
            value
        })
        .collect()
}

#[test]
fn a_widget_with_the_documents_id_has_a_different_runtime_id() {
    let window = TestWindow::with_tree(
        EditorView::from_text("text").unwrap(),
        root_with(vec![widget(
            1000,
            WidgetRole::Button,
            "Same",
            0.0,
            Vec::new(),
        )]),
    );
    assert_eq!(window.ui.document.borrow().as_ref().unwrap().id.0, 1000);
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let button = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let document = unsafe { root.Navigate(NavigateDirection_LastChild) }.unwrap();
    assert_ne!(runtime_id(&button), runtime_id(&document));
    // Each still finds itself, and neither is taken for the other.
    let name = |node: &IRawElementProviderFragment| {
        let simple: IRawElementProviderSimple = node.cast().unwrap();
        let value = unsafe { simple.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
        BSTR::try_from(&value).unwrap().to_string()
    };
    assert_eq!(name(&button), "Same");
    assert_eq!(name(&document), "Untitled");
    let kept: IUnknown = document.cast().unwrap();
    assert!(!kept.as_raw().is_null());
    let _ = ViewContext::default();
}

#[test]
fn each_widget_change_a_client_can_read_raises_its_event() {
    let mut field = widget(30, WidgetRole::TextField, "Find", 0.0, Vec::new());
    field.value = Some("one".into());
    field.selection = Some(0..1);
    let tab = widget(31, WidgetRole::Tab, "One", 50.0, Vec::new());
    let check = widget(32, WidgetRole::Checkbox, "Wrap", 100.0, Vec::new());
    let mut combo = widget(33, WidgetRole::ComboBox, "Encoding", 150.0, Vec::new());
    combo.value = Some("UTF-8".into());
    let mut scroll = widget(34, WidgetRole::ScrollBar, "Scroll", 200.0, Vec::new());
    scroll.range_value = Some(qe_api::WidgetRange {
        minimum: 0.0,
        maximum: 9.0,
        value: 1.0,
    });
    let window = TestWindow::with_tree(
        EditorView::from_text("x").unwrap(),
        root_with(vec![field, tab, check, combo, scroll]),
    );
    let ui = window.accessibility();
    // The first report only sets the baseline.
    let first = ui.widgets_changed_when(true).unwrap();
    assert_eq!(first.properties + first.text_changed, 0);
    let change = |edit: &dyn Fn(&mut Vec<qe_api::Widget>)| {
        edit(&mut window.ui.tree.borrow_mut().children);
        ui.widgets_changed_when(true).unwrap()
    };

    let raised = change(&|c| c[0].value = Some("two".into()));
    assert_eq!((raised.text_changed, raised.properties), (1, 1), "a value");
    let raised = change(&|c| c[0].selection = Some(1..2));
    assert_eq!(
        (raised.selection_changed, raised.properties),
        (1, 0),
        "a selection"
    );
    let raised = change(&|c| c[1].states.selected = true);
    assert_eq!(
        (raised.element_selected, raised.properties),
        (1, 1),
        "selected"
    );
    let raised = change(&|c| c[1].states.selected = false);
    assert_eq!(
        (raised.element_selected, raised.properties),
        (0, 1),
        "not selected"
    );
    assert_eq!(change(&|c| c[2].states.checked = true).properties, 1, "on");
    assert_eq!(
        change(&|c| c[3].states.expanded = true).properties,
        1,
        "open"
    );
    assert_eq!(
        change(&|c| c[2].states.enabled = false).properties,
        1,
        "turned off"
    );
    assert_eq!(
        change(&|c| c[3].name = "Other".into()).properties,
        1,
        "a name"
    );
    let raised = change(&|c| c[3].value = Some("ASCII".into()));
    assert_eq!(
        (raised.text_changed, raised.properties),
        (0, 1),
        "a combo box value"
    );
    let raised = change(&|c| {
        if let Some(range) = &mut c[4].range_value {
            range.value = 2.0;
        }
    });
    assert_eq!(raised.properties, 1, "a range value");
    window.ui.document.borrow_mut().as_mut().unwrap().name = "Renamed".into();
    assert_eq!(
        ui.widgets_changed_when(true).unwrap().properties,
        1,
        "the tab title"
    );
    // Nothing changed, nothing raised.
    assert_eq!(ui.widgets_changed_when(true).unwrap(), Default::default());
    // With no client, nothing is read, and the next listener starts a new baseline.
    let calls = window
        .counts
        .widget_calls
        .load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(ui.widgets_changed_when(false).unwrap(), Default::default());
    assert_eq!(
        window
            .counts
            .widget_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        calls
    );
}

#[test]
fn a_shutdown_that_fails_can_be_tried_again() {
    let _one_at_a_time = one_ui_thread();
    use std::sync::atomic::Ordering;
    let window = window();
    let ui = provider_of(&window);
    let document = ui.document().unwrap();
    let held = document_range(&document);
    let value = field_value_of(&ui);
    drop(ui);
    let answer: Rc<RefCell<Option<windows::core::Result<()>>>> = Rc::default();
    let (slot, said) = (window.slot(), answer.clone());
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if matches!(action, WidgetAction::SetValue(_)) {
            qe_a11y::widgets::SKIP_REPLY_FOR_TESTS.store(true, Ordering::Relaxed);
            *said.borrow_mut() = Some(slot.shutdown());
            qe_a11y::widgets::SKIP_REPLY_FOR_TESTS.store(false, Ordering::Relaxed);
        }
    })));
    set_value_from_another_thread(&window, value);
    let said = answer.borrow_mut().take().expect("the action ran");
    assert_eq!(
        said.expect_err("UIA refuses inside a sent message")
            .code()
            .0,
        0x8001010Du32 as i32
    );
    // It kept what it needs, and it works from outside the sent message.
    assert!(window.slot().current().is_some(), "kept for another try");
    window.slot().shutdown().expect("the second try");
    assert!(window.slot().current().is_none());
    assert_code(
        unsafe { held.GetText(-1) },
        GONE,
        "a held range after the second try",
    );
}

#[test]
fn a_read_that_fails_on_its_own_thread_is_an_error_too() {
    // This store fails every waiting read, so a short answer could only be made up.
    let text = FirstSliceOnly {
        text: "x".repeat(10),
    };
    assert!(matches!(
        qe_a11y::text::read_text_waiting(&text, TextRange::new(0, 10), -1),
        Err(ReadError::Failed(_))
    ));
    assert_eq!(
        qe_a11y::text::read_text_waiting(&text, TextRange::new(0, 11), -1),
        Err(ReadError::OutOfRange)
    );
}

#[test]
fn a_client_read_of_the_whole_cap_answers_in_one_call_with_the_window_running() {
    let body = "x".repeat(qe_a11y::GET_TEXT_CAP + 100);
    let window = TestWindow::from_text(&body);
    let ui = window.accessibility();
    let doc = SendIt(document_range(&ui.document().unwrap()));
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let doc = doc;
        let answer = unsafe { doc.0.GetText(-1) }.map(|text| text.len());
        say.send(answer.map_err(|e| e.code().0)).expect("report");
    });
    let mut got = None;
    window.pump_until("GetText", 30, || {
        if let Ok(answer) = heard.try_recv() {
            got = Some(answer);
        }
        got.is_some()
    });
    assert_eq!(got.expect("an answer"), Ok(qe_a11y::GET_TEXT_CAP));
}
