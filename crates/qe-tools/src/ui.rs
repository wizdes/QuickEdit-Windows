//! The painted find bar and Select Line panel. `ToolsHost` owns them.
//!
//! The Mac's bar is AppKit's `NSTextFinder`, so only some wording shows in its source and its
//! UI tests: `All`, `Replace`, and `Ignore Case` on by default. The rest comes from how
//! that bar looks on the Mac: `Done`, the two arrows, the options list and the count.
use crate::lines::{
    LINE_PLACEHOLDER, LINE_TOOLTIP, SELECT_LINE_BUTTON, SELECT_LINE_FIELD, SELECT_LINE_SIZE,
};
use qe_api::{
    Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Point, Rect, TextMeasurer,
    Theme, View, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};
use qe_ui::{Button, Control, Label, LabelKind, SharedClipboard, TextField, UiTokens};

pub const BAR_ROW_HEIGHT: f32 = 32.0;
const FIELD_HEIGHT: f32 = 24.0;
const BUTTON_HEIGHT: f32 = 26.0;
const GAP: f32 = 6.0;
const COUNT_WIDTH: f32 = 110.0;
const ARROW_WIDTH: f32 = 28.0;
const OPTIONS_WIDTH: f32 = 66.0;
const DONE_WIDTH: f32 = 58.0;
const REPLACE_WIDTH: f32 = 70.0;
const ALL_WIDTH: f32 = 50.0;
const MIN_FIELD_WIDTH: f32 = 80.0;
const PANEL_TITLE_HEIGHT: f32 = 26.0;

const ID_BAR: u64 = 12100;

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(width),
        height: Dip(height),
    }
}
pub(crate) fn contains(r: Rect, at: Point) -> bool {
    at.x.0 >= r.x.0 && at.y.0 >= r.y.0 && at.x.0 < r.x.0 + r.width.0 && at.y.0 < r.y.0 + r.height.0
}
fn event(kind: InputKind) -> InputEvent {
    InputEvent { time_us: 0, kind }
}
pub(crate) fn mouse_point(kind: &InputKind) -> Option<Point> {
    match kind {
        InputKind::MouseMove { at }
        | InputKind::MouseDown { at, .. }
        | InputKind::MouseUp { at, .. }
        | InputKind::Wheel { at, .. } => Some(*at),
        _ => None,
    }
}

/// What the user asked of the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarAction {
    None,
    QueryChanged,
    Next,
    Previous,
    Replace,
    ReplaceAll,
    Close,
    Options,
}

/// The stops of the bar, in Tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarStop {
    Find,
    Previous,
    Next,
    Options,
    Done,
    Replace,
    ReplaceOne,
    ReplaceAll,
}
const STOPS: [BarStop; 8] = [
    BarStop::Find,
    BarStop::Previous,
    BarStop::Next,
    BarStop::Options,
    BarStop::Done,
    BarStop::Replace,
    BarStop::ReplaceOne,
    BarStop::ReplaceAll,
];

