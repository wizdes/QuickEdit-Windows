//! UIA fragment tree for the painted `Widget` nodes.
#![allow(non_snake_case, non_upper_case_globals)]
use crate::provider::DocumentProvider;
use qe_api::{Widget, WidgetId, WidgetRole};
use std::{ffi::c_void, sync::Arc};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{Com::SAFEARRAY, Variant::VARIANT},
        UI::Accessibility::*,
    },
    core::{
        BOOL, BSTR, ComObject, Error, HRESULT, IUnknown, IUnknownImpl, Interface, PCWSTR, Result,
        implement,
    },
};

const E_INVALIDARG: HRESULT = HRESULT(0x80070057u32 as i32);
fn unavailable() -> Error {
    Error::new(
        HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32),
        "This widget is not available in the current tree.",
    )
}
fn unsupported() -> Error {
    Error::new(
        HRESULT(UIA_E_NOTSUPPORTED as i32),
        "This widget does not support that action.",
    )
}

#[derive(Clone, Debug, PartialEq)]
pub enum WidgetAction {
    Invoke,
    Toggle,
    SetValue(String),
    Select,
    Expand,
    Collapse,
    SetRange(f64),
    Focus,
}

/// Each call must return the latest painted tree. Implementations marshal to
/// the UI thread; they must never shape text or wait for disk there.
pub trait WidgetHost: Send + Sync {
    fn widgets(&self) -> Widget;
    fn action(&self, id: WidgetId, action: WidgetAction) -> Result<()>;
    /// A stable process-wide number for this window, distinct across windows.
    fn window_id(&self) -> u64;
    fn window_origin(&self) -> (f64, f64) {
        (0.0, 0.0)
    }
    /// Pixels per device-independent pixel for this window.
    fn window_scale(&self) -> f64;
    /// When present, this appears as the last child of the window root.
    /// `document_widget` gives its bounds, name, focus and stable ID.
    fn document_provider(&self) -> Option<Arc<DocumentProvider>> {
        None
    }
    fn document_widget(&self) -> Option<Widget> {
        None
    }
}

const DOCUMENT_PATH: usize = usize::MAX;

pub struct WidgetProvider {
    simple: IRawElementProviderSimple,
}
impl WidgetProvider {
    pub fn new(host: Arc<dyn WidgetHost>) -> Self {
        let simple = make_node(host, Vec::new()).to_interface();
        Self { simple }
    }
    pub fn simple(&self) -> &IRawElementProviderSimple {
        &self.simple
    }
    /// Call after the widget focus changes. The host tree must already show
    /// the new focused node. With no UIA client, this reads nothing.
    pub fn raise_focus_changed(&self, host: &Arc<dyn WidgetHost>) -> Result<()> {
        if !unsafe { UiaClientsAreListening().as_bool() } {
            return Ok(());
        }
        let root = host.widgets();
        let mut path = Vec::new();
        if !focus_path(&root, &mut path) {
            if host.document_widget().is_some_and(|w| w.states.focused) {
                path.push(DOCUMENT_PATH);
            } else {
                return Err(unavailable());
            }
        }
        let focused: IRawElementProviderSimple = make_node(host.clone(), path).to_interface();
        unsafe { UiaRaiseAutomationEvent(&focused, UIA_AutomationFocusChangedEventId) }
    }
}

/// The shell calls this from WM_GETOBJECT and uses DefWindowProcW for `None`.
/// `factory` is not called for other object IDs.
pub fn wm_getobject(
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
    factory: impl FnOnce() -> WidgetProvider,
) -> Option<LRESULT> {
    if lparam.0 != UiaRootObjectId as isize {
        return None;
    }
    let provider = factory();
    Some(unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, provider.simple()) })
}

fn make_node(host: Arc<dyn WidgetHost>, path: Vec<usize>) -> ComObject<WidgetNode> {
    ComObject::new(WidgetNode { host, path })
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot,
    IInvokeProvider,
    IToggleProvider,
    IValueProvider,
    ISelectionItemProvider,
    IExpandCollapseProvider,
    IRangeValueProvider
)]
struct WidgetNode {
    host: Arc<dyn WidgetHost>,
    path: Vec<usize>,
}

