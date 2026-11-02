//! One test for each thing a review found that the other tests let through.
mod support;
use qe_a11y::{
    TextRange,
    search::{FindError, FindRequest, find_on_window, find_waiting},
};
use qe_api::{
    ByteOffset, Dip, LineIndex, TextPosition, TextSnapshot, Utf16Offset, ViewContext, WidgetId,
    WidgetRole,
};
use qe_editor::{EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
use std::{
    ops::Range,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use support::*;
use windows::{
    Win32::{
        System::{Com::SAFEARRAY, Ole::SafeArrayGetElement},
        UI::Accessibility::*,
    },
    core::{BOOL, BSTR, Interface, PCWSTR},
};

const START: TextPatternRangeEndpoint = TextPatternRangeEndpoint_Start;
const END: TextPatternRangeEndpoint = TextPatternRangeEndpoint_End;

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

fn walk(doc: &ITextRangeProvider, forward: bool) -> Vec<usize> {
    let range = unsafe { doc.Clone() }.unwrap();
    let (moved, to) = if forward { (END, START) } else { (START, END) };
    unsafe { range.MoveEndpointByRange(moved, doc, to) }.unwrap();
    let mut stops = vec![ends(&range, doc).0];
    let step = if forward { 1 } else { -1 };
    while unsafe { range.Move(TextUnit_Character, step) }.unwrap() != 0 {
        stops.push(ends(&range, doc).0);
    }
    if !forward {
        stops.reverse();
    }
    stops
}

#[test]
fn a_long_flag_run_stays_paired_in_both_directions() {
    for flags in [1usize, 2, 17, 60, 1000] {
        let body = format!("q{}r", "\u{1F1E6}\u{1F1E7}".repeat(flags));
        let window = TestWindow::from_text(&body);
        let ui = window.accessibility();
        let doc = document_range(&ui.document().unwrap());
        let want: Vec<usize> = std::iter::once(0)
            .chain((0..=flags).map(|i| 1 + i * 4))
            .chain(std::iter::once(flags * 4 + 2))
            .collect();
        assert_eq!(walk(&doc, true), want, "forward, {flags} flags");
        assert_eq!(walk(&doc, false), want, "backward, {flags} flags");
    }
}

#[test]
fn a_range_with_text_moves_onto_a_final_empty_line_and_not_past_it() {
    let window = TestWindow::from_text("ab\ncd\n");
    let ui = window.accessibility();
    let doc = document_range(&ui.document().unwrap());
    for unit in [TextUnit_Line, TextUnit_Paragraph] {
        let line = unsafe { doc.Clone() }.unwrap();
        unsafe { line.ExpandToEnclosingUnit(unit) }.unwrap();
        assert_eq!(ends(&line, &doc), (0, 3));
        assert_eq!(unsafe { line.Move(unit, 1) }.unwrap(), 1);
        assert_eq!(ends(&line, &doc), (3, 6));
        assert_eq!(
            unsafe { line.Move(unit, 1) }.unwrap(),
            1,
            "onto the empty line"
        );
        assert_eq!(ends(&line, &doc), (6, 6));
        assert_eq!(unsafe { line.Move(unit, 1) }.unwrap(), 0, "nothing past it");
        assert_eq!(unsafe { line.Move(unit, -1) }.unwrap(), -1);
        assert_eq!(ends(&line, &doc).0, 3);
    }
    // Characters and words start no unit at the end, so they stay.
    let last = unsafe { doc.Clone() }.unwrap();
    unsafe { last.MoveEndpointByRange(START, &doc, END) }.unwrap();
    assert_eq!(unsafe { last.Move(TextUnit_Character, -1) }.unwrap(), -1);
    unsafe { last.ExpandToEnclosingUnit(TextUnit_Character) }.unwrap();
    assert_eq!(unsafe { last.Move(TextUnit_Character, 1) }.unwrap(), 0);
}

#[test]
fn an_unchecked_item_the_host_calls_checkable_offers_toggle() {
    let item = widget(7, WidgetRole::MenuItem, "Wrap", 0.0, Vec::new());
    let plain = widget(8, WidgetRole::MenuItem, "Save", 50.0, Vec::new());
    let window = TestWindow::with_tree(
        EditorView::from_text("x").unwrap(),
        root_with(vec![item, plain]),
    );
    window.ui.checkable.borrow_mut().push(WidgetId(7));
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let first = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let second = unsafe { first.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let first: IRawElementProviderSimple = first.cast().unwrap();
    let second: IRawElementProviderSimple = second.cast().unwrap();
    assert!(unsafe { first.GetPatternProvider(UIA_TogglePatternId) }.is_ok());
    assert_null(
        unsafe { second.GetPatternProvider(UIA_TogglePatternId) },
        "a plain command",
    );
}

#[test]
fn a_point_inside_the_document_but_outside_the_text_gets_the_nearest_position() {
    let window = TestWindow::from_text("hello\nworld");
    window.lay_out(400.0, 200.0);
    window
        .ui
        .view
        .borrow_mut()
        .set_margins(Dip(40.0), Dip(0.0), &mut ViewContext::default());
    window.settle();
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let fragment: IRawElementProviderFragment = document.cast().unwrap();
    let element = unsafe { fragment.BoundingRectangle() }.unwrap();
    // In the left margin, on the first row.
    let text = text_of(&document);
    let hit = unsafe {
        text.RangeFromPoint(UiaPoint {
            x: element.left + 5.0,
            y: element.top + 5.0,
        })
    }
    .expect("a point in the margin");
    assert_eq!(ends(&hit, &doc), (0, 0));
    // Below all the text, near the bottom of the box.
    let hit = unsafe {
        text.RangeFromPoint(UiaPoint {
            x: element.left + 100.0,
            y: element.top + element.height - 2.0,
        })
    }
    .expect("a point below the text");
    assert_eq!(ends(&hit, &doc).0, 11);
}

#[test]
fn set_value_with_a_null_pointer_is_refused_not_a_crash() {
    let mut find = widget(9, WidgetRole::TextField, "Find", 0.0, Vec::new());
    find.value = Some("x".into());
    let window = TestWindow::with_tree(EditorView::from_text("x").unwrap(), root_with(vec![find]));
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let node: IRawElementProviderSimple = unsafe { root.Navigate(NavigateDirection_FirstChild) }
        .unwrap()
        .cast()
        .unwrap();
    let value: IValueProvider = unsafe { node.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert_code(
        unsafe { value.SetValue(PCWSTR::null()) },
        -2147024809,
        "a null pointer",
    );
}

#[test]
fn a_scale_other_than_100_percent_reaches_every_rectangle_and_point() {
    let window = TestWindow::with_tree(
        EditorView::from_text("hello").unwrap(),
        root_with(vec![widget(5, WidgetRole::Button, "Do", 30.0, Vec::new())]),
    );
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let word = range_at(&document, 0, 5);
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let root_face: IRawElementProviderFragmentRoot = ui.root().cast().unwrap();
    let button = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();

    window.ui.dpi.set(Some(96));
    let text_at_96 = doubles(unsafe { word.GetBoundingRectangles() }.unwrap());
    let box_at_96 = unsafe { button.BoundingRectangle() }.unwrap();
    window.ui.dpi.set(Some(192));
    let text_at_192 = doubles(unsafe { word.GetBoundingRectangles() }.unwrap());
    let box_at_192 = unsafe { button.BoundingRectangle() }.unwrap();

    // The window's origin stays, and all else doubles.
    let origin_x = box_at_96.left - 30.0;
    assert_eq!(box_at_192.width, box_at_96.width * 2.0);
    assert_eq!(box_at_192.height, box_at_96.height * 2.0);
    assert_eq!(box_at_192.left - origin_x, 60.0);
    assert_eq!(text_at_192[2], text_at_96[2] * 2.0);
    assert_eq!(text_at_192[3], text_at_96[3] * 2.0);

    // A point goes the other way: 30% across the word at 200% is its second character.
    let far = text_at_192[0] + text_at_192[2] * 0.3;
    let hit = unsafe {
        text_of(&document).RangeFromPoint(UiaPoint {
            x: far,
            y: text_at_192[1] + 1.0,
        })
    }
    .unwrap();
    let at = ends(&hit, &doc).0;
    assert!((1..=2).contains(&at), "got {at}");
    let found = unsafe {
        root_face.ElementProviderFromPoint(
            box_at_192.left + box_at_192.width - 2.0,
            box_at_192.top + 2.0,
        )
    }
    .unwrap();
    let found: IRawElementProviderSimple = found.cast().unwrap();
    let name = unsafe { found.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
    assert_eq!(BSTR::try_from(&name).unwrap(), "Do");
}

#[test]
fn caret_range_says_whether_the_document_has_focus() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    let text = text_of(&ui.document().unwrap());
    let mut active = BOOL(0);
    unsafe { text.GetCaretRange(&mut active) }.unwrap();
    assert!(active.as_bool());
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = false;
    unsafe { text.GetCaretRange(&mut active) }.unwrap();
    assert!(!active.as_bool());
}

#[test]
fn select_brings_far_text_on_screen() {
    let lines: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let window = TestWindow::from_text(&lines);
    window.lay_out(400.0, 200.0);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let far = unsafe { doc.Clone() }.unwrap();
    let mut moved = 0;
    while moved < 300 {
        moved += unsafe { far.Move(TextUnit_Line, 300 - moved) }.unwrap();
    }
    unsafe { far.Select() }.unwrap();
    window.settle();
    assert_eq!(unsafe { far.GetText(-1) }.unwrap(), "line 300\n");
    assert!(
        !doubles(unsafe { far.GetBoundingRectangles() }.unwrap()).is_empty(),
        "the selected text is on screen"
    );
}

#[test]
fn a_section_reports_a_heading_level() {
    let mut head = widget(5, WidgetRole::Label, "Options", 0.0, Vec::new());
    head.states.section = true;
    let plain = widget(6, WidgetRole::Label, "Plain", 50.0, Vec::new());
    let window = TestWindow::with_tree(
        EditorView::from_text("x").unwrap(),
        root_with(vec![head, plain]),
    );
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let first = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let second = unsafe { first.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let level = |node: &IRawElementProviderFragment| -> windows::Win32::System::Variant::VARIANT {
        let simple: IRawElementProviderSimple = node.cast().unwrap();
        unsafe { simple.GetPropertyValue(UIA_HeadingLevelPropertyId) }.unwrap()
    };
    assert_eq!(i32::try_from(&level(&first)).unwrap(), HeadingLevel2.0);
    assert!(level(&second).is_empty());
}

#[test]
fn a_range_on_a_mapped_file_survives_its_index_growing() {
    let path = std::env::temp_dir().join(format!("qe-a11y-{}-grow.txt", std::process::id()));
    let mut body = Vec::with_capacity(32 << 20);
    while body.len() < 32 << 20 {
        body.extend_from_slice(b"some line of text\n");
    }
    std::fs::write(&path, &body).unwrap();
    drop(body);
    let mut load = FileLoad::start(
        &path,
        LoadOptions {
            force_read_only: true,
            ..Default::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let file = loop {
        match load.poll() {
            Some(LoadResult::Loaded(file)) => break file,
            Some(_) => panic!("the load failed"),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            None => panic!("the load timed out"),
        }
    };
    let LoadedFile::ReadOnly(mapped) = file else {
        panic!("this file should be mapped")
    };
    mapped.set_test_read_delay(Duration::from_millis(1));
    let view = EditorView::from_mapped(mapped).unwrap();
    let window = TestWindow::new(view);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let held = document_range(&document);
    let revision = window.ui.view.borrow().revision();
    while Instant::now() < deadline {
        let view = window.ui.view.borrow();
        assert_eq!(view.revision(), revision, "HOOKS.md: it keeps one value");
        if view.store_status().complete {
            break;
        }
        drop(view);
        std::thread::sleep(Duration::from_millis(1));
    }
    let text = loop {
        match unsafe { held.GetText(17) } {
            Ok(text) => break text,
            Err(error) if error.code().0 == NOT_YET && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(text, "some line of text");
    drop(held);
    drop(document);
    drop(ui);
    window.close();
    drop(window);
    if let Err(error) = std::fs::remove_file(&path) {
        eprintln!("could not delete {}: {error}", path.display());
    }
}

#[test]
fn focus_from_nowhere_to_the_document_raises_an_event() {
    let window = TestWindow::from_text("abc");
    let ui = window.accessibility();
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = false;
    let first = ui.widgets_changed_when(true).unwrap();
    assert!(first.structure, "the first report says the tree is there");
    assert!(!first.focus, "nothing has focus, so nothing to say");
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = true;
    let second = ui.widgets_changed_when(true).unwrap();
    assert!(second.focus, "focus came to the document");
    assert!(!second.structure);
    assert!(
        !ui.widgets_changed_when(true).unwrap().focus,
        "no change, no event"
    );
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = false;
    assert!(!ui.widgets_changed_when(true).unwrap().focus);
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = true;
    assert!(ui.widgets_changed_when(true).unwrap().focus, "back again");
}

#[test]
fn widget_focus_and_shape_changes_raise_events_and_no_client_raises_none() {
    let window = TestWindow::with_tree(
        EditorView::from_text("x").unwrap(),
        root_with(vec![widget(5, WidgetRole::Button, "Do", 0.0, Vec::new())]),
    );
    let ui = window.accessibility();
    ui.widgets_changed_when(true).unwrap();
    window.ui.tree.borrow_mut().children[0].states.focused = true;
    let moved = ui.widgets_changed_when(true).unwrap();
    assert!(moved.focus && !moved.structure);
    window.ui.tree.borrow_mut().children.push(widget(
        6,
        WidgetRole::Button,
        "New",
        50.0,
        Vec::new(),
    ));
    let grew = ui.widgets_changed_when(true).unwrap();
    assert!(grew.structure && !grew.focus);
    let calls = window.counts.widget_calls.load(Ordering::SeqCst);
    assert_eq!(ui.widgets_changed_when(false).unwrap(), Default::default());
    assert_eq!(window.counts.widget_calls.load(Ordering::SeqCst), calls);
}

/// A snapshot whose chunks take time, for a search that outruns its limit.
struct Slow {
    text: String,
    each: Duration,
}
impl TextSnapshot for Slow {
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
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        let mut going = true;
        self.visit_chunks_while(range, &mut |part| {
            if going {
                visitor(part);
            }
            going = true;
            true
        })
    }
    fn visit_chunks_while(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str) -> bool,
    ) -> bool {
        let Some(part) = self.text.get(range.start.0..range.end.0) else {
            return false;
        };
        for piece in part.as_bytes().chunks(16) {
            std::thread::sleep(self.each);
            let Ok(piece) = std::str::from_utf8(piece) else {
                return false;
            };
            if !visitor(piece) {
                return true;
            }
        }
        true
    }
    fn is_read_only(&self) -> bool {
        true
    }
}

fn request(end: usize, query: &str, backward: bool, ignore_case: bool) -> FindRequest {
    FindRequest {
        range: TextRange::new(0, end),
        query: query.into(),
        backward,
        ignore_case,
    }
}

#[test]
fn a_search_past_its_limit_fails_honestly_and_never_says_not_found() {
    let text = Slow {
        text: "x".repeat(16 * 200) + "needle",
        each: Duration::from_millis(1),
    };
    let len = text.text.len();
    let began = Instant::now();
    let answer = find_waiting(
        &text,
        &request(len, "needle", false, false),
        Duration::from_millis(30),
    );
    assert_eq!(answer, Err(FindError::TooLong));
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "it stopped at its limit"
    );
    // Given time, the same search finds it, across the chunk edges.
    let answer = find_waiting(
        &text,
        &request(len, "needle", false, false),
        Duration::from_secs(60),
    );
    assert_eq!(answer, Ok(Some(TextRange::new(16 * 200, 16 * 200 + 6))));
    let answer = find_waiting(
        &text,
        &request(len, "NEEDLE", false, true),
        Duration::from_secs(60),
    );
    assert_eq!(answer, Ok(Some(TextRange::new(16 * 200, 16 * 200 + 6))));
}

#[test]
fn a_long_query_costs_no_more_than_the_text_and_the_query() {
    // This input made the old search take seconds in release and 20 s in debug.
    let text = qe_api::testing::StringStore::new("a".repeat(60_000));
    let query = format!("{}b", "a".repeat(30_000));
    for ignore_case in [true, false] {
        let began = Instant::now();
        let answer = find_on_window(&text, &request(60_000, &query, false, ignore_case));
        assert_eq!(answer, Ok(None));
        let took = began.elapsed();
        // The old search would take 400 times this limit in debug. This limit allows for load.
        assert!(
            took < Duration::from_millis(1000),
            "ignore_case {ignore_case}: {took:?}"
        );
    }
    // Overlaps still match: the prefix table is what makes that work.
    let text = qe_api::testing::StringStore::new("aabaabaaab");
    assert_eq!(
        find_on_window(&text, &request(10, "aabaaab", false, true)),
        Ok(Some(TextRange::new(3, 10)))
    );
    assert_eq!(
        find_on_window(&text, &request(10, "aab", true, true)),
        Ok(Some(TextRange::new(7, 10)))
    );
}

#[test]
fn a_query_longer_than_the_range_finds_nothing_without_reading() {
    let text = Slow {
        text: "abc".into(),
        each: Duration::from_secs(60),
    };
    let answer = find_waiting(
        &text,
        &request(3, "abcd", false, false),
        Duration::from_secs(60),
    );
    assert_eq!(answer, Ok(None));
}
