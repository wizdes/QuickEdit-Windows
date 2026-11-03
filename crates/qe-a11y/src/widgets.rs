//! The UIA tree: the window's root, the painted widgets, and the document, plus the object
//! the shell keeps for one window.
#![allow(non_snake_case, non_upper_case_globals)]
use crate::{
    com::{
        bstr, gone, int_array, interface_array, invalid, invalid_operation, none, not_enabled,
        not_supported, read_error, scale_of, to_screen, to_window,
    },
    events::{EventFlags, EventTracker},
    host::{UiaHost, WidgetAction},
    marshal::WindowThread,
    provider::{Shared, Source, make_range, with_doc},
    text::{TextRange, known_len},
};
use qe_api::{Dip, Rect, Widget, WidgetId, WidgetRole};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND, LPARAM, LRESULT, WPARAM},
        System::{Com::SAFEARRAY, Variant::VARIANT},
        UI::{
            Accessibility::*,
            WindowsAndMessaging::{InSendMessage, ReplyMessage, WM_GETOBJECT},
        },
    },
    core::{
        BOOL, BSTR, ComObject, IUnknown, IUnknownImpl, Interface, PCWSTR, Ref, Result, implement,
    },
};

/// The window's root and the one document object, so a client always sees the same COM object
/// for each. `Accessibility::shutdown` clears this, and that ends the reference cycle.
pub(crate) struct CachedNodes {
    root: IRawElementProviderSimple,
    document: Option<IRawElementProviderSimple>,
}
// SAFETY: these are this crate's own COM objects, with atomic reference counts. The objects
// keep their state behind `Arc` and `Mutex`.
unsafe impl Send for CachedNodes {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Root,
    Document,
    Widget(WidgetId),
}

pub(crate) fn find_widget(node: &Widget, id: WidgetId) -> Option<&Widget> {
    if node.id == id {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_widget(child, id))
}

fn path_to(node: &Widget, id: WidgetId, path: &mut Vec<usize>) -> bool {
    if node.id == id {
        return true;
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        if path_to(child, id, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn at_path<'a>(root: &'a Widget, path: &[usize]) -> Option<&'a Widget> {
    let mut node = root;
    for &index in path {
        node = node.children.get(index)?;
    }
    Some(node)
}

fn focused_child(node: &Widget) -> Option<WidgetId> {
    node.children
        .iter()
        .find_map(|child| focused_child(child).or_else(|| child.states.focused.then_some(child.id)))
}

fn contains(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.x.0 && y >= rect.y.0 && x < rect.x.0 + rect.width.0 && y < rect.y.0 + rect.height.0
}

/// The deepest widget under a point. A later child paints over an earlier one.
fn hit(node: &Widget, x: f32, y: f32) -> Option<WidgetId> {
    node.children.iter().rev().find_map(|child| {
        hit(child, x, y).or_else(|| contains(child.bounds, x, y).then_some(child.id))
    })
}

fn overlap(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.0.max(b.x.0);
    let top = a.y.0.max(b.y.0);
    let right = (a.x.0 + a.width.0).min(b.x.0 + b.width.0);
    let bottom = (a.y.0 + a.height.0).min(b.y.0 + b.height.0);
    (right > left && bottom > top).then_some(Rect {
        x: Dip(left),
        y: Dip(top),
        width: Dip(right - left),
        height: Dip(bottom - top),
    })
}

/// True when none of this rectangle can show: it has no size, it is outside the window, or it is
/// scrolled out of a list that holds it. Only lists clip, because a popup paints outside the item
/// that owns it.
fn is_offscreen(root: &Widget, path: &[usize], bounds: Rect) -> bool {
    let mut showing = overlap(bounds, bounds);
    if root.bounds.width.0 > 0.0 && root.bounds.height.0 > 0.0 {
        showing = showing.and_then(|rect| overlap(rect, root.bounds));
    }
    let mut node = root;
    for &index in path {
        if matches!(node.role, WidgetRole::List | WidgetRole::Table) {
            showing = showing.and_then(|rect| overlap(rect, node.bounds));
        }
        let Some(next) = node.children.get(index) else {
            break;
        };
        node = next;
    }
    showing.is_none()
}

/// UIA's control type for a role. A table here has no Grid pattern, so it shows as a list
/// of items, which is what UIA's rules ask of a control with no grid.
fn control_type(role: WidgetRole) -> UIA_CONTROLTYPE_ID {
    match role {
        WidgetRole::Button => UIA_ButtonControlTypeId,
        WidgetRole::Checkbox => UIA_CheckBoxControlTypeId,
        WidgetRole::Tab => UIA_TabItemControlTypeId,
        WidgetRole::TabList => UIA_TabControlTypeId,
        WidgetRole::Menu => UIA_MenuControlTypeId,
        WidgetRole::MenuBar => UIA_MenuBarControlTypeId,
        WidgetRole::MenuItem => UIA_MenuItemControlTypeId,
        WidgetRole::TextField => UIA_EditControlTypeId,
        WidgetRole::Table | WidgetRole::List => UIA_ListControlTypeId,
        WidgetRole::Row | WidgetRole::ListItem => UIA_ListItemControlTypeId,
        WidgetRole::Cell | WidgetRole::Label => UIA_TextControlTypeId,
        WidgetRole::Dialog => UIA_WindowControlTypeId,
        WidgetRole::Group => UIA_GroupControlTypeId,
        WidgetRole::ComboBox => UIA_ComboBoxControlTypeId,
        WidgetRole::ScrollBar => UIA_ScrollBarControlTypeId,
    }
}

fn focusable(role: WidgetRole) -> bool {
    !matches!(
        role,
        WidgetRole::Label
            | WidgetRole::Group
            | WidgetRole::Cell
            | WidgetRole::Menu
            | WidgetRole::MenuBar
            | WidgetRole::Dialog
    )
}

fn make_node(shared: &Arc<Shared>, target: Target) -> Result<IRawElementProviderFragment> {
    match target {
        Target::Root => root_simple(shared)?.cast(),
        Target::Document => document_simple(shared)?.cast(),
        Target::Widget(_) => Ok(new_node(shared, target).to_interface()),
    }
}

fn root_simple(shared: &Arc<Shared>) -> Result<IRawElementProviderSimple> {
    shared
        .nodes
        .lock()
        .map_err(|_| gone())?
        .as_ref()
        .map(|nodes| nodes.root.clone())
        .ok_or_else(gone)
}

fn document_simple(shared: &Arc<Shared>) -> Result<IRawElementProviderSimple> {
    let mut nodes = shared.nodes.lock().map_err(|_| gone())?;
    let nodes = nodes.as_mut().ok_or_else(gone)?;
    if let Some(found) = &nodes.document {
        return Ok(found.clone());
    }
    let made = new_node(shared, Target::Document);
    let made: IRawElementProviderSimple = made.to_interface();
    nodes.document = Some(made.clone());
    Ok(made)
}

/// A node whose "none" answers write null. See `abi.rs`.
fn new_node(shared: &Arc<Shared>, target: Target) -> ComObject<Node> {
    let made = ComObject::new(Node {
        shared: shared.clone(),
        target,
    });
    crate::abi::node_simple::protect(&made.to_interface());
    crate::abi::node_fragment::protect(&made.to_interface());
    crate::abi::node_selection_item::protect(&made.to_interface());
    made
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    IToggleProvider,
    IValueProvider,
    ISelectionItemProvider,
    ISelectionProvider,
    IExpandCollapseProvider,
    IRangeValueProvider,
    ITextProvider2
)]
struct Node {
    shared: Arc<Shared>,
    target: Target,
}

