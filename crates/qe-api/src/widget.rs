use crate::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetRole {
    Button,
    Checkbox,
    Tab,
    TabList,
    Menu,
    MenuItem,
    TextField,
    Table,
    Row,
    Cell,
    Dialog,
    Group,
    Label,
    ComboBox,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WidgetStates {
    pub enabled: bool,
    pub focused: bool,
    pub selected: bool,
    pub checked: bool,
    pub expanded: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub id: WidgetId,
    pub bounds: Rect,
    pub role: WidgetRole,
    pub name: String,
    pub value: Option<String>,
    pub states: WidgetStates,
    pub children: Vec<Widget>,
}
