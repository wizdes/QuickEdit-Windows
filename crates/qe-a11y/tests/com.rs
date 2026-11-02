//! Direct COM calls, in the way UIA makes them, on the window's thread. Each answer is checked
//! against text worked out by hand. The tests in `real_client.rs` go through UIA itself.
mod support;
use qe_a11y::GET_TEXT_CAP;
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorCommand, EditorView};
use support::*;
use windows::{
    Win32::{
        System::{Ole::SafeArrayGetElement, Variant::VARIANT},
        UI::Accessibility::*,
    },
    core::{BOOL, BSTR, IUnknown, Interface},
};

const START: TextPatternRangeEndpoint = TextPatternRangeEndpoint_Start;
const END: TextPatternRangeEndpoint = TextPatternRangeEndpoint_End;

fn text(range: &ITextRangeProvider) -> String {
    unsafe { range.GetText(-1) }.expect("GetText").to_string()
}
fn first_range(array: *mut windows::Win32::System::Com::SAFEARRAY) -> Option<ITextRangeProvider> {
    let count = unsafe { (*array).rgsabound[0].cElements };
    if count == 0 {
        return None;
    }
    let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
    let index = 0i32;
    unsafe { SafeArrayGetElement(array, &index, (&raw mut raw).cast()) }.expect("element");
    let unknown = unsafe { IUnknown::from_raw(raw) };
    Some(unknown.cast().expect("a range"))
}
fn rectangles(range: &ITextRangeProvider) -> Vec<f64> {
    let array = unsafe { range.GetBoundingRectangles() }.expect("GetBoundingRectangles");
    let count = unsafe { (*array).rgsabound[0].cElements } as i32;
    (0..count)
        .map(|index| {
            let mut value = 0f64;
            unsafe { SafeArrayGetElement(array, &index, (&raw mut value).cast()) }.expect("element");
            value
        })
        .collect()
}

#[test]
fn get_text_keeps_utf16_units_and_its_limit() {
    let window = TestWindow::from_text("a\u{1f600}e\u{301}\r\nb");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    assert_eq!(text(&doc), "a\u{1f600}e\u{301}\r\nb");
    assert_eq!(unsafe { doc.GetText(0) }.unwrap().to_string(), "");
    assert_eq!(unsafe { doc.GetText(1) }.unwrap().to_string(), "a");
    // Two units end inside the emoji, so the answer stops before it.
    assert_eq!(unsafe { doc.GetText(2) }.unwrap().to_string(), "a");
    assert_eq!(unsafe { doc.GetText(3) }.unwrap().to_string(), "a\u{1f600}");
    assert_eq!(unsafe { doc.GetText(100) }.unwrap().to_string(), text(&doc));
    assert_code(unsafe { doc.GetText(-2) }, -2147024809, "a limit below -1");
}

#[test]
fn get_text_with_no_limit_stops_at_the_cap() {
    let body = "x".repeat(GET_TEXT_CAP + 100);
    let window = TestWindow::from_text(&body);
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    assert_eq!(unsafe { doc.GetText(-1) }.unwrap().len(), GET_TEXT_CAP);
    assert_eq!(unsafe { doc.GetText(i32::MAX) }.unwrap().len(), GET_TEXT_CAP);
    assert_eq!(unsafe { doc.GetText(3) }.unwrap().to_string(), "xxx");
    // A client reads on from where the first answer stopped.
    let later = unsafe { doc.Clone() }.unwrap();
    let moved = unsafe { later.MoveEndpointByUnit(START, TextUnit_Character, 99) }.unwrap();
    assert_eq!(moved, 99);
    assert_eq!(ends(&later, &doc).0, 99);
}

