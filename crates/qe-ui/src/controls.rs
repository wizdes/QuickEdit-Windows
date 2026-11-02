use crate::UiTokens;
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers, MouseButton,
    Point, Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole,
    WidgetStates,
};

const LEFT: u16 = 0x25;
const UP: u16 = 0x26;
const RIGHT: u16 = 0x27;
const DOWN: u16 = 0x28;
const ENTER: u16 = 0x0d;
const SPACE: u16 = 0x20;
const HOME: u16 = 0x24;
const END: u16 = 0x23;
const BACK: u16 = 0x08;
const DELETE: u16 = 0x2e;

fn contains(r: Rect, p: Point) -> bool {
    p.x.0 >= r.x.0 && p.y.0 >= r.y.0 && p.x.0 < r.x.0 + r.width.0 && p.y.0 < r.y.0 + r.height.0
}
fn text(list: &mut DisplayList, at: Point, value: &str, color: Color, t: UiTokens) {
    list.push(DrawCommand::TextRun {
        at,
        text: value.into(),
        font: t.font.into(),
        color,
    });
}
fn inset(r: Rect, x: f32, y: f32) -> Rect {
    Rect {
        x: Dip(r.x.0 + x),
        y: Dip(r.y.0 + y),
        width: Dip((r.width.0 - 2.0 * x).max(0.0)),
        height: Dip((r.height.0 - 2.0 * y).max(0.0)),
    }
}
fn node(
    id: WidgetId,
    bounds: Rect,
    role: WidgetRole,
    name: &str,
    value: Option<String>,
    states: WidgetStates,
    children: Vec<Widget>,
) -> Widget {
    Widget {
        id,
        bounds,
        role,
        name: name.into(),
        value,
        states,
        children,
    }
}
fn state(enabled: bool, focused: bool, selected: bool, checked: bool) -> WidgetStates {
    WidgetStates {
        enabled,
        focused,
        selected,
        checked,
        ..Default::default()
    }
}
fn invalidate(ctx: &mut ViewContext, r: Rect) {
    ctx.invalidate(r);
}
fn focus_ring(list: &mut DisplayList, r: Rect, t: UiTokens) {
    list.push(DrawCommand::StrokeRect {
        rect: inset(r, 1.0, 1.0),
        radius: t.radius,
        width: t.focus_width,
        color: t.focus,
    });
}

#[derive(Clone, Debug)]
pub struct Button {
    pub id: WidgetId,
    pub label: String,
    pub bounds: Rect,
    pub enabled: bool,
    pub focused: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub is_default: bool,
    pub icon: Option<String>,
    pub clicks: usize,
}
impl Button {
    pub fn new(id: WidgetId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            bounds: Rect::default(),
            enabled: true,
            focused: false,
            hovered: false,
            pressed: false,
            is_default: false,
            icon: None,
            clicks: 0,
        }
    }
    fn activate(&mut self, ctx: &mut ViewContext) {
        self.clicks += 1;
        invalidate(ctx, self.bounds);
    }
}
impl View for Button {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.pressed = false;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                self.hovered = contains(self.bounds, *at);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if self.enabled && contains(self.bounds, *at) => {
                self.pressed = true;
                c.capture_mouse(true);
                c.set_focus(self.id);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } if self.pressed => {
                self.pressed = false;
                c.capture_mouse(false);
                if contains(self.bounds, *at) {
                    self.activate(c);
                }
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown { virtual_key, .. }
                if self.enabled
                    && ((*virtual_key == SPACE && self.focused)
                        || (*virtual_key == ENTER && (self.focused || self.is_default))) =>
            {
                self.pressed = true;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyUp { virtual_key, .. }
                if self.enabled
                    && self.pressed
                    && (*virtual_key == SPACE || *virtual_key == ENTER) =>
            {
                self.pressed = false;
                self.activate(c);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let fill = if !self.enabled {
            t.background
        } else if self.pressed {
            t.pressed
        } else if self.hovered {
            t.hover
        } else {
            t.surface
        };
        l.push(DrawCommand::RoundedRect {
            rect: self.bounds,
            radius: t.radius,
            color: fill,
        });
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: if self.is_default { t.accent } else { t.border },
        });
        if let Some(icon) = &self.icon {
            l.push(DrawCommand::Icon {
                at: inset(self.bounds, t.inset.0, t.inset.0),
                icon_id: icon.clone(),
                color: if self.enabled { t.text } else { t.disabled },
            });
        } else {
            text(
                l,
                Point {
                    x: Dip(self.bounds.x.0 + t.inset.0),
                    y: Dip(self.bounds.y.0 + t.inset.0),
                },
                &self.label,
                if self.enabled { t.text } else { t.disabled },
                t,
            );
        }
        if self.focused {
            focus_ring(l, self.bounds, t);
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Button,
            &self.label,
            None,
            state(self.enabled, self.focused, false, false),
            vec![],
        ))
    }
}