/// The widget a target stands for, as it is now. Call on the window's thread.
fn lookup(host: &Arc<dyn UiaHost>, target: Target) -> Result<Widget> {
    match target {
        Target::Root => host.widgets().ok_or_else(gone),
        Target::Document => host.document_widget().ok_or_else(gone),
        Target::Widget(id) => {
            let root = host.widgets().ok_or_else(gone)?;
            find_widget(&root, id).cloned().ok_or_else(gone)
        }
    }
}

/// Fragment navigation. Call on the window's thread.
fn walk(
    shared: &Arc<Shared>,
    host: &Arc<dyn UiaHost>,
    target: Target,
    direction: NavigateDirection,
) -> Result<IRawElementProviderFragment> {
    let root = host.widgets().ok_or_else(gone)?;
    let has_document = host.document_widget().is_some();
    let child = |index: usize| {
        root.children
            .get(index)
            .map_or_else(|| Err(none()), |w| make_node(shared, Target::Widget(w.id)))
    };
    match target {
        Target::Root => match direction {
            NavigateDirection_FirstChild if !root.children.is_empty() => child(0),
            NavigateDirection_FirstChild | NavigateDirection_LastChild if has_document => {
                make_node(shared, Target::Document)
            }
            NavigateDirection_LastChild if !root.children.is_empty() => {
                child(root.children.len() - 1)
            }
            _ => Err(none()),
        },
        Target::Document => match direction {
            NavigateDirection_Parent => make_node(shared, Target::Root),
            NavigateDirection_PreviousSibling if !root.children.is_empty() => {
                child(root.children.len() - 1)
            }
            _ => Err(none()),
        },
        Target::Widget(id) => {
            let mut path = Vec::new();
            if !path_to(&root, id, &mut path) {
                return Err(gone());
            }
            let node = at_path(&root, &path).ok_or_else(gone)?;
            let index = path.pop().ok_or_else(gone)?;
            let parent = at_path(&root, &path).ok_or_else(gone)?;
            let of = |w: Option<&Widget>| {
                w.map_or_else(|| Err(none()), |w| make_node(shared, Target::Widget(w.id)))
            };
            match direction {
                NavigateDirection_Parent if path.is_empty() => make_node(shared, Target::Root),
                NavigateDirection_Parent => make_node(shared, Target::Widget(parent.id)),
                NavigateDirection_FirstChild => of(node.children.first()),
                NavigateDirection_LastChild => of(node.children.last()),
                NavigateDirection_NextSibling => {
                    if path.is_empty() && index + 1 == parent.children.len() && has_document {
                        make_node(shared, Target::Document)
                    } else {
                        of(parent.children.get(index + 1))
                    }
                }
                NavigateDirection_PreviousSibling => {
                    of(index.checked_sub(1).and_then(|i| parent.children.get(i)))
                }
                _ => Err(none()),
            }
        }
    }
}

fn refuse_read_only(widget: &Widget) -> Result<()> {
    if widget.states.read_only {
        return Err(invalid_operation("This control is read only."));
    }
    Ok(())
}

