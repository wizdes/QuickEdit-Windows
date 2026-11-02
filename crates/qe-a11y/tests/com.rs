use qe_a11y::{
    TextModel, TextRange,
    provider::{DocumentProvider, EditorHost},
};
use qe_editor::EditorView;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::UI::Accessibility::{
        ITextProvider2, ITextRangeProvider, IValueProvider, TextPatternRangeEndpoint_End,
        TextPatternRangeEndpoint_Start, TextUnit_Character, TextUnit_Word, UIA_FontNameAttributeId,
        UIA_FontSizeAttributeId, UIA_IsReadOnlyAttributeId, UIA_TextPattern2Id, UIA_TextPatternId,
        UIA_ValuePatternId,
    },
    core::Interface,
};

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SafeArrayGetElement(array: *mut c_void, index: *const i32, out: *mut *mut c_void) -> i32;
    fn SafeArrayDestroy(array: *mut c_void) -> i32;
}

struct GeometryHost {
    selected: Mutex<Option<TextRange>>,
}
impl EditorHost for GeometryHost {
    fn visible_range(&self) -> windows::core::Result<TextRange> {
        Ok(TextRange::new(0, 3))
    }
    fn range_from_point(
        &self,
        _: windows::Win32::UI::Accessibility::UiaPoint,
    ) -> windows::core::Result<TextRange> {
        Ok(TextRange::new(1, 1))
    }
    fn rectangles(&self, _: TextRange) -> windows::core::Result<Vec<[f64; 4]>> {
        Ok(vec![[10.0, 20.0, 30.0, 10.0], [10.0, 30.0, 20.0, 10.0]])
    }
    fn select(&self, range: TextRange) -> windows::core::Result<()> {
        *self.selected.lock().unwrap() = Some(range);
        Ok(())
    }
    fn scroll_into_view(&self, _: TextRange, _: bool) -> windows::core::Result<()> {
        Ok(())
    }
    fn expand_to_unit(
        &self,
        _: TextRange,
        unit: windows::Win32::UI::Accessibility::TextUnit,
    ) -> windows::core::Result<TextRange> {
        assert_eq!(unit, TextUnit_Word);
        Ok(TextRange::new(0, 3))
    }
    fn move_endpoint(
        &self,
        range: TextRange,
        endpoint: windows::Win32::UI::Accessibility::TextPatternRangeEndpoint,
        unit: windows::Win32::UI::Accessibility::TextUnit,
        count: i32,
    ) -> windows::core::Result<(TextRange, i32)> {
        assert_eq!(unit, TextUnit_Character);
        assert_eq!(endpoint, TextPatternRangeEndpoint_Start);
        assert_eq!(count, 1);
        Ok((TextRange::new(range.start + 1, range.end), 1))
    }
}

#[test]
fn old_range_rejects_end_after_edit() {
    let mut view = EditorView::from_text("abcdef").unwrap();
    let provider = DocumentProvider::from_editor(&view).unwrap();
    let text: ITextProvider2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let old = unsafe { text.DocumentRange() }.unwrap();
    let mut ctx = qe_api::ViewContext::default();
    view.replace_range(qe_api::ByteOffset(1)..qe_api::ByteOffset(6), "x", &mut ctx);
    provider.refresh_from_editor(&view).unwrap();
    assert!(unsafe { old.GetText(-1) }.is_err());
}

#[test]
fn prevent_editing_reports_read_only() {
    let mut view = EditorView::from_text("abc").unwrap();
    view.set_prevent_editing(true);
    let provider = DocumentProvider::from_editor(&view).unwrap();
    let text: ITextProvider2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    let value = unsafe { range.GetAttributeValue(UIA_IsReadOnlyAttributeId) }.unwrap();
    assert_eq!(
        unsafe { (*value.Anonymous.Anonymous).Anonymous.boolVal.0 },
        -1
    );
    let value_pattern: IValueProvider =
        unsafe { provider.simple().GetPatternProvider(UIA_ValuePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert!(unsafe { value_pattern.IsReadOnly() }.unwrap().as_bool());
    assert_eq!(unsafe { value_pattern.Value() }.unwrap().to_string(), "abc");
}

#[test]
fn seeded_endpoint_moves_match_plain_text_model() {
    let plain = "abcdefghijklmnopqrst";
    let view = EditorView::from_text(plain).unwrap();
    let provider = DocumentProvider::new(
        TextModel::new(view.worker_snapshot()),
        TextRange::new(0, 0),
        false,
    );
    let text: ITextProvider2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let whole = unsafe { text.DocumentRange() }.unwrap();
    let work = unsafe { whole.Clone() }.unwrap();
    let mut expected = 0..plain.len();
    let mut seed = 0x91e10da5u32;
    for _ in 0..200 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let a = (seed as usize) % (plain.len() + 1);
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let b = (seed as usize) % (plain.len() + 1);
        let selection = TextRange::new(a.min(b), a.max(b));
        provider.set_selection(selection).unwrap();
        let array = unsafe { text.GetSelection() }.unwrap();
        let mut raw = std::ptr::null_mut();
        assert_eq!(
            unsafe { SafeArrayGetElement(array.cast(), &0, &mut raw) },
            0
        );
        let target = unsafe { ITextRangeProvider::from_raw(raw) };
        if seed & 1 == 0 {
            unsafe {
                work.MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &target,
                    TextPatternRangeEndpoint_Start,
                )
            }
            .unwrap();
            expected.start = selection.start;
            if expected.start > expected.end {
                expected.end = expected.start;
            }
        } else {
            unsafe {
                work.MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &target,
                    TextPatternRangeEndpoint_End,
                )
            }
            .unwrap();
            expected.end = selection.end;
            if expected.end < expected.start {
                expected.start = expected.end;
            }
        }
        assert_eq!(
            unsafe { work.GetText(-1) }.unwrap().to_string(),
            &plain[expected.clone()]
        );
        assert_eq!(unsafe { SafeArrayDestroy(array.cast()) }, 0);
    }
}