/// The find bar: a row for finding and, in Find and Replace, a row for replacing.
pub struct FindBar {
    pub find: TextField,
    pub replace: TextField,
    pub count: Label,
    pub previous: Button,
    pub next: Button,
    pub options: Button,
    pub done: Button,
    pub replace_one: Button,
    pub replace_all: Button,
    pub open: bool,
    pub replacing: bool,
    pub bounds: Rect,
    pub focus: Option<BarStop>,
}
impl FindBar {
    pub fn new(clipboard: SharedClipboard) -> Self {
        let id = |n: u64| WidgetId(ID_BAR + n);
        let mut find = TextField::new(id(1), "Find").with_clipboard(clipboard.clone());
        find.placeholder = "Find".to_string();
        let mut previous = Button::new(id(4), "Previous match");
        previous.icon = Some("E76B".to_string());
        let mut next = Button::new(id(5), "Next match");
        next.icon = Some("E76C".to_string());
        let mut replace = TextField::new(id(2), "Replace").with_clipboard(clipboard);
        replace.placeholder = "Replace".to_string();
        Self {
            find,
            replace,
            count: Label::new(id(3), "", LabelKind::Hint),
            previous,
            next,
            options: Button::new(id(6), "Options"),
            done: Button::new(id(7), "Done"),
            replace_one: Button::new(id(8), "Replace"),
            replace_all: Button::new(id(9), "All"),
            open: false,
            replacing: false,
            bounds: Rect::default(),
            focus: None,
        }
    }
    /// The height the bar takes above the text.
    pub fn height(&self) -> f32 {
        match (self.open, self.replacing) {
            (false, _) => 0.0,
            (true, false) => BAR_ROW_HEIGHT,
            (true, true) => BAR_ROW_HEIGHT * 2.0,
        }
    }
    pub(crate) fn control(&mut self, stop: BarStop) -> &mut dyn Control {
        match stop {
            BarStop::Find => &mut self.find,
            BarStop::Previous => &mut self.previous,
            BarStop::Next => &mut self.next,
            BarStop::Options => &mut self.options,
            BarStop::Done => &mut self.done,
            BarStop::Replace => &mut self.replace,
            BarStop::ReplaceOne => &mut self.replace_one,
            BarStop::ReplaceAll => &mut self.replace_all,
        }
    }
    fn stop_shown(&self, stop: BarStop) -> bool {
        match stop {
            BarStop::Replace => self.replacing && self.replace.enabled,
            BarStop::ReplaceOne => self.replacing && self.replace_one.enabled,
            BarStop::ReplaceAll => self.replacing && self.replace_all.enabled,
            _ => true,
        }
    }
    /// Moves the keyboard to `stop`, or out of the bar with `None`.
    pub fn set_focus(&mut self, stop: Option<BarStop>, ctx: &mut ViewContext) {
        if self.focus == stop {
            return;
        }
        if let Some(old) = self.focus {
            self.control(old).handle(&event(InputKind::Blur), ctx);
        }
        self.focus = stop;
        if let Some(new) = stop {
            self.control(new).handle(&event(InputKind::Focus), ctx);
        }
    }
    fn cycle(&mut self, back: bool, ctx: &mut ViewContext) {
        let Some(at) = self
            .focus
            .and_then(|focus| STOPS.iter().position(|s| *s == focus))
        else {
            return;
        };
        let n = STOPS.len();
        for step in 1..n {
            let i = if back {
                (at + n - step) % n
            } else {
                (at + step) % n
            };
            if self.stop_shown(STOPS[i]) {
                self.set_focus(Some(STOPS[i]), ctx);
                return;
            }
        }
    }
    fn take_action(&mut self) -> BarAction {
        let query = self.find.take_activated();
        self.replace.take_activated();
        if self.previous.take_activated() {
            BarAction::Previous
        } else if self.next.take_activated() {
            BarAction::Next
        } else if self.options.take_activated() {
            BarAction::Options
        } else if self.done.take_activated() {
            BarAction::Close
        } else if self.replace_one.take_activated() {
            BarAction::Replace
        } else if self.replace_all.take_activated() {
            BarAction::ReplaceAll
        } else if query {
            BarAction::QueryChanged
        } else {
            BarAction::None
        }
    }
    pub(crate) fn handle_bar(
        &mut self,
        e: &InputEvent,
        ctx: &mut ViewContext,
    ) -> (Handled, BarAction) {
        if !self.open {
            return (Handled::No, BarAction::None);
        }
        if let Some(at) = mouse_point(&e.kind) {
            let inside = contains(self.bounds, at);
            if let InputKind::MouseDown { .. } = e.kind {
                if inside {
                    let mut hit = None;
                    for stop in STOPS {
                        if self.stop_shown(stop)
                            && self
                                .control(stop)
                                .hit_bounds()
                                .is_some_and(|bounds| contains(bounds, at))
                        {
                            hit = Some(stop);
                            break;
                        }
                    }
                    if let Some(stop) = hit {
                        self.set_focus(Some(stop), ctx);
                    }
                } else {
                    self.set_focus(None, ctx);
                }
            }
            // Every control sees the mouse, so hover clears and a press ends where it began.
            let mut handled = inside;
            for stop in STOPS {
                if self.stop_shown(stop) && self.control(stop).handle(e, ctx) == Handled::Yes {
                    handled = true;
                }
            }
            let action = self.take_action();
            return (if handled { Handled::Yes } else { Handled::No }, action);
        }
        let Some(focus) = self.focus else {
            return (Handled::No, BarAction::None);
        };
        if let InputKind::KeyDown {
            virtual_key,
            modifiers,
            ..
        } = &e.kind
        {
            let composing = self.control(focus).uses_key(*virtual_key);
            if !composing && !modifiers.alt && !modifiers.ctrl {
                match *virtual_key {
                    0x09 => {
                        self.cycle(modifiers.shift, ctx);
                        return (Handled::Yes, BarAction::None);
                    }
                    0x1b => return (Handled::Yes, BarAction::Close),
                    0x0d if focus == BarStop::Find => {
                        let action = if modifiers.shift {
                            BarAction::Previous
                        } else {
                            BarAction::Next
                        };
                        return (Handled::Yes, action);
                    }
                    0x0d if focus == BarStop::Replace => {
                        return (Handled::Yes, BarAction::Replace);
                    }
                    _ => {}
                }
            }
        }
        let handled = self.control(focus).handle(e, ctx);
        (handled, self.take_action())
    }
    pub fn set_count(&mut self, text: impl Into<String>, warning: bool) {
        self.count.text = text.into();
        self.count.kind = if warning {
            LabelKind::Warning
        } else {
            LabelKind::Hint
        };
    }
    /// Puts the controls in place inside the bar.
    pub fn place(&mut self, bounds: Rect, m: &dyn TextMeasurer) {
        self.bounds = bounds;
        let (x, y, width) = (bounds.x.0, bounds.y.0, bounds.width.0);
        let field_y = y + (BAR_ROW_HEIGHT - FIELD_HEIGHT) / 2.0;
        let button_y = y + (BAR_ROW_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let right = COUNT_WIDTH + ARROW_WIDTH * 2.0 + OPTIONS_WIDTH + DONE_WIDTH + GAP * 6.0;
        let field_width = (width - right - GAP).max(MIN_FIELD_WIDTH);
        let mut at = x + GAP;
        self.find
            .layout(rect(at, field_y, field_width, FIELD_HEIGHT), m);
        at += field_width + GAP;
        self.count
            .layout(rect(at, field_y, COUNT_WIDTH, FIELD_HEIGHT), m);
        at += COUNT_WIDTH + GAP;
        self.previous
            .layout(rect(at, button_y, ARROW_WIDTH, BUTTON_HEIGHT), m);
        at += ARROW_WIDTH + GAP;
        self.next
            .layout(rect(at, button_y, ARROW_WIDTH, BUTTON_HEIGHT), m);
        at += ARROW_WIDTH + GAP;
        self.options
            .layout(rect(at, button_y, OPTIONS_WIDTH, BUTTON_HEIGHT), m);
        at += OPTIONS_WIDTH + GAP;
        self.done
            .layout(rect(at, button_y, DONE_WIDTH, BUTTON_HEIGHT), m);
        if self.replacing {
            let row = BAR_ROW_HEIGHT;
            self.replace
                .layout(rect(x + GAP, field_y + row, field_width, FIELD_HEIGHT), m);
            let mut at = x + GAP + field_width + GAP;
            self.replace_one
                .layout(rect(at, button_y + row, REPLACE_WIDTH, BUTTON_HEIGHT), m);
            at += REPLACE_WIDTH + GAP;
            self.replace_all
                .layout(rect(at, button_y + row, ALL_WIDTH, BUTTON_HEIGHT), m);
        }
    }
    /// The options button's place, for its list.
    pub fn options_bounds(&self) -> Rect {
        self.options.bounds
    }
    pub fn draw_bar(&self, list: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        if !self.open {
            return;
        }
        let t = UiTokens::from_theme(theme);
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: t.background,
        });
        let bottom = self.bounds.y.0 + self.bounds.height.0 - 0.5;
        list.push(DrawCommand::Line {
            from: Point {
                x: self.bounds.x,
                y: Dip(bottom),
            },
            to: Point {
                x: Dip(self.bounds.x.0 + self.bounds.width.0),
                y: Dip(bottom),
            },
            width: Dip(1.0),
            color: t.border,
        });
        self.find.draw(list, theme, m);
        self.count.draw(list, theme, m);
        self.previous.draw(list, theme, m);
        self.next.draw(list, theme, m);
        self.options.draw(list, theme, m);
        self.done.draw(list, theme, m);
        if self.replacing {
            self.replace.draw(list, theme, m);
            self.replace_one.draw(list, theme, m);
            self.replace_all.draw(list, theme, m);
        }
    }
    /// What a screen reader sees, or `None` while closed.
    pub fn widget(&self) -> Option<Widget> {
        if !self.open {
            return None;
        }
        let mut children: Vec<Widget> = [
            self.find.widgets(),
            self.count.widgets(),
            self.previous.widgets().map(|w| named(w, "Previous match")),
            self.next.widgets().map(|w| named(w, "Next match")),
            self.options.widgets(),
            self.done.widgets(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if self.replacing {
            children.extend(
                [
                    self.replace.widgets(),
                    self.replace_one.widgets(),
                    self.replace_all.widgets().map(|w| named(w, "Replace All")),
                ]
                .into_iter()
                .flatten(),
            );
        }
        Some(group(
            WidgetId(ID_BAR),
            self.bounds,
            WidgetRole::Group,
            "Find",
            None,
            children,
        ))
    }
}

fn named(mut widget: Widget, name: &str) -> Widget {
    widget.name = name.to_string();
    widget
}
fn group(
    id: WidgetId,
    bounds: Rect,
    role: WidgetRole,
    name: &str,
    description: Option<String>,
    children: Vec<Widget>,
) -> Widget {
    Widget {
        id,
        bounds,
        role,
        name: name.to_string(),
        value: None,
        states: WidgetStates {
            enabled: true,
            ..WidgetStates::default()
        },
        selection: None,
        range_value: None,
        description,
        children,
    }
}

/// Which control of the panel has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelStop {
    Field,
    Select,
    Close,
}