impl Node {
    fn widget(&self) -> Result<Widget> {
        let target = self.target;
        self.shared.ui.on_ui(move |host| lookup(host, target))
    }
    /// Runs now, inside the call. The action must not open a dialog. The check and the action
    /// share one crossing to the window's thread, so nothing can change between them.
    fn send(&self, action: WidgetAction) -> Result<()> {
        self.send_if(|_| Ok(()), action)
    }
    fn send_if(
        &self,
        check: impl FnOnce(&Widget) -> Result<()> + 'static,
        action: WidgetAction,
    ) -> Result<()> {
        let target = self.target;
        self.shared.ui.on_ui(move |host| {
            let widget = lookup(host, target)?;
            if !widget.states.enabled {
                return Err(not_enabled("This control is turned off."));
            }
            check(&widget)?;
            host.action(widget.id, action)
        })
    }
    /// Checks the control, then queues the action to run after this call returns, so an
    /// action that opens a dialog does not hold up the client. It checks again when it runs,
    /// because the control can change in between. A failure goes to the host.
    fn post(&self, action: WidgetAction) -> Result<()> {
        let widget = self.widget()?;
        if !widget.states.enabled {
            return Err(not_enabled("This control is turned off."));
        }
        let id = widget.id;
        let target = self.target;
        self.shared.ui.post(move |host| {
            let answer = lookup(host, target).and_then(|now| {
                if now.states.enabled {
                    host.action(id, action.clone())
                } else {
                    Err(not_enabled("This control was turned off."))
                }
            });
            if let Err(error) = answer {
                host.action_failed(id, &action, &error);
            }
        })
    }
    fn offscreen(&self) -> Result<bool> {
        let target = self.target;
        self.shared.ui.on_ui(move |host| {
            let root = host.widgets().ok_or_else(gone)?;
            Ok(match target {
                Target::Root => false,
                Target::Document => {
                    let bounds = host.document_widget().ok_or_else(gone)?.bounds;
                    is_offscreen(&root, &[], bounds)
                }
                Target::Widget(id) => {
                    let mut path = Vec::new();
                    if !path_to(&root, id, &mut path) {
                        return Err(gone());
                    }
                    let bounds = at_path(&root, &path).ok_or_else(gone)?.bounds;
                    is_offscreen(&root, &path, bounds)
                }
            })
        })
    }
    fn text_source(&self) -> Result<Source> {
        match self.target {
            Target::Document => {
                self.shared.ui.check_open()?;
                Ok(Source::Editor)
            }
            Target::Widget(id) => {
                if self.widget()?.role == WidgetRole::TextField {
                    Ok(Source::Field(id))
                } else {
                    Err(not_supported("This control has no text."))
                }
            }
            Target::Root => Err(not_supported("The window has no text of its own.")),
        }
    }
    fn document_read_only(&self) -> Result<bool> {
        with_doc(&self.shared, Source::Editor, |doc| Ok(doc.read_only()))
    }
    /// Value is offered on a document only when it is read-only: there, it reports that. An
    /// editable document is changed through its keys and commands, not by a whole new value.
    fn offers_value(&self, widget: &Widget) -> Result<bool> {
        match self.target {
            Target::Document => self.document_read_only(),
            _ => Ok(matches!(
                widget.role,
                WidgetRole::TextField | WidgetRole::ComboBox
            )),
        }
    }
    fn is_read_only(&self) -> Result<bool> {
        match self.target {
            Target::Document => self.document_read_only(),
            _ => Ok(self.widget()?.states.read_only),
        }
    }
    fn property(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let widget = self.widget()?;
        let is_root = self.target == Target::Root;
        Ok(match id {
            // The window's own provider gives the root its type.
            UIA_ControlTypePropertyId if is_root => VARIANT::default(),
            UIA_ControlTypePropertyId if self.target == Target::Document => {
                VARIANT::from(UIA_DocumentControlTypeId.0)
            }
            UIA_ControlTypePropertyId => VARIANT::from(control_type(widget.role).0),
            UIA_NamePropertyId if is_root && widget.name.is_empty() => VARIANT::default(),
            UIA_NamePropertyId => VARIANT::from(widget.name.as_str()),
            UIA_AccessKeyPropertyId => widget
                .access_key
                .as_deref()
                .map_or_else(VARIANT::default, VARIANT::from),
            UIA_AutomationIdPropertyId if !is_root => {
                VARIANT::from(format!("qe-{}", widget.id.0).as_str())
            }
            UIA_HelpTextPropertyId => widget
                .description
                .as_deref()
                .map_or_else(VARIANT::default, VARIANT::from),
            UIA_IsEnabledPropertyId => VARIANT::from(widget.states.enabled),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(widget.states.focused),
            UIA_IsKeyboardFocusablePropertyId => {
                let can = self.target == Target::Document || focusable(widget.role);
                VARIANT::from(can && widget.states.enabled)
            }
            UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => VARIANT::from(true),
            UIA_IsOffscreenPropertyId => VARIANT::from(self.offscreen()?),
            UIA_IsDialogPropertyId => VARIANT::from(widget.role == WidgetRole::Dialog),
            UIA_HeadingLevelPropertyId if widget.states.section => VARIANT::from(HeadingLevel2.0),
            _ => VARIANT::default(),
        })
    }
    fn navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let (shared, target) = (self.shared.clone(), self.target);
        self.shared
            .ui
            .on_ui(move |host| walk(&shared, host, target, direction))
    }
    fn runtime_id(&self) -> Result<*mut SAFEARRAY> {
        if self.target == Target::Root {
            // The window's own provider gives the root its ID.
            self.shared.ui.check_open()?;
            return Ok(std::ptr::null_mut());
        }
        let id = self.widget()?.id.0;
        let (low, high) = (id as i32, (id >> 32) as i32);
        // The document's number comes from the shell separately from the painted tree, so a
        // fourth part keeps it apart from a widget with the same number.
        if self.target == Target::Document {
            return int_array(&[UiaAppendRuntimeId as i32, low, high, 1]);
        }
        int_array(&[UiaAppendRuntimeId as i32, low, high])
    }
    fn rect(&self) -> Result<UiaRect> {
        let (target, window) = (self.target, self.shared.window);
        let [left, top, width, height] = self.shared.ui.on_ui(move |host| {
            let widget = lookup(host, target)?;
            to_screen(window, scale_of(window, host.dpi())?, widget.bounds)
        })?;
        Ok(UiaRect {
            left,
            top,
            width,
            height,
        })
    }
}

impl IRawElementProviderSimple_Impl for Node_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        let target = self.target;
        let (widget, checkable) = self.shared.ui.on_ui(move |host| {
            let widget = lookup(host, target)?;
            let checkable = host.is_checkable(widget.id);
            Ok((widget, checkable))
        })?;
        let role = widget.role;
        let states = widget.states;
        let is_document = self.target == Target::Document;
        let is_field = !is_document && role == WidgetRole::TextField;
        let offered = match id {
            UIA_TextPatternId | UIA_TextPattern2Id => is_document || is_field,
            UIA_ValuePatternId => self.offers_value(&widget)?,
            UIA_InvokePatternId => {
                !is_document
                    && (role == WidgetRole::Button
                        || (role == WidgetRole::MenuItem && !states.has_popup))
            }
            UIA_TogglePatternId => {
                !is_document
                    && (role == WidgetRole::Checkbox
                        || (role == WidgetRole::MenuItem && (states.checked || checkable)))
            }
            UIA_SelectionItemPatternId => {
                !is_document
                    && matches!(
                        role,
                        WidgetRole::Tab | WidgetRole::ListItem | WidgetRole::Row
                    )
            }
            UIA_SelectionPatternId => {
                !is_document
                    && matches!(
                        role,
                        WidgetRole::TabList | WidgetRole::List | WidgetRole::Table
                    )
            }
            UIA_ExpandCollapsePatternId => {
                !is_document && (role == WidgetRole::ComboBox || states.has_popup)
            }
            UIA_RangeValuePatternId => !is_document && widget.range_value.is_some(),
            _ => false,
        };
        if !offered {
            return Err(none());
        }
        let this = self.to_object();
        match id {
            UIA_TextPatternId | UIA_TextPattern2Id => this.to_interface::<ITextProvider2>().cast(),
            UIA_ValuePatternId => this.to_interface::<IValueProvider>().cast(),
            UIA_InvokePatternId => this.to_interface::<IInvokeProvider>().cast(),
            UIA_TogglePatternId => this.to_interface::<IToggleProvider>().cast(),
            UIA_SelectionItemPatternId => this.to_interface::<ISelectionItemProvider>().cast(),
            UIA_SelectionPatternId => this.to_interface::<ISelectionProvider>().cast(),
            UIA_ExpandCollapsePatternId => this.to_interface::<IExpandCollapseProvider>().cast(),
            UIA_RangeValuePatternId => this.to_interface::<IRangeValueProvider>().cast(),
            _ => Err(none()),
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.property(id)
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        self.shared.ui.check_open()?;
        Err(none())
    }
}