#[test]
fn empty_document_answers_every_unit_with_an_empty_range() {
    let window = TestWindow::from_text("");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    assert_eq!(text(&doc), "");
    for unit in [
        TextUnit_Character,
        TextUnit_Format,
        TextUnit_Word,
        TextUnit_Line,
        TextUnit_Paragraph,
        TextUnit_Page,
        TextUnit_Document,
    ] {
        let range = unsafe { doc.Clone() }.unwrap();
        unsafe { range.ExpandToEnclosingUnit(unit) }.unwrap();
        assert_eq!(ends(&range, &doc), (0, 0), "{unit:?}");
        assert_eq!(unsafe { range.Move(unit, 1) }.unwrap(), 0, "{unit:?}");
        assert_eq!(unsafe { range.Move(unit, -1) }.unwrap(), 0, "{unit:?}");
        assert_eq!(
            unsafe { range.MoveEndpointByUnit(END, unit, 1) }.unwrap(),
            0,
            "{unit:?}"
        );
    }
    assert_null(
        unsafe { doc.FindText(&BSTR::from("x"), false, false) },
        "FindText in an empty document",
    );
}

#[test]
fn character_moves_stop_only_between_clusters() {
    // a | emoji | e + mark | flag | family | CRLF | b
    let body = "a\u{1f600}e\u{301}\u{1f1eb}\u{1f1f7}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\r\nb";
    let window = TestWindow::from_text(body);
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let caret = unsafe { doc.Clone() }.unwrap();
    unsafe { caret.MoveEndpointByRange(END, &doc, START) }.unwrap();
    let mut stops = vec![0];
    while unsafe { caret.Move(TextUnit_Character, 1) }.unwrap() == 1 {
        stops.push(ends(&caret, &doc).0);
    }
    assert_eq!(stops, vec![0, 1, 3, 5, 9, 17, 19, 20]);
    let mut back = vec![20];
    while unsafe { caret.Move(TextUnit_Character, -1) }.unwrap() == -1 {
        back.push(ends(&caret, &doc).0);
    }
    stops.reverse();
    assert_eq!(back, stops);
}

#[test]
fn expand_character_from_inside_a_cluster_covers_the_cluster() {
    let window = TestWindow::from_text("xe\u{301}y");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    // Put a range on the mark itself, by the selection a client cannot place by units.
    window
        .ui
        .view
        .borrow_mut()
        .select(ByteOffset(1)..ByteOffset(1), &mut ViewContext::default());
    let range = range_at(&ui.document().unwrap(), 1, 1);
    unsafe { range.ExpandToEnclosingUnit(TextUnit_Character) }.unwrap();
    assert_eq!(ends(&range, &doc), (1, 3));
    assert_eq!(text(&range), "e\u{301}");
}

#[test]
fn word_units_take_the_blank_text_after_them() {
    let window = TestWindow::from_text("hello, big  world\r\n\r\n  next");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let caret = unsafe { doc.Clone() }.unwrap();
    unsafe { caret.MoveEndpointByRange(END, &doc, START) }.unwrap();
    let mut stops = vec![0];
    while unsafe { caret.Move(TextUnit_Word, 1) }.unwrap() == 1 {
        stops.push(ends(&caret, &doc).0);
    }
    assert_eq!(stops, vec![0, 5, 7, 12, 19, 21, 23, 27]);
    let word = unsafe { doc.Clone() }.unwrap();
    unsafe { word.ExpandToEnclosingUnit(TextUnit_Word) }.unwrap();
    assert_eq!(text(&word), "hello");
    assert_eq!(unsafe { word.Move(TextUnit_Word, 2) }.unwrap(), 2);
    assert_eq!(text(&word), "big  ");
}