impl WidgetNode_Impl {
    fn node(&self) -> Result<Widget> {
        if self.path.as_slice() == [DOCUMENT_PATH] {
            return self.host.document_widget().ok_or_else(unavailable);
        }
        let mut node = self.host.widgets();
        for &index in &self.path {
            node = node.children.get(index).cloned().ok_or_else(unavailable)?;
        }
        Ok(node)
    }
    fn at(&self, path: Vec<usize>) -> IRawElementProviderFragment {
        make_node(self.host.clone(), path).to_interface()
    }
    fn send(&self, action: WidgetAction) -> Result<()> {
        let node = self.node()?;
        if !node.states.enabled {
            return Err(Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTENABLED as i32)));
        }
        self.host.action(node.id, action)
    }
}

fn variant_int(n: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt.0 = 3;
        (*v.Anonymous.Anonymous).Anonymous.lVal = n;
    }
    v
}
fn variant_bool(b: bool) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt.0 = 11;
        (*v.Anonymous.Anonymous).Anonymous.boolVal.0 = if b { -1 } else { 0 };
    }
    v
}
fn variant_string(s: &str) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt.0 = 8;
        (*v.Anonymous.Anonymous).Anonymous.bstrVal = std::mem::ManuallyDrop::new(BSTR::from(s));
    }
    v
}
fn control_type(role: WidgetRole) -> i32 {
    match role {
        WidgetRole::Button => 50000,
        WidgetRole::Checkbox => 50002,
        WidgetRole::Tab => 50019,
        WidgetRole::TabList => 50018,
        WidgetRole::Menu => 50009,
        WidgetRole::MenuItem => 50011,
        WidgetRole::TextField => 50004,
        WidgetRole::Table => 50036,
        WidgetRole::Row => 50029,
        WidgetRole::Cell => 50029,
        WidgetRole::Dialog => 50032,
        WidgetRole::Group => 50026,
        WidgetRole::Label => 50020,
        WidgetRole::ComboBox => 50003,
        WidgetRole::ScrollBar => 50014,
        WidgetRole::List => 50008,
        WidgetRole::ListItem => 50007,
    }
}

impl IRawElementProviderSimple_Impl for WidgetNode_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        if self.path.as_slice() == [DOCUMENT_PATH] {
            let document = self.host.document_provider().ok_or_else(unavailable)?;
            return unsafe { document.simple().GetPatternProvider(id) };
        }
        let node = self.node()?;
        let object = self.to_object();
        match (id, node.role) {
            (UIA_InvokePatternId, WidgetRole::Button | WidgetRole::MenuItem) => {
                object.to_interface::<IInvokeProvider>().cast()
            }
            (UIA_TogglePatternId, WidgetRole::Checkbox) => {
                object.to_interface::<IToggleProvider>().cast()
            }
            (UIA_ValuePatternId, WidgetRole::TextField | WidgetRole::ComboBox) => {
                object.to_interface::<IValueProvider>().cast()
            }
            (
                UIA_SelectionItemPatternId,
                WidgetRole::Tab | WidgetRole::ListItem | WidgetRole::Row,
            ) => object.to_interface::<ISelectionItemProvider>().cast(),
            (UIA_ExpandCollapsePatternId, WidgetRole::ComboBox | WidgetRole::Menu) => {
                object.to_interface::<IExpandCollapseProvider>().cast()
            }
            (UIA_RangeValuePatternId, WidgetRole::ScrollBar) if node.range_value.is_some() => {
                object.to_interface::<IRangeValueProvider>().cast()
            }
            _ => Err(unsupported()),
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let node = self.node()?;
        Ok(match id {
            UIA_ControlTypePropertyId => variant_int(if self.path.as_slice() == [DOCUMENT_PATH] {
                UIA_DocumentControlTypeId.0
            } else {
                control_type(node.role)
            }),
            UIA_NamePropertyId => variant_string(&node.name),
            UIA_HelpTextPropertyId => node
                .description
                .as_deref()
                .map_or_else(VARIANT::default, variant_string),
            UIA_IsEnabledPropertyId => variant_bool(node.states.enabled),
            UIA_HasKeyboardFocusPropertyId => variant_bool(node.states.focused),
            UIA_IsKeyboardFocusablePropertyId => {
                variant_bool(!matches!(node.role, WidgetRole::Label | WidgetRole::Group))
            }
            UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => variant_bool(true),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(unsupported())
    }
}

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SafeArrayCreateVector(vt: u16, lower: i32, count: u32) -> *mut SAFEARRAY;
    fn SafeArrayPutElement(array: *mut SAFEARRAY, index: *const i32, value: *const c_void) -> i32;
    fn SafeArrayDestroy(array: *mut SAFEARRAY) -> i32;
}
fn runtime_id(window: u64, id: WidgetId) -> Result<*mut SAFEARRAY> {
    let array = unsafe { SafeArrayCreateVector(3, 0, 5) };
    if array.is_null() {
        return Err(unavailable());
    }
    let parts = [
        UiaAppendRuntimeId as i32,
        window as i32,
        (window >> 32) as i32,
        id.0 as i32,
        (id.0 >> 32) as i32,
    ];
    for (i, part) in parts.iter().enumerate() {
        let hr = unsafe { SafeArrayPutElement(array, &(i as i32), (part as *const i32).cast()) };
        if hr < 0 {
            unsafe {
                SafeArrayDestroy(array);
            }
            return Err(Error::from_hresult(HRESULT(hr)));
        }
    }
    Ok(array)
}