impl IRawElementProviderFragment_Impl for Node_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        self.navigate(direction)
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        self.runtime_id()
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        self.rect()
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        self.shared.ui.check_open()?;
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        self.send(WidgetAction::Focus)
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.shared.ui.check_open()?;
        root_simple(&self.shared)?.cast()
    }
}

impl IInvokeProvider_Impl for Node_Impl {
    fn Invoke(&self) -> Result<()> {
        self.post(WidgetAction::Invoke)
    }
}
impl IToggleProvider_Impl for Node_Impl {
    fn Toggle(&self) -> Result<()> {
        self.post(WidgetAction::Toggle)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        Ok(if self.widget()?.states.checked {
            ToggleState_On
        } else {
            ToggleState_Off
        })
    }
}
impl IValueProvider_Impl for Node_Impl {
    fn SetValue(&self, value: &PCWSTR) -> Result<()> {
        if value.is_null() {
            return Err(invalid("No text was given."));
        }
        // A document never takes a whole new value, whatever a pattern kept from when it was
        // read-only says. It is changed through its keys and commands.
        if self.target == Target::Document {
            self.shared.ui.check_open()?;
            return Err(invalid_operation(
                "This document does not take a new value.",
            ));
        }
        let value = unsafe { value.to_string() }.map_err(|_| invalid("This text is not valid."))?;
        self.send_if(refuse_read_only, WidgetAction::SetValue(value))
    }
    /// A document's value stops at the same cap as GetText, because tools ask for this
    /// property with no limit.
    fn Value(&self) -> Result<BSTR> {
        if self.target == Target::Document {
            let text = with_doc(
                &self.shared,
                Source::Editor,
                |doc| Ok(doc.shared_snapshot()),
            )?;
            let (len, _) = known_len(text.as_ref()).map_err(read_error)?;
            let whole = TextRange::new(0, len);
            let read = if self.shared.ui.is_window_thread() {
                crate::text::read_text(text.as_ref(), whole, -1)
            } else {
                crate::text::read_text_waiting(text.as_ref(), whole, -1)
            };
            return bstr(&read.map_err(read_error)?);
        }
        bstr(self.widget()?.value.as_deref().unwrap_or(""))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.is_read_only()?))
    }
}
impl ISelectionItemProvider_Impl for Node_Impl {
    fn Select(&self) -> Result<()> {
        self.post(WidgetAction::Select)
    }
    fn AddToSelection(&self) -> Result<()> {
        self.shared.ui.check_open()?;
        Err(not_supported("Only one item can be selected here."))
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        self.shared.ui.check_open()?;
        Err(not_supported("Only one item can be selected here."))
    }
    fn IsSelected(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.widget()?.states.selected))
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        let Target::Widget(id) = self.target else {
            return Err(none());
        };
        let shared = self.shared.clone();
        self.shared.ui.on_ui(move |host| {
            let root = host.widgets().ok_or_else(gone)?;
            let mut path = Vec::new();
            if !path_to(&root, id, &mut path) {
                return Err(gone());
            }
            path.pop();
            if path.is_empty() {
                return root_simple(&shared);
            }
            let parent = at_path(&root, &path).ok_or_else(gone)?;
            make_node(&shared, Target::Widget(parent.id))?.cast()
        })
    }
}
impl ISelectionProvider_Impl for Node_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let widget = self.widget()?;
        let mut selected = Vec::new();
        for child in widget.children.iter().filter(|c| c.states.selected) {
            let node: IRawElementProviderSimple =
                make_node(&self.shared, Target::Widget(child.id))?.cast()?;
            selected.push(node);
        }
        interface_array(&selected)
    }
    fn CanSelectMultiple(&self) -> Result<BOOL> {
        self.shared.ui.check_open()?;
        Ok(BOOL::from(false))
    }
    fn IsSelectionRequired(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.widget()?.role == WidgetRole::TabList))
    }
}
impl IExpandCollapseProvider_Impl for Node_Impl {
    fn Expand(&self) -> Result<()> {
        self.post(WidgetAction::Expand)
    }
    fn Collapse(&self) -> Result<()> {
        self.post(WidgetAction::Collapse)
    }
    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        Ok(if self.widget()?.states.expanded {
            ExpandCollapseState_Expanded
        } else {
            ExpandCollapseState_Collapsed
        })
    }
}
impl IRangeValueProvider_Impl for Node_Impl {
    fn SetValue(&self, value: f64) -> Result<()> {
        self.send_if(
            move |widget| {
                refuse_read_only(widget)?;
                let range = widget.range_value.ok_or_else(gone)?;
                if !value.is_finite() || value < range.minimum || value > range.maximum {
                    return Err(invalid("This value is outside the control's range."));
                }
                Ok(())
            },
            WidgetAction::SetRange(value),
        )
    }
    fn Value(&self) -> Result<f64> {
        Ok(self.widget()?.range_value.ok_or_else(gone)?.value)
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.widget()?.states.read_only))
    }
    fn Maximum(&self) -> Result<f64> {
        Ok(self.widget()?.range_value.ok_or_else(gone)?.maximum)
    }
    fn Minimum(&self) -> Result<f64> {
        Ok(self.widget()?.range_value.ok_or_else(gone)?.minimum)
    }
    fn LargeChange(&self) -> Result<f64> {
        let range = self.widget()?.range_value.ok_or_else(gone)?;
        Ok(range.step.unwrap_or(1.0) * LARGE_CHANGE_STEPS)
    }
    fn SmallChange(&self) -> Result<f64> {
        let range = self.widget()?.range_value.ok_or_else(gone)?;
        Ok(range.step.unwrap_or(1.0))
    }
}

impl Node_Impl {
    fn enclosing(&self) -> IRawElementProviderSimple {
        self.to_object().to_interface()
    }
}

