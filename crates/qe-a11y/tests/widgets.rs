//! The painted tree through direct COM calls.
mod support;
use qe_a11y::{TextRange, WidgetAction};
use qe_api::{WidgetId, WidgetRange, WidgetRole};
use qe_editor::EditorView;
use support::*;
use windows::{
    Win32::{System::Variant::VARIANT, UI::Accessibility::*},
    core::{BSTR, Interface, w},
};

fn sample() -> qe_api::Widget {
    let mut tab_one = widget(11, WidgetRole::Tab, "One", 0.0, Vec::new());
    tab_one.states.selected = true;
    let tab_two = widget(12, WidgetRole::Tab, "Two", 40.0, Vec::new());
    let tabs = widget(10, WidgetRole::TabList, "Tabs", 0.0, vec![tab_one, tab_two]);
    let mut check = widget(20, WidgetRole::Checkbox, "Wrap", 100.0, Vec::new());
    check.states.checked = true;
    let mut off = widget(21, WidgetRole::Button, "Off", 150.0, Vec::new());
    off.states.enabled = false;
    let mut field = widget(30, WidgetRole::TextField, "Find", 200.0, Vec::new());
    field.value = Some("a\u{1f600} word".into());
    field.selection = Some(1..3);
    field.states.focused = true;
    let mut combo = widget(31, WidgetRole::ComboBox, "Encoding", 250.0, Vec::new());
    combo.value = Some("UTF-8".into());
    combo.states.expanded = true;
    let mut scroll = widget(40, WidgetRole::ScrollBar, "Scroll", 300.0, Vec::new());
    scroll.range_value = Some(WidgetRange {
        step: None,
        minimum: 0.0,
        maximum: 10.0,
        value: 3.0,
    });
    let mut popup = widget(51, WidgetRole::MenuItem, "File", 350.0, Vec::new());
    popup.states.has_popup = true;
    let item = widget(52, WidgetRole::MenuItem, "Save", 390.0, Vec::new());
    let menu = widget(50, WidgetRole::Menu, "Menu bar", 350.0, vec![popup, item]);
    let label = widget(60, WidgetRole::Label, "A label", 450.0, Vec::new());
    root_with(vec![tabs, check, off, field, combo, scroll, menu, label])
}

fn fragment_root(window: &TestWindow) -> (qe_a11y::Accessibility, IRawElementProviderFragment) {
    let ui = window.accessibility();
    let fragment = ui.root().cast().unwrap();
    (ui, fragment)
}

fn name(node: &IRawElementProviderFragment) -> String {
    let simple: IRawElementProviderSimple = node.cast().unwrap();
    let value = unsafe { simple.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
    BSTR::try_from(&value).unwrap().to_string()
}

fn child(node: &IRawElementProviderFragment, wanted: &str) -> IRawElementProviderFragment {
    let mut at = unsafe { node.Navigate(NavigateDirection_FirstChild) }.unwrap();
    loop {
        if name(&at) == wanted {
            return at;
        }
        at = unsafe { at.Navigate(NavigateDirection_NextSibling) }
            .unwrap_or_else(|_| panic!("no child named {wanted}"));
    }
}

fn simple(node: &IRawElementProviderFragment) -> IRawElementProviderSimple {
    node.cast().unwrap()
}

fn window() -> TestWindow {
    TestWindow::with_tree(EditorView::from_text("doc").unwrap(), sample())
}

#[test]
fn navigation_walks_the_whole_tree_and_ends_in_real_null() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let mut names = Vec::new();
    let mut at = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    loop {
        names.push(name(&at));
        match unsafe { at.Navigate(NavigateDirection_NextSibling) } {
            Ok(next) => at = next,
            Err(error) => {
                assert_eq!(error.code().0, 0, "the end of a list is null: {error}");
                break;
            }
        }
    }
    assert_eq!(
        names,
        [
            "Tabs", "Wrap", "Off", "Find", "Encoding", "Scroll", "Menu bar", "A label", "Untitled"
        ]
    );
    // The document is last, and it knows its way back.
    assert_eq!(
        name(&unsafe { root.Navigate(NavigateDirection_LastChild) }.unwrap()),
        "Untitled"
    );
    let document = unsafe { root.Navigate(NavigateDirection_LastChild) }.unwrap();
    assert_eq!(
        name(&unsafe { document.Navigate(NavigateDirection_PreviousSibling) }.unwrap()),
        "A label"
    );
    assert_null(
        unsafe { document.Navigate(NavigateDirection_NextSibling) },
        "after the document",
    );
    assert_null(
        unsafe { document.Navigate(NavigateDirection_FirstChild) },
        "the document's children",
    );
    assert_null(
        unsafe { root.Navigate(NavigateDirection_Parent) },
        "the root's parent",
    );
    assert_null(
        unsafe { root.Navigate(NavigateDirection_NextSibling) },
        "the root's sibling",
    );
    let tabs = child(&root, "Tabs");
    let one = unsafe { tabs.Navigate(NavigateDirection_FirstChild) }.unwrap();
    assert_eq!(name(&one), "One");
    assert_eq!(
        name(&unsafe { one.Navigate(NavigateDirection_Parent) }.unwrap()),
        "Tabs"
    );
    assert_null(
        unsafe { one.Navigate(NavigateDirection_PreviousSibling) },
        "before the first",
    );
    assert_null(
        unsafe { one.Navigate(NavigateDirection_FirstChild) },
        "a leaf's children",
    );
    assert_eq!(
        unsafe { tabs.FragmentRoot() }.unwrap().as_raw(),
        root.cast::<IRawElementProviderFragmentRoot>()
            .unwrap()
            .as_raw()
    );
}

