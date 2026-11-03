//! The flat five-label menu bar. Models are made only when a label opens.
use crate::{UI_MEASURE_TOKEN, UiTokens};
use qe_api::{
    Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, Point, Rect,
    TextMeasurer, Theme, ThemeKind, View, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};

pub const MENU_LABELS: [&str; 5] = ["File", "Edit", "Format", "View", "Window"];
const MNEMONICS: [u16; 5] = [
    b'F' as u16,
    b'E' as u16,
    b'O' as u16,
    b'V' as u16,
    b'W' as u16,
];
/// Sizes are the same in every theme.
const SIZES: Theme = Theme::new(ThemeKind::Light);
const VK_F10: u16 = 0x79;

/// Keys that are only half of a chord.
fn is_modifier_key(virtual_key: u16) -> bool {
    matches!(
        virtual_key,
        0x10 | 0x11 | 0x12 | 0x5b | 0x5c | 0xa0 | 0xa1 | 0xa2 | 0xa3 | 0xa4 | 0xa5
    )
}

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
    /// The keyboard reached the bar with no key of its own, as F10 from another menu.
    pub fn focus_first(&mut self, ctx: &mut ViewContext) {
        self.change(Some(0), ctx);
    }
    /// Something else happened while Alt was down, so its release is no tap.
    pub fn cancel_alt(&mut self) {
        self.alt_armed = false;
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
            role: WidgetRole::MenuBar,
            name: "Menu bar".into(),
            value: None,
            // The focused label says so, and the bar does not, so that one widget has it.
            states: WidgetStates {
                enabled: true,
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
                        // While a menu is open, its row or its list has the focus.
                        focused: self.focused == Some(i) && self.open.is_none(),
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
        let mut x = bounds.x.0 + SIZES.menu_bar_inset.0;
        for (i, label) in MENU_LABELS.iter().enumerate() {
            let width = measure.measure(label, UI_MEASURE_TOKEN).width.0
                + SIZES.menu_bar_label_padding.0 * 2.0;
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
                repeat,
            } => {
                self.alt_armed = false;
                if modifiers.ctrl || modifiers.win {
                    // One rule: a chord, AltGr included, goes back to the text and reaches
                    // it. A modifier alone is not a chord yet.
                    if self.focused.is_some() {
                        if is_modifier_key(virtual_key) {
                            return Handled::Yes;
                        }
                        self.leave(ctx);
                    }
                    return Handled::No;
                }
                if virtual_key == VK_F10 && !modifiers.shift && !modifiers.alt {
                    // A held F10 acts once.
                    if !repeat {
                        if self.focused.is_some() {
                            self.leave(ctx);
                        } else {
                            self.change(Some(0), ctx);
                        }
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
                    // The rest of a held Enter must not reach the menu it opened, and a
                    // repeat that arrives first opens nothing.
                    0x0d if repeat => Handled::Yes,
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
                self.alt_armed = false;
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
            InputKind::MouseDown { at, .. } => {
                self.alt_armed = false;
                if self.inside(at) {
                    Handled::Yes
                } else {
                    // The right and middle buttons let go of the focus as the left does.
                    if self.focused.is_some() {
                        self.leave(ctx);
                    }
                    Handled::No
                }
            }
            InputKind::MouseUp { at, .. } if self.inside(at) => Handled::Yes,
            InputKind::Wheel { at, .. } => {
                self.alt_armed = false;
                if self.inside(at) {
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
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
            // The active tab's color, so that the tab joins the bar with no line.
            color: theme.text_background,
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
                    x: Dip(r.x.0 + SIZES.menu_bar_label_padding.0),
                    y: Dip(r.y.0 + (r.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0),
                },
                font: t.font.into(),
                font_size: t.text_size,
                color: ink,
            });
            if focused {
                let mnemonic = if i == 2 { 1 } else { 0 };
                let x = r.x.0
                    + SIZES.menu_bar_label_padding.0
                    + measure.measure(&label[..mnemonic], t.measure_font).width.0;
                let w = measure
                    .measure(&label[mnemonic..mnemonic + 1], t.measure_font)
                    .width
                    .0;
                let y = r.y.0
                    + (r.height.0 - metrics.height.0) / 2.0
                    + metrics.baseline.0
                    + SIZES.menu_bar_underline_offset.0;
                list.push(DrawCommand::Line {
                    from: Point {
                        x: Dip(x),
                        y: Dip(y),
                    },
                    to: Point {
                        x: Dip(x + w),
                        y: Dip(y),
                    },
                    width: SIZES.menu_bar_underline_width,
                    color: ink,
                });
            }
        }
    }
}