/// The Select Line panel. Its content is 253 by 62, with the Mac's field and button in their
/// places, under a title row with a close button, as the Mac's window has. It is not modal:
/// it stays open while the keyboard goes back to the text.
pub struct SelectLinePanel {
    pub field: TextField,
    pub select: Button,
    pub close: Button,
    pub title: Label,
    pub open: bool,
    /// The 253 by 62 content.
    pub bounds: Rect,
    /// The content and the title row.
    pub frame: Rect,
    pub keyboard: Option<PanelStop>,
}
impl SelectLinePanel {
    pub fn new(clipboard: SharedClipboard) -> Self {
        let mut field = TextField::new(WidgetId(12001), "line number").with_clipboard(clipboard);
        field.placeholder = LINE_PLACEHOLDER.to_string();
        let mut select = Button::new(WidgetId(12002), "Select");
        select.is_default = true;
        let mut close = Button::new(WidgetId(12003), "Close");
        close.icon = Some("E711".to_string());
        Self {
            field,
            select,
            close,
            title: Label::new(WidgetId(12004), "Select Line", LabelKind::Plain),
            open: false,
            bounds: Rect::default(),
            frame: Rect::default(),
            keyboard: None,
        }
    }
    /// Moves the keyboard to a control, or out of the panel with `None`. Only one control
    /// has it, so only one reports focus.
    pub fn set_keyboard(&mut self, stop: Option<PanelStop>, ctx: &mut ViewContext) {
        if self.keyboard == stop {
            return;
        }
        if let Some(old) = self.keyboard {
            self.control(old).handle(&event(InputKind::Blur), ctx);
        }
        self.keyboard = stop;
        if let Some(new) = stop {
            self.control(new).handle(&event(InputKind::Focus), ctx);
        }
    }
    pub(crate) fn control(&mut self, stop: PanelStop) -> &mut dyn Control {
        match stop {
            PanelStop::Field => &mut self.field,
            PanelStop::Select => &mut self.select,
            PanelStop::Close => &mut self.close,
        }
    }
    /// Shows the panel with the keyboard in its field and the old entry selected.
    pub fn show(&mut self, ctx: &mut ViewContext) {
        self.open = true;
        self.set_keyboard(Some(PanelStop::Field), ctx);
        self.field.anchor = 0;
        self.field.caret = self.field.text.len();
    }
    pub fn hide(&mut self, ctx: &mut ViewContext) {
        self.set_keyboard(None, ctx);
        self.open = false;
    }
    pub fn place(&mut self, area: Rect, m: &dyn TextMeasurer) {
        let (width, height) = SELECT_LINE_SIZE;
        let total = height + PANEL_TITLE_HEIGHT;
        let x = area.x.0 + ((area.width.0 - width) / 2.0).max(0.0);
        let top = area.y.0 + 24.0_f32.min((area.height.0 - total).max(0.0));
        let y = top + PANEL_TITLE_HEIGHT;
        self.frame = rect(x, top, width, total);
        self.bounds = rect(x, y, width, height);
        self.title
            .layout(rect(x + 12.0, top, width - 48.0, PANEL_TITLE_HEIGHT), m);
        self.close.layout(
            rect(
                x + width - PANEL_TITLE_HEIGHT - 2.0,
                top + 2.0,
                PANEL_TITLE_HEIGHT,
                PANEL_TITLE_HEIGHT - 2.0,
            ),
            m,
        );
        let (fx, fy, fw, fh) = SELECT_LINE_FIELD;
        self.field.layout(rect(x + fx, y + fy, fw, fh), m);
        let (bx, by, bw, bh) = SELECT_LINE_BUTTON;
        self.select.layout(rect(x + bx, y + by, bw, bh), m);
    }
    pub fn draw_panel(&self, list: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        if !self.open {
            return;
        }
        let t = UiTokens::from_theme(theme);
        list.push(DrawCommand::RoundedRect {
            rect: self.frame,
            radius: t.popup_radius,
            color: t.surface,
        });
        list.push(DrawCommand::StrokeRect {
            rect: self.frame,
            radius: t.popup_radius,
            width: Dip(1.0),
            color: t.popup_border,
        });
        self.title.draw(list, theme, m);
        self.close.draw(list, theme, m);
        self.field.draw(list, theme, m);
        self.select.draw(list, theme, m);
    }
    pub fn widget(&self) -> Option<Widget> {
        if !self.open {
            return None;
        }
        let children = [
            self.close.widgets(),
            self.field.widgets(),
            self.select.widgets(),
        ]
        .into_iter()
        .flatten()
        .collect();
        Some(group(
            WidgetId(12000),
            self.frame,
            WidgetRole::Dialog,
            "Select Line",
            Some(LINE_TOOLTIP.to_string()),
            children,
        ))
    }
}
