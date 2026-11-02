use crate::control::*;
use crate::{Control, UiTokens, child_id, place_popup};
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers, MouseButton,
    Point, Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole,
    WidgetStates,
};
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation;

/// The checkmark, U+E73E in Segoe Fluent Icons.
pub const CHECK_ICON: char = '\u{e73e}';
/// ChevronDownSmall, U+E96E: the same caret as the top row.
pub const CARET_ICON: char = '\u{e96e}';
/// The arrow on a menu row with a submenu.
pub const SUBMENU_ICON: char = '\u{e76c}';

pub(crate) fn contains(r: Rect, p: Point) -> bool {
    p.x.0 >= r.x.0 && p.y.0 >= r.y.0 && p.x.0 < r.x.0 + r.width.0 && p.y.0 < r.y.0 + r.height.0
}
pub(crate) fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(width.max(0.0)),
        height: Dip(height.max(0.0)),
    }
}
pub(crate) fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.0.min(b.x.0);
    let y = a.y.0.min(b.y.0);
    let right = (a.x.0 + a.width.0).max(b.x.0 + b.width.0);
    let bottom = (a.y.0 + a.height.0).max(b.y.0 + b.height.0);
    rect(x, y, right - x, bottom - y)
}
pub(crate) fn inset(r: Rect, x: f32, y: f32) -> Rect {
    rect(
        r.x.0 + x,
        r.y.0 + y,
        r.width.0 - 2.0 * x,
        r.height.0 - 2.0 * y,
    )
}
/// Text centered on its line in a box that starts at `box_y`. `at` on a `TextRun`
/// is the baseline, so the position comes from the measurer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text_line(
    list: &mut DisplayList,
    m: &dyn TextMeasurer,
    t: UiTokens,
    x: f32,
    box_y: f32,
    box_height: f32,
    value: &str,
    color: Color,
) {
    if value.is_empty() {
        return;
    }
    let metrics = m.measure(value, t.measure_font);
    list.push(DrawCommand::TextRun {
        at: Point {
            x: Dip(x),
            y: Dip(box_y + (box_height - metrics.height.0) / 2.0 + metrics.baseline.0),
        },
        text: value.into(),
        font: t.font.into(),
        font_size: t.text_size,
        color,
    });
}
pub(crate) fn text_width(m: &dyn TextMeasurer, t: UiTokens, value: &str) -> f32 {
    m.measure(value, t.measure_font).width.0
}
pub(crate) fn clip(list: &mut DisplayList, r: Rect) {
    list.push(DrawCommand::PushClip(r));
}
pub(crate) fn unclip(list: &mut DisplayList) {
    list.push(DrawCommand::PopClip);
}
/// A real icon from the icon font, centered in `area`.
pub(crate) fn icon(
    list: &mut DisplayList,
    m: &dyn TextMeasurer,
    t: UiTokens,
    area: Rect,
    glyph: char,
    color: Color,
) {
    let metrics = m.measure(&glyph.to_string(), t.icon_measure_font);
    list.push(DrawCommand::Icon {
        at: Point {
            x: Dip(area.x.0 + (area.width.0 - metrics.width.0) / 2.0),
            y: Dip(area.y.0 + (area.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0),
        },
        icon_id: format!("{:04X}", glyph as u32),
        font_size: t.icon_size,
        color,
    });
}
pub(crate) fn node(
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
        selection: None,
        range_value: None,
        description: None,
        children,
    }
}
pub(crate) fn state(enabled: bool, focused: bool, selected: bool, checked: bool) -> WidgetStates {
    WidgetStates {
        enabled,
        focused,
        selected,
        checked,
        ..Default::default()
    }
}
/// The focus ring on a selected or hovered fill, in a color that shows against it.
pub(crate) fn focus_ring_on_fill(list: &mut DisplayList, r: Rect, t: UiTokens) {
    list.push(DrawCommand::StrokeRect {
        rect: inset(r, 1.0, 1.0),
        radius: t.radius,
        width: t.focus_width,
        color: t.focus_on_selected,
    });
}
/// The bar at the left of a selected row.
pub(crate) fn row_mark(list: &mut DisplayList, r: Rect, t: UiTokens) {
    list.push(DrawCommand::RoundedRect {
        rect: rect(r.x.0 + 2.0, r.y.0 + 6.0, 3.0, r.height.0 - 12.0),
        radius: Dip(1.5),
        color: t.selected_mark,
    });
}
/// How far a list's rows stop short of its right edge, to leave the thumb clear.
pub(crate) const THUMB_ROOM: f32 = 8.0;
const THUMB_STRIP: f32 = 12.0;