#[derive(Clone, Debug)]
pub struct Checkbox {
    pub id: WidgetId,
    pub label: String,
    pub bounds: Rect,
    pub enabled: bool,
    pub checked: bool,
    pub focused: bool,
    pub hovered: bool,
    pub pressed: bool,
}
impl Checkbox {
    pub fn new(id: WidgetId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            bounds: Rect::default(),
            enabled: true,
            checked: false,
            focused: false,
            hovered: false,
            pressed: false,
        }
    }
}
impl View for Checkbox {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.pressed = false;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                self.hovered = contains(self.bounds, *at);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if self.enabled && contains(self.bounds, *at) => {
                self.pressed = true;
                c.set_focus(self.id);
                Handled::Yes
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } if self.pressed => {
                self.pressed = false;
                if contains(self.bounds, *at) {
                    self.checked = !self.checked;
                }
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: SPACE, ..
            } if self.enabled && self.focused => {
                self.checked = !self.checked;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let box_r = Rect {
            x: self.bounds.x,
            y: Dip(self.bounds.y.0 + (self.bounds.height.0 - t.check_size.0) / 2.0),
            width: t.check_size,
            height: t.check_size,
        };
        l.push(DrawCommand::RoundedRect {
            rect: box_r,
            radius: t.radius,
            color: if self.checked {
                t.accent
            } else if self.pressed {
                t.pressed
            } else if self.hovered {
                t.hover
            } else {
                t.surface
            },
        });
        l.push(DrawCommand::StrokeRect {
            rect: box_r,
            radius: t.radius,
            width: Dip(1.0),
            color: t.border,
        });
        if self.checked {
            text(
                l,
                Point {
                    x: Dip(box_r.x.0 + 2.0),
                    y: box_r.y,
                },
                "✓",
                t.background,
                t,
            );
        }
        text(
            l,
            Point {
                x: Dip(self.bounds.x.0 + t.check_size.0 + t.spacing.0),
                y: Dip(self.bounds.y.0 + t.inset.0),
            },
            &self.label,
            if self.enabled { t.text } else { t.disabled },
            t,
        );
        if self.focused {
            focus_ring(l, box_r, t)
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Checkbox,
            &self.label,
            None,
            state(self.enabled, self.focused, false, self.checked),
            vec![],
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelKind {
    Plain,
    Group,
    Hint,
    Warning,
}
#[derive(Clone, Debug)]
pub struct Label {
    pub id: WidgetId,
    pub text: String,
    pub kind: LabelKind,
    pub bounds: Rect,
}
impl Label {
    pub fn new(id: WidgetId, text: impl Into<String>, kind: LabelKind) -> Self {
        Self {
            id,
            text: text.into(),
            kind,
            bounds: Rect::default(),
        }
    }
}
impl View for Label {
    fn handle(&mut self, _: &InputEvent, _: &mut ViewContext) -> Handled {
        Handled::No
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let color = match self.kind {
            LabelKind::Plain => t.text,
            LabelKind::Group => theme.group_header,
            LabelKind::Hint => theme.dim_text,
            LabelKind::Warning => t.warning,
        };
        text(
            l,
            Point {
                x: self.bounds.x,
                y: self.bounds.y,
            },
            &self.text,
            color,
            t,
        )
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Label,
            &self.text,
            None,
            state(true, false, false, false),
            vec![],
        ))
    }
}

pub type NumericValidator = fn(&str) -> bool;
#[derive(Clone, Debug)]
pub struct TextField {
    pub id: WidgetId,
    pub name: String,
    pub text: String,
    pub placeholder: String,
    pub bounds: Rect,
    pub enabled: bool,
    pub focused: bool,
    pub caret: usize,
    pub anchor: usize,
    pub composition: Option<(String, usize)>,
    pub validator: Option<NumericValidator>,
    undo: Vec<String>,
    pub clipboard: String,
}
impl TextField {
    pub fn new(id: WidgetId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            text: String::new(),
            placeholder: String::new(),
            bounds: Rect::default(),
            enabled: true,
            focused: false,
            caret: 0,
            anchor: 0,
            composition: None,
            validator: None,
            undo: vec![],
            clipboard: String::new(),
        }
    }
    fn selected(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
    fn replace_selection(&mut self, s: &str) {
        let (a, b) = self.selected();
        let mut next = self.text.clone();
        next.replace_range(a..b, s);
        if self.validator.is_some_and(|v| !v(&next)) {
            return;
        }
        self.undo.push(self.text.clone());
        self.text = next;
        self.caret = a + s.len();
        self.anchor = self.caret;
    }
    fn prev(&self) -> usize {
        let s = &self.text[..self.caret];
        let mut i = s.char_indices().last().map_or(0, |(i, _)| i);
        while i > 0 {
            let current = self.text[i..].chars().next().unwrap_or('\0');
            let (before, previous) = self.text[..i].char_indices().last().unwrap_or((0, '\0'));
            if matches!(current, '\u{0300}'..='\u{036f}' | '\u{fe00}'..='\u{fe0f}' | '\u{200d}')
                || previous == '\u{200d}'
            {
                i = before;
            } else {
                break;
            }
        }
        i
    }
    fn next(&self) -> usize {
        let s = &self.text[self.caret..];
        let mut iter = s.char_indices();
        iter.next();
        let mut end = iter.next().map_or(s.len(), |(i, _)| i);
        while end < s.len() {
            let ch = s[end..].chars().next().unwrap_or('\0');
            if matches!(ch, '\u{0300}'..='\u{036f}' | '\u{fe00}'..='\u{fe0f}') {
                end += ch.len_utf8();
            } else if ch == '\u{200d}' {
                end += ch.len_utf8();
                if let Some(joined) = s[end..].chars().next() {
                    end += joined.len_utf8();
                }
            } else {
                break;
            }
        }
        self.caret + end
    }
}
impl View for TextField {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if !self.enabled {
            return Handled::No;
        }
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.composition = None;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                self.focused = true;
                c.set_focus(self.id);
                self.caret = self.text.len();
                self.anchor = self.caret;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::Char(ch) if self.focused && !ch.is_control() => {
                self.replace_selection(&ch.to_string());
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::ImeStart if self.focused => {
                self.composition = Some((String::new(), 0));
                Handled::Yes
            }
            InputKind::ImeUpdate { text, cursor_utf16 } if self.focused => {
                self.composition = Some((text.clone(), *cursor_utf16));
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::ImeEnd { text } if self.focused => {
                self.composition = None;
                self.replace_selection(text);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } if self.focused => {
                let old = self.caret;
                match *virtual_key {
                    LEFT => self.caret = self.prev(),
                    RIGHT => self.caret = self.next(),
                    HOME => self.caret = 0,
                    END => self.caret = self.text.len(),
                    BACK => {
                        if self.caret != self.anchor {
                            self.replace_selection("")
                        } else if self.caret > 0 {
                            self.anchor = self.prev();
                            self.replace_selection("")
                        }
                    }
                    DELETE => {
                        if self.caret != self.anchor {
                            self.replace_selection("")
                        } else if self.caret < self.text.len() {
                            self.anchor = self.next();
                            self.replace_selection("")
                        }
                    }
                    0x41 if modifiers.ctrl => {
                        self.anchor = 0;
                        self.caret = self.text.len()
                    }
                    0x43 if modifiers.ctrl => {
                        let (a, b) = self.selected();
                        self.clipboard = self.text[a..b].into()
                    }
                    0x58 if modifiers.ctrl => {
                        let (a, b) = self.selected();
                        self.clipboard = self.text[a..b].into();
                        self.replace_selection("")
                    }
                    0x56 if modifiers.ctrl => {
                        let s = self.clipboard.clone();
                        self.replace_selection(&s)
                    }
                    0x5a if modifiers.ctrl => {
                        if let Some(s) = self.undo.pop() {
                            self.text = s;
                            self.caret = self.text.len();
                            self.anchor = self.caret
                        }
                    }
                    _ => return Handled::No,
                }
                if !modifiers.shift && matches!(*virtual_key, LEFT | RIGHT | HOME | END) {
                    self.anchor = self.caret
                }
                if old != self.caret || matches!(*virtual_key, BACK | DELETE) {
                    invalidate(c, self.bounds)
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        l.push(DrawCommand::RoundedRect {
            rect: self.bounds,
            radius: t.radius,
            color: t.surface,
        });
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: t.border,
        });
        let start = Point {
            x: Dip(self.bounds.x.0 + t.inset.0),
            y: Dip(self.bounds.y.0 + t.inset.0),
        };
        let shown = if self.text.is_empty() && self.composition.is_none() {
            &self.placeholder
        } else {
            &self.text
        };
        let color = if self.text.is_empty() {
            t.disabled
        } else {
            t.text
        };
        let (a, b) = self.selected();
        if a < b {
            let x = m.measure(&self.text[..a], t.font).width.0;
            let w = m.measure(&self.text[a..b], t.font).width.0;
            l.push(DrawCommand::FillRect {
                rect: Rect {
                    x: Dip(start.x.0 + x),
                    y: start.y,
                    width: Dip(w),
                    height: t.text_size,
                },
                color: t.selected,
            });
        }
        text(l, start, shown, color, t);
        if let Some((comp, _)) = &self.composition {
            text(
                l,
                Point {
                    x: Dip(start.x.0 + m.measure(&self.text[..self.caret], t.font).width.0),
                    y: start.y,
                },
                comp,
                t.text,
                t,
            );
        }
        if self.focused {
            let x = m.measure(&self.text[..self.caret], t.font).width.0;
            l.push(DrawCommand::Line {
                from: Point {
                    x: Dip(start.x.0 + x),
                    y: start.y,
                },
                to: Point {
                    x: Dip(start.x.0 + x),
                    y: Dip(start.y.0 + t.text_size.0),
                },
                width: Dip(1.0),
                color: t.text,
            });
            focus_ring(l, self.bounds, t)
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::TextField,
            &self.name,
            Some(self.text.clone()),
            state(self.enabled, self.focused, false, false),
            vec![],
        ))
    }
}

#[derive(Clone, Debug)]
pub struct TabBar {
    pub id: WidgetId,
    pub tabs: Vec<String>,
    pub selected: usize,
    pub focused: bool,
    pub bounds: Rect,
}
impl TabBar {
    pub fn new(id: WidgetId, tabs: Vec<String>) -> Self {
        Self {
            id,
            tabs,
            selected: 0,
            focused: false,
            bounds: Rect::default(),
        }
    }
}
impl View for TabBar {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: LEFT, ..
            } if self.focused && !self.tabs.is_empty() => {
                self.selected = (self.selected + self.tabs.len() - 1) % self.tabs.len();
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: RIGHT, ..
            } if self.focused && !self.tabs.is_empty() => {
                self.selected = (self.selected + 1) % self.tabs.len();
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) && !self.tabs.is_empty() => {
                let w = self.bounds.width.0 / self.tabs.len() as f32;
                self.selected =
                    (((at.x.0 - self.bounds.x.0) / w) as usize).min(self.tabs.len() - 1);
                c.set_focus(self.id);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        if self.tabs.is_empty() {
            return;
        }
        let w = self.bounds.width.0 / self.tabs.len() as f32;
        for (i, s) in self.tabs.iter().enumerate() {
            let r = Rect {
                x: Dip(self.bounds.x.0 + i as f32 * w),
                y: self.bounds.y,
                width: Dip(w),
                height: self.bounds.height,
            };
            if i == self.selected {
                l.push(DrawCommand::RoundedRect {
                    rect: r,
                    radius: t.radius,
                    color: t.selected,
                });
            }
            text(
                l,
                Point {
                    x: Dip(r.x.0 + t.inset.0),
                    y: Dip(r.y.0 + t.inset.0),
                },
                s,
                t.text,
                t,
            );
            if self.focused && i == self.selected {
                focus_ring(l, r, t)
            }
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let n = self.tabs.len().max(1);
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::TabList,
            "Settings tabs",
            None,
            state(true, self.focused, false, false),
            self.tabs
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    node(
                        WidgetId(self.id.0 + i as u64 + 1),
                        Rect {
                            x: Dip(self.bounds.x.0 + self.bounds.width.0 * i as f32 / n as f32),
                            y: self.bounds.y,
                            width: Dip(self.bounds.width.0 / n as f32),
                            height: self.bounds.height,
                        },
                        WidgetRole::Tab,
                        s,
                        None,
                        state(
                            true,
                            self.focused && i == self.selected,
                            i == self.selected,
                            false,
                        ),
                        vec![],
                    )
                })
                .collect(),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct TableRow {
    pub label: String,
    pub detail: String,
    pub section: bool,
    pub indent: usize,
}
#[derive(Clone, Debug)]
pub struct Table {
    pub id: WidgetId,
    pub headers: Vec<String>,
    pub rows: Vec<TableRow>,
    pub bounds: Rect,
    pub selected: usize,
    pub first_visible: usize,
    pub focused: bool,
}
impl Table {
    pub fn new(id: WidgetId, headers: Vec<String>, rows: Vec<TableRow>) -> Self {
        Self {
            id,
            headers,
            rows,
            bounds: Rect::default(),
            selected: 0,
            first_visible: 0,
            focused: false,
        }
    }
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let row = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light))
            .row
            .0;
        let count = ((self.bounds.height.0 / row).ceil() as usize).saturating_sub(1);
        self.first_visible..(self.first_visible + count).min(self.rows.len())
    }
}
impl View for Table {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: UP, ..
            } if self.focused && !self.rows.is_empty() => {
                self.selected = self.selected.saturating_sub(1);
                self.first_visible = self.first_visible.min(self.selected);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: DOWN, ..
            } if self.focused && !self.rows.is_empty() => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                let capacity = self.visible_range().len().max(1);
                if self.selected >= self.first_visible + capacity {
                    self.first_visible = self.selected + 1 - capacity
                }
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: HOME, ..
            } if self.focused => {
                self.selected = 0;
                self.first_visible = 0;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: END, ..
            } if self.focused && !self.rows.is_empty() => {
                self.selected = self.rows.len() - 1;
                let capacity = self.visible_range().len().max(1);
                self.first_visible = self.rows.len().saturating_sub(capacity);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                let row = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light))
                    .row
                    .0;
                let i = self.first_visible + ((at.y.0 - self.bounds.y.0) / row) as usize;
                if i > 0 && i - 1 < self.rows.len() {
                    self.selected = i - 1;
                    c.set_focus(self.id);
                    invalidate(c, self.bounds)
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        l.push(DrawCommand::PushClip(self.bounds));
        let h = self.bounds.y.0;
        text(
            l,
            Point {
                x: Dip(self.bounds.x.0 + t.inset.0),
                y: Dip(h + t.inset.0),
            },
            &self.headers.join("    "),
            t.text,
            t,
        );
        for i in self.visible_range() {
            let r = Rect {
                x: self.bounds.x,
                y: Dip(h + t.row.0 * (i - self.first_visible + 1) as f32),
                width: self.bounds.width,
                height: t.row,
            };
            if i == self.selected {
                l.push(DrawCommand::FillRect {
                    rect: r,
                    color: t.selected,
                });
            }
            let row = &self.rows[i];
            text(
                l,
                Point {
                    x: Dip(r.x.0 + t.inset.0 * (row.indent + 1) as f32),
                    y: Dip(r.y.0 + t.inset.0),
                },
                &row.label,
                if row.section {
                    theme.group_header
                } else {
                    t.text
                },
                t,
            );
            text(
                l,
                Point {
                    x: Dip(r.x.0 + r.width.0 * 0.6),
                    y: Dip(r.y.0 + t.inset.0),
                },
                &row.detail,
                t.disabled,
                t,
            );
        }
        l.push(DrawCommand::PopClip)
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let t = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light));
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Table,
            "Table",
            None,
            state(true, self.focused, false, false),
            self.visible_range()
                .map(|i| {
                    let row = &self.rows[i];
                    node(
                        WidgetId(self.id.0 + i as u64 + 1),
                        Rect {
                            x: self.bounds.x,
                            y: Dip(self.bounds.y.0 + t.row.0 * (i - self.first_visible + 1) as f32),
                            width: self.bounds.width,
                            height: t.row,
                        },
                        WidgetRole::Row,
                        &row.label,
                        Some(row.detail.clone()),
                        state(
                            true,
                            self.focused && i == self.selected,
                            i == self.selected,
                            false,
                        ),
                        vec![],
                    )
                })
                .collect(),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct ScrollBar {
    pub id: WidgetId,
    pub bounds: Rect,
    pub content: f32,
    pub viewport: f32,
    pub offset: f32,
    pub hovered: bool,
    pub dragging: bool,
    pub focused: bool,
    drag_start: f32,
    offset_start: f32,
}
impl ScrollBar {
    pub fn new(id: WidgetId, content: f32, viewport: f32) -> Self {
        Self {
            id,
            bounds: Rect::default(),
            content,
            viewport,
            offset: 0.0,
            hovered: false,
            dragging: false,
            focused: false,
            drag_start: 0.0,
            offset_start: 0.0,
        }
    }
    fn thumb(&self, t: UiTokens) -> Rect {
        let h = (self.viewport / self.content.max(self.viewport) * self.bounds.height.0)
            .max(t.scrollbar_wide.0 * 2.0);
        let y =
            self.bounds.y.0 + self.offset / self.content.max(self.viewport) * self.bounds.height.0;
        Rect {
            x: self.bounds.x,
            y: Dip(y),
            width: self.bounds.width,
            height: Dip(h.min(self.bounds.height.0)),
        }
    }
}
impl View for ScrollBar {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        let t = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light));
        match &e.kind {
            InputKind::MouseMove { at } => {
                self.hovered = contains(self.bounds, *at);
                if self.dragging {
                    let scale = self.content.max(self.viewport) / self.bounds.height.0.max(1.0);
                    self.offset = (self.offset_start + (at.y.0 - self.drag_start) * scale)
                        .clamp(0.0, (self.content - self.viewport).max(0.0));
                }
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                if contains(self.thumb(t), *at) {
                    self.dragging = true;
                    self.drag_start = at.y.0;
                    self.offset_start = self.offset;
                    c.capture_mouse(true)
                } else {
                    self.offset = (self.offset
                        + if at.y.0 < self.thumb(t).y.0 {
                            -self.viewport
                        } else {
                            self.viewport
                        })
                    .clamp(0.0, (self.content - self.viewport).max(0.0))
                }
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.dragging => {
                self.dragging = false;
                c.capture_mouse(false);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let mut r = self.thumb(t);
        r.width = if self.hovered || self.dragging {
            t.scrollbar_wide
        } else {
            t.scrollbar_thin
        };
        r.x = Dip(self.bounds.x.0 + self.bounds.width.0 - r.width.0);
        l.push(DrawCommand::RoundedRect {
            rect: r,
            radius: Dip(r.width.0 / 2.0),
            color: t.disabled,
        });
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Group,
            "Scroll bar",
            Some(format!("{}", self.offset)),
            state(true, self.focused, false, false),
            vec![],
        ))
    }
}