impl ITextProvider_Impl for Node_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let source = self.text_source()?;
        let (shared, enclosing) = (self.shared.clone(), self.enclosing());
        let range = with_doc(&self.shared, source, move |doc| {
            let selection = doc.selection().map_err(read_error)?;
            Ok(make_range(
                &shared,
                source,
                doc.stamp(),
                selection,
                enclosing,
            ))
        })?;
        interface_array(&[range])
    }
    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let source = self.text_source()?;
        let (shared, enclosing) = (self.shared.clone(), self.enclosing());
        let range = with_doc(&self.shared, source, move |doc| {
            Ok(doc
                .visible()?
                .map(|range| make_range(&shared, source, doc.stamp(), range, enclosing)))
        })?;
        match range {
            Some(range) => interface_array(&[range]),
            None => interface_array::<ITextRangeProvider>(&[]),
        }
    }
    fn RangeFromChild(&self, _: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        self.text_source()?;
        Err(invalid("This text has no child elements."))
    }
    fn RangeFromPoint(&self, point: &UiaPoint) -> Result<ITextRangeProvider> {
        let source = self.text_source()?;
        let (shared, enclosing) = (self.shared.clone(), self.enclosing());
        let (x, y) = (point.x, point.y);
        if !x.is_finite() || !y.is_finite() {
            return Err(invalid("This point is not a number."));
        }
        with_doc(&self.shared, source, move |doc| {
            let range = doc.at_point(x, y)?;
            Ok(make_range(&shared, source, doc.stamp(), range, enclosing))
        })
    }
    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let source = self.text_source()?;
        let (shared, enclosing) = (self.shared.clone(), self.enclosing());
        with_doc(&self.shared, source, move |doc| {
            let (len, _) = known_len(doc.snapshot()).map_err(read_error)?;
            Ok(make_range(
                &shared,
                source,
                doc.stamp(),
                TextRange::new(0, len),
                enclosing,
            ))
        })
    }
    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        self.text_source()?;
        Ok(SupportedTextSelection_Single)
    }
}
impl ITextProvider2_Impl for Node_Impl {
    fn RangeFromAnnotation(&self, _: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        self.text_source()?;
        Err(invalid("This text has no annotations."))
    }
    fn GetCaretRange(&self, active: *mut BOOL) -> Result<ITextRangeProvider> {
        if active.is_null() {
            return Err(invalid("No place was given for the answer."));
        }
        let source = self.text_source()?;
        let focused = self.widget()?.states.focused;
        let (shared, enclosing) = (self.shared.clone(), self.enclosing());
        let range = with_doc(&self.shared, source, move |doc| {
            let at = doc.caret().map_err(read_error)?;
            Ok(make_range(
                &shared,
                source,
                doc.stamp(),
                TextRange::new(at, at),
                enclosing,
            ))
        })?;
        // SAFETY: the caller gave this pointer for the answer, and it is not null.
        unsafe { *active = BOOL::from(focused) };
        Ok(range)
    }
}

/// The window's root. It is a separate type because UIA treats anything that answers
/// `IRawElementProviderFragmentRoot` as a root.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct Root {
    node: Node,
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> Result<IUnknown> {
        self.node.shared.ui.check_open()?;
        Err(none())
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.node.property(id)
    }
    /// This joins the root to the window, so UIA finds the tree under the window's element.
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        self.node.shared.ui.check_open()?;
        unsafe { UiaHostProviderFromHwnd(HWND(self.node.shared.window as *mut c_void)) }
    }
}
impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        self.node.navigate(direction)
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        self.node.runtime_id()
    }
    /// The window's own provider gives the root its rectangle.
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        self.node.shared.ui.check_open()?;
        Ok(UiaRect::default())
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        self.node.shared.ui.check_open()?;
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        self.node.shared.ui.check_open()?;
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.node.shared.ui.check_open()?;
        Ok(self.to_object().to_interface())
    }
}
impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let shared = self.node.shared.clone();
        let window = shared.window;
        let this = self.to_object();
        self.node.shared.ui.on_ui(move |host| {
            let (x, y) = to_window(window, scale_of(window, host.dpi())?, x, y)?;
            let root = host.widgets().ok_or_else(gone)?;
            if let Some(id) = hit(&root, x, y) {
                return make_node(&shared, Target::Widget(id));
            }
            if host
                .document_widget()
                .is_some_and(|d| contains(d.bounds, x, y))
            {
                return make_node(&shared, Target::Document);
            }
            Ok(this.to_interface())
        })
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let shared = self.node.shared.clone();
        self.node.shared.ui.on_ui(move |host| {
            let root = host.widgets().ok_or_else(gone)?;
            if let Some(id) = focused_child(&root) {
                return make_node(&shared, Target::Widget(id));
            }
            if host.document_widget().is_some_and(|d| d.states.focused) {
                return make_node(&shared, Target::Document);
            }
            Err(none())
        })
    }
}

/// True when WM_GETOBJECT asks for UIA's tree. Windows may send the ID sign-extended or not,
/// and its docs say to read it as 32 bits.
pub fn is_uia_request(lparam: LPARAM) -> bool {
    lparam.0 as i32 == UiaRootObjectId
}

/// What the shell keeps for one window, and all it calls. It starts empty, and nothing is made
/// until a client asks. No method holds a borrow while host code runs, so host code may call
/// any of them from anywhere: inside an action, inside a dialog, or in WM_DESTROY.
#[derive(Default)]
pub struct UiaSlot {
    current: RefCell<Option<Rc<Accessibility>>>,
    closed: Cell<bool>,
}