#[test]
fn geometry_and_select_are_sent_to_host() {
    let view = EditorView::from_text("abc").unwrap();
    let host = Arc::new(GeometryHost {
        selected: Mutex::new(None),
    });
    let provider = DocumentProvider::new(
        TextModel::new(view.worker_snapshot()),
        TextRange::new(0, 0),
        false,
    )
    .with_editor_host(host.clone());
    let text: ITextProvider2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let visible = unsafe { text.GetVisibleRanges() }.unwrap();
    let mut raw = std::ptr::null_mut();
    assert_eq!(
        unsafe { SafeArrayGetElement(visible.cast(), &0, &mut raw) },
        0
    );
    let range = unsafe { ITextRangeProvider::from_raw(raw) };
    assert_eq!(unsafe { range.GetText(-1) }.unwrap().to_string(), "abc");
    assert_eq!(
        unsafe { range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, 1) }
            .unwrap(),
        1
    );
    assert_eq!(unsafe { range.GetText(-1) }.unwrap().to_string(), "bc");
    unsafe { range.ExpandToEnclosingUnit(TextUnit_Word) }.unwrap();
    assert_eq!(unsafe { range.GetText(-1) }.unwrap().to_string(), "abc");
    let rectangles = unsafe { range.GetBoundingRectangles() }.unwrap();
    let mut left = 0.0_f64;
    assert_eq!(
        unsafe { SafeArrayGetElement(rectangles.cast(), &0, (&mut left as *mut f64).cast()) },
        0
    );
    assert_eq!(left, 10.0);
    let mut second_top = 0.0_f64;
    assert_eq!(
        unsafe { SafeArrayGetElement(rectangles.cast(), &5, (&mut second_top as *mut f64).cast()) },
        0
    );
    assert_eq!(second_top, 30.0);
    assert_eq!(unsafe { SafeArrayDestroy(rectangles.cast()) }, 0);
    unsafe { range.Select() }.unwrap();
    assert_eq!(*host.selected.lock().unwrap(), Some(TextRange::new(0, 3)));
    assert_eq!(unsafe { SafeArrayDestroy(visible.cast()) }, 0);
}

#[test]
fn layout_not_settled_returns_error() {
    let view = EditorView::from_text("wrapped text").unwrap();
    let provider = DocumentProvider::new(
        TextModel::new(view.worker_snapshot()),
        TextRange::new(0, 0),
        false,
    );
    let text: ITextProvider2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert!(unsafe { text.GetVisibleRanges() }.is_err());
    let range = unsafe { text.DocumentRange() }.unwrap();
    assert!(unsafe { range.GetBoundingRectangles() }.is_err());
}

#[test]
fn direct_com_document_text_and_selection() {
    let view = EditorView::from_text("a😀e\u{301}\r\nb").unwrap();
    let provider = DocumentProvider::new(
        TextModel::new(view.worker_snapshot()),
        TextRange::new(1, 3),
        false,
    );
    provider.set_font("Consolas", 12.0).unwrap();
    let pattern = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }.unwrap();
    let text: ITextProvider2 = pattern.cast().unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    assert_eq!(
        unsafe { range.GetText(-1) }.unwrap().to_string(),
        "a😀e\u{301}\r\nb"
    );
    assert_eq!(unsafe { range.GetText(3) }.unwrap().to_string(), "a😀");
    let font = unsafe { range.GetAttributeValue(UIA_FontNameAttributeId) }.unwrap();
    assert_eq!(
        unsafe { (*font.Anonymous.Anonymous).Anonymous.bstrVal.to_string() },
        "Consolas"
    );
    let size = unsafe { range.GetAttributeValue(UIA_FontSizeAttributeId) }.unwrap();
    assert_eq!(
        unsafe { (*size.Anonymous.Anonymous).Anonymous.dblVal },
        12.0
    );
    let found = unsafe { range.FindText(&windows::core::BSTR::from("😀e"), false, false) }.unwrap();
    assert_eq!(unsafe { found.GetText(-1) }.unwrap().to_string(), "😀e");

    let array = unsafe { text.GetSelection() }.unwrap();
    assert!(!array.is_null());
    let mut raw = std::ptr::null_mut();
    assert_eq!(
        unsafe { SafeArrayGetElement(array.cast(), &0, &mut raw) },
        0
    );
    assert!(!raw.is_null());
    let selected = unsafe { ITextRangeProvider::from_raw(raw) };
    assert_eq!(unsafe { selected.GetText(-1) }.unwrap().to_string(), "😀");
    assert_eq!(
        unsafe {
            range.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &selected,
                TextPatternRangeEndpoint_Start,
            )
        }
        .unwrap(),
        -1
    );
    assert_eq!(unsafe { SafeArrayDestroy(array.cast()) }, 0);

    provider.set_selection(TextRange::new(3, 5)).unwrap();
    let pattern2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPattern2Id) }.unwrap();
    let text2: ITextProvider2 = pattern2.cast().unwrap();
    let caret = unsafe { text2.GetCaretRange(&mut windows::core::BOOL(0)) }.unwrap();
    assert_eq!(unsafe { caret.GetText(-1) }.unwrap().to_string(), "");
}
