use crate::Rect;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetRole {
    Button,
    Checkbox,
    Tab,
    TabList,
    Menu,
    MenuBar,
    MenuItem,
    TextField,
    Table,
    Row,
    Cell,
    Dialog,
    Group,
    Label,
    ComboBox,
    ScrollBar,
    List,
    ListItem,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WidgetStates {
    pub enabled: bool,
    pub focused: bool,
    pub selected: bool,
    pub checked: bool,
    pub expanded: bool,
    pub read_only: bool,
    /// Opening it shows a popup list.
    pub has_popup: bool,
    /// It heads a section of a list or a dialog.
    pub section: bool,
}

/// The value of a scroll bar or another control with a range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidgetRange {
    pub minimum: f64,
    pub maximum: f64,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub id: WidgetId,
    pub bounds: Rect,
    pub role: WidgetRole,
    pub name: String,
    pub value: Option<String>,
    pub states: WidgetStates,
    /// The selected text, in UTF-16 units of `value` or of the text the widget shows.
    pub selection: Option<Range<usize>>,
    pub range_value: Option<WidgetRange>,
    /// Longer help than `name`, for a screen reader.
    pub description: Option<String>,
    pub children: Vec<Widget>,
}