#[test]
fn patterns_follow_the_role_and_others_are_real_null() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let has = |name: &str, pattern| {
        let node = simple(&child(&root, name));
        match unsafe { node.GetPatternProvider(pattern) } {
            Ok(_) => true,
            Err(error) => {
                assert_eq!(error.code().0, 0, "a missing pattern is null: {error}");
                false
            }
        }
    };
    assert!(has("Wrap", UIA_TogglePatternId));
    assert!(!has("Wrap", UIA_InvokePatternId));
    assert!(has("Off", UIA_InvokePatternId));
    assert!(has("Find", UIA_ValuePatternId));
    assert!(has("Find", UIA_TextPatternId));
    assert!(has("Encoding", UIA_ExpandCollapsePatternId));
    assert!(has("Encoding", UIA_ValuePatternId));
    assert!(has("Scroll", UIA_RangeValuePatternId));
    assert!(has("Tabs", UIA_SelectionPatternId));
    assert!(!has("A label", UIA_InvokePatternId));
    assert!(!has("A label", UIA_ValuePatternId));
    assert!(!has("Find", UIA_RangeValuePatternId));
}

#[test]
fn a_menu_item_with_a_popup_expands_and_a_plain_one_invokes() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let menu = child(&root, "Menu bar");
    let file = simple(&child(&menu, "File"));
    let save = simple(&child(&menu, "Save"));
    let expand: IExpandCollapseProvider =
        unsafe { file.GetPatternProvider(UIA_ExpandCollapsePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert_eq!(
        unsafe { expand.ExpandCollapseState() }.unwrap(),
        ExpandCollapseState_Collapsed
    );
    unsafe { expand.Expand() }.unwrap();
    assert_null(
        unsafe { file.GetPatternProvider(UIA_InvokePatternId) },
        "Invoke with a popup",
    );
    let invoke: IInvokeProvider = unsafe { save.GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { invoke.Invoke() }.unwrap();
    assert_null(
        unsafe { save.GetPatternProvider(UIA_ExpandCollapsePatternId) },
        "ExpandCollapse with no popup",
    );
    let actions = window.actions_after_pump();
    assert_eq!(
        actions,
        vec![
            (WidgetId(51), WidgetAction::Expand),
            (WidgetId(52), WidgetAction::Invoke)
        ]
    );
}

#[test]
fn actions_reach_the_host_and_refuse_when_turned_off() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let toggle: IToggleProvider =
        unsafe { simple(&child(&root, "Wrap")).GetPatternProvider(UIA_TogglePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert_eq!(unsafe { toggle.ToggleState() }.unwrap(), ToggleState_On);
    unsafe { toggle.Toggle() }.unwrap();
    let invoke: IInvokeProvider =
        unsafe { simple(&child(&root, "Off")).GetPatternProvider(UIA_InvokePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert_code(
        unsafe { invoke.Invoke() },
        UIA_E_ELEMENTNOTENABLED as i32,
        "a turned-off button",
    );
    let scroll: IRangeValueProvider =
        unsafe { simple(&child(&root, "Scroll")).GetPatternProvider(UIA_RangeValuePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert_eq!(unsafe { scroll.Value() }.unwrap(), 3.0);
    assert_eq!(unsafe { scroll.Maximum() }.unwrap(), 10.0);
    unsafe { scroll.SetValue(4.0) }.unwrap();
    assert_code(
        unsafe { scroll.SetValue(11.0) },
        -2147024809,
        "over the maximum",
    );
    assert_code(
        unsafe { scroll.SetValue(f64::NAN) },
        -2147024809,
        "not a number",
    );
    let value: IValueProvider =
        unsafe { simple(&child(&root, "Find")).GetPatternProvider(UIA_ValuePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert_eq!(unsafe { value.Value() }.unwrap(), "a\u{1f600} word");
    unsafe { value.SetValue(w!("next")) }.unwrap();
    let actions = window.actions_after_pump();
    assert_eq!(
        actions,
        vec![
            (WidgetId(40), WidgetAction::SetRange(4.0)),
            (WidgetId(30), WidgetAction::SetValue("next".into())),
            // A posted action runs after the calls that run at once.
            (WidgetId(20), WidgetAction::Toggle),
        ]
    );
}

#[test]
fn tabs_report_selection_both_ways() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let tabs = child(&root, "Tabs");
    let selection: ISelectionProvider =
        unsafe { simple(&tabs).GetPatternProvider(UIA_SelectionPatternId) }
            .unwrap()
            .cast()
            .unwrap();
    let array = unsafe { selection.GetSelection() }.unwrap();
    assert_eq!(unsafe { (*array).rgsabound[0].cElements }, 1);
    assert!(!unsafe { selection.CanSelectMultiple() }.unwrap().as_bool());
    let two = child(&tabs, "Two");
    let item: ISelectionItemProvider =
        unsafe { simple(&two).GetPatternProvider(UIA_SelectionItemPatternId) }
            .unwrap()
            .cast()
            .unwrap();
    assert!(!unsafe { item.IsSelected() }.unwrap().as_bool());
    unsafe { item.Select() }.unwrap();
    let container = unsafe { item.SelectionContainer() }.unwrap();
    let value = unsafe { container.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
    assert_eq!(BSTR::try_from(&value).unwrap(), "Tabs");
    assert_eq!(
        window.actions_after_pump()[0],
        (WidgetId(12), WidgetAction::Select)
    );
}

#[test]
fn a_text_field_exposes_its_text_and_selection_in_utf16() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let field = simple(&child(&root, "Find"));
    let text = text_of(&field);
    let doc = unsafe { text.DocumentRange() }.unwrap();
    assert_eq!(unsafe { doc.GetText(-1) }.unwrap(), "a\u{1f600} word");
    let array = unsafe { text.cast::<ITextProvider>().unwrap().GetSelection() }.unwrap();
    assert_eq!(unsafe { (*array).rgsabound[0].cElements }, 1);
    let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
    let index = 0i32;
    unsafe {
        windows::Win32::System::Ole::SafeArrayGetElement(array, &index, (&raw mut raw).cast())
    }
    .unwrap();
    let selection: ITextRangeProvider = unsafe { windows::core::IUnknown::from_raw(raw) }
        .cast()
        .unwrap();
    assert_eq!(unsafe { selection.GetText(-1) }.unwrap(), "\u{1f600}");
    // A word move and a character move work in a field too.
    let word = unsafe { doc.Clone() }.unwrap();
    unsafe { word.ExpandToEnclosingUnit(TextUnit_Word) }.unwrap();
    assert_eq!(unsafe { word.GetText(-1) }.unwrap(), "a");
    assert_eq!(unsafe { word.Move(TextUnit_Character, 1) }.unwrap(), 1);
    assert_eq!(unsafe { word.GetText(-1) }.unwrap(), "\u{1f600}");
    unsafe { word.Select() }.unwrap();
    assert_eq!(
        window.counts.actions.lock().unwrap()[0],
        (WidgetId(30), WidgetAction::SelectText(TextRange::new(1, 3)))
    );
    let rects = unsafe { doc.GetBoundingRectangles() }.unwrap();
    assert_eq!(unsafe { (*rects).rgsabound[0].cElements }, 4);
    // A field whose value changes keeps the range inside the new text.
    window.ui.tree.borrow_mut().children[3].value = Some("x".into());
    assert_eq!(unsafe { doc.GetText(-1) }.unwrap(), "x");
    let found = unsafe { doc.FindText(&BSTR::from("x"), false, false) }.unwrap();
    assert_eq!(unsafe { found.GetText(-1) }.unwrap(), "x");
}

#[test]
fn a_held_widget_that_left_the_tree_reports_gone() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let find = child(&root, "Find");
    let text = text_of(&simple(&find));
    let range = unsafe { text.DocumentRange() }.unwrap();
    window.ui.tree.borrow_mut().children.remove(3);
    assert_code(
        unsafe { find.Navigate(NavigateDirection_Parent) },
        GONE,
        "Navigate",
    );
    assert_code(
        unsafe { simple(&find).GetPropertyValue(UIA_NamePropertyId) },
        GONE,
        "a property",
    );
    assert_code(unsafe { range.GetText(-1) }, GONE, "a range in the field");
}

#[test]
fn focus_and_hit_tests_find_the_right_element() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let root_face: IRawElementProviderFragmentRoot = root.cast().unwrap();
    assert_eq!(name(&unsafe { root_face.GetFocus() }.unwrap()), "Find");
    window.ui.tree.borrow_mut().children[3].states.focused = false;
    assert_eq!(name(&unsafe { root_face.GetFocus() }.unwrap()), "Untitled");
    window
        .ui
        .document
        .borrow_mut()
        .as_mut()
        .unwrap()
        .states
        .focused = false;
    assert_null(unsafe { root_face.GetFocus() }, "nothing has focus");

    let wrap = unsafe { child(&root, "Wrap").BoundingRectangle() }.unwrap();
    let found =
        unsafe { root_face.ElementProviderFromPoint(wrap.left + 1.0, wrap.top + 1.0) }.unwrap();
    assert_eq!(name(&found), "Wrap");
    let document = unsafe { child(&root, "Untitled").BoundingRectangle() }.unwrap();
    let found =
        unsafe { root_face.ElementProviderFromPoint(document.left + 5.0, document.top + 5.0) }
            .unwrap();
    assert_eq!(name(&found), "Untitled");
}

#[test]
fn properties_say_what_a_client_needs() {
    let window = window();
    let (_ui, root) = fragment_root(&window);
    let property =
        |name: &str, id| unsafe { simple(&child(&root, name)).GetPropertyValue(id) }.unwrap();
    let kind = |name: &str| i32::try_from(&property(name, UIA_ControlTypePropertyId)).unwrap();
    assert_eq!(kind("Untitled"), UIA_DocumentControlTypeId.0);
    assert_eq!(kind("Wrap"), UIA_CheckBoxControlTypeId.0);
    assert_eq!(kind("Tabs"), UIA_TabControlTypeId.0);
    assert_eq!(kind("Find"), UIA_EditControlTypeId.0);
    assert!(!bool::try_from(&property("Off", UIA_IsEnabledPropertyId)).unwrap());
    assert!(bool::try_from(&property("Find", UIA_HasKeyboardFocusPropertyId)).unwrap());
    assert!(!bool::try_from(&property("A label", UIA_IsKeyboardFocusablePropertyId)).unwrap());
    // A property with no answer is an empty variant, not an error.
    let empty: VARIANT = property("Wrap", UIA_HelpTextPropertyId);
    assert!(empty.is_empty());
    let ids = unsafe { child(&root, "Wrap").GetRuntimeId() }.unwrap();
    assert_eq!(unsafe { (*ids).rgsabound[0].cElements }, 3);
    assert!(
        unsafe { root.GetRuntimeId() }.unwrap().is_null(),
        "the window gives the root's ID"
    );
    let simple_root = simple(&root);
    assert_eq!(
        unsafe { simple_root.ProviderOptions() }.unwrap(),
        ProviderOptions_ServerSideProvider
    );
    assert!(unsafe { simple_root.HostRawElementProvider() }.is_ok());
    assert_null(
        unsafe { simple(&child(&root, "Wrap")).HostRawElementProvider() },
        "a child's host",
    );
}