#[derive(Clone, Debug)]
pub struct ComboBox {
    pub id: WidgetId,
    pub name: String,
    pub items: Vec<Option<String>>,
    pub selected: usize,
    pub open: bool,
    pub bounds: Rect,
    pub focused: bool,
    pub enabled: bool,
}
impl ComboBox {
    pub fn new(id: WidgetId, name: impl Into<String>, items: Vec<Option<String>>) -> Self {
        Self {
            id,
            name: name.into(),
            items,
            selected: 0,
            open: false,
            bounds: Rect::default(),
            focused: false,
            enabled: true,
        }
    }
    fn step(&mut self, delta: isize) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected;
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.items[i].is_some() {
                self.selected = i;
                break;
            }
        }
    }
}
impl View for ComboBox {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if !self.enabled {
            return Handled::No;
        }
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.open = false;
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                self.open = !self.open;
                c.set_focus(self.id);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: DOWN,
                modifiers,
                ..
            } if self.focused => {
                if modifiers.alt {
                    self.open = true
                } else {
                    self.step(1)
                }
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: UP, ..
            } if self.focused => {
                self.step(-1);
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: ENTER | SPACE,
                ..
            } if self.focused => {
                self.open = !self.open;
                invalidate(c, self.bounds);
                Handled::Yes
            }
            InputKind::Char(ch) if self.focused => {
                if let Some(i) = self.items.iter().position(|s| {
                    s.as_ref().is_some_and(|v| {
                        v.to_lowercase().starts_with(&ch.to_lowercase().to_string())
                    })
                }) {
                    self.selected = i;
                    invalidate(c, self.bounds)
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        l.push(DrawCommand::RoundedRect {
            rect: self.bounds,
            radius: t.radius,
            color: t.surface,
        });
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: t.border,
        });
        text(
            l,
            Point {
                x: Dip(self.bounds.x.0 + t.inset.0),
                y: Dip(self.bounds.y.0 + t.inset.0),
            },
            self.items
                .get(self.selected)
                .and_then(|v| v.as_deref())
                .unwrap_or(""),
            t.text,
            t,
        );
        text(
            l,
            Point {
                x: Dip(self.bounds.x.0 + self.bounds.width.0 - t.text_size.0 - t.inset.0),
                y: Dip(self.bounds.y.0 + t.inset.0),
            },
            "⌄",
            t.text,
            t,
        );
        if self.focused {
            focus_ring(l, self.bounds, t)
        }
        if self.open {
            let popup = Rect {
                x: self.bounds.x,
                y: Dip(self.bounds.y.0 + self.bounds.height.0),
                width: self.bounds.width,
                height: Dip(self.items.len() as f32 * t.menu_row.0 + 2.0 * t.inset.0),
            };
            l.push(DrawCommand::RoundedRect {
                rect: popup,
                radius: t.popup_radius,
                color: t.surface,
            });
            for (i, item) in self.items.iter().enumerate() {
                let y = popup.y.0 + t.inset.0 + i as f32 * t.menu_row.0;
                if let Some(s) = item {
                    text(
                        l,
                        Point {
                            x: Dip(popup.x.0 + t.inset.0),
                            y: Dip(y),
                        },
                        s,
                        t.text,
                        t,
                    )
                } else {
                    l.push(DrawCommand::Line {
                        from: Point {
                            x: Dip(popup.x.0 + t.inset.0),
                            y: Dip(y),
                        },
                        to: Point {
                            x: Dip(popup.x.0 + popup.width.0 - t.inset.0),
                            y: Dip(y),
                        },
                        width: Dip(1.0),
                        color: t.border,
                    })
                }
            }
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let mut s = state(self.enabled, self.focused, false, false);
        s.expanded = self.open;
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::ComboBox,
            &self.name,
            self.items.get(self.selected).cloned().flatten(),
            s,
            vec![],
        ))
    }
}

pub fn key(v: u16, modifiers: Modifiers) -> InputEvent {
    InputEvent {
        time_us: 0,
        kind: InputKind::KeyDown {
            virtual_key: v,
            modifiers,
            repeat: false,
        },
    }
}