impl IRawElementProviderFragment_Impl for WidgetNode_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let mut path = self.path.clone();
        let node = self.node()?;
        let document = self.host.document_widget().is_some();
        if path.as_slice() == [DOCUMENT_PATH] {
            match direction {
                NavigateDirection_Parent => return Ok(self.at(Vec::new())),
                NavigateDirection_PreviousSibling => {
                    let root = self.host.widgets();
                    if root.children.is_empty() {
                        return Err(unavailable());
                    }
                    return Ok(self.at(vec![root.children.len() - 1]));
                }
                _ => return Err(unavailable()),
            }
        }
        match direction {
            NavigateDirection_FirstChild if !node.children.is_empty() => path.push(0),
            NavigateDirection_FirstChild if path.is_empty() && document => path.push(DOCUMENT_PATH),
            NavigateDirection_LastChild if path.is_empty() && document => path.push(DOCUMENT_PATH),
            NavigateDirection_LastChild if !node.children.is_empty() => {
                path.push(node.children.len() - 1)
            }
            NavigateDirection_Parent if !path.is_empty() => {
                path.pop();
            }
            NavigateDirection_NextSibling | NavigateDirection_PreviousSibling
                if !path.is_empty() =>
            {
                let index = path.pop().ok_or_else(unavailable)?;
                let mut parent = self.host.widgets();
                for &part in &path {
                    parent = parent.children.get(part).cloned().ok_or_else(unavailable)?;
                }
                let next = if direction == NavigateDirection_NextSibling {
                    index.checked_add(1)
                } else {
                    index.checked_sub(1)
                };
                if direction == NavigateDirection_NextSibling
                    && path.is_empty()
                    && next == Some(parent.children.len())
                    && document
                {
                    path.push(DOCUMENT_PATH);
                } else {
                    path.push(
                        next.filter(|n| *n < parent.children.len())
                            .ok_or_else(unavailable)?,
                    );
                }
            }
            _ => return Err(unavailable()),
        }
        Ok(self.at(path))
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        runtime_id(self.host.window_id(), self.node()?.id)
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let b = self.node()?.bounds;
        let (x, y) = self.host.window_origin();
        let scale = self.host.window_scale();
        if !scale.is_finite() || scale <= 0.0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        Ok(UiaRect {
            left: x + f64::from(b.x.0) * scale,
            top: y + f64::from(b.y.0) * scale,
            width: f64::from(b.width.0) * scale,
            height: f64::from(b.height.0) * scale,
        })
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        let array = unsafe { SafeArrayCreateVector(13, 0, 0) };
        if array.is_null() {
            Err(unavailable())
        } else {
            Ok(array)
        }
    }
    fn SetFocus(&self) -> Result<()> {
        self.send(WidgetAction::Focus)
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(make_node(self.host.clone(), Vec::new()).to_interface())
    }
}