#[test]
fn lines_and_paragraphs_include_their_endings_and_stop_at_the_last() {
    let window = TestWindow::from_text("one\r\ntwo\nthree");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let line = unsafe { doc.Clone() }.unwrap();
    unsafe { line.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
    assert_eq!(text(&line), "one\r\n");
    assert_eq!(unsafe { line.Move(TextUnit_Line, 1) }.unwrap(), 1);
    assert_eq!(text(&line), "two\n");
    assert_eq!(unsafe { line.Move(TextUnit_Paragraph, 5) }.unwrap(), 1);
    assert_eq!(text(&line), "three");
    // A range with text stays put at the last line: no line starts at the very end.
    assert_eq!(unsafe { line.Move(TextUnit_Line, 1) }.unwrap(), 0);
    assert_eq!(text(&line), "three");
    assert_eq!(unsafe { line.Move(TextUnit_Line, -100) }.unwrap(), -2);
    assert_eq!(text(&line), "one\r\n");
    assert_eq!(unsafe { line.Move(TextUnit_Line, 0) }.unwrap(), 0);
    assert_eq!(text(&line), "one\r\n");
}

#[test]
fn format_and_page_act_on_the_whole_document() {
    let window = TestWindow::from_text("one\ntwo");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    for unit in [TextUnit_Format, TextUnit_Page, TextUnit_Document] {
        let range = range_at(&ui.document().unwrap(), 2, 2);
        unsafe { range.ExpandToEnclosingUnit(unit) }.unwrap();
        assert_eq!(text(&range), "one\ntwo", "{unit:?}");
        assert_eq!(unsafe { range.Move(unit, 1) }.unwrap(), 0, "{unit:?}");
        assert_eq!(ends(&range, &doc), (0, 7), "{unit:?}");
    }
    assert_code(
        unsafe { doc.Move(TextUnit(99), 1) },
        -2147024809,
        "a unit that does not exist",
    );
}

#[test]
fn move_endpoint_past_the_other_end_brings_it_along() {
    let window = TestWindow::from_text("abcdef");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let range = range_at(&document, 2, 4);
    assert_eq!(unsafe { range.MoveEndpointByUnit(START, TextUnit_Character, 3) }.unwrap(), 3);
    assert_eq!(ends(&range, &doc), (5, 5));
    assert_eq!(unsafe { range.MoveEndpointByUnit(END, TextUnit_Character, -4) }.unwrap(), -4);
    assert_eq!(ends(&range, &doc), (1, 1));
    // Out of range stops at the end and reports how far it went.
    assert_eq!(unsafe { range.MoveEndpointByUnit(END, TextUnit_Character, 100) }.unwrap(), 5);
    assert_eq!(unsafe { range.MoveEndpointByUnit(START, TextUnit_Character, -100) }.unwrap(), -1);
    assert_eq!(ends(&range, &doc), (0, 6));
    assert_eq!(unsafe { range.MoveEndpointByUnit(START, TextUnit_Character, i32::MIN) }.unwrap(), 0);
    assert_code(
        unsafe { range.MoveEndpointByUnit(TextPatternRangeEndpoint(9), TextUnit_Character, 1) },
        -2147024809,
        "an end that does not exist",
    );
}

#[test]
fn compare_and_endpoints_and_move_by_range() {
    let window = TestWindow::from_text("abcdef");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let a = range_at(&document, 1, 3);
    let b = range_at(&document, 1, 3);
    let c = range_at(&document, 2, 3);
    assert!(unsafe { a.Compare(&b) }.unwrap().as_bool());
    assert!(!unsafe { a.Compare(&c) }.unwrap().as_bool());
    // A range compared with itself must not lock up.
    assert!(unsafe { a.Compare(&a) }.unwrap().as_bool());
    assert_eq!(unsafe { a.CompareEndpoints(START, &c, START) }.unwrap(), -1);
    assert_eq!(unsafe { c.CompareEndpoints(END, &a, START) }.unwrap(), 2);
    assert_eq!(unsafe { a.CompareEndpoints(END, &c, END) }.unwrap(), 0);
    unsafe { a.MoveEndpointByRange(START, &doc, END) }.unwrap();
    assert_eq!(ends(&a, &doc), (6, 6));
    unsafe { a.MoveEndpointByRange(END, &doc, START) }.unwrap();
    assert_eq!(ends(&a, &doc), (0, 0));
    assert!(unsafe { a.Compare(None) }.is_err(), "no range given");

    let other_window = TestWindow::from_text("abcdef");
    let other_ui = other_window.accessibility();
    let foreign = document_range(&other_ui.document().unwrap());
    assert_code(unsafe { a.Compare(&foreign) }, -2147024809, "another document's range");
    assert_code(
        unsafe { a.CompareEndpoints(START, &foreign, START) },
        -2147024809,
        "another document's range",
    );
    assert_code(
        unsafe { a.MoveEndpointByRange(START, &foreign, START) },
        -2147024809,
        "another document's range",
    );
}

#[test]
fn find_text_gives_real_null_and_utf16_ranges() {
    let window = TestWindow::from_text("a\u{1f600}b e\u{301} \u{1f600}B and K");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let found = unsafe { doc.FindText(&BSTR::from("\u{1f600}b"), false, false) }.unwrap();
    assert_eq!(ends(&found, &doc), (1, 4));
    let found = unsafe { doc.FindText(&BSTR::from("\u{1f600}b"), true, true) }.unwrap();
    assert_eq!(ends(&found, &doc), (8, 11));
    assert_eq!(text(&found), "\u{1f600}B");
    assert_null(
        unsafe { doc.FindText(&BSTR::from("\u{1f600}b"), true, false).and_then(|f| {
            // Case-sensitive and backward finds the first one, not null.
            assert_eq!(ends(&f, &doc), (1, 4));
            doc.FindText(&BSTR::from("zzz"), false, false)
        }) },
        "no match",
    );
    // The Kelvin sign folds to k, and the answer covers it in its real units.
    let found = unsafe { doc.FindText(&BSTR::from("\u{212a}"), false, true) }.unwrap();
    assert_eq!(text(&found), "K");
    // An empty range holds nothing. An empty query is the caller's mistake.
    let caret = range_at(&ui.document().unwrap(), 1, 1);
    assert_null(unsafe { caret.FindText(&BSTR::from("b"), false, false) }, "an empty range");
    assert_code(
        unsafe { doc.FindText(&BSTR::from(""), false, false) },
        -2147024809,
        "an empty query",
    );
    // A lone surrogate cannot match valid text.
    let lone = BSTR::from_wide(&[0xd83d]);
    assert_null(unsafe { doc.FindText(&lone, false, false) }, "a lone surrogate");
    // The search stays inside the range.
    let part = range_at(&ui.document().unwrap(), 3, 5);
    assert_null(unsafe { part.FindText(&BSTR::from("\u{1f600}"), false, false) }, "outside the range");
}

#[test]
fn find_text_over_the_old_cap_finds_text_far_in() {
    let mut body = "x".repeat(70_000);
    body.push_str("needle");
    let window = TestWindow::from_text(&body);
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let found = loop {
        match unsafe { doc.FindText(&BSTR::from("NEEDLE"), false, true) } {
            Ok(found) => break found,
            Err(error) if error.code().0 == NOT_YET => {
                assert!(std::time::Instant::now() < deadline, "the search never ended");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(ends(&found, &doc), (70_000, 70_006));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match unsafe { doc.FindText(&BSTR::from("missing"), false, false) } {
            Ok(_) => panic!("found text that is not there"),
            Err(error) if error.code().0 == 0 => break,
            Err(error) if error.code().0 == NOT_YET => {
                assert!(std::time::Instant::now() < deadline, "the search never ended");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

#[test]
fn attributes_report_read_only_font_and_not_supported() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let value = |id| unsafe { doc.GetAttributeValue(id) }.unwrap();
    assert!(!bool::try_from(&value(UIA_IsReadOnlyAttributeId)).unwrap());
    assert_eq!(BSTR::try_from(&value(UIA_FontNameAttributeId)).unwrap(), "Cascadia Mono");
    assert_eq!(f64::try_from(&value(UIA_FontSizeAttributeId)).unwrap(), 10.0);
    let unknown = IUnknown::try_from(&value(UIA_BackgroundColorAttributeId)).unwrap();
    assert!(unknown_is_not_supported(&unknown));
    // With no font reported, the font attributes say so rather than guess.
    *window.ui.font.borrow_mut() = None;
    let unknown = IUnknown::try_from(&value(UIA_FontNameAttributeId)).unwrap();
    assert!(unknown_is_not_supported(&unknown));
    *window.ui.font.borrow_mut() = Some(("Cascadia Mono".into(), 10.0));

    let same = VARIANT::from("Cascadia Mono");
    let found = unsafe { doc.FindAttribute(UIA_FontNameAttributeId, &same, false) }.unwrap();
    assert_eq!(text(&found), "abc");
    assert_null(
        unsafe { doc.FindAttribute(UIA_FontNameAttributeId, &VARIANT::from("Arial"), false) },
        "another font",
    );
    assert_null(
        unsafe { doc.FindAttribute(UIA_BackgroundColorAttributeId, &VARIANT::from(1i32), false) },
        "an attribute with no value",
    );
}

#[test]
fn read_only_view_reports_it_and_refuses_value_changes() {
    let mut view = EditorView::from_text("abc").unwrap();
    view.set_prevent_editing(true);
    let window = TestWindow::new(view);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let read_only = unsafe { doc.GetAttributeValue(UIA_IsReadOnlyAttributeId) }.unwrap();
    assert!(bool::try_from(&read_only).unwrap());
    let value: IValueProvider = unsafe { document.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert!(unsafe { value.IsReadOnly() }.unwrap().as_bool());
    assert_eq!(unsafe { value.Value() }.unwrap(), "abc");
    assert_code(
        unsafe { value.SetValue(windows::core::w!("new")) },
        UIA_E_ELEMENTNOTENABLED as i32,
        "SetValue on read-only text",
    );
    assert_eq!(window.ui.view.borrow().text().unwrap(), "abc");
    // A selection does not edit, so it still works.
    let range = range_at(&document, 1, 2);
    unsafe { range.Select() }.unwrap();
    assert_eq!(window.ui.view.borrow().selection(), ByteOffset(1)..ByteOffset(2));
}

#[test]
fn editable_view_has_no_value_pattern() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    assert_null(
        unsafe { document.GetPatternProvider(UIA_ValuePatternId) },
        "Value on editable text",
    );
    assert_null(
        unsafe { document.GetPatternProvider(UIA_InvokePatternId) },
        "Invoke on a document",
    );
}

#[test]
fn selection_and_caret_follow_the_editor() {
    let window = TestWindow::from_text("a\u{1f600}bc");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let text_pattern = text_of(&document);
    let doc = document_range(&document);
    window.ui.view.borrow_mut().select(ByteOffset(1)..ByteOffset(6), &mut ViewContext::default());
    let array = unsafe { ITextProvider::GetSelection(&text_pattern.cast::<ITextProvider>().unwrap()) }.unwrap();
    let selection = first_range(array).unwrap();
    assert_eq!(ends(&selection, &doc), (1, 4));
    assert_eq!(text(&selection), "\u{1f600}b");
    let mut active = BOOL(0);
    let caret = unsafe { text_pattern.GetCaretRange(&mut active) }.unwrap();
    assert!(active.as_bool(), "the document has focus");
    assert_eq!(ends(&caret, &doc).0, ends(&caret, &doc).1);
    // Selecting from a range moves the editor's real selection.
    let range = range_at(&document, 3, 5);
    unsafe { range.Select() }.unwrap();
    assert_eq!(window.ui.view.borrow().selection(), ByteOffset(5)..ByteOffset(7));
    assert_code(unsafe { range.AddToSelection() }, UIA_E_NOTSUPPORTED as i32, "a second selection");
    assert_code(unsafe { range.RemoveFromSelection() }, UIA_E_NOTSUPPORTED as i32, "a second selection");
    assert_eq!(
        unsafe { text_pattern.SupportedTextSelection() }.unwrap(),
        SupportedTextSelection_Single
    );
}

#[test]
fn bounding_rectangles_cover_only_what_shows() {
    let lines: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let window = TestWindow::from_text(&lines);
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let word = range_at(&document, 0, 4);
    let rects = rectangles(&word);
    assert_eq!(rects.len(), 4, "one row is one rectangle");
    assert!(rects[2] > 0.0 && rects[3] > 0.0, "{rects:?}");
    // A caret has a place too, with a width of one pixel or so.
    let caret = range_at(&document, 2, 2);
    assert_eq!(rectangles(&caret).len(), 4);
    // Text off screen has no rectangles, and this must not be an error.
    let far = unsafe { doc.Clone() }.unwrap();
    unsafe { far.MoveEndpointByRange(START, &doc, END) }.unwrap();
    assert!(rectangles(&far).is_empty());
    // The whole document gives the rows on screen, clipped to the text area.
    let all = rectangles(&doc);
    assert!(!all.is_empty() && all.len().is_multiple_of(4));
    assert!(all.len() / 4 < 20, "only rows on screen: {}", all.len() / 4);
    let visible = first_range(unsafe {
        ITextProvider::GetVisibleRanges(&text_of(&document).cast::<ITextProvider>().unwrap())
    }.unwrap())
    .unwrap();
    assert!(text(&visible).starts_with("line 0\n"));
    assert!(!text(&visible).contains("line 400"));
}

#[test]
fn no_layout_gives_no_rectangles_and_no_visible_range() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    assert!(rectangles(&document_range(&document)).is_empty());
    let array = unsafe {
        ITextProvider::GetVisibleRanges(&text_of(&document).cast::<ITextProvider>().unwrap())
    }
    .unwrap();
    assert!(first_range(array).is_none());
}

#[test]
fn range_from_point_finds_text_and_refuses_points_outside() {
    let window = TestWindow::from_text("hello");
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let rect = rectangles(&range_at(&document, 0, 1));
    let hit = unsafe {
        text_of(&document).RangeFromPoint(UiaPoint {
            x: rect[0] + 1.0,
            y: rect[1] + 1.0,
        })
    }
    .unwrap();
    assert_eq!(ends(&hit, &doc), (0, 0));
    assert_code(
        unsafe { text_of(&document).RangeFromPoint(UiaPoint { x: -5000.0, y: -5000.0 }) },
        -2147024809,
        "a point outside",
    );
}

#[test]
fn scroll_into_view_brings_far_text_on_screen_and_keeps_the_selection() {
    let lines: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let window = TestWindow::from_text(&lines);
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let far = unsafe { doc.Clone() }.unwrap();
    let mut moved = 0;
    // A long move may stop at its time budget, and says how far it got.
    while moved < 300 {
        moved += unsafe { far.Move(TextUnit_Line, 300 - moved) }.unwrap();
    }
    assert_eq!(moved, 300);
    assert_eq!(text(&far), "line 300\n");
    let selection = window.ui.view.borrow().selection();
    unsafe { far.ScrollIntoView(true) }.unwrap();
    window.lay_out(400.0, 200.0);
    assert_eq!(window.ui.view.borrow().selection(), selection);
    assert!(!rectangles(&far).is_empty(), "the text is on screen now");
}

#[test]
fn enclosing_element_and_document_are_one_object() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let enclosing = unsafe { doc.GetEnclosingElement() }.unwrap();
    assert_eq!(
        enclosing.cast::<IUnknown>().unwrap().as_raw(),
        document.cast::<IUnknown>().unwrap().as_raw(),
    );
    let child = unsafe {
        ui.root()
            .cast::<IRawElementProviderFragment>()
            .unwrap()
            .Navigate(NavigateDirection_LastChild)
    }
    .unwrap();
    assert_eq!(
        child.cast::<IUnknown>().unwrap().as_raw(),
        document.cast::<IUnknown>().unwrap().as_raw(),
    );
    assert_eq!(ui.document().unwrap().as_raw(), document.as_raw());
    let empty = unsafe { doc.GetChildren() }.unwrap();
    assert!(first_range(empty).is_none());
}

#[test]
fn a_kept_range_follows_edits_and_undo() {
    let window = TestWindow::from_text("hello big world");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let range = range_at(&document, 6, 9);
    assert_eq!(text(&range), "big");
    let edit = |range, new: &str| {
        window.ui.view.borrow_mut().replace_range(range, new, &mut ViewContext::default());
    };
    edit(ByteOffset(0)..ByteOffset(0), "\u{1f600}");
    assert_eq!(text(&range), "big", "text before moved it by 2 units");
    edit(ByteOffset(15)..ByteOffset(15), "xx");
    assert_eq!(text(&range), "big", "text after does not touch it");
    window.ui.view.borrow_mut().run(EditorCommand::Undo, &mut ViewContext::default());
    window.ui.view.borrow_mut().run(EditorCommand::Undo, &mut ViewContext::default());
    assert_eq!(window.ui.view.borrow().text().unwrap(), "hello big world");
    assert_eq!(text(&range), "big", "undo moved it back");
    // Replacing all of it leaves an empty range where the text began.
    edit(ByteOffset(5)..ByteOffset(10), "-");
    assert_eq!(text(&range), "");
}

#[test]
fn a_range_from_another_document_or_tab_is_stale() {
    let window = TestWindow::from_text("first");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    assert_eq!(text(&doc), "first");
    // Another tab comes forward: another view number.
    window.ui.view_id.set(Some(8));
    assert_code(unsafe { doc.GetText(-1) }, GONE, "a range from another tab");
    assert_code(unsafe { doc.Move(TextUnit_Character, 1) }, GONE, "a range from another tab");
    window.ui.view_id.set(None);
    assert_code(unsafe { doc.GetText(-1) }, GONE, "no view at all");
    window.ui.view_id.set(Some(7));
    assert_eq!(text(&doc), "first");
}

#[test]
fn a_change_log_that_ran_out_makes_a_range_stale_not_wrong() {
    let window = TestWindow::from_text("a");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    let mut ctx = ViewContext::default();
    for _ in 0..4100 {
        window.ui.view.borrow_mut().replace_range(ByteOffset(0)..ByteOffset(0), "x", &mut ctx);
    }
    assert_code(unsafe { doc.GetText(-1) }, GONE, "a range older than the log");
    let fresh = document_range(&ui.document().unwrap());
    assert_eq!(text(&fresh).len(), 4101);
}

#[test]
fn every_held_object_reports_gone_after_the_window_closes() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let text_pattern = text_of(&document);
    let doc = document_range(&document);
    let other = unsafe { doc.Clone() }.unwrap();
    let fragment: IRawElementProviderFragment = document.cast().unwrap();
    let root = ui.root();
    ui.shutdown().unwrap();
    ui.shutdown().unwrap();
    drop(ui);
    window.close();
    drop(window);

    assert_code(unsafe { doc.GetText(-1) }, GONE, "GetText");
    assert_code(unsafe { doc.Clone() }, GONE, "Clone");
    assert_code(unsafe { doc.Compare(&other) }, GONE, "Compare");
    assert_code(unsafe { doc.CompareEndpoints(TextPatternRangeEndpoint_Start, &other, TextPatternRangeEndpoint_Start) }, GONE, "CompareEndpoints");
    assert_code(unsafe { doc.ExpandToEnclosingUnit(TextUnit_Word) }, GONE, "Expand");
    assert_code(unsafe { doc.FindText(&BSTR::from("a"), false, false) }, GONE, "FindText");
    assert_code(unsafe { doc.FindAttribute(UIA_IsReadOnlyAttributeId, &VARIANT::from(true), false) }, GONE, "FindAttribute");
    assert_code(unsafe { doc.GetAttributeValue(UIA_FontNameAttributeId) }, GONE, "GetAttributeValue");
    assert_code(unsafe { doc.GetBoundingRectangles() }, GONE, "GetBoundingRectangles");
    assert_code(unsafe { doc.GetEnclosingElement() }, GONE, "GetEnclosingElement");
    assert_code(unsafe { doc.Move(TextUnit_Word, 1) }, GONE, "Move");
    assert_code(unsafe { doc.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Word, 1) }, GONE, "MoveEndpointByUnit");
    assert_code(unsafe { doc.MoveEndpointByRange(TextPatternRangeEndpoint_End, &other, TextPatternRangeEndpoint_End) }, GONE, "MoveEndpointByRange");
    assert_code(unsafe { doc.Select() }, GONE, "Select");
    assert_code(unsafe { doc.ScrollIntoView(true) }, GONE, "ScrollIntoView");
    assert_code(unsafe { doc.GetChildren() }, GONE, "GetChildren");
    assert_code(unsafe { text_pattern.DocumentRange() }, GONE, "DocumentRange");
    let mut active = BOOL(0);
    assert_code(unsafe { text_pattern.GetCaretRange(&mut active) }, GONE, "GetCaretRange");
    assert_code(unsafe { document.GetPatternProvider(UIA_TextPatternId) }, GONE, "GetPatternProvider");
    assert_code(unsafe { document.GetPropertyValue(UIA_NamePropertyId) }, GONE, "GetPropertyValue");
    assert_code(unsafe { fragment.Navigate(NavigateDirection_Parent) }, GONE, "Navigate");
    assert_code(unsafe { fragment.BoundingRectangle() }, GONE, "BoundingRectangle");
    assert_code(unsafe { fragment.GetRuntimeId() }, GONE, "GetRuntimeId");
    assert_code(unsafe { fragment.FragmentRoot() }, GONE, "FragmentRoot");
    assert_code(unsafe { root.HostRawElementProvider() }, GONE, "the root's host");
}