impl UiaSlot {
    pub fn new() -> Self {
        Self::default()
    }
    /// The one call the window procedure needs, before anything else. `Some` means this message
    /// was for UIA, and the procedure returns that answer. Other messages cost one comparison.
    pub fn on_window_message(
        &self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        host: impl FnOnce() -> Arc<dyn UiaHost>,
    ) -> Option<LRESULT> {
        // Work from another thread runs here. No borrow exists, and none is taken.
        if let Some(answer) = crate::marshal::dispatch(message, wparam) {
            return Some(answer);
        }
        if message != WM_GETOBJECT || !is_uia_request(lparam) || self.closed.get() {
            return None;
        }
        let accessibility = match self.current() {
            Some(found) => found,
            None => {
                let made = Rc::new(Accessibility::new(hwnd, host()));
                *self.current.borrow_mut() = Some(made.clone());
                made
            }
        };
        Some(accessibility.respond(hwnd, wparam, lparam))
    }
    /// `on_window_message` for a window whose popups forward their `WM_GETOBJECT`. The provider
    /// always belongs to `home`, whose procedure runs this crate's work, whichever of them a
    /// client asked first. `hwnd` is the window that gets the answer.
    pub fn on_message_for_home(
        &self,
        home: HWND,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        host: impl FnOnce() -> Arc<dyn UiaHost>,
    ) -> Option<LRESULT> {
        if let Some(answer) = crate::marshal::dispatch(message, wparam) {
            return Some(answer);
        }
        if message != WM_GETOBJECT || !is_uia_request(lparam) || self.closed.get() {
            return None;
        }
        let accessibility = match self.current() {
            Some(found) => found,
            None => {
                let made = Rc::new(Accessibility::new(home, host()));
                *self.current.borrow_mut() = Some(made.clone());
                made
            }
        };
        Some(accessibility.respond(hwnd, wparam, lparam))
    }
    /// The provider, if a client has asked. The borrow ends before this returns.
    pub fn current(&self) -> Option<Rc<Accessibility>> {
        self.current.borrow().clone()
    }
    /// Call once after each input batch, and after `poll_layout` changes the text. If no client
    /// ever asked, this reads one `Option`.
    pub fn after_input(&self) -> Result<EventFlags> {
        match self.current() {
            Some(ui) => ui.after_input(),
            None => Ok(EventFlags::default()),
        }
    }
    /// Call after focus moves, or a dialog or menu opens or closes.
    pub fn widgets_changed(&self) -> Result<WidgetEvents> {
        match self.current() {
            Some(ui) => ui.widgets_changed(),
            None => Ok(WidgetEvents::default()),
        }
    }
    /// Says this to a screen reader, as a live region would: a count of matches, or a message
    /// that has opened. With no client, this reads one `Option`.
    pub fn announce(&self, text: &str) -> Result<()> {
        match self.current() {
            Some(ui) => ui.announce(text),
            None => Ok(()),
        }
    }
    /// Call from WM_DESTROY. After this, a client's objects say UIA_E_ELEMENTNOTAVAILABLE. A
    /// second call does nothing.
    pub fn shutdown(&self) -> Result<()> {
        self.closed.set(true);
        // If this fails, the provider stays here, so a later call can try again.
        if let Some(ui) = self.current() {
            let host = ui.close()?;
            self.current.borrow_mut().take();
            drop(ui);
            // This must stay last. The host may hold the last reference to what holds this slot,
            // so `self` may be gone after this.
            drop(host);
        }
        Ok(())
    }
}

/// What one `widgets_changed` raised.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WidgetEvents {
    pub structure: bool,
    pub focus: bool,
    /// A field's text changed.
    pub text_changed: u32,
    /// A field's selection changed.
    pub selection_changed: u32,
    /// An item in a tab list or a list became selected.
    pub element_selected: u32,
    /// Property changes: name, value, on or off, turned on or off, expanded, and range value.
    pub properties: u32,
}

/// What a client can read of a widget, to compare with the last report.
#[derive(Clone, PartialEq)]
struct SeenWidget {
    role: WidgetRole,
    name: String,
    value: Option<String>,
    selection: Option<std::ops::Range<usize>>,
    enabled: bool,
    selected: bool,
    checked: bool,
    expanded: bool,
    range: Option<f64>,
}

fn remember(node: &Widget, into: &mut HashMap<WidgetId, SeenWidget>) {
    for child in &node.children {
        into.insert(
            child.id,
            SeenWidget {
                role: child.role,
                name: child.name.clone(),
                value: child.value.clone(),
                selection: child.selection.clone(),
                enabled: child.states.enabled,
                selected: child.states.selected,
                checked: child.states.checked,
                expanded: child.states.expanded,
                range: child.range_value.map(|r| r.value),
            },
        );
        remember(child, into);
    }
}

thread_local! {
    static FAIL_RAISES: Cell<u32> = const { Cell::new(0) };
}

/// Makes the next `count` raises on this thread fail, because a test has no other way to reach
/// that failure.
#[doc(hidden)]
pub fn fail_next_raises_for_tests(count: u32) {
    FAIL_RAISES.with(|left| left.set(count));
}

fn check_raise() -> Result<()> {
    FAIL_RAISES.with(|left| {
        if left.get() == 0 {
            return Ok(());
        }
        left.set(left.get() - 1);
        Err(windows::core::Error::new(
            E_FAIL,
            "A test made this raise fail.",
        ))
    })
}

/// A large change is this many small ones.
const LARGE_CHANGE_STEPS: f64 = 10.0;

fn raise_event(node: &IRawElementProviderSimple, id: UIA_EVENT_ID) -> Result<()> {
    check_raise()?;
    unsafe { UiaRaiseAutomationEvent(node, id) }
}

fn raise_property(
    node: &IRawElementProviderSimple,
    id: UIA_PROPERTY_ID,
    old: VARIANT,
    new: VARIANT,
    raised: &mut WidgetEvents,
) -> Result<()> {
    check_raise()?;
    unsafe { UiaRaiseAutomationPropertyChangedEvent(node, id, &old, &new) }?;
    raised.properties += 1;
    Ok(())
}

fn toggle_of(on: bool) -> VARIANT {
    VARIANT::from(if on {
        ToggleState_On.0
    } else {
        ToggleState_Off.0
    })
}
fn expand_of(open: bool) -> VARIANT {
    VARIANT::from(if open {
        ExpandCollapseState_Expanded.0
    } else {
        ExpandCollapseState_Collapsed.0
    })
}

