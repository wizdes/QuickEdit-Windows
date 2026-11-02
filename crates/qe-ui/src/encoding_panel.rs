//! The Customize Encodings panel (parity row I39): 336 by 187, resizable, a checkbox
//! per encoding, and the buttons Select all, Clear all and Revert to default.
use crate::control::*;
use crate::controls::{
    CheckLook, ListThumb, THUMB_ROOM, ThumbDrag, check_box, clip, contains, focus_ring, inset,
    node, rect, state, text_line, unclip,
};
use crate::{Button, Control, DialogFrame, Label, LabelKind, UiTokens, child_id};
use qe_api::{
    Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, Rect, TextEncoding,
    TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole,
};

pub const ENCODING_PANEL_WIDTH: f32 = 336.0;
pub const ENCODING_PANEL_HEIGHT: f32 = 187.0;

/// A scrolling list with a checkbox on each row.
///
/// Up, Down, Page Up, Page Down, Home and End move the current row. Space flips it.
#[derive(Clone, Debug)]
pub struct CheckList {
    pub id: WidgetId,
    pub name: String,
    pub rows: Vec<(String, bool)>,
    pub bounds: Rect,
    pub current: usize,
    pub first_visible: usize,
    pub focused: bool,
    pub hover_row: Option<usize>,
    activated: bool,
    thumb_drag: Option<ThumbDrag>,
}
impl CheckList {
    pub fn new(id: WidgetId, name: impl Into<String>, rows: Vec<(String, bool)>) -> Self {
        Self {
            id,
            name: name.into(),
            rows,
            bounds: Rect::default(),
            current: 0,
            first_visible: 0,
            focused: false,
            hover_row: None,
            activated: false,
            thumb_drag: None,
        }
    }
    fn capacity(&self) -> usize {
        ((self.bounds.height.0 / UiTokens::sizes().check_row.0).floor() as usize).max(1)
    }
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let first = self.first_visible.min(self.rows.len());
        first..(first + self.capacity()).min(self.rows.len())
    }
    fn max_first(&self) -> usize {
        self.rows.len().saturating_sub(self.capacity())
    }
    fn thumb(&self, t: UiTokens) -> Option<ListThumb> {
        ListThumb::new(
            inset(self.bounds, 0.0, 2.0),
            self.visible_range().len(),
            self.rows.len(),
            self.first_visible.min(self.rows.len()),
            self.max_first(),
            t,
        )
    }
    fn row_at(&self, at: qe_api::Point) -> Option<usize> {
        if self
            .thumb(UiTokens::sizes())
            .is_some_and(|thumb| contains(thumb.strip, at))
        {
            return None;
        }
        self.visible_range()
            .find(|i| contains(self.row_rect(*i), at))
    }
    fn row_rect(&self, i: usize) -> Rect {
        let h = UiTokens::sizes().check_row.0;
        rect(
            self.bounds.x.0,
            self.bounds.y.0 + (i as f32 - self.first_visible as f32) * h,
            self.bounds.width.0,
            h,
        )
    }
    fn move_to(&mut self, i: usize, c: &mut ViewContext) {
        if self.rows.is_empty() {
            return;
        }
        self.current = i.min(self.rows.len() - 1);
        let capacity = self.capacity();
        if self.current < self.first_visible {
            self.first_visible = self.current;
        } else if self.current >= self.first_visible + capacity {
            self.first_visible = self.current + 1 - capacity;
        }
        c.invalidate(self.bounds);
    }
    fn flip(&mut self, i: usize, c: &mut ViewContext) {
        if let Some(row) = self.rows.get_mut(i) {
            row.1 = !row.1;
            self.activated = true;
            c.invalidate(self.bounds);
        }
    }
}
impl View for CheckList {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        let page = self.capacity();
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                if self.thumb_drag.take().is_some() {
                    c.capture_mouse(false);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key,
                repeat,
                ..
            } if self.focused => {
                match *virtual_key {
                    UP => self.move_to(self.current.saturating_sub(1), c),
                    DOWN => self.move_to(self.current + 1, c),
                    PAGE_UP => self.move_to(self.current.saturating_sub(page), c),
                    PAGE_DOWN => self.move_to(self.current + page, c),
                    HOME => self.move_to(0, c),
                    END => self.move_to(self.rows.len().saturating_sub(1), c),
                    SPACE => {
                        if !*repeat {
                            self.flip(self.current, c);
                        }
                    }
                    _ => return Handled::No,
                }
                Handled::Yes
            }
            InputKind::Wheel { at, delta_y, .. } if contains(self.bounds, *at) => {
                let rows = (-delta_y / WHEEL_NOTCH * 3.0).round() as isize;
                let max = self.rows.len().saturating_sub(self.capacity()) as isize;
                self.first_visible =
                    (self.first_visible as isize + rows).clamp(0, max.max(0)) as usize;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                if let Some(drag) = self.thumb_drag {
                    if let Some(thumb) = self.thumb(UiTokens::sizes()) {
                        let first = thumb.dragged_to(drag, at.y.0);
                        if first != self.first_visible {
                            self.first_visible = first;
                            c.invalidate(self.bounds);
                        }
                    }
                    return Handled::Yes;
                }
                let hit = self.row_at(*at);
                if hit != self.hover_row {
                    self.hover_row = hit;
                    c.invalidate(self.bounds);
                }
                if hit.is_some() {
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                c.set_focus(self.id);
                // The thumb takes its own presses. Changes apply at once here, so a
                // press meant for scrolling must never flip the row under it.
                if let Some(thumb) = self.thumb(UiTokens::sizes())
                    && contains(thumb.strip, *at)
                {
                    match thumb.press(at.y.0) {
                        Ok(drag) => {
                            self.thumb_drag = Some(drag);
                            c.capture_mouse(true);
                        }
                        Err(first) => self.first_visible = first,
                    }
                    c.invalidate(self.bounds);
                    return Handled::Yes;
                }
                if let Some(i) = self.row_at(*at) {
                    self.current = i;
                    self.flip(i, c);
                }
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.thumb_drag.is_some() => {
                self.thumb_drag = None;
                c.capture_mouse(false);
                c.invalidate(self.bounds);
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
        clip(l, self.bounds);
        let thumb = self.thumb(t);
        for i in self.visible_range() {
            let mut r = self.row_rect(i);
            if thumb.is_some() {
                r.width.0 -= THUMB_ROOM;
            }
            let (label, checked) = &self.rows[i];
            let current = self.focused && i == self.current;
            let mut ink = t.text;
            if self.hover_row == Some(i) {
                l.push(DrawCommand::RoundedRect {
                    rect: inset(r, 2.0, 0.0),
                    radius: t.radius,
                    color: t.hover,
                });
                ink = t.hover_text;
            }
            let box_r = rect(
                r.x.0 + t.inset.0,
                r.y.0 + (r.height.0 - t.check_size.0) / 2.0,
                t.check_size.0,
                t.check_size.0,
            );
            check_box(
                l,
                m,
                t,
                box_r,
                CheckLook {
                    checked: *checked,
                    enabled: true,
                    hovered: false,
                    pressed: false,
                },
            );
            text_line(
                l,
                m,
                t,
                box_r.x.0 + t.check_size.0 + t.spacing.0,
                r.y.0,
                r.height.0,
                label,
                ink,
            );
            if current {
                let ring = inset(r, 2.0, 0.0);
                if self.hover_row == Some(i) {
                    crate::controls::focus_ring_on_fill(l, ring, t);
                } else {
                    focus_ring(l, ring, t);
                }
            }
        }
        unclip(l);
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: t.border,
        });
        if let Some(thumb) = thumb {
            thumb.paint(l, t, self.thumb_drag.is_some());
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let visible = self.visible_range();
        let on_row = self.focused && !self.rows.is_empty();
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Group,
            &self.name,
            Some(format!("{} rows", self.rows.len())),
            state(true, self.focused && !on_row, false, false),
            self.rows
                .iter()
                .enumerate()
                .map(|(i, (label, checked))| {
                    node(
                        child_id(self.id, i as u64),
                        if visible.contains(&i) {
                            self.row_rect(i)
                        } else {
                            Rect::default()
                        },
                        WidgetRole::Checkbox,
                        label,
                        None,
                        state(true, on_row && i == self.current, false, *checked),
                        vec![],
                    )
                })
                .collect(),
        ))
    }
}
impl Control for CheckList {
    fn focusable(&self) -> bool {
        !self.rows.is_empty()
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        virtual_key == SPACE
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
}

const LIST: usize = 0;
const SELECT_ALL: usize = 1;
const CLEAR_ALL: usize = 2;
const REVERT: usize = 3;
const NOTE: usize = 4;
/// The Mac panel's line above the list.
const NOTE_TEXT: &str = "Select the encodings you want to appear in text encoding menus.";

/// The encoding panel as a painted dialog. Changes apply as they happen, as on the
/// Mac. The caller reads `enabled_encodings` after any event and closes on `closed`.
pub struct EncodingPanel {
    pub frame: DialogFrame,
    enabled: Vec<TextEncoding>,
}
impl EncodingPanel {
    pub fn new(id: WidgetId, enabled: &[TextEncoding], bounds: Rect) -> Self {
        let mut frame = DialogFrame::new(id, "Customize Encodings List");
        frame.bounds = bounds;
        let rows = TextEncoding::ALL
            .iter()
            .map(|e| (e.name().to_owned(), enabled.contains(e)))
            .collect();
        frame.add(Box::new(CheckList::new(
            WidgetId(id.0 + 1),
            "Encodings",
            rows,
        )));
        for (i, label) in ["Select all", "Clear all", "Revert to default"]
            .into_iter()
            .enumerate()
        {
            frame.add(Box::new(Button::new(WidgetId(id.0 + 2 + i as u64), label)));
        }
        let mut note = Label::new(WidgetId(id.0 + 5), NOTE_TEXT, LabelKind::Plain);
        note.wrap = true;
        frame.add(Box::new(note));
        let mut panel = Self {
            frame,
            enabled: enabled.to_vec(),
        };
        panel.arrange();
        panel
    }
    fn arrange(&mut self) {
        let t = UiTokens::sizes();
        let b = self.frame.bounds;
        let gap = t.inset.0;
        let button_y = b.y.0 + b.height.0 - t.control_height.0 - gap;
        // Two lines of text at this size. The Mac's smaller font fits in one.
        let note_height = 36.0;
        if let Some(note) = self.frame.child_mut::<Label>(NOTE) {
            note.bounds = rect(b.x.0 + gap, b.y.0 + 4.0, b.width.0 - 2.0 * gap, note_height);
        }
        let list_y = b.y.0 + 4.0 + note_height;
        if let Some(list) = self.frame.child_mut::<CheckList>(LIST) {
            list.bounds = rect(
                b.x.0 + gap,
                list_y,
                b.width.0 - 2.0 * gap,
                button_y - list_y - gap,
            );
        }
        // The third label is longer, so it takes more of the row.
        let room = (b.width.0 - 4.0 * gap).max(0.0);
        let short = room * 0.29;
        let long = room * 0.42;
        let mut x = b.x.0 + gap;
        for (index, width) in [(SELECT_ALL, short), (CLEAR_ALL, short), (REVERT, long)] {
            if let Some(button) = self.frame.child_mut::<Button>(index) {
                button.bounds = rect(x, button_y, width, t.control_height.0);
            }
            x += width + gap;
        }
    }
    /// The enabled list. As on the Mac, it keeps the order it came in (row I42) until
    /// the user changes it. A checkbox or Select all then gives the panel's order, and
    /// Revert gives the default list's order.
    pub fn enabled_encodings(&self) -> Vec<TextEncoding> {
        self.enabled.clone()
    }
    fn read_checks(&self) -> Vec<TextEncoding> {
        let Some(list) = self.frame.child::<CheckList>(LIST) else {
            return vec![];
        };
        TextEncoding::ALL
            .iter()
            .zip(&list.rows)
            .filter(|(_, row)| row.1)
            .map(|(e, _)| *e)
            .collect()
    }
    pub fn closed(&self) -> bool {
        self.frame.closed
    }
    fn set_all(&mut self, pick: impl Fn(TextEncoding) -> bool, c: &mut ViewContext) {
        if let Some(list) = self.frame.child_mut::<CheckList>(LIST) {
            for (row, encoding) in list.rows.iter_mut().zip(TextEncoding::ALL) {
                row.1 = pick(*encoding);
            }
            c.invalidate(list.bounds);
        }
    }
}
impl View for EncodingPanel {
    fn handle(&mut self, event: &InputEvent, c: &mut ViewContext) -> Handled {
        let handled = self.frame.handle(event, c);
        match self.frame.take_last_activated() {
            Some(LIST) => self.enabled = self.read_checks(),
            Some(SELECT_ALL) => {
                self.set_all(|_| true, c);
                self.enabled = self.read_checks();
            }
            Some(CLEAR_ALL) => {
                self.set_all(|_| false, c);
                self.enabled.clear();
            }
            Some(REVERT) => {
                self.set_all(|e| TextEncoding::DEFAULT_LIST.contains(&e), c);
                self.enabled = TextEncoding::DEFAULT_LIST.to_vec();
            }
            _ => {}
        }
        handled
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        self.frame.draw(list, theme, measure);
    }
    fn layout(&mut self, bounds: Rect, measure: &dyn TextMeasurer) {
        self.frame.layout(bounds, measure);
        self.arrange();
    }
    fn widgets(&self) -> Option<Widget> {
        self.frame.widgets()
    }
}
