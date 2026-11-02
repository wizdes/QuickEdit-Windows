use crate::UiTokens;
use qe_api::{
    CommandId, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, Point,
    Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    pub key_text: String,
    pub enabled: bool,
    pub checked: bool,
    pub submenu: Option<MenuModel>,
    pub separator: bool,
    pub command: Option<CommandId>,
    pub mnemonic: Option<char>,
}
impl MenuItem {
    pub fn command(label: impl Into<String>, command: CommandId) -> Self {
        Self {
            label: label.into(),
            key_text: String::new(),
            enabled: true,
            checked: false,
            submenu: None,
            separator: false,
            command: Some(command),
            mnemonic: None,
        }
    }
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            key_text: String::new(),
            enabled: false,
            checked: false,
            submenu: None,
            separator: true,
            command: None,
            mnemonic: None,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct MenuModel {
    pub items: Vec<MenuItem>,
}
#[derive(Clone, Debug)]
pub struct MenuView {
    pub id: WidgetId,
    pub model: MenuModel,
    pub bounds: Rect,
    pub selected: Option<usize>,
    pub child: Option<Box<MenuView>>,
    pub keyboard_opened: bool,
    pub closed: bool,
    pending_submenu: Option<(usize, u64)>,
}
impl MenuView {
    pub fn new(id: WidgetId, model: MenuModel) -> Self {
        Self {
            id,
            model,
            bounds: Rect::default(),
            selected: None,
            child: None,
            keyboard_opened: false,
            closed: false,
            pending_submenu: None,
        }
    }
    pub fn open_keyboard(&mut self) {
        self.keyboard_opened = true;
        self.selected = self.first_enabled()
    }
    fn first_enabled(&self) -> Option<usize> {
        self.model
            .items
            .iter()
            .position(|i| i.enabled && !i.separator)
    }
    fn move_selection(&mut self, delta: isize) {
        let n = self.model.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected.unwrap_or(if delta > 0 { n - 1 } else { 0 });
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.model.items[i].enabled && !self.model.items[i].separator {
                self.selected = Some(i);
                self.child = None;
                break;
            }
        }
    }
    fn open_child(&mut self) {
        let Some(i) = self.selected else { return };
        let Some(model) = self.model.items[i].submenu.clone() else {
            return;
        };
        let mut child = MenuView::new(WidgetId(self.id.0 + 1000 + i as u64), model);
        child.keyboard_opened = self.keyboard_opened;
        child.selected = child.first_enabled();
        child.bounds = Rect {
            x: Dip(self.bounds.x.0 + self.bounds.width.0),
            y: Dip(self.bounds.y.0
                + UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light))
                    .inset
                    .0
                + i as f32
                    * UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light))
                        .menu_row
                        .0),
            width: self.bounds.width,
            height: Dip(UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light))
                .inset
                .0
                * 2.0
                + child.model.items.len() as f32
                    * UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light))
                        .menu_row
                        .0),
        };
        self.child = Some(Box::new(child));
    }
    fn activate(&mut self, c: &mut ViewContext) {
        if let Some(i) = self.selected {
            if self.model.items[i].submenu.is_some() {
                self.open_child()
            } else if let Some(cmd) = self.model.items[i].command {
                c.run(cmd);
                self.closed = true
            }
        }
    }
    pub fn tick(&mut self, now_us: u64) {
        if let Some((i, start)) = self.pending_submenu
            && self.selected == Some(i)
            && now_us.saturating_sub(start)
                >= UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light)).submenu_delay_ms
                    * 1000
        {
            self.open_child();
            self.pending_submenu = None
        }
    }
}
impl View for MenuView {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if let Some(child) = &mut self.child {
            if matches!(
                &e.kind,
                InputKind::KeyDown {
                    virtual_key: 0x25,
                    ..
                } | InputKind::KeyDown {
                    virtual_key: 0x1b,
                    ..
                }
            ) {
                self.child = None;
                return Handled::Yes;
            }
            let h = child.handle(e, c);
            if h == Handled::Yes {
                return h;
            }
        }
        match &e.kind {
            InputKind::KeyDown {
                virtual_key: 0x26, ..
            } => {
                self.move_selection(-1);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x28, ..
            } => {
                self.move_selection(1);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x27, ..
            } => {
                self.open_child();
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x0d, ..
            } => {
                self.activate(c);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x1b, ..
            } => {
                self.closed = true;
                Handled::Yes
            }
            InputKind::Char(ch) => {
                let needle = ch.to_lowercase().to_string();
                if let Some(i) = self.model.items.iter().position(|item| {
                    item.enabled
                        && !item.separator
                        && (item
                            .mnemonic
                            .is_some_and(|m| m.to_lowercase().to_string() == needle)
                            || item.label.to_lowercase().starts_with(&needle))
                }) {
                    self.selected = Some(i);
                    if self.keyboard_opened && self.model.items[i].mnemonic.is_some() {
                        self.activate(c)
                    }
                    c.invalidate(self.bounds)
                }
                Handled::Yes
            }
            InputKind::MouseMove { at }
                if at.x.0 >= self.bounds.x.0 && at.x.0 < self.bounds.x.0 + self.bounds.width.0 =>
            {
                let t = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light));
                let i = ((at.y.0 - self.bounds.y.0 - t.inset.0) / t.menu_row.0).floor() as isize;
                if i >= 0
                    && self
                        .model
                        .items
                        .get(i as usize)
                        .is_some_and(|item| item.enabled && !item.separator)
                {
                    self.selected = Some(i as usize);
                    if self.model.items[i as usize].submenu.is_some() {
                        self.pending_submenu = Some((i as usize, e.time_us));
                        c.request_timer(t.submenu_delay_ms as u32)
                    }
                    c.invalidate(self.bounds)
                }
                Handled::Yes
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } if at.x.0 >= self.bounds.x.0 && at.x.0 < self.bounds.x.0 + self.bounds.width.0 => {
                self.activate(c);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        l.push(DrawCommand::RoundedRect {
            rect: self.bounds,
            radius: t.popup_radius,
            color: t.surface,
        });
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.popup_radius,
            width: Dip(1.0),
            color: t.border,
        });
        for (i, item) in self.model.items.iter().enumerate() {
            let y = self.bounds.y.0 + t.inset.0 + i as f32 * t.menu_row.0;
            if item.separator {
                l.push(DrawCommand::Line {
                    from: Point {
                        x: Dip(self.bounds.x.0 + t.inset.0),
                        y: Dip(y + t.menu_row.0 / 2.0),
                    },
                    to: Point {
                        x: Dip(self.bounds.x.0 + self.bounds.width.0 - t.inset.0),
                        y: Dip(y + t.menu_row.0 / 2.0),
                    },
                    width: Dip(1.0),
                    color: t.border,
                });
                continue;
            }
            let r = Rect {
                x: Dip(self.bounds.x.0 + t.inset.0 / 2.0),
                y: Dip(y),
                width: Dip(self.bounds.width.0 - t.inset.0),
                height: t.menu_row,
            };
            if self.selected == Some(i) {
                l.push(DrawCommand::RoundedRect {
                    rect: r,
                    radius: t.radius,
                    color: t.hover,
                })
            }
            let color = if item.enabled { t.text } else { t.disabled };
            if item.checked {
                l.push(DrawCommand::TextRun {
                    at: Point {
                        x: Dip(r.x.0 + t.inset.0),
                        y: Dip(y + t.inset.0),
                    },
                    text: "✓".into(),
                    font: t.font.into(),
                    color,
                })
            }
            let label_x = r.x.0 + t.inset.0 + t.check_size.0;
            if self.keyboard_opened
                && let Some(mn) = item.mnemonic
                && let Some(pos) = item
                    .label
                    .to_lowercase()
                    .find(mn.to_lowercase().to_string().as_str())
            {
                let pre = &item.label[..pos];
                let x = label_x + m.measure(pre, t.font).width.0;
                l.push(DrawCommand::Line {
                    from: Point {
                        x: Dip(x),
                        y: Dip(y + t.text_size.0 + t.inset.0),
                    },
                    to: Point {
                        x: Dip(x + t.text_size.0 / 2.0),
                        y: Dip(y + t.text_size.0 + t.inset.0),
                    },
                    width: Dip(1.0),
                    color,
                })
            }
            l.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(label_x),
                    y: Dip(y + t.inset.0),
                },
                text: item.label.clone(),
                font: t.font.into(),
                color,
            });
            if !item.key_text.is_empty() {
                let w = m.measure(&item.key_text, t.font).width.0;
                l.push(DrawCommand::TextRun {
                    at: Point {
                        x: Dip(r.x.0 + r.width.0 - w - t.inset.0),
                        y: Dip(y + t.inset.0),
                    },
                    text: item.key_text.clone(),
                    font: t.font.into(),
                    color: t.disabled,
                })
            }
            if item.submenu.is_some() {
                l.push(DrawCommand::TextRun {
                    at: Point {
                        x: Dip(r.x.0 + r.width.0 - t.text_size.0 - t.inset.0),
                        y: Dip(y + t.inset.0),
                    },
                    text: "\u{e76c}".into(),
                    font: t.icon_font.into(),
                    color,
                })
            }
        }
        if let Some(child) = &self.child {
            child.draw(l, theme, m)
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let t = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light));
        let mut children: Vec<_> = self
            .model
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| !i.separator)
            .map(|(i, item)| Widget {
                id: WidgetId(self.id.0 + i as u64 + 1),
                bounds: Rect {
                    x: self.bounds.x,
                    y: Dip(self.bounds.y.0 + t.inset.0 + i as f32 * t.menu_row.0),
                    width: self.bounds.width,
                    height: t.menu_row,
                },
                role: WidgetRole::MenuItem,
                name: item.label.clone(),
                value: Some(item.key_text.clone()),
                states: WidgetStates {
                    enabled: item.enabled,
                    selected: self.selected == Some(i),
                    checked: item.checked,
                    expanded: self.child.is_some() && self.selected == Some(i),
                    ..Default::default()
                },
                children: vec![],
            })
            .collect();
        if let Some(child) = &self.child
            && let Some(node) = child.widgets()
        {
            children.push(node)
        }
        Some(Widget {
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Menu,
            name: "Menu".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            children,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AltKeyTracker {
    armed: bool,
}
impl AltKeyTracker {
    pub fn event(&mut self, event: &InputEvent) -> bool {
        match &event.kind {
            InputKind::KeyDown {
                virtual_key: 0xa4,
                modifiers,
                ..
            } if !modifiers.ctrl => {
                self.armed = true;
                false
            }
            InputKind::KeyUp {
                virtual_key: 0xa4,
                modifiers,
            } => {
                let fire = self.armed && !modifiers.ctrl;
                self.armed = false;
                fire
            }
            InputKind::KeyDown { .. } | InputKind::Char(_) | InputKind::MouseDown { .. } => {
                self.armed = false;
                false
            }
            _ => false,
        }
    }
}

pub fn place_popup(anchor: Rect, size: (f32, f32), work: Rect, submenu: bool) -> Rect {
    let mut x = if submenu {
        anchor.x.0 + anchor.width.0
    } else {
        anchor.x.0
    };
    let mut y = if submenu {
        anchor.y.0
    } else {
        anchor.y.0 + anchor.height.0
    };
    if x + size.0 > work.x.0 + work.width.0 {
        x = if submenu {
            anchor.x.0 - size.0
        } else {
            work.x.0 + work.width.0 - size.0
        }
    }
    if y + size.1 > work.y.0 + work.height.0 {
        y = if submenu {
            work.y.0 + work.height.0 - size.1
        } else {
            anchor.y.0 - size.1
        }
    }
    Rect {
        x: Dip(x.max(work.x.0)),
        y: Dip(y.max(work.y.0)),
        width: Dip(size.0.min(work.width.0)),
        height: Dip(size.1.min(work.height.0)),
    }
}
