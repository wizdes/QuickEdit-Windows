use crate::{AlertModel, Button, Label, LabelKind, UiTokens};
use qe_api::{
    Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Rect, TextMeasurer, Theme, View,
    ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};

/// Painted dialog content and focus order. The native title bar belongs to DialogWindow.
pub struct DialogFrame {
    pub id: WidgetId,
    pub title: String,
    pub bounds: Rect,
    pub children: Vec<Box<dyn View>>,
    pub focus_index: usize,
    pub default_index: Option<usize>,
    pub closed: bool,
}
impl DialogFrame {
    pub fn new(id: WidgetId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            bounds: Rect::default(),
            children: vec![],
            focus_index: 0,
            default_index: None,
            closed: false,
        }
    }
    pub fn add(&mut self, child: Box<dyn View>) {
        self.children.push(child)
    }
    pub fn from_alert(id: WidgetId, alert: &AlertModel, bounds: Rect) -> Self {
        let mut frame = Self::new(id, &alert.title);
        frame.bounds = bounds;
        let t = UiTokens::from_theme(&Theme::new(qe_api::ThemeKind::Light));
        let mut label = Label::new(WidgetId(id.0 + 1), &alert.message, LabelKind::Plain);
        label.bounds = Rect {
            x: Dip(bounds.x.0 + t.inset.0 * 2.0),
            y: Dip(bounds.y.0 + t.inset.0 * 2.0),
            width: Dip(bounds.width.0 - t.inset.0 * 4.0),
            height: t.row,
        };
        frame.add(Box::new(label));
        for (i, name) in alert.buttons.iter().enumerate() {
            let mut button = Button::new(WidgetId(id.0 + i as u64 + 2), name);
            button.is_default = i == 0;
            button.bounds = Rect {
                x: Dip(bounds.x.0 + bounds.width.0
                    - (alert.buttons.len() - i) as f32 * (t.alert_button_width.0 + t.inset.0)
                    - t.inset.0),
                y: Dip(bounds.y.0 + bounds.height.0 - t.control_height.0 - t.inset.0),
                width: t.alert_button_width,
                height: t.control_height,
            };
            frame.add(Box::new(button));
        }
        if !alert.buttons.is_empty() {
            frame.focus_index = 1;
            frame.default_index = Some(1)
        }
        frame
    }
}
impl View for DialogFrame {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        match &event.kind {
            InputKind::KeyDown {
                virtual_key: 0x1b, ..
            } => {
                self.closed = true;
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x09,
                modifiers,
                ..
            } if !self.children.is_empty() => {
                let n = self.children.len();
                self.focus_index = if modifiers.shift {
                    (self.focus_index + n - 1) % n
                } else {
                    (self.focus_index + 1) % n
                };
                if let Some(w) = self.children[self.focus_index].widgets() {
                    ctx.set_focus(w.id)
                }
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x0d, ..
            }
            | InputKind::KeyUp {
                virtual_key: 0x0d, ..
            } => {
                if let Some(i) = self.default_index
                    && let Some(child) = self.children.get_mut(i)
                {
                    return child.handle(event, ctx);
                }
                Handled::No
            }
            _ => self
                .children
                .get_mut(self.focus_index)
                .map_or(Handled::No, |child| child.handle(event, ctx)),
        }
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: t.background,
        });
        for child in &self.children {
            child.draw(list, theme, measure)
        }
    }
    fn layout(&mut self, bounds: Rect, _: &dyn TextMeasurer) {
        self.bounds = bounds
    }
    fn widgets(&self) -> Option<Widget> {
        Some(Widget {
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Dialog,
            name: self.title.clone(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            children: self.children.iter().filter_map(|c| c.widgets()).collect(),
        })
    }
}