/// Raises what changed in one widget between two reports.
fn raise_changes(
    node: &IRawElementProviderSimple,
    old: &SeenWidget,
    new: &SeenWidget,
    raised: &mut WidgetEvents,
) -> Result<()> {
    if old.name != new.name {
        raise_property(
            node,
            UIA_NamePropertyId,
            VARIANT::from(old.name.as_str()),
            VARIANT::from(new.name.as_str()),
            raised,
        )?;
    }
    if old.enabled != new.enabled {
        raise_property(
            node,
            UIA_IsEnabledPropertyId,
            VARIANT::from(old.enabled),
            VARIANT::from(new.enabled),
            raised,
        )?;
    }
    if old.value != new.value {
        raise_property(
            node,
            UIA_ValueValuePropertyId,
            VARIANT::from(old.value.as_deref().unwrap_or("")),
            VARIANT::from(new.value.as_deref().unwrap_or("")),
            raised,
        )?;
        if new.role == WidgetRole::TextField {
            raise_event(node, UIA_Text_TextChangedEventId)?;
            raised.text_changed += 1;
        }
    }
    if old.selection != new.selection && new.role == WidgetRole::TextField {
        raise_event(node, UIA_Text_TextSelectionChangedEventId)?;
        raised.selection_changed += 1;
    }
    if old.selected != new.selected {
        raise_property(
            node,
            UIA_SelectionItemIsSelectedPropertyId,
            VARIANT::from(old.selected),
            VARIANT::from(new.selected),
            raised,
        )?;
        if new.selected {
            raise_event(node, UIA_SelectionItem_ElementSelectedEventId)?;
            raised.element_selected += 1;
        }
    }
    if old.checked != new.checked {
        raise_property(
            node,
            UIA_ToggleToggleStatePropertyId,
            toggle_of(old.checked),
            toggle_of(new.checked),
            raised,
        )?;
    }
    if old.expanded != new.expanded {
        raise_property(
            node,
            UIA_ExpandCollapseExpandCollapseStatePropertyId,
            expand_of(old.expanded),
            expand_of(new.expanded),
            raised,
        )?;
    }
    if let (Some(before), Some(after)) = (old.range, new.range)
        && before != after
    {
        raise_property(
            node,
            UIA_RangeValueValuePropertyId,
            VARIANT::from(before),
            VARIANT::from(after),
            raised,
        )?;
    }
    Ok(())
}

/// Where keyboard focus is, as a client should hear it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Nowhere,
    Document,
    Widget(WidgetId),
}

/// All of one window's UIA state. The shell uses this through `UiaSlot`.
pub struct Accessibility {
    shared: Arc<Shared>,
    root: IRawElementProviderSimple,
    tracker: RefCell<EventTracker>,
    last_focus: Cell<Option<Focus>>,
    last_shape: Cell<Option<u64>>,
    last_states: RefCell<Option<HashMap<WidgetId, SeenWidget>>>,
    last_document_name: RefCell<Option<String>>,
    closed: Cell<bool>,
}

fn shape_of(node: &Widget, into: &mut std::collections::hash_map::DefaultHasher) {
    use std::hash::Hash;
    node.id.0.hash(into);
    node.children.len().hash(into);
    for child in &node.children {
        shape_of(child, into);
    }
}

