use crate::control::*;
use crate::controls::{contains, rect};
use crate::{AlertModel, Button, Control, Label, LabelKind, UiTokens};
use qe_api::{
    DisplayList, DrawCommand, Handled, InputEvent, InputKind, Rect, TextMeasurer, Theme, View,
    ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};
use std::any::Any;

/// How a dialog ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogResult {
    /// A closing button, by its result number. For an alert it is the index in
    /// `AlertModel::buttons`.
    Button(usize),
    /// Esc, or the window's own close button through `DialogFrame::dismiss`.
    Dismissed,
}

/// Painted dialog content and focus order. The native title bar belongs to DialogWindow.
///
/// The frame owns focus among its children. It sends `Blur` to the old child, then
/// `Focus` to the new one, so that each child's `focused` flag is the truth.
pub struct DialogFrame {
    pub id: WidgetId,
    pub title: String,
    pub bounds: Rect,
    pub children: Vec<Box<dyn Control>>,
    pub focus_index: usize,
    pub default_index: Option<usize>,
    pub closed: bool,
    result: Option<DialogResult>,
    closing: Vec<(usize, usize)>,
    last_activated: Option<usize>,
    focus_given: bool,
    enter_target: Option<usize>,
    mouse_target: Option<usize>,
    alert_layout: bool,
    problem: Option<String>,
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
            result: None,
            closing: vec![],
            last_activated: None,
            focus_given: false,
            enter_target: None,
            mouse_target: None,
            alert_layout: false,
            problem: None,
        }
    }
    /// Adds a child. The first child that can take focus has it from the start.
    pub fn add(&mut self, mut child: Box<dyn Control>) {
        child.set_popup_area(self.bounds);
        let index = self.children.len();
        let take_focus = !self.focus_given && child.focusable();
        self.children.push(child);
        if take_focus {
            self.focus_given = true;
            self.focus_index = index;
            let mut ignored = ViewContext::default();
            self.children[index].handle(
                &InputEvent {
                    time_us: 0,
                    kind: InputKind::Focus,
                },
                &mut ignored,
            );
        }
    }
    /// Makes activating this child close the dialog with `DialogResult::Button(result)`.
    pub fn close_on_activate(&mut self, child: usize, result: usize) {
        self.closing.push((child, result));
    }
    /// A mistake in how the dialog was set up, such as a `default_index` that names
    /// no button. Enter does nothing in that case, and this says why.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }
    /// How the dialog ended. `None` while it is open.
    pub fn result(&self) -> Option<DialogResult> {
        self.result
    }
    /// Which child was activated last without closing the dialog. Reading clears it.
    pub fn take_last_activated(&mut self) -> Option<usize> {
        self.last_activated.take()
    }
    /// Ends the dialog as Esc does. A host calls it for the window's close notice.
    pub fn dismiss(&mut self) {
        self.finish(DialogResult::Dismissed);
    }
    fn finish(&mut self, result: DialogResult) {
        self.closed = true;
        if self.result.is_none() {
            self.result = Some(result);
        }
    }
    /// A child as its own type.
    pub fn child<T: Control>(&self, index: usize) -> Option<&T> {
        let child: &dyn Any = self.children.get(index)?.as_ref();
        child.downcast_ref()
    }
    pub fn child_mut<T: Control>(&mut self, index: usize) -> Option<&mut T> {
        let child: &mut dyn Any = self.children.get_mut(index)?.as_mut();
        child.downcast_mut()
    }
    /// An alert frame. Its children take the ids from `id + 1` up: the message, then
    /// one for each button, so a caller leaves `id + 1` to `id + 4` free.
    pub fn from_alert(id: WidgetId, alert: &AlertModel, bounds: Rect) -> Self {
        let mut frame = Self::new(id, &alert.title);
        frame.bounds = bounds;
        let mut label = Label::new(WidgetId(id.0 + 1), &alert.message, LabelKind::Plain);
        label.wrap = true;
        frame.add(Box::new(label));
        for (i, name) in alert.buttons.iter().enumerate() {
            let mut button = Button::new(WidgetId(id.0 + i as u64 + 2), name);
            button.is_default = i == 0;
            frame.add(Box::new(button));
            frame.close_on_activate(i + 1, i);
        }
        if !alert.buttons.is_empty() {
            frame.default_index = Some(1)
        }
        frame.alert_layout = true;
        frame.layout_alert();
        frame
    }
    /// Buttons sit in a row at the lower right and shrink to fit a narrow frame. The
    /// message takes the rest, wraps, and is clipped.
    fn layout_alert(&mut self) {
        let t = UiTokens::sizes();
        let bounds = self.bounds;
        let count = self.children.len().saturating_sub(1);
        let gap = t.inset.0;
        let room = (bounds.width.0 - gap * (count as f32 + 1.0)).max(0.0);
        let width = if count > 0 {
            t.alert_button_width.0.min(room / count as f32)
        } else {
            0.0
        };
        let button_y = bounds.y.0 + bounds.height.0 - t.control_height.0 - gap;
        for i in 0..count {
            let x = bounds.x.0 + bounds.width.0 - (count - i) as f32 * (width + gap);
            let r = rect(x, button_y, width, t.control_height.0);
            if let Some(button) = self.child_mut::<Button>(i + 1) {
                button.bounds = r;
            }
        }
        let message = rect(
            bounds.x.0 + gap * 2.0,
            bounds.y.0 + gap * 2.0,
            bounds.width.0 - gap * 4.0,
            button_y - bounds.y.0 - gap * 3.0,
        );
        if let Some(label) = self.child_mut::<Label>(0) {
            label.bounds = message;
        }
    }
    fn next_focusable(&self, back: bool) -> Option<usize> {
        let n = self.children.len();
        (1..=n)
            .map(|k| {
                if back {
                    (self.focus_index + n - k % n) % n
                } else {
                    (self.focus_index + k) % n
                }
            })
            .find(|i| self.children[*i].focusable())
    }
    fn send(&mut self, index: usize, kind: InputKind, time_us: u64, ctx: &mut ViewContext) {
        if let Some(child) = self.children.get_mut(index) {
            child.handle(&InputEvent { time_us, kind }, ctx);
        }
    }
    fn move_focus(&mut self, index: usize, time_us: u64, ctx: &mut ViewContext) {
        if index != self.focus_index {
            self.cancel_enter(ctx);
            self.send(self.focus_index, InputKind::Blur, time_us, ctx);
            self.focus_index = index;
            self.send(index, InputKind::Focus, time_us, ctx);
        }
        self.focus_given = true;
        if let Some(w) = self.children[index].widgets() {
            ctx.set_focus(w.id)
        }
        ctx.invalidate(self.bounds);
    }
    /// Ends a press of Enter that focus has moved away from. The button under it must
    /// not stay pressed, because its Enter up will not come.
    fn cancel_enter(&mut self, ctx: &mut ViewContext) {
        if let Some(target) = self.enter_target.take()
            && let Some(child) = self.children.get_mut(target)
        {
            child.cancel_press(ctx);
        }
    }
    fn focused_takes(&self, virtual_key: u16) -> bool {
        self.children
            .get(self.focus_index)
            .is_some_and(|c| c.uses_key(virtual_key))
    }
    /// The open list of the focused control, which is above everything else.
    fn open_overlay(&self) -> Option<Rect> {
        self.children.get(self.focus_index)?.overlay_bounds()
    }
    fn child_at(&self, at: qe_api::Point) -> Option<usize> {
        self.children
            .iter()
            .rposition(|c| c.hit_bounds().is_some_and(|b| contains(b, at)))
    }
    /// Records what a child's activation means for the dialog.
    fn collect_activations(&mut self) {
        for i in 0..self.children.len() {
            if self.children[i].take_activated() {
                if let Some((_, result)) = self.closing.iter().find(|(c, _)| *c == i) {
                    let result = *result;
                    self.finish(DialogResult::Button(result));
                } else {
                    self.last_activated = Some(i);
                }
            }
        }
    }
    fn route(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        let time = event.time_us;
        match &event.kind {
            InputKind::Focus => {
                self.send(self.focus_index, InputKind::Focus, time, ctx);
                Handled::Yes
            }
            InputKind::Blur => {
                self.cancel_enter(ctx);
                self.mouse_target = None;
                self.send(self.focus_index, InputKind::Blur, time, ctx);
                Handled::Yes
            }
            // Alt chords belong to the system and the host, so the frame leaves them.
            _ if alt_chord(&event.kind) => self
                .children
                .get_mut(self.focus_index)
                .map_or(Handled::No, |child| child.handle(event, ctx)),
            InputKind::KeyDown {
                virtual_key: ESCAPE,
                ..
            } => {
                if self.focused_takes(ESCAPE) {
                    return self.children[self.focus_index].handle(event, ctx);
                }
                self.finish(DialogResult::Dismissed);
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: TAB,
                modifiers,
                ..
            } if modifiers.ctrl && !self.focused_takes(TAB) => {
                for child in &mut self.children {
                    if child.switch_tab(modifiers.shift, ctx) {
                        return Handled::Yes;
                    }
                }
                Handled::No
            }
            InputKind::KeyDown {
                virtual_key: TAB,
                modifiers,
                ..
            } if !self.children.is_empty() => {
                if self.focused_takes(TAB) {
                    return self.children[self.focus_index].handle(event, ctx);
                }
                if let Some(next) = self.next_focusable(modifiers.shift) {
                    self.move_focus(next, time, ctx);
                }
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: ENTER, ..
            } => {
                let target = if self.focused_takes(ENTER) {
                    Some(self.focus_index)
                } else {
                    self.default_index
                };
                let Some(target) = target else {
                    return Handled::No;
                };
                if !self.focused_takes(ENTER) {
                    // `default_index` is enough to make a button the default one.
                    let usable = self
                        .children
                        .get_mut(target)
                        .is_some_and(|child| child.mark_default());
                    if !usable {
                        self.problem = Some(format!(
                            "default_index {target} names no control that Enter can press"
                        ));
                        return Handled::No;
                    }
                }
                self.enter_target = Some(target);
                self.children
                    .get_mut(target)
                    .map_or(Handled::No, |c| c.handle(event, ctx))
            }
            InputKind::KeyUp {
                virtual_key: ENTER, ..
            } => {
                let Some(target) = self.enter_target.take() else {
                    return Handled::No;
                };
                self.children
                    .get_mut(target)
                    .map_or(Handled::No, |c| c.handle(event, ctx))
            }
            InputKind::MouseDown { at, .. } => {
                if self.open_overlay().is_some() {
                    self.mouse_target = Some(self.focus_index);
                    return self.children[self.focus_index].handle(event, ctx);
                }
                let Some(hit) = self.child_at(*at) else {
                    return Handled::No;
                };
                if self.children[hit].focusable() {
                    self.move_focus(hit, time, ctx);
                }
                self.mouse_target = Some(hit);
                self.children[hit].handle(event, ctx)
            }
            InputKind::MouseUp { .. } => {
                let Some(target) = self.mouse_target.take() else {
                    return Handled::No;
                };
                self.children
                    .get_mut(target)
                    .map_or(Handled::No, |c| c.handle(event, ctx))
            }
            InputKind::MouseMove { at } => {
                if let Some(target) = self.mouse_target {
                    return self
                        .children
                        .get_mut(target)
                        .map_or(Handled::No, |c| c.handle(event, ctx));
                }
                // An open list covers the controls under it, so they get no hover.
                if self.open_overlay().is_some_and(|list| contains(list, *at)) {
                    return self.children[self.focus_index].handle(event, ctx);
                }
                // Every child sees moves, so that hover clears on the control left behind.
                let mut handled = Handled::No;
                for child in &mut self.children {
                    if child.handle(event, ctx) == Handled::Yes {
                        handled = Handled::Yes;
                    }
                }
                handled
            }
            InputKind::Wheel { at, .. } => {
                let target = if self.open_overlay().is_some() {
                    Some(self.focus_index)
                } else {
                    self.child_at(*at)
                };
                target
                    .and_then(|t| self.children.get_mut(t))
                    .map_or(Handled::No, |c| c.handle(event, ctx))
            }
            InputKind::DpiChange { .. } | InputKind::Resize { .. } => Handled::No,
            _ => self
                .children
                .get_mut(self.focus_index)
                .map_or(Handled::No, |child| child.handle(event, ctx)),
        }
    }
}
impl View for DialogFrame {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        if self.closed {
            return Handled::No;
        }
        let handled = self.route(event, ctx);
        self.collect_activations();
        if self.closed {
            ctx.invalidate(self.bounds);
        }
        handled
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: t.background,
        });
        for child in &self.children {
            child.draw_base(list, theme, measure)
        }
        // Open lists come last, so that they paint above the controls below them.
        for child in &self.children {
            child.draw_overlay(list, theme, measure)
        }
    }
    fn layout(&mut self, bounds: Rect, _: &dyn TextMeasurer) {
        self.bounds = bounds;
        for child in &mut self.children {
            child.set_popup_area(bounds);
        }
        if self.alert_layout {
            self.layout_alert();
        }
    }
    fn widgets(&self) -> Option<Widget> {
        Some(Widget {
            char_boxes: Vec::new(),
            access_key: None,
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Dialog,
            name: self.title.clone(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children: self.children.iter().filter_map(|c| c.widgets()).collect(),
        })
    }
}