fn focus_path(node: &Widget, path: &mut Vec<usize>) -> bool {
    if node.states.focused {
        return true;
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        if focus_path(child, path) {
            return true;
        }
        path.pop();
    }
    false
}
fn point_path(node: &Widget, x: f64, y: f64, path: &mut Vec<usize>) -> bool {
    let b = node.bounds;
    let inside = x >= f64::from(b.x.0)
        && y >= f64::from(b.y.0)
        && x < f64::from(b.x.0 + b.width.0)
        && y < f64::from(b.y.0 + b.height.0);
    if !inside {
        return false;
    }
    for (index, child) in node.children.iter().enumerate().rev() {
        path.push(index);
        if point_path(child, x, y, path) {
            return true;
        }
        path.pop();
    }
    true
}
impl IRawElementProviderFragmentRoot_Impl for WidgetNode_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let (ox, oy) = self.host.window_origin();
        let scale = self.host.window_scale();
        if !scale.is_finite() || scale <= 0.0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let x = (x - ox) / scale;
        let y = (y - oy) / scale;
        if let Some(document) = self.host.document_widget() {
            let b = document.bounds;
            if x >= f64::from(b.x.0)
                && y >= f64::from(b.y.0)
                && x < f64::from(b.x.0 + b.width.0)
                && y < f64::from(b.y.0 + b.height.0)
            {
                return Ok(self.at(vec![DOCUMENT_PATH]));
            }
        }
        let mut path = Vec::new();
        if point_path(&self.host.widgets(), x, y, &mut path) {
            Ok(self.at(path))
        } else {
            Err(unavailable())
        }
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let mut path = Vec::new();
        if focus_path(&self.host.widgets(), &mut path) {
            return Ok(self.at(path));
        }
        if self
            .host
            .document_widget()
            .is_some_and(|w| w.states.focused)
        {
            return Ok(self.at(vec![DOCUMENT_PATH]));
        }
        Err(unavailable())
    }
}

impl IInvokeProvider_Impl for WidgetNode_Impl {
    fn Invoke(&self) -> Result<()> {
        self.send(WidgetAction::Invoke)
    }
}
impl IToggleProvider_Impl for WidgetNode_Impl {
    fn Toggle(&self) -> Result<()> {
        self.send(WidgetAction::Toggle)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        Ok(if self.node()?.states.checked {
            ToggleState_On
        } else {
            ToggleState_Off
        })
    }
}
impl IValueProvider_Impl for WidgetNode_Impl {
    fn SetValue(&self, val: &PCWSTR) -> Result<()> {
        if self.node()?.states.read_only {
            return Err(unsupported());
        }
        let value = unsafe { val.to_string()? };
        self.send(WidgetAction::SetValue(value))
    }
    fn Value(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.node()?.value.as_deref().unwrap_or("")))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL(i32::from(self.node()?.states.read_only)))
    }
}
impl ISelectionItemProvider_Impl for WidgetNode_Impl {
    fn Select(&self) -> Result<()> {
        self.send(WidgetAction::Select)
    }
    fn AddToSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn IsSelected(&self) -> Result<BOOL> {
        Ok(BOOL(i32::from(self.node()?.states.selected)))
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        let mut path = self.path.clone();
        if path.pop().is_none() {
            return Err(unavailable());
        }
        Ok(make_node(self.host.clone(), path).to_interface())
    }
}
impl IExpandCollapseProvider_Impl for WidgetNode_Impl {
    fn Expand(&self) -> Result<()> {
        self.send(WidgetAction::Expand)
    }
    fn Collapse(&self) -> Result<()> {
        self.send(WidgetAction::Collapse)
    }
    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        Ok(if self.node()?.states.expanded {
            ExpandCollapseState_Expanded
        } else {
            ExpandCollapseState_Collapsed
        })
    }
}
impl IRangeValueProvider_Impl for WidgetNode_Impl {
    fn SetValue(&self, value: f64) -> Result<()> {
        let node = self.node()?;
        if node.states.read_only {
            return Err(unsupported());
        }
        let range = node.range_value.ok_or_else(unavailable)?;
        if !value.is_finite() || value < range.minimum || value > range.maximum {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        self.send(WidgetAction::SetRange(value))
    }
    fn Value(&self) -> Result<f64> {
        Ok(self.node()?.range_value.ok_or_else(unavailable)?.value)
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL(i32::from(self.node()?.states.read_only)))
    }
    fn Maximum(&self) -> Result<f64> {
        Ok(self.node()?.range_value.ok_or_else(unavailable)?.maximum)
    }
    fn Minimum(&self) -> Result<f64> {
        Ok(self.node()?.range_value.ok_or_else(unavailable)?.minimum)
    }
    fn LargeChange(&self) -> Result<f64> {
        Ok(1.0)
    }
    fn SmallChange(&self) -> Result<f64> {
        Ok(1.0)
    }
}