/// A press on a list's thumb that is still held.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ThumbDrag {
    pub start_y: f32,
    pub start_first: usize,
}
/// The small scroll thumb of a list. Its strip at the right edge takes its own mouse
/// presses, so that a press there never reaches the row under it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListThumb {
    pub strip: Rect,
    pub thumb: Rect,
    pub shown: usize,
    pub max_first: usize,
    pub first: usize,
}
impl ListThumb {
    /// `None` when every row fits. `track` is the height the thumb runs along.
    pub fn new(
        track: Rect,
        shown: usize,
        total: usize,
        first: usize,
        max_first: usize,
        t: UiTokens,
    ) -> Option<Self> {
        if shown >= total || total == 0 {
            return None;
        }
        let height = (track.height.0 * shown as f32 / total as f32)
            .max(t.scrollbar_wide.0 * 2.0)
            .min(track.height.0);
        let room = (track.height.0 - height).max(0.0);
        let part = first as f32 / max_first.max(1) as f32;
        let right = track.x.0 + track.width.0;
        Some(Self {
            strip: rect(right - THUMB_STRIP, track.y.0, THUMB_STRIP, track.height.0),
            thumb: rect(
                right - THUMB_STRIP,
                track.y.0 + room * part.min(1.0),
                THUMB_STRIP,
                height,
            ),
            shown,
            max_first,
            first,
        })
    }
    pub fn paint(&self, l: &mut DisplayList, t: UiTokens, dragging: bool) {
        let width = if dragging {
            t.scrollbar_wide.0 / 2.0
        } else {
            t.scrollbar_thin.0
        };
        l.push(DrawCommand::RoundedRect {
            rect: rect(
                self.strip.x.0 + THUMB_STRIP - width - 2.0,
                self.thumb.y.0,
                width,
                self.thumb.height.0,
            ),
            radius: Dip(width / 2.0),
            color: t.thumb,
        });
    }
    /// What a press in the strip does: a drag on the thumb, or a page above or below.
    pub fn press(&self, y: f32) -> Result<ThumbDrag, usize> {
        if y >= self.thumb.y.0 && y < self.thumb.y.0 + self.thumb.height.0 {
            Ok(ThumbDrag {
                start_y: y,
                start_first: self.first,
            })
        } else if y < self.thumb.y.0 {
            Err(self.first.saturating_sub(self.shown))
        } else {
            Err((self.first + self.shown).min(self.max_first))
        }
    }
    pub fn dragged_to(&self, drag: ThumbDrag, y: f32) -> usize {
        let room = (self.strip.height.0 - self.thumb.height.0).max(1.0);
        let rows = (y - drag.start_y) / room * self.max_first as f32;
        ((drag.start_first as f32 + rows).round().max(0.0) as usize).min(self.max_first)
    }
}
pub(crate) fn focus_ring(list: &mut DisplayList, r: Rect, t: UiTokens) {
    list.push(DrawCommand::StrokeRect {
        rect: inset(r, 1.0, 1.0),
        radius: t.radius,
        width: t.focus_width,
        color: t.focus,
    });
}
/// Updates a hover flag. It repaints only on a change and leaves a move elsewhere
/// for the control under the pointer.
fn track_hover(hovered: &mut bool, bounds: Rect, at: Point, c: &mut ViewContext) -> Handled {
    let now = contains(bounds, at);
    let changed = *hovered != now;
    *hovered = now;
    if changed {
        c.invalidate(bounds);
    }
    if now || changed {
        Handled::Yes
    } else {
        Handled::No
    }
}
fn scroll_notches(delta_y: f32) -> f32 {
    delta_y / WHEEL_NOTCH
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
    activated: bool,
    captured: bool,
    key_down: Option<u16>,
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
            activated: false,
            captured: false,
            key_down: None,
        }
    }
    fn activate(&mut self, ctx: &mut ViewContext) {
        self.clicks += 1;
        self.activated = true;
        ctx.invalidate(self.bounds);
    }
    fn release_capture(&mut self, c: &mut ViewContext) {
        if self.captured {
            self.captured = false;
            c.capture_mouse(false);
        }
    }
}
impl View for Button {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.pressed = false;
                self.key_down = None;
                self.release_capture(c);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => track_hover(&mut self.hovered, self.bounds, *at, c),
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if self.enabled && contains(self.bounds, *at) => {
                self.pressed = true;
                self.captured = true;
                c.capture_mouse(true);
                c.set_focus(self.id);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } if self.pressed || self.captured => {
                let click = self.pressed && self.enabled && contains(self.bounds, *at);
                self.pressed = false;
                self.key_down = None;
                self.release_capture(c);
                if click {
                    self.activate(c);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key,
                repeat,
                ..
            } if self.enabled
                && ((*virtual_key == SPACE && self.focused)
                    || (*virtual_key == ENTER && (self.focused || self.is_default))) =>
            {
                if !*repeat && self.key_down.is_none() {
                    self.pressed = true;
                    self.key_down = Some(*virtual_key);
                    c.invalidate(self.bounds);
                }
                Handled::Yes
            }
            InputKind::KeyUp { virtual_key, .. } if self.key_down == Some(*virtual_key) => {
                self.key_down = None;
                self.pressed = false;
                if self.enabled {
                    self.activate(c);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let (fill, ink) = if !self.enabled {
            (t.disabled_fill, t.disabled)
        } else if self.pressed {
            (t.pressed, t.pressed_text)
        } else if self.hovered {
            (t.hover, t.hover_text)
        } else {
            (t.surface, t.text)
        };
        // Hover and press fills are translucent, so the surface goes under them.
        l.push(DrawCommand::RoundedRect {
            rect: self.bounds,
            radius: t.radius,
            color: if self.enabled {
                t.surface
            } else {
                t.disabled_fill
            },
        });
        if self.enabled && (self.pressed || self.hovered) {
            l.push(DrawCommand::RoundedRect {
                rect: self.bounds,
                radius: t.radius,
                color: fill,
            });
        }
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: if !self.enabled {
                t.disabled_border
            } else if self.is_default {
                t.accent
            } else {
                t.border
            },
        });
        if let Some(id) = &self.icon {
            if let Some(glyph) = u32::from_str_radix(id, 16).ok().and_then(char::from_u32) {
                icon(l, m, t, self.bounds, glyph, ink);
            } else {
                // Sent as it is, so the renderer reports the bad id instead of a
                // wrong picture.
                l.push(DrawCommand::Icon {
                    at: Point {
                        x: self.bounds.x,
                        y: self.bounds.y,
                    },
                    icon_id: id.clone(),
                    font_size: t.icon_size,
                    color: ink,
                });
            }
        } else {
            let width = text_width(m, t, &self.label);
            let x = self.bounds.x.0 + ((self.bounds.width.0 - width) / 2.0).max(t.inset.0);
            clip(l, inset(self.bounds, 2.0, 2.0));
            text_line(
                l,
                m,
                t,
                x,
                self.bounds.y.0,
                self.bounds.height.0,
                &self.label,
                ink,
            );
            unclip(l);
        }
        if self.focused && self.enabled {
            if self.pressed || self.hovered {
                focus_ring_on_fill(l, self.bounds, t);
            } else {
                focus_ring(l, self.bounds, t);
            }
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
impl Control for Button {
    fn focusable(&self) -> bool {
        self.enabled
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        self.enabled && matches!(virtual_key, ENTER | SPACE)
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
    fn cancel_press(&mut self, c: &mut ViewContext) {
        if self.key_down.take().is_some() {
            self.pressed = false;
            c.invalidate(self.bounds);
        }
    }
    fn mark_default(&mut self) -> bool {
        self.is_default = true;
        true
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
    captured: bool,
    activated: bool,
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
            captured: false,
            activated: false,
        }
    }
    fn release_capture(&mut self, c: &mut ViewContext) {
        if self.captured {
            self.captured = false;
            c.capture_mouse(false);
        }
    }
    fn toggle(&mut self, c: &mut ViewContext) {
        self.checked = !self.checked;
        self.activated = true;
        c.invalidate(self.bounds);
    }
}
impl View for Checkbox {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.pressed = false;
                self.release_capture(c);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => track_hover(&mut self.hovered, self.bounds, *at, c),
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if self.enabled && contains(self.bounds, *at) => {
                self.pressed = true;
                self.captured = true;
                c.capture_mouse(true);
                c.set_focus(self.id);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } if self.pressed || self.captured => {
                let click = self.pressed && self.enabled && contains(self.bounds, *at);
                self.pressed = false;
                self.release_capture(c);
                if click {
                    self.toggle(c);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: SPACE,
                repeat,
                ..
            } if self.enabled && self.focused => {
                if !*repeat {
                    self.toggle(c);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let box_r = rect(
            self.bounds.x.0,
            self.bounds.y.0 + (self.bounds.height.0 - t.check_size.0) / 2.0,
            t.check_size.0,
            t.check_size.0,
        );
        check_box(
            l,
            m,
            t,
            box_r,
            CheckLook {
                checked: self.checked,
                enabled: self.enabled,
                hovered: self.hovered,
                pressed: self.pressed,
            },
        );
        let x = self.bounds.x.0 + t.check_size.0 + t.spacing.0;
        clip(l, self.bounds);
        text_line(
            l,
            m,
            t,
            x,
            self.bounds.y.0,
            self.bounds.height.0,
            &self.label,
            if self.enabled { t.text } else { t.disabled },
        );
        unclip(l);
        if self.focused && self.enabled {
            focus_ring(l, inset(box_r, -3.0, -3.0), t)
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
impl Control for Checkbox {
    fn focusable(&self) -> bool {
        self.enabled
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        self.enabled && virtual_key == SPACE
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CheckLook {
    pub checked: bool,
    pub enabled: bool,
    pub hovered: bool,
    pub pressed: bool,
}
/// The square of a checkbox. A checked box shows hover and press in its accent fill.
pub(crate) fn check_box(
    l: &mut DisplayList,
    m: &dyn TextMeasurer,
    t: UiTokens,
    box_r: Rect,
    look: CheckLook,
) {
    let base = if !look.enabled {
        if look.checked {
            t.disabled
        } else {
            t.disabled_fill
        }
    } else if look.checked {
        if look.pressed {
            t.accent_pressed
        } else if look.hovered {
            t.accent_hover
        } else {
            t.accent
        }
    } else {
        t.surface
    };
    l.push(DrawCommand::RoundedRect {
        rect: box_r,
        radius: t.radius,
        color: base,
    });
    if look.enabled && !look.checked && (look.pressed || look.hovered) {
        l.push(DrawCommand::RoundedRect {
            rect: box_r,
            radius: t.radius,
            color: if look.pressed { t.pressed } else { t.hover },
        });
    }
    l.push(DrawCommand::StrokeRect {
        rect: box_r,
        radius: t.radius,
        width: Dip(1.0),
        color: if !look.enabled {
            t.disabled_border
        } else if look.checked {
            base
        } else {
            t.border
        },
    });
    if look.checked {
        icon(
            l,
            m,
            t,
            box_r,
            CHECK_ICON,
            if look.enabled {
                t.on_accent
            } else {
                t.disabled_fill
            },
        );
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
    /// Breaks long text into lines, from the top of the bounds. All text is clipped
    /// to the bounds.
    pub wrap: bool,
}
impl Label {
    pub fn new(id: WidgetId, text: impl Into<String>, kind: LabelKind) -> Self {
        Self {
            id,
            text: text.into(),
            kind,
            bounds: Rect::default(),
            wrap: false,
        }
    }
}
/// Breaks text at spaces, or inside a word that is wider than the line.
pub fn wrap_lines(value: &str, width: f32, m: &dyn TextMeasurer, font: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in value.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_word_bounds() {
            let candidate = format!("{line}{word}");
            if m.measure(&candidate, font).width.0 <= width {
                line = candidate;
                continue;
            }
            if !line.trim().is_empty() {
                lines.push(line.trim_end().to_owned());
                line = String::new();
            }
            if word.trim().is_empty() {
                continue;
            }
            for g in word.graphemes(true) {
                let candidate = format!("{line}{g}");
                if !line.is_empty() && m.measure(&candidate, font).width.0 > width {
                    lines.push(std::mem::take(&mut line));
                    line = g.to_owned();
                } else {
                    line = candidate;
                }
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    lines
}
impl View for Label {
    fn handle(&mut self, _: &InputEvent, _: &mut ViewContext) -> Handled {
        Handled::No
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let color = match self.kind {
            LabelKind::Plain => t.text,
            LabelKind::Group => theme.group_header,
            LabelKind::Hint => t.hint,
            LabelKind::Warning => t.warning,
        };
        clip(l, self.bounds);
        if self.wrap {
            let mut y = self.bounds.y.0;
            let bottom = y + self.bounds.height.0;
            for line in wrap_lines(&self.text, self.bounds.width.0, m, t.measure_font) {
                if y >= bottom {
                    break;
                }
                let height = m.measure(&line, t.measure_font).height.0;
                text_line(l, m, t, self.bounds.x.0, y, height, &line, color);
                y += height;
            }
        } else {
            text_line(
                l,
                m,
                t,
                self.bounds.x.0,
                self.bounds.y.0,
                self.bounds.height.0,
                &self.text,
                color,
            );
        }
        unclip(l);
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
impl Control for Label {
    fn focusable(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug)]
pub struct TabBar {
    pub id: WidgetId,
    /// The name a screen reader gives this list of tabs.
    pub name: String,
    pub tabs: Vec<String>,
    pub selected: usize,
    pub focused: bool,
    pub bounds: Rect,
    pub hovered: Option<usize>,
    pub pressed: Option<usize>,
    activated: bool,
}
impl TabBar {
    /// `name` is what a screen reader says for the whole list of tabs.
    pub fn new(id: WidgetId, name: impl Into<String>, tabs: Vec<String>) -> Self {
        let name = name.into();
        debug_assert!(!name.is_empty(), "a list of tabs needs a name");
        Self {
            id,
            name,
            tabs,
            selected: 0,
            focused: false,
            bounds: Rect::default(),
            hovered: None,
            pressed: None,
            activated: false,
        }
    }
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    fn tab_rect(&self, i: usize) -> Rect {
        let w = self.bounds.width.0 / self.tabs.len().max(1) as f32;
        rect(
            self.bounds.x.0 + i as f32 * w,
            self.bounds.y.0,
            w,
            self.bounds.height.0,
        )
    }
    fn tab_at(&self, at: Point) -> Option<usize> {
        (0..self.tabs.len()).find(|i| contains(self.tab_rect(*i), at))
    }
    fn select(&mut self, i: usize, c: &mut ViewContext) {
        if i != self.selected {
            self.activated = true;
        }
        self.selected = i;
        c.invalidate(self.bounds);
    }
}
impl View for TabBar {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        let n = self.tabs.len();
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                if self.pressed.take().is_some() {
                    c.capture_mouse(false);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: LEFT, ..
            } if self.focused && n > 0 => {
                self.select((self.selected + n - 1) % n, c);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: RIGHT, ..
            } if self.focused && n > 0 => {
                self.select((self.selected + 1) % n, c);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: HOME, ..
            } if self.focused && n > 0 => {
                self.select(0, c);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: END, ..
            } if self.focused && n > 0 => {
                self.select(n - 1, c);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                let now = self.tab_at(*at);
                let changed = now != self.hovered;
                self.hovered = now;
                if changed {
                    c.invalidate(self.bounds);
                }
                if changed || now.is_some() {
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) && n > 0 => {
                if let Some(i) = self.tab_at(*at) {
                    self.pressed = Some(i);
                    c.capture_mouse(true);
                    self.select(i, c);
                }
                c.set_focus(self.id);
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.pressed.is_some() => {
                self.pressed = None;
                c.capture_mouse(false);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        for (i, s) in self.tabs.iter().enumerate() {
            let r = self.tab_rect(i);
            let mut ink = t.text;
            if i == self.selected {
                l.push(DrawCommand::RoundedRect {
                    rect: r,
                    radius: t.radius,
                    color: t.selected,
                });
                l.push(DrawCommand::RoundedRect {
                    rect: rect(
                        r.x.0 + t.inset.0,
                        r.y.0 + r.height.0 - 5.0,
                        r.width.0 - 2.0 * t.inset.0,
                        3.0,
                    ),
                    radius: Dip(1.5),
                    color: t.selected_mark,
                });
                ink = t.selected_text;
            } else if self.pressed == Some(i) {
                l.push(DrawCommand::RoundedRect {
                    rect: r,
                    radius: t.radius,
                    color: t.pressed,
                });
                ink = t.pressed_text;
            } else if self.hovered == Some(i) {
                l.push(DrawCommand::RoundedRect {
                    rect: r,
                    radius: t.radius,
                    color: t.hover,
                });
                ink = t.hover_text;
            }
            clip(l, r);
            text_line(l, m, t, r.x.0 + t.inset.0, r.y.0, r.height.0, s, ink);
            unclip(l);
            if self.focused && i == self.selected {
                focus_ring_on_fill(l, r, t)
            }
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let on_tab = self.focused && self.selected < self.tabs.len();
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::TabList,
            &self.name,
            None,
            state(true, self.focused && !on_tab, false, false),
            self.tabs
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    node(
                        child_id(self.id, i as u64),
                        self.tab_rect(i),
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
impl Control for TabBar {
    fn focusable(&self) -> bool {
        !self.tabs.is_empty()
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
    fn switch_tab(&mut self, back: bool, c: &mut ViewContext) -> bool {
        let n = self.tabs.len();
        if n == 0 {
            return false;
        }
        let next = if back {
            (self.selected + n - 1) % n
        } else {
            (self.selected + 1) % n
        };
        self.select(next, c);
        true
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
    /// The name a screen reader gives this table.
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<TableRow>,
    pub bounds: Rect,
    pub selected: usize,
    pub first_visible: usize,
    pub focused: bool,
    /// The cell that Left and Right move between, in a table with two columns.
    pub focused_column: usize,
    /// How many times Enter was pressed on a row.
    pub activations: usize,
    activated: bool,
}
impl Table {
    /// `name` is what a screen reader says for the table.
    pub fn new(
        id: WidgetId,
        name: impl Into<String>,
        headers: Vec<String>,
        rows: Vec<TableRow>,
    ) -> Self {
        let name = name.into();
        debug_assert!(!name.is_empty(), "a table needs a name");
        let selected = rows.iter().position(|r| !r.section).unwrap_or(0);
        Self {
            id,
            name,
            headers,
            rows,
            bounds: Rect::default(),
            selected,
            first_visible: 0,
            focused: false,
            focused_column: 0,
            activations: 0,
            activated: false,
        }
    }
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    fn capacity(&self) -> usize {
        let row = UiTokens::sizes().row.0;
        ((self.bounds.height.0 - row).max(0.0) / row).floor() as usize
    }
    /// The rows that fit whole below the header. A row cut by the bottom edge is not
    /// visible, and it is not painted.
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let first = self.first_visible.min(self.rows.len());
        first..(first + self.capacity()).min(self.rows.len())
    }
    fn max_first(&self) -> usize {
        self.rows.len().saturating_sub(self.capacity().max(1))
    }
    fn selectable(&self, i: usize) -> bool {
        self.rows.get(i).is_some_and(|r| !r.section)
    }
    /// The nearest row that can be selected at `from`, then toward the end `step`
    /// points to, then the other way.
    fn nearest_selectable(&self, from: usize, down: bool) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        let from = from.min(last);
        let forward = (from..=last).find(|i| self.selectable(*i));
        let backward = (0..=from).rev().find(|i| self.selectable(*i));
        if down {
            forward.or(backward)
        } else {
            backward.or(forward)
        }
    }
    fn select(&mut self, target: usize, down: bool, c: &mut ViewContext) {
        if let Some(i) = self.nearest_selectable(target, down) {
            if i != self.selected {
                self.activated = true;
            }
            self.selected = i;
            let capacity = self.capacity().max(1);
            if i < self.first_visible {
                self.first_visible = i;
            } else if i >= self.first_visible + capacity {
                self.first_visible = i + 1 - capacity;
            }
            // Keeps a section's title on screen with its first row.
            if i == self.first_visible && i > 0 && self.rows[i - 1].section {
                self.first_visible = i - 1;
            }
            if i >= self.first_visible + capacity {
                self.first_visible = i + 1 - capacity;
            }
            c.invalidate(self.bounds);
        }
    }
    /// Scrolls to the very top, with any section titles there, if the selected row
    /// still shows. Titles must not push the selection out of a short table.
    fn show_top(&mut self, c: &mut ViewContext) {
        if self.selected < self.capacity().max(1) && self.first_visible != 0 {
            self.first_visible = 0;
            c.invalidate(self.bounds);
        }
    }
    fn row_rect(&self, i: usize) -> Rect {
        let row = UiTokens::sizes().row.0;
        rect(
            self.bounds.x.0,
            self.bounds.y.0 + row * (i as f32 - self.first_visible as f32 + 1.0),
            self.bounds.width.0,
            row,
        )
    }
    /// The left edge of a column. Headings and cells share it.
    fn column_x(&self, column: usize) -> f32 {
        if column == 0 {
            return self.bounds.x.0;
        }
        let rest = self.headers.len().max(2) - 1;
        self.bounds.x.0 + self.bounds.width.0 * (0.6 + 0.4 * (column - 1) as f32 / rest as f32)
    }
    fn cell_rect(&self, row: Rect, column: usize) -> Rect {
        let columns = self.headers.len().max(1);
        let x = self.column_x(column);
        let right = if column + 1 < columns {
            self.column_x(column + 1)
        } else {
            self.bounds.x.0 + self.bounds.width.0
        };
        rect(x, row.y.0, right - x, row.height.0)
    }
}
impl View for Table {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        let page = self.capacity().max(1);
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown { virtual_key, .. } if self.focused && !self.rows.is_empty() => {
                match *virtual_key {
                    UP => {
                        if let Some(i) = self.selected.checked_sub(1)
                            && (0..=i).any(|r| self.selectable(r))
                        {
                            self.select(i, false, c);
                        } else {
                            self.show_top(c);
                        }
                    }
                    DOWN => self.select(self.selected + 1, true, c),
                    PAGE_UP => self.select(self.selected.saturating_sub(page), false, c),
                    PAGE_DOWN => self.select(self.selected + page, true, c),
                    HOME => {
                        self.select(0, true, c);
                        self.show_top(c);
                    }
                    END => self.select(self.rows.len() - 1, false, c),
                    LEFT => {
                        if self.focused_column > 0 {
                            self.focused_column -= 1;
                            c.invalidate(self.bounds);
                        }
                    }
                    RIGHT => {
                        if self.focused_column + 1 < self.headers.len() {
                            self.focused_column += 1;
                            c.invalidate(self.bounds);
                        }
                    }
                    ENTER => {
                        if self.selectable(self.selected) {
                            self.activations += 1;
                            self.activated = true;
                            c.invalidate(self.bounds);
                        }
                    }
                    _ => return Handled::No,
                }
                Handled::Yes
            }
            InputKind::Wheel { at, delta_y, .. } if contains(self.bounds, *at) => {
                let rows = (-scroll_notches(*delta_y) * 3.0).round() as isize;
                let next = (self.first_visible as isize + rows).clamp(0, self.max_first() as isize)
                    as usize;
                if next != self.first_visible {
                    self.first_visible = next;
                    c.invalidate(self.bounds);
                }
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                c.set_focus(self.id);
                let hit = self
                    .visible_range()
                    .find(|i| contains(self.row_rect(*i), *at));
                if let Some(i) = hit
                    && self.selectable(i)
                {
                    if i != self.selected {
                        self.activated = true;
                    }
                    self.selected = i;
                    self.focused_column = (0..self.headers.len().max(1))
                        .rev()
                        .find(|column| at.x.0 >= self.column_x(*column))
                        .unwrap_or(0);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        clip(l, self.bounds);
        let head = rect(
            self.bounds.x.0,
            self.bounds.y.0,
            self.bounds.width.0,
            t.row.0,
        );
        for (column, title) in self.headers.iter().enumerate() {
            text_line(
                l,
                m,
                t,
                self.column_x(column) + t.inset.0,
                head.y.0,
                head.height.0,
                title,
                t.text,
            );
        }
        l.push(DrawCommand::Line {
            from: Point {
                x: head.x,
                y: Dip(head.y.0 + head.height.0 - 0.5),
            },
            to: Point {
                x: Dip(head.x.0 + head.width.0),
                y: Dip(head.y.0 + head.height.0 - 0.5),
            },
            width: Dip(1.0),
            color: t.popup_border,
        });
        for i in self.visible_range() {
            let r = self.row_rect(i);
            let row = &self.rows[i];
            let selected = i == self.selected && !row.section;
            if selected {
                l.push(DrawCommand::FillRect {
                    rect: r,
                    color: t.selected,
                });
            }
            if selected {
                row_mark(l, r, t);
            }
            let ink = if selected { t.selected_text } else { t.text };
            let label_x = r.x.0 + t.inset.0 * (row.indent + 1) as f32;
            let has_detail = !row.detail.is_empty();
            if has_detail {
                clip(l, rect(r.x.0, r.y.0, self.column_x(1) - r.x.0, r.height.0));
            }
            text_line(
                l,
                m,
                t,
                label_x,
                r.y.0,
                r.height.0,
                &row.label,
                if row.section { theme.group_header } else { ink },
            );
            if has_detail {
                unclip(l);
                text_line(
                    l,
                    m,
                    t,
                    self.column_x(1) + t.inset.0,
                    r.y.0,
                    r.height.0,
                    &row.detail,
                    if selected { t.selected_text } else { t.hint },
                );
            }
            if selected && self.focused {
                let ring = if self.headers.len() > 1 {
                    self.cell_rect(r, self.focused_column)
                } else {
                    r
                };
                focus_ring_on_fill(l, ring, t);
            }
        }
        if self.focused && !self.selectable(self.selected) {
            focus_ring(l, self.bounds, t);
        }
        unclip(l);
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let on_row =
            self.visible_range().contains(&self.selected) && self.selectable(self.selected);
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Table,
            &self.name,
            Some(format!("{} rows", self.rows.len())),
            state(true, self.focused && !on_row, false, false),
            self.visible_range()
                .map(|i| {
                    let row = &self.rows[i];
                    let row_id = child_id(self.id, i as u64);
                    let row_rect = self.row_rect(i);
                    // Focus is on the cell that Left and Right move between.
                    let mut states = state(true, false, i == self.selected && !row.section, false);
                    // No field says "section" yet. See the report's qe-api list.
                    states.read_only = row.section;
                    let cells = (0..self.headers.len().max(1))
                        .map(|column| {
                            let value = match column {
                                0 => &row.label,
                                1 => &row.detail,
                                _ => "",
                            };
                            let here = states.selected && column == self.focused_column;
                            let mut cell_state = state(true, self.focused && here, here, false);
                            cell_state.read_only = row.section;
                            node(
                                child_id(row_id, column as u64),
                                self.cell_rect(row_rect, column),
                                WidgetRole::Cell,
                                self.headers.get(column).map_or("", String::as_str),
                                (!value.is_empty()).then(|| value.to_owned()),
                                cell_state,
                                vec![],
                            )
                        })
                        .collect();
                    node(
                        row_id,
                        row_rect,
                        WidgetRole::Row,
                        &row.label,
                        if row.section {
                            Some("Section".to_owned())
                        } else {
                            (!row.detail.is_empty()).then(|| row.detail.clone())
                        },
                        states,
                        cells,
                    )
                })
                .collect(),
        ))
    }
}
impl Control for Table {
    fn focusable(&self) -> bool {
        !self.rows.is_empty()
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        virtual_key == ENTER
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
}

#[derive(Clone, Debug)]
pub struct ScrollBar {
    pub id: WidgetId,
    pub name: String,
    pub bounds: Rect,
    pub content: f32,
    pub viewport: f32,
    pub offset: f32,
    /// How far an arrow key moves.
    pub line: f32,
    pub hovered: bool,
    pub dragging: bool,
    pub focused: bool,
    drag_start: f32,
    offset_start: f32,
    activated: bool,
}
impl ScrollBar {
    pub fn new(id: WidgetId, content: f32, viewport: f32) -> Self {
        Self {
            id,
            name: "Scroll bar".into(),
            bounds: Rect::default(),
            content,
            viewport,
            offset: 0.0,
            line: UiTokens::sizes().row.0,
            hovered: false,
            dragging: false,
            focused: false,
            drag_start: 0.0,
            offset_start: 0.0,
            activated: false,
        }
    }
    fn max_offset(&self) -> f32 {
        (self.content - self.viewport).max(0.0)
    }
    fn thumb_height(&self, t: UiTokens) -> f32 {
        let track = self.bounds.height.0;
        (self.viewport / self.content.max(self.viewport) * track)
            .max(t.scrollbar_wide.0 * 2.0)
            .min(track)
    }
    /// The thumb stays in the track: its top runs over the track less its own height.
    fn thumb(&self, t: UiTokens) -> Rect {
        let height = self.thumb_height(t);
        let room = (self.bounds.height.0 - height).max(0.0);
        let max = self.max_offset();
        let part = if max > 0.0 {
            (self.offset / max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        rect(
            self.bounds.x.0,
            self.bounds.y.0 + part * room,
            self.bounds.width.0,
            height,
        )
    }
    fn scroll_to(&mut self, offset: f32, c: &mut ViewContext) {
        let next = offset.clamp(0.0, self.max_offset());
        if next != self.offset {
            self.offset = next;
            self.activated = true;
            c.invalidate(self.bounds);
        }
    }
    fn stop_drag(&mut self, c: &mut ViewContext) {
        if self.dragging {
            self.dragging = false;
            c.capture_mouse(false);
            c.invalidate(self.bounds);
        }
    }
}
impl View for ScrollBar {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        let t = UiTokens::sizes();
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.stop_drag(c);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown { virtual_key, .. } if self.focused => {
                let target = match *virtual_key {
                    UP | LEFT => self.offset - self.line,
                    DOWN | RIGHT => self.offset + self.line,
                    PAGE_UP => self.offset - self.viewport,
                    PAGE_DOWN => self.offset + self.viewport,
                    HOME => 0.0,
                    END => self.max_offset(),
                    _ => return Handled::No,
                };
                self.scroll_to(target, c);
                Handled::Yes
            }
            InputKind::Wheel { delta_y, .. } => {
                self.scroll_to(self.offset - scroll_notches(*delta_y) * 3.0 * self.line, c);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                if self.dragging {
                    let room = (self.bounds.height.0 - self.thumb_height(t)).max(1.0);
                    let target =
                        self.offset_start + (at.y.0 - self.drag_start) * self.max_offset() / room;
                    self.scroll_to(target, c);
                    self.hovered = contains(self.bounds, *at);
                    return Handled::Yes;
                }
                track_hover(&mut self.hovered, self.bounds, *at, c)
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if contains(self.bounds, *at) => {
                c.set_focus(self.id);
                let thumb = self.thumb(t);
                if contains(thumb, *at) {
                    self.dragging = true;
                    self.drag_start = at.y.0;
                    self.offset_start = self.offset;
                    c.capture_mouse(true);
                } else if at.y.0 < thumb.y.0 {
                    self.scroll_to(self.offset - self.viewport, c);
                } else {
                    self.scroll_to(self.offset + self.viewport, c);
                }
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.dragging => {
                self.stop_drag(c);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let mut r = self.thumb(t);
        r.width = if self.hovered || self.dragging || self.focused {
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
        if self.focused {
            l.push(DrawCommand::StrokeRect {
                rect: self.bounds,
                radius: t.radius,
                width: Dip(1.0),
                color: t.focus,
            });
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let max = self.max_offset();
        let percent = if max > 0.0 {
            (self.offset / max * 100.0).round()
        } else {
            0.0
        };
        // Group until qe-api has a scroll bar role. See the report's qe-api list.
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::Group,
            &self.name,
            Some(format!(
                "{percent:.0} percent, {:.0} of {max:.0}",
                self.offset
            )),
            state(true, self.focused, false, false),
            vec![],
        ))
    }
}
impl Control for ScrollBar {
    fn focusable(&self) -> bool {
        self.max_offset() > 0.0
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
}

#[derive(Clone, Debug)]
pub struct ComboBox {
    pub id: WidgetId,
    pub name: String,
    /// `None` is a separator. To show a named row that cannot be chosen, keep its
    /// name here and add its index to `disabled_items`.
    pub items: Vec<Option<String>>,
    pub disabled_items: BTreeSet<usize>,
    pub selected: usize,
    pub open: bool,
    pub bounds: Rect,
    pub focused: bool,
    pub enabled: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub hover_row: Option<usize>,
    /// An open list stays inside this area. A dialog sets it to its own bounds.
    pub popup_area: Option<Rect>,
    pub first_visible: usize,
    selected_on_open: usize,
    typed: String,
    typed_at_us: u64,
    activated: bool,
    captured: bool,
    thumb_drag: Option<ThumbDrag>,
}
impl ComboBox {
    pub fn new(id: WidgetId, name: impl Into<String>, items: Vec<Option<String>>) -> Self {
        Self {
            id,
            name: name.into(),
            items,
            disabled_items: BTreeSet::new(),
            selected: 0,
            open: false,
            bounds: Rect::default(),
            focused: false,
            enabled: true,
            hovered: false,
            pressed: false,
            hover_row: None,
            popup_area: None,
            first_visible: 0,
            selected_on_open: 0,
            typed: String::new(),
            typed_at_us: 0,
            activated: false,
            captured: false,
            thumb_drag: None,
        }
    }
    fn choosable(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(Option::is_some) && !self.disabled_items.contains(&i)
    }
    fn step(&mut self, delta: isize) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected;
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.choosable(i) {
                self.selected = i;
                break;
            }
        }
    }
    fn row_height(&self, i: usize, t: UiTokens) -> f32 {
        if self.items[i].is_some() {
            t.menu_row.0
        } else {
            t.separator_row.0
        }
    }
    /// Where the open list goes, inside `popup_area` when there is one.
    pub fn popup_rect(&self) -> Rect {
        let t = UiTokens::sizes();
        let content: f32 = (0..self.items.len()).map(|i| self.row_height(i, t)).sum();
        let limit = t.popup_rows as f32 * t.menu_row.0;
        let height = content.min(limit) + 2.0 * t.inset.0;
        let area = self
            .popup_area
            .unwrap_or_else(|| rect(-1.0e6, -1.0e6, 2.0e6, 2.0e6));
        place_popup(self.bounds, (self.bounds.width.0, height), area, false)
    }
    fn visible_rows(&self, popup: Rect, t: UiTokens) -> Vec<(usize, Rect)> {
        let bottom = popup.y.0 + popup.height.0 - t.inset.0;
        let mut y = popup.y.0 + t.inset.0;
        let mut rows = Vec::new();
        for i in self.first_visible..self.items.len() {
            let h = self.row_height(i, t);
            if y + h > bottom + 0.5 {
                break;
            }
            rows.push((i, rect(popup.x.0, y, popup.width.0, h)));
            y += h;
        }
        rows
    }
    fn keep_selected_visible(&mut self) {
        let t = UiTokens::sizes();
        let popup = self.popup_rect();
        self.first_visible = self.first_visible.min(self.selected);
        while self.first_visible < self.selected
            && !self
                .visible_rows(popup, t)
                .iter()
                .any(|(i, _)| *i == self.selected)
        {
            self.first_visible += 1;
        }
    }
    fn repaint(&self, c: &mut ViewContext) {
        c.invalidate(self.bounds);
        if self.open {
            c.invalidate(self.popup_rect());
        }
    }
    fn open_list(&mut self, c: &mut ViewContext) {
        if !self.open {
            self.open = true;
            self.selected_on_open = self.selected;
            self.keep_selected_visible();
        }
        self.repaint(c);
    }
    fn close_list(&mut self, c: &mut ViewContext) {
        // The list's area comes first: it needs repainting after the list is gone.
        self.repaint(c);
        if self.open && self.selected != self.selected_on_open {
            self.activated = true;
        }
        self.open = false;
        self.hover_row = None;
        self.end_thumb_drag(c);
    }
    /// Closes the list and puts back the value from before it opened. A value only
    /// browsed in the list is not a choice.
    fn cancel_list(&mut self, c: &mut ViewContext) {
        if self.open {
            self.selected = self.selected_on_open;
        }
        self.close_list(c);
    }
    fn end_thumb_drag(&mut self, c: &mut ViewContext) {
        if self.thumb_drag.take().is_some() {
            c.capture_mouse(false);
        }
    }
    fn release_press(&mut self, c: &mut ViewContext) {
        self.pressed = false;
        if self.captured {
            self.captured = false;
            c.capture_mouse(false);
        }
    }
    /// The last first row that still fills the list.
    fn max_first(&self, popup: Rect, t: UiTokens) -> usize {
        let view = popup.height.0 - 2.0 * t.inset.0;
        let mut used = 0.0;
        let mut first = self.items.len();
        for i in (0..self.items.len()).rev() {
            used += self.row_height(i, t);
            if used > view + 0.5 {
                break;
            }
            first = i;
        }
        first.min(self.items.len().saturating_sub(1))
    }
    fn list_thumb(&self, t: UiTokens) -> Option<ListThumb> {
        let popup = self.popup_rect();
        let track = inset(popup, 0.0, t.inset.0);
        ListThumb::new(
            track,
            self.visible_rows(popup, t).len(),
            self.items.len(),
            self.first_visible,
            self.max_first(popup, t),
            t,
        )
    }
    /// Runs after any key, letter or wheel move. A closed combo's value changes for
    /// good at once, so the change is reported here. An open list reports on closing.
    fn after_move(&mut self, before: usize, c: &mut ViewContext) {
        if self.open {
            self.keep_selected_visible();
        } else if self.selected != before {
            self.activated = true;
        }
        self.repaint(c);
    }
    fn jump(&mut self, target: usize, down: bool) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let target = target.min(n - 1);
        let found = if down {
            (target..n).find(|i| self.choosable(*i))
        } else {
            (0..=target).rev().find(|i| self.choosable(*i))
        };
        if let Some(i) = found {
            self.selected = i;
        }
    }
    /// Typing the same letter again cycles through the rows it starts. Different
    /// letters within a second build a longer prefix.
    fn type_to_select(&mut self, ch: char, now_us: u64) {
        let lower: String = ch.to_lowercase().collect();
        let expired = now_us.saturating_sub(self.typed_at_us) > 1_000_000;
        self.typed_at_us = now_us;
        let same_letter =
            !self.typed.is_empty() && self.typed.chars().all(|c| c.to_string() == lower);
        if expired || same_letter || self.typed.is_empty() {
            self.typed = lower.clone();
            self.find_from(self.selected + 1, &lower);
        } else {
            self.typed.push_str(&lower);
            let prefix = self.typed.clone();
            self.find_from(self.selected, &prefix);
        }
    }
    fn find_from(&mut self, start: usize, prefix: &str) {
        let n = self.items.len();
        for k in 0..n {
            let i = (start + k) % n;
            if self.choosable(i)
                && self.items[i]
                    .as_ref()
                    .is_some_and(|v| v.to_lowercase().starts_with(prefix))
            {
                self.selected = i;
                return;
            }
        }
    }
    fn row_at(&self, at: Point) -> Option<usize> {
        if self
            .list_thumb(UiTokens::sizes())
            .is_some_and(|thumb| contains(thumb.strip, at))
        {
            return None;
        }
        self.visible_rows(self.popup_rect(), UiTokens::sizes())
            .into_iter()
            .find(|(_, r)| contains(*r, at))
            .map(|(i, _)| i)
    }
    fn draw_closed(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        l.push(DrawCommand::RoundedRect {
            rect: self.bounds,
            radius: t.radius,
            color: if self.enabled {
                t.surface
            } else {
                t.disabled_fill
            },
        });
        let mut ink = if self.enabled { t.text } else { t.disabled };
        if self.enabled && (self.pressed || self.hovered) {
            l.push(DrawCommand::RoundedRect {
                rect: self.bounds,
                radius: t.radius,
                color: if self.pressed { t.pressed } else { t.hover },
            });
            ink = if self.pressed {
                t.pressed_text
            } else {
                t.hover_text
            };
        }
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: if self.enabled {
                t.border
            } else {
                t.disabled_border
            },
        });
        let caret = rect(
            self.bounds.x.0 + self.bounds.width.0 - t.caret_box.0,
            self.bounds.y.0,
            t.caret_box.0,
            self.bounds.height.0,
        );
        clip(
            l,
            rect(
                self.bounds.x.0,
                self.bounds.y.0,
                self.bounds.width.0 - t.caret_box.0,
                self.bounds.height.0,
            ),
        );
        text_line(
            l,
            m,
            t,
            self.bounds.x.0 + t.inset.0,
            self.bounds.y.0,
            self.bounds.height.0,
            self.items
                .get(self.selected)
                .and_then(|v| v.as_deref())
                .unwrap_or(""),
            ink,
        );
        unclip(l);
        icon(l, m, t, caret, CARET_ICON, ink);
        if self.focused && self.enabled {
            if self.pressed || self.hovered {
                focus_ring_on_fill(l, self.bounds, t);
            } else {
                focus_ring(l, self.bounds, t);
            }
        }
    }
    fn draw_list(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let popup = self.popup_rect();
        l.push(DrawCommand::RoundedRect {
            rect: popup,
            radius: t.popup_radius,
            color: t.surface,
        });
        l.push(DrawCommand::StrokeRect {
            rect: popup,
            radius: t.popup_radius,
            width: Dip(1.0),
            color: t.border,
        });
        clip(l, popup);
        let rows = self.visible_rows(popup, t);
        let thumb = self.list_thumb(t);
        for (i, r) in &rows {
            match &self.items[*i] {
                Some(s) => {
                    let mut row = inset(*r, 4.0, 0.0);
                    if thumb.is_some() {
                        // The thumb never lies over a highlighted row.
                        row.width.0 -= THUMB_ROOM;
                    }
                    let enabled = self.choosable(*i);
                    let mut ink = if enabled { t.text } else { t.disabled };
                    if *i == self.selected {
                        l.push(DrawCommand::RoundedRect {
                            rect: row,
                            radius: t.radius,
                            color: t.selected,
                        });
                        row_mark(l, row, t);
                        ink = t.selected_text;
                    } else if self.hover_row == Some(*i) && enabled {
                        l.push(DrawCommand::RoundedRect {
                            rect: row,
                            radius: t.radius,
                            color: t.hover,
                        });
                        ink = t.hover_text;
                    }
                    // Past the bar that marks the selected row.
                    text_line(l, m, t, row.x.0 + t.inset.0, r.y.0, r.height.0, s, ink);
                }
                None => {
                    let y = r.y.0 + r.height.0 / 2.0;
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
                        color: t.popup_border,
                    })
                }
            }
        }
        unclip(l);
        if let Some(thumb) = thumb {
            thumb.paint(l, t, self.thumb_drag.is_some());
        }
    }
}
impl View for ComboBox {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if matches!(e.kind, InputKind::Blur) {
            self.cancel_list(c);
            self.focused = false;
            self.release_press(c);
            c.invalidate(self.bounds);
            return Handled::Yes;
        }
        if !self.enabled {
            return Handled::No;
        }
        let before = self.selected;
        // Alt+Up and Alt+Down belong to the list. Every other Alt chord passes on.
        if alt_chord(&e.kind)
            && !matches!(
                e.kind,
                InputKind::KeyDown {
                    virtual_key: UP | DOWN,
                    ..
                }
            )
        {
            return Handled::No;
        }
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown { at, button, .. } => {
                let left = *button == MouseButton::Left;
                if self.open {
                    if !contains(self.popup_rect(), *at) {
                        // As in native combos, a press elsewhere cancels. Any button
                        // counts, as it does for menus. A press on the control itself
                        // is the toggle, which keeps the browsed value.
                        if left && contains(self.bounds, *at) {
                            self.close_list(c);
                        } else {
                            self.cancel_list(c);
                        }
                        return Handled::Yes;
                    }
                    if !left {
                        return Handled::Yes;
                    }
                    let t = UiTokens::sizes();
                    if let Some(thumb) = self.list_thumb(t)
                        && contains(thumb.strip, *at)
                    {
                        match thumb.press(at.y.0) {
                            Ok(drag) => {
                                self.thumb_drag = Some(drag);
                                c.capture_mouse(true);
                            }
                            Err(first) => self.first_visible = first,
                        }
                        c.invalidate(self.popup_rect());
                        return Handled::Yes;
                    }
                    if let Some(i) = self.row_at(*at)
                        && self.choosable(i)
                    {
                        self.selected = i;
                        self.close_list(c);
                    }
                    return Handled::Yes;
                }
                if !left || !contains(self.bounds, *at) {
                    return Handled::No;
                }
                self.pressed = true;
                self.captured = true;
                c.capture_mouse(true);
                self.focused = true;
                c.set_focus(self.id);
                self.open_list(c);
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.thumb_drag.is_some() || self.pressed => {
                self.end_thumb_drag(c);
                self.release_press(c);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                if let Some(drag) = self.thumb_drag {
                    if let Some(thumb) = self.list_thumb(UiTokens::sizes()) {
                        let first = thumb.dragged_to(drag, at.y.0);
                        if first != self.first_visible {
                            self.first_visible = first;
                            c.invalidate(self.popup_rect());
                        }
                    }
                    return Handled::Yes;
                }
                if self.open {
                    let row = self.row_at(*at);
                    if row != self.hover_row {
                        self.hover_row = row;
                        c.invalidate(self.popup_rect());
                    }
                    if contains(self.popup_rect(), *at) {
                        return Handled::Yes;
                    }
                }
                track_hover(&mut self.hovered, self.bounds, *at, c)
            }
            InputKind::Wheel { at, delta_y, .. } => {
                if self.open && contains(self.popup_rect(), *at) {
                    let rows = (-scroll_notches(*delta_y) * 3.0).round() as isize;
                    let popup = self.popup_rect();
                    let max = self.max_first(popup, UiTokens::sizes()) as isize;
                    self.first_visible =
                        (self.first_visible as isize + rows).clamp(0, max) as usize;
                    c.invalidate(popup);
                    Handled::Yes
                } else if self.focused && contains(self.bounds, *at) {
                    self.step(if *delta_y > 0.0 { -1 } else { 1 });
                    self.after_move(before, c);
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                repeat,
            } if self.focused => {
                match *virtual_key {
                    // A held key must not open and close the list over and over.
                    ENTER | SPACE | F4 if *repeat => {}
                    DOWN if modifiers.alt && *repeat => {}
                    DOWN if modifiers.alt => {
                        if self.open {
                            self.close_list(c);
                        } else {
                            self.open_list(c);
                        }
                    }
                    UP if modifiers.alt => self.close_list(c),
                    F4 => {
                        if self.open {
                            self.close_list(c);
                        } else {
                            self.open_list(c);
                        }
                    }
                    DOWN => {
                        self.step(1);
                        self.after_move(before, c);
                    }
                    UP => {
                        self.step(-1);
                        self.after_move(before, c);
                    }
                    HOME => {
                        self.jump(0, true);
                        self.after_move(before, c);
                    }
                    END => {
                        self.jump(self.items.len(), false);
                        self.after_move(before, c);
                    }
                    PAGE_DOWN => {
                        self.jump(self.selected + UiTokens::sizes().popup_rows, false);
                        self.after_move(before, c);
                    }
                    PAGE_UP => {
                        self.jump(
                            self.selected.saturating_sub(UiTokens::sizes().popup_rows),
                            true,
                        );
                        self.after_move(before, c);
                    }
                    ENTER | SPACE => {
                        if self.open {
                            self.close_list(c);
                        } else {
                            self.open_list(c);
                        }
                    }
                    ESCAPE if self.open => self.cancel_list(c),
                    _ => return Handled::No,
                }
                Handled::Yes
            }
            InputKind::Char(ch) if self.focused => {
                if !ch.is_control() && !ch.is_whitespace() {
                    self.type_to_select(*ch, e.time_us);
                    self.after_move(before, c);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        self.draw_closed(l, theme, m);
        if self.open {
            self.draw_list(l, theme, m);
        }
    }
    fn layout(&mut self, r: Rect, _: &dyn TextMeasurer) {
        self.bounds = r;
    }
    fn widgets(&self) -> Option<Widget> {
        let mut s = state(self.enabled, self.focused && !self.open, false, false);
        s.expanded = self.open;
        let mut children = Vec::new();
        if self.open {
            let popup = self.popup_rect();
            let t = UiTokens::sizes();
            let list_id = child_id(self.id, 0);
            let rows = self.visible_rows(popup, t);
            let options = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let name = item.as_deref()?;
                    let bounds = rows
                        .iter()
                        .find(|(row, _)| *row == i)
                        .map_or_else(Rect::default, |(_, r)| *r);
                    let chosen = i == self.selected;
                    Some(node(
                        child_id(list_id, i as u64),
                        bounds,
                        // List and ListItem roles are on the report's qe-api list.
                        WidgetRole::MenuItem,
                        name,
                        None,
                        state(self.choosable(i), chosen && self.focused, chosen, false),
                        vec![],
                    ))
                })
                .collect();
            children.push(node(
                list_id,
                popup,
                WidgetRole::Menu,
                &self.name,
                None,
                state(true, false, false, false),
                options,
            ));
        }
        Some(node(
            self.id,
            self.bounds,
            WidgetRole::ComboBox,
            &self.name,
            self.items.get(self.selected).cloned().flatten(),
            s,
            children,
        ))
    }
}
impl Control for ComboBox {
    fn focusable(&self) -> bool {
        self.enabled
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        self.enabled
            && (matches!(virtual_key, ENTER | SPACE) || (virtual_key == ESCAPE && self.open))
    }
    fn take_activated(&mut self) -> bool {
        std::mem::take(&mut self.activated)
    }
    fn overlay_bounds(&self) -> Option<Rect> {
        self.open.then(|| self.popup_rect())
    }
    fn set_popup_area(&mut self, area: Rect) {
        self.popup_area = Some(area);
    }
    fn draw_base(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        self.draw_closed(list, theme, measure);
    }
    fn draw_overlay(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        if self.open {
            self.draw_list(list, theme, measure);
        }
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
