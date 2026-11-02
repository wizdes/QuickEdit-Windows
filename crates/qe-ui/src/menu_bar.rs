//! The flat five-label menu bar. Models are made only when a label opens.
use crate::{UI_MEASURE_TOKEN, UiTokens};
use qe_api::{
    Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, Point, Rect,
    TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};

pub const MENU_LABELS: [&str; 5] = ["File", "Edit", "Format", "View", "Window"];
const MNEMONICS: [u16; 5] = [
    b'F' as u16,
    b'E' as u16,
    b'O' as u16,
    b'V' as u16,
    b'W' as u16,
];

pub struct MenuBar {
    id: WidgetId,
    bounds: Rect,
    labels: [Rect; 5],
    focused: Option<usize>,
    hover: Option<usize>,
    open: Option<usize>,
    request: Option<(usize, Rect)>,
    alt_armed: bool,
}
impl MenuBar {
    pub fn new(id: WidgetId) -> Self {
        Self {
            id,
            bounds: Rect::default(),
            labels: [Rect::default(); 5],
            focused: None,
            hover: None,
            open: None,
            request: None,
            alt_armed: false,
        }
    }
    pub fn focused(&self) -> Option<usize> {
        self.focused
    }
    pub fn open(&self) -> Option<usize> {
        self.open
    }
    pub fn bounds(&self) -> Rect {
        self.bounds
    }
    pub fn label_rect(&self, index: usize) -> Rect {
        self.labels[index]
    }
    /// The label that this key's letter opens.
    pub fn mnemonic_label(virtual_key: u16) -> Option<usize> {
        MNEMONICS.iter().position(|key| *key == virtual_key)
    }
    /// The pointer left the bar with no message over it, so no label stays highlighted.
    pub fn pointer_left(&mut self, ctx: &mut ViewContext) {
        if let Some(i) = self.hover.take() {
            ctx.invalidate(self.labels[i]);
        }
    }
    pub fn take_open_request(&mut self) -> Option<(usize, Rect)> {
        self.request.take()
    }
    fn hit(&self, p: Point) -> Option<usize> {
        self.labels.iter().position(|r| {
            p.x.0 >= r.x.0
                && p.x.0 < r.x.0 + r.width.0
                && p.y.0 >= r.y.0
                && p.y.0 < r.y.0 + r.height.0
        })
    }
    fn inside(&self, p: Point) -> bool {
        p.x.0 >= self.bounds.x.0
            && p.x.0 < self.bounds.x.0 + self.bounds.width.0
            && p.y.0 >= self.bounds.y.0
            && p.y.0 < self.bounds.y.0 + self.bounds.height.0
    }
    fn change(&mut self, next: Option<usize>, ctx: &mut ViewContext) {
        if self.focused != next {
            if let Some(i) = self.focused {
                ctx.invalidate(self.labels[i]);
            }
            if let Some(i) = next {
                ctx.invalidate(self.labels[i]);
            }
            self.focused = next;
        }
    }
    fn show(&mut self, index: usize, ctx: &mut ViewContext) {
        if let Some(old) = self.open {
            ctx.invalidate(self.labels[old]);
        }
        self.change(Some(index), ctx);
        self.open = Some(index);
        self.request = Some((index, self.labels[index]));
        ctx.invalidate(self.labels[index]);
    }
    pub fn close_popup(&mut self, ctx: &mut ViewContext) {
        if let Some(old) = self.open.take() {
            ctx.invalidate(self.labels[old]);
        }
    }
    pub fn leave(&mut self, ctx: &mut ViewContext) {
        self.close_popup(ctx);
        self.change(None, ctx);
        self.alt_armed = false;
    }
    pub fn widgets(&self) -> Widget {
        Widget {
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Menu,
            name: "Menu bar".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                focused: self.focused.is_some(),
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children: MENU_LABELS
                .iter()
                .enumerate()
                .map(|(i, name)| Widget {
                    id: WidgetId(self.id.0 + i as u64 + 1),
                    bounds: self.labels[i],
                    role: WidgetRole::MenuItem,
                    name: (*name).into(),
                    value: None,
                    states: WidgetStates {
                        enabled: true,
                        focused: self.focused == Some(i),
                        expanded: self.open == Some(i),
                        has_popup: true,
                        ..Default::default()
                    },
                    selection: None,
                    range_value: None,
                    description: None,
                    children: Vec::new(),
                })
                .collect(),
        }
    }
}
impl View for MenuBar {
    fn widgets(&self) -> Option<Widget> {
        Some(self.widgets())
    }
    fn layout(&mut self, bounds: Rect, measure: &dyn TextMeasurer) {
        self.bounds = bounds;
        let mut x = bounds.x.0 + 4.0;
        for (i, label) in MENU_LABELS.iter().enumerate() {
            let width = measure.measure(label, UI_MEASURE_TOKEN).width.0 + 24.0;
            self.labels[i] = Rect {
                x: Dip(x),
                y: bounds.y,
                width: Dip(width),
                height: bounds.height,
            };
            x += width;
        }
    }
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        match event.kind {
            InputKind::KeyDown {
                virtual_key: 0x12 | 0xa4 | 0xa5,
                modifiers,
                repeat,
            } => {
                if !repeat {
                    self.alt_armed = !modifiers.ctrl && !modifiers.shift && !modifiers.win;
                }
                Handled::No
            }
            InputKind::KeyUp {
                virtual_key: 0x12 | 0xa4 | 0xa5,
                modifiers,
            } => {
                let tap = self.alt_armed && !modifiers.ctrl && !modifiers.shift && !modifiers.win;
                self.alt_armed = false;
                if tap {
                    if self.focused.is_some() {
                        self.leave(ctx);
                    } else {
                        self.change(Some(0), ctx);
                    }
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } => {
                self.alt_armed = false;
                if modifiers.ctrl || modifiers.win {
                    return Handled::No;
                }
                if virtual_key == 0x79 {
                    if self.focused.is_some() {
                        self.leave(ctx);
                    } else {
                        self.change(Some(0), ctx);
                    }
                    return Handled::Yes;
                }
                if modifiers.alt {
                    if let Some(i) = MNEMONICS.iter().position(|key| *key == virtual_key) {
                        self.show(i, ctx);
                        return Handled::Yes;
                    }
                    // Alt+Space and Alt+F4 go on to Windows, and the text gets the focus.
                    if matches!(virtual_key, 0x20 | 0x73) && self.focused.is_some() {
                        self.leave(ctx);
                    }
                    return Handled::No;
                }
                let Some(current) = self.focused else {
                    return Handled::No;
                };
                match virtual_key {
                    0x25 | 0x27 => {
                        let next = if virtual_key == 0x25 {
                            (current + 4) % 5
                        } else {
                            (current + 1) % 5
                        };
                        if self.open.is_some() {
                            self.show(next, ctx);
                        } else {
                            self.change(Some(next), ctx);
                        }
                        Handled::Yes
                    }
                    0x28 | 0x0d => {
                        self.show(current, ctx);
                        Handled::Yes
                    }
                    0x1b => {
                        if self.open.is_some() {
                            self.close_popup(ctx);
                        } else {
                            self.leave(ctx);
                        }
                        Handled::Yes
                    }
                    _ => {
                        if let Some(i) = MNEMONICS.iter().position(|key| *key == virtual_key) {
                            self.show(i, ctx);
                        }
                        Handled::Yes
                    }
                }
            }
            InputKind::KeyUp { modifiers, .. }
                if self.focused.is_some() && !modifiers.ctrl && !modifiers.win =>
            {
                Handled::Yes
            }
            InputKind::Char(_) if self.focused.is_some() => Handled::Yes,
            InputKind::MouseMove { at } => {
                let next = self.hit(at);
                if next != self.hover {
                    if let Some(i) = self.hover {
                        ctx.invalidate(self.labels[i]);
                    }
                    if let Some(i) = next {
                        ctx.invalidate(self.labels[i]);
                    }
                    self.hover = next;
                    if self.open.is_some()
                        && let Some(i) = next
                        && self.open != Some(i)
                    {
                        self.show(i, ctx);
                    }
                }
                if self.inside(at) {
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self.hit(at) {
                    if self.open == Some(i) {
                        self.leave(ctx);
                    } else {
                        self.show(i, ctx);
                    }
                    Handled::Yes
                } else if self.inside(at) {
                    self.leave(ctx);
                    Handled::Yes
                } else {
                    if self.focused.is_some() {
                        self.leave(ctx);
                    }
                    Handled::No
                }
            }
            InputKind::MouseDown { at, .. } if self.inside(at) => Handled::Yes,
            InputKind::MouseUp { at, .. } if self.inside(at) => Handled::Yes,
            InputKind::Wheel { at, .. } if self.inside(at) => Handled::Yes,
            InputKind::Blur => {
                self.leave(ctx);
                Handled::No
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: theme.tab_background,
        });
        for (i, label) in MENU_LABELS.iter().enumerate() {
            let r = self.labels[i];
            let focused = self.focused == Some(i);
            let hovered = self.hover == Some(i);
            if focused || hovered {
                list.push(DrawCommand::FillRect {
                    rect: r,
                    color: if self.open == Some(i) {
                        t.pressed
                    } else {
                        t.hover
                    },
                });
            }
            let ink = if self.open == Some(i) {
                t.pressed_text
            } else if focused || hovered {
                t.hover_text
            } else {
                t.text
            };
            let metrics = measure.measure(label, t.measure_font);
            list.push(DrawCommand::TextRun {
                text: (*label).into(),
                at: Point {
                    x: Dip(r.x.0 + 12.0),
                    y: Dip(r.y.0 + (r.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0),
                },
                font: t.font.into(),
                font_size: t.text_size,
                color: ink,
            });
            if focused {
                let mnemonic = if i == 2 { 1 } else { 0 };
                let x = r.x.0 + 12.0 + measure.measure(&label[..mnemonic], t.measure_font).width.0;
                let w = measure
                    .measure(&label[mnemonic..mnemonic + 1], t.measure_font)
                    .width
                    .0;
                let y = r.y.0 + (r.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0 + 2.0;
                list.push(DrawCommand::Line {
                    from: Point {
                        x: Dip(x),
                        y: Dip(y),
                    },
                    to: Point {
                        x: Dip(x + w),
                        y: Dip(y),
                    },
                    width: Dip(1.0),
                    color: ink,
                });
            }
        }
    }
}
