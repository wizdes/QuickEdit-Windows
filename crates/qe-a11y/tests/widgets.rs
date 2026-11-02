use qe_a11y::{
    TextModel, TextRange,
    provider::DocumentProvider,
    widgets::{WidgetAction, WidgetHost, WidgetProvider},
};
use qe_api::{Dip, Rect, Widget, WidgetId, WidgetRole, WidgetStates};
use qe_editor::EditorView;
use std::sync::{Arc, Mutex};
use windows::{
    Win32::UI::Accessibility::{
        IInvokeProvider, IRawElementProviderFragment, IValueProvider, NavigateDirection_FirstChild,
        UIA_ControlTypePropertyId, UIA_InvokePatternId, UIA_NamePropertyId, UIA_ValuePatternId,
    },
    core::Interface,
};

struct Host {
    actions: Mutex<Vec<(WidgetId, WidgetAction)>>,
    document: Option<Arc<DocumentProvider>>,
    scale: f64,
}
impl WidgetHost for Host {
    fn window_id(&self) -> u64 {
        7
    }
    fn window_scale(&self) -> f64 {
        self.scale
    }
    fn widgets(&self) -> Widget {
        let button = Widget {
            id: WidgetId(2),
            bounds: Rect {
                x: Dip(10.0),
                y: Dip(10.0),
                width: Dip(50.0),
                height: Dip(20.0),
            },
            role: WidgetRole::Button,
            name: "Find".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: Some("Find in document".into()),
            children: vec![],
        };
        let field = Widget {
            id: WidgetId(3),
            bounds: button.bounds,
            role: WidgetRole::TextField,
            name: "Search".into(),
            value: Some("abc".into()),
            states: WidgetStates {
                enabled: true,
                read_only: true,
                ..Default::default()
            },
            selection: Some(1..2),
            range_value: None,
            description: None,
            children: vec![],
        };
        Widget {
            id: WidgetId(1),
            bounds: button.bounds,
            role: WidgetRole::Group,
            name: "Find bar".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children: vec![button, field],
        }
    }
    fn action(&self, id: WidgetId, action: WidgetAction) -> windows::core::Result<()> {
        self.actions.lock().unwrap().push((id, action));
        Ok(())
    }
    fn document_provider(&self) -> Option<Arc<DocumentProvider>> {
        self.document.clone()
    }
    fn document_widget(&self) -> Option<Widget> {
        self.document.as_ref().map(|_| Widget {
            id: WidgetId(9),
            bounds: Rect {
                x: Dip(0.0),
                y: Dip(40.0),
                width: Dip(300.0),
                height: Dip(200.0),
            },
            role: WidgetRole::Group,
            name: "Document".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children: vec![],
        })
    }
}

#[test]
fn widget_roles_names_navigation_and_invoke() {
    let host = Arc::new(Host {
        actions: Mutex::new(Vec::new()),
        document: None,
        scale: 1.0,
    });
    let tree = WidgetProvider::new(host.clone());
    let root: IRawElementProviderFragment = tree.simple().cast().unwrap();
    let child = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let simple = child
        .cast::<windows::Win32::UI::Accessibility::IRawElementProviderSimple>()
        .unwrap();
    let name = unsafe { simple.GetPropertyValue(UIA_NamePropertyId) }.unwrap();
    assert_eq!(
        unsafe { (*name.Anonymous.Anonymous).Anonymous.bstrVal.to_string() },
        "Find"
    );
    let role = unsafe { simple.GetPropertyValue(UIA_ControlTypePropertyId) }.unwrap();
    assert_eq!(unsafe { (*role.Anonymous.Anonymous).Anonymous.lVal }, 50000);
    let invoke: IInvokeProvider = unsafe { simple.GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { invoke.Invoke() }.unwrap();
    assert_eq!(
        host.actions.lock().unwrap().as_slice(),
        &[(WidgetId(2), WidgetAction::Invoke)]
    );
}

#[test]
fn read_only_widget_value_rejects_edit() {
    let host = Arc::new(Host {
        actions: Mutex::new(Vec::new()),
        document: None,
        scale: 1.0,
    });
    let tree = WidgetProvider::new(host);
    let root: IRawElementProviderFragment = tree.simple().cast().unwrap();
    let first = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let second =
        unsafe { first.Navigate(windows::Win32::UI::Accessibility::NavigateDirection_NextSibling) }
            .unwrap();
    let simple = second
        .cast::<windows::Win32::UI::Accessibility::IRawElementProviderSimple>()
        .unwrap();
    let value: IValueProvider = unsafe { simple.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert_eq!(unsafe { value.Value() }.unwrap().to_string(), "abc");
    assert!(unsafe { value.IsReadOnly() }.unwrap().as_bool());
}

#[test]
fn document_is_last_child_of_widget_root() {
    let view = EditorView::from_text("hello😀").unwrap();
    let document = Arc::new(DocumentProvider::new(
        TextModel::new(view.worker_snapshot()),
        TextRange::new(0, 0),
        false,
    ));
    let host = Arc::new(Host {
        actions: Mutex::new(Vec::new()),
        document: Some(document),
        scale: 1.0,
    });
    let tree = WidgetProvider::new(host);
    let root: IRawElementProviderFragment = tree.simple().cast().unwrap();
    let child =
        unsafe { root.Navigate(windows::Win32::UI::Accessibility::NavigateDirection_LastChild) }
            .unwrap();
    let simple = child
        .cast::<windows::Win32::UI::Accessibility::IRawElementProviderSimple>()
        .unwrap();
    let role = unsafe { simple.GetPropertyValue(UIA_ControlTypePropertyId) }.unwrap();
    assert_eq!(unsafe { (*role.Anonymous.Anonymous).Anonymous.lVal }, 50030);
    let text: windows::Win32::UI::Accessibility::ITextProvider2 =
        unsafe { simple.GetPatternProvider(windows::Win32::UI::Accessibility::UIA_TextPatternId) }
            .unwrap()
            .cast()
            .unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    assert_eq!(unsafe { range.GetText(-1) }.unwrap().to_string(), "hello😀");
}

#[test]
fn widget_bounds_use_screen_pixels_at_150_percent() {
    let host = Arc::new(Host {
        actions: Mutex::new(Vec::new()),
        document: None,
        scale: 1.5,
    });
    let tree = WidgetProvider::new(host);
    let root: IRawElementProviderFragment = tree.simple().cast().unwrap();
    let child = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let bounds = unsafe { child.BoundingRectangle() }.unwrap();
    assert_eq!(bounds.left, 15.0);
    assert_eq!(bounds.top, 15.0);
    assert_eq!(bounds.width, 75.0);
    assert_eq!(bounds.height, 30.0);
}