impl Accessibility {
    /// Call on the window's thread.
    pub fn new(hwnd: HWND, host: Arc<dyn UiaHost>) -> Self {
        let window = hwnd.0 as isize;
        let shared = Arc::new(Shared {
            window,
            ui: Arc::new(WindowThread::new(window, host)),
            nodes: Mutex::new(None),
        });
        let made = ComObject::new(Root {
            node: Node {
                shared: shared.clone(),
                target: Target::Root,
            },
        });
        crate::abi::root_simple::protect(&made.to_interface());
        crate::abi::root_fragment::protect(&made.to_interface());
        crate::abi::root_face::protect(&made.to_interface());
        let root: IRawElementProviderSimple = made.to_interface();
        if let Ok(mut nodes) = shared.nodes.lock() {
            *nodes = Some(CachedNodes {
                root: root.clone(),
                document: None,
            });
        }
        Self {
            shared,
            root,
            tracker: RefCell::default(),
            last_focus: Cell::new(None),
            last_shape: Cell::new(None),
            last_states: RefCell::new(None),
            last_document_name: RefCell::new(None),
            closed: Cell::new(false),
        }
    }
    /// The answer to WM_GETOBJECT for UIA's tree.
    pub fn respond(&self, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &self.root) }
    }
    pub fn root(&self) -> IRawElementProviderSimple {
        self.root.clone()
    }
    /// The one document object. An error when the window has closed.
    pub fn document(&self) -> Result<IRawElementProviderSimple> {
        document_simple(&self.shared)
    }
    /// How many objects still share this window's state, for tests of what UIA lets go of.
    #[doc(hidden)]
    pub fn live_objects(&self) -> usize {
        Arc::strong_count(&self.shared)
    }
    /// With no client listening, this makes one system call and does nothing else.
    pub fn after_input(&self) -> Result<EventFlags> {
        let listening = unsafe { UiaClientsAreListening() }.as_bool();
        self.after_input_when(listening)
    }
    /// `after_input` with the answer to "is a client listening" given by the caller.
    pub fn after_input_when(&self, listening: bool) -> Result<EventFlags> {
        if !listening {
            self.tracker.borrow_mut().no_client();
            return Ok(EventFlags::default());
        }
        let host = self.shared.ui.host_here()?;
        let Some(view_id) = host.view_id() else {
            return Ok(EventFlags::default());
        };
        let mut flags = EventFlags::default();
        let reached = host.with_editor(&mut |view, _| {
            flags = self.tracker.borrow_mut().poll(view_id, view);
        });
        if !reached {
            return Ok(EventFlags::default());
        }
        if flags == EventFlags::default() {
            return Ok(flags);
        }
        let answer = self.raise_input(flags);
        if answer.is_err() {
            // The next batch then speaks for this one, so a client does not miss the change.
            self.tracker.borrow_mut().no_client();
        }
        answer.map(|()| flags)
    }
    fn raise_input(&self, flags: EventFlags) -> Result<()> {
        let document = self.document()?;
        if flags.text_changed {
            raise_event(&document, UIA_Text_TextChangedEventId)?;
        }
        // UIA has no event for the caret alone. A caret move is a selection change.
        if flags.selection_changed || flags.caret_moved {
            raise_event(&document, UIA_Text_TextSelectionChangedEventId)?;
        }
        Ok(())
    }
    /// Raises a notification that a screen reader says at once.
    pub fn announce(&self, text: &str) -> Result<()> {
        if !unsafe { UiaClientsAreListening() }.as_bool() {
            return Ok(());
        }
        let root = self.root();
        check_raise()?;
        unsafe {
            UiaRaiseNotificationEvent(
                &root,
                NotificationKind_ActionCompleted,
                NotificationProcessing_ImportantMostRecent,
                &BSTR::from(text),
                &BSTR::from("qe-announcement"),
            )
        }
    }
    /// It raises a focus event if focus moved, a structure event if the shape of the tree
    /// changed, and an event for each change a client can read in a widget.
    pub fn widgets_changed(&self) -> Result<WidgetEvents> {
        let listening = unsafe { UiaClientsAreListening() }.as_bool();
        self.widgets_changed_when(listening)
    }
    /// `widgets_changed` with the answer to "is a client listening" given by the caller.
    pub fn widgets_changed_when(&self, listening: bool) -> Result<WidgetEvents> {
        if !listening {
            self.last_focus.set(None);
            self.last_shape.set(None);
            self.last_states.replace(None);
            self.last_document_name.replace(None);
            return Ok(WidgetEvents::default());
        }
        let host = self.shared.ui.host_here()?;
        let root = host.widgets().ok_or_else(gone)?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        shape_of(&root, &mut hasher);
        let shape = std::hash::Hasher::finish(&hasher);
        let now = match focused_child(&root) {
            Some(id) => Focus::Widget(id),
            None if host.document_widget().is_some_and(|d| d.states.focused) => Focus::Document,
            None => Focus::Nowhere,
        };
        let mut states = HashMap::new();
        remember(&root, &mut states);
        let document_name = host.document_widget().map(|d| d.name);
        let mut raised = WidgetEvents::default();
        // Nothing is saved until every event is out. If one fails, the error shows now and the
        // next report raises the lot again. A client can take an event twice, but not none.
        self.raise_widgets(shape, now, &states, document_name.as_ref(), &mut raised)?;
        self.last_shape.set(Some(shape));
        self.last_focus.set(Some(now));
        self.last_states.replace(Some(states));
        self.last_document_name.replace(document_name);
        Ok(raised)
    }
    fn raise_widgets(
        &self,
        shape: u64,
        now: Focus,
        states: &HashMap<WidgetId, SeenWidget>,
        document_name: Option<&String>,
        raised: &mut WidgetEvents,
    ) -> Result<()> {
        if self.last_shape.get() != Some(shape) {
            check_raise()?;
            unsafe {
                UiaRaiseStructureChangedEvent(
                    &self.root,
                    StructureChangeType_ChildrenInvalidated,
                    std::ptr::null_mut(),
                    0,
                )
            }?;
            raised.structure = true;
        }
        if self.last_focus.get() != Some(now) {
            match now {
                Focus::Widget(id) => {
                    let node: IRawElementProviderSimple =
                        make_node(&self.shared, Target::Widget(id))?.cast()?;
                    raise_event(&node, UIA_AutomationFocusChangedEventId)?;
                    raised.focus = true;
                }
                Focus::Document => {
                    let document = self.document()?;
                    raise_event(&document, UIA_AutomationFocusChangedEventId)?;
                    raised.focus = true;
                }
                Focus::Nowhere => {}
            }
        }
        // Taken for the raises, because a raise can take a sent message that reaches this object.
        let before = self.last_states.take();
        let answer: Result<()> = (|| {
            for (id, new) in states {
                if let Some(old) = before.as_ref().and_then(|map| map.get(id))
                    && old != new
                {
                    let node: IRawElementProviderSimple =
                        make_node(&self.shared, Target::Widget(*id))?.cast()?;
                    raise_changes(&node, old, new, raised)?;
                }
            }
            Ok(())
        })();
        self.last_states.replace(before);
        answer?;
        let last_name = self.last_document_name.borrow().clone();
        if let (Some(old), Some(new)) = (last_name, document_name)
            && old != *new
        {
            raise_property(
                &self.document()?,
                UIA_NamePropertyId,
                VARIANT::from(old.as_str()),
                VARIANT::from(new.as_str()),
                raised,
            )?;
        }
        Ok(())
    }
    /// Call on the window's thread. After this, every object a client holds says
    /// UIA_E_ELEMENTNOTAVAILABLE. A second call does nothing, unless the first one failed: it
    /// then tries again, with what it needs still in place.
    pub fn shutdown(&self) -> Result<()> {
        self.close().map(drop)
    }
    /// `shutdown`, which gives back the host. The caller ends it when it no longer needs
    /// anything the host could be keeping alive.
    fn close(&self) -> Result<Option<Arc<dyn UiaHost>>> {
        if self.closed.get() {
            return Ok(None);
        }
        // Calls from other threads now fail at once, so nothing here waits for them.
        self.shared.ui.begin_close();
        // UIA refuses to disconnect while a sender is waiting for this thread. A window can be
        // closed from inside a sent message, so this lets the sender go first. A call of ours
        // that is waiting then hears "gone", and any other sender hears 0.
        if !SKIP_REPLY_FOR_TESTS.load(Ordering::Relaxed) && unsafe { InSendMessage() }.as_bool() {
            let _ = unsafe { ReplyMessage(LRESULT(0)) };
        }
        let document = self
            .shared
            .nodes
            .lock()
            .map_err(|_| gone())?
            .as_ref()
            .and_then(|nodes| nodes.document.clone());
        // UIA asks these objects for their IDs while it disconnects them, on this thread, so
        // they must still answer here. This also tells UIA the window has no tree now, as its
        // docs ask at WM_DESTROY.
        let window = HWND(self.shared.window as *mut c_void);
        unsafe {
            UiaReturnRawElementProvider(
                window,
                WPARAM(0),
                LPARAM(0),
                None::<&IRawElementProviderSimple>,
            )
        };
        if let Some(document) = document {
            // "No longer available" means UIA holds no such element now, which is this goal.
            match unsafe { UiaDisconnectProvider(&document) } {
                Err(error) if error.code() != crate::com::gone().code() => {
                    return Err(error);
                }
                _ => {}
            }
        }
        self.closed.set(true);
        Ok(self.finish())
    }
    /// Lets go of the cached objects, and gives back the host.
    fn finish(&self) -> Option<Arc<dyn UiaHost>> {
        if let Ok(mut nodes) = self.shared.nodes.lock() {
            nodes.take();
        }
        self.shared.ui.finish_close()
    }
}

/// Lets a test reach the failure that a shutdown inside a sent message would have.
#[doc(hidden)]
pub static SKIP_REPLY_FOR_TESTS: AtomicBool = AtomicBool::new(false);

impl Drop for Accessibility {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("UI Automation did not close cleanly: {error}");
            self.closed.set(true);
            drop(self.finish());
        }
    }
}
