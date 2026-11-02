use crate::control::*;
use crate::controls::{
    CHECK_ICON, ListThumb, SUBMENU_ICON, THUMB_ROOM, ThumbDrag, clip, contains, focus_ring_on_fill,
    icon, inset, rect, text_line, text_width, unclip, union,
};
use crate::{Control, UiTokens, child_id};
use qe_api::{
    CommandId, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, Point,
    Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates,
};

const MIN_MENU_WIDTH: f32 = 160.0;

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
    /// What this row is about when its command alone cannot say, such as the path of
    /// an Open Recent file. `MenuView::take_chosen_data` gives it after a choice.
    pub data: Option<String>,
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
            data: None,
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
            data: None,
        }
    }
    pub fn submenu(label: impl Into<String>, model: MenuModel) -> Self {
        Self {
            label: label.into(),
            key_text: String::new(),
            enabled: true,
            checked: false,
            submenu: Some(model),
            separator: false,
            command: None,
            mnemonic: None,
            data: None,
        }
    }
    fn usable(&self) -> bool {
        self.enabled && !self.separator
    }
}
#[derive(Clone, Debug, Default)]
pub struct MenuModel {
    pub items: Vec<MenuItem>,
}
impl MenuModel {
    fn row_height(&self, i: usize, t: UiTokens) -> f32 {
        if self.items[i].separator {
            t.separator_row.0
        } else {
            t.menu_row.0
        }
    }
    fn content_height(&self, t: UiTokens) -> f32 {
        (0..self.items.len()).map(|i| self.row_height(i, t)).sum()
    }
    /// The size that shows every row: long labels never reach the shortcut column.
    pub fn preferred_size(&self, m: &dyn TextMeasurer) -> (f32, f32) {
        let t = UiTokens::sizes();
        let mut width = MIN_MENU_WIDTH;
        for item in &self.items {
            let label = text_width(m, t, &item.label);
            let key = if item.key_text.is_empty() {
                0.0
            } else {
                text_width(m, t, &item.key_text) + t.inset.0 * 3.0
            };
            let arrow = if item.submenu.is_some() {
                t.icon_size.0 + t.inset.0
            } else {
                0.0
            };
            width = width.max(label + key + arrow + t.check_size.0 + t.inset.0 * 3.0);
        }
        (width, self.content_height(t) + t.inset.0 * 2.0)
    }
}

/// Why a menu level ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Command,
    Back,
}

/// One level of the caret list. A level with an open child is the parent of that child.
///
/// The letter rule: while the list was opened by keyboard, a letter that is an
/// enabled row's mnemonic runs that row. Any other letter only moves to the next
/// enabled row whose label starts with it, and never runs a command. In a list
/// opened by mouse, no letter runs a command.
#[derive(Clone, Debug)]
pub struct MenuView {
    pub id: WidgetId,
    /// The name a screen reader gives this list.
    pub name: String,
    pub model: MenuModel,
    pub bounds: Rect,
    pub selected: Option<usize>,
    pub child: Option<Box<MenuView>>,
    pub keyboard_opened: bool,
    pub closed: bool,
    /// The screen's usable area. Submenus stay inside it, on any monitor.
    pub work_area: Option<Rect>,
    /// The first row shown when the list is longer than its bounds.
    pub first_visible: usize,
    pending_submenu: Option<(usize, u64)>,
    press_began: bool,
    is_submenu: bool,
    ended: Option<Ended>,
    child_row: Option<usize>,
    /// Submenu sizes, measured in `layout`, because `handle` has no measurer.
    measured: Vec<Option<MeasuredMenu>>,
    keys_used: bool,
    alt: AltKeyTracker,
    chosen_data: Option<String>,
    thumb_drag: Option<ThumbDrag>,
}
/// The size of a submenu and of the submenus inside it.
#[derive(Clone, Debug)]
struct MeasuredMenu {
    size: (f32, f32),
    rows: Vec<Option<MeasuredMenu>>,
}
fn measure_rows(model: &MenuModel, m: &dyn TextMeasurer) -> Vec<Option<MeasuredMenu>> {
    model
        .items
        .iter()
        .map(|item| {
            item.submenu.as_ref().map(|sub| MeasuredMenu {
                size: sub.preferred_size(m),
                rows: measure_rows(sub, m),
            })
        })
        .collect()
}
impl MenuView {
    /// `name` is what a screen reader says for this list.
    pub fn new(id: WidgetId, name: impl Into<String>, model: MenuModel) -> Self {
        let name = name.into();
        debug_assert!(!name.is_empty(), "a menu needs a name");
        Self {
            id,
            name,
            model,
            bounds: Rect::default(),
            selected: None,
            child: None,
            keyboard_opened: false,
            closed: false,
            work_area: None,
            first_visible: 0,
            pending_submenu: None,
            press_began: false,
            is_submenu: false,
            ended: None,
            child_row: None,
            measured: vec![],
            keys_used: false,
            alt: AltKeyTracker::default(),
            chosen_data: None,
            thumb_drag: None,
        }
    }
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    pub fn open_keyboard(&mut self) {
        self.keyboard_opened = true;
        self.selected = self.first_enabled()
    }
    fn first_enabled(&self) -> Option<usize> {
        self.model.items.iter().position(MenuItem::usable)
    }
    fn last_enabled(&self) -> Option<usize> {
        self.model.items.iter().rposition(MenuItem::usable)
    }
    /// The extra text of the row chosen last, such as an Open Recent file's path.
    /// Reading clears it.
    pub fn take_chosen_data(&mut self) -> Option<String> {
        self.chosen_data.take()
    }
    /// The bounds of each open level, from this one down. A host gives each its own
    /// window, because the levels can overlap the screen edge in different ways.
    pub fn levels(&self) -> Vec<Rect> {
        let mut out = vec![self.bounds];
        let mut level = self;
        while let Some(child) = &level.child {
            out.push(child.bounds);
            level = child;
        }
        out
    }
    fn level(&self, depth: usize) -> Option<&MenuView> {
        let mut level = self;
        for _ in 0..depth {
            level = level.child.as_deref()?;
        }
        Some(level)
    }
    /// Paints one level alone, with its top left corner at (0, 0) of its own window.
    pub fn draw_level(
        &self,
        depth: usize,
        list: &mut DisplayList,
        theme: &Theme,
        measure: &dyn TextMeasurer,
    ) {
        if let Some(level) = self.level(depth) {
            let mut own = DisplayList::default();
            level.draw_own(&mut own, theme, measure);
            shift_list(&mut own, level.bounds.x.0, level.bounds.y.0);
            list.commands.append(&mut own.commands);
        }
    }
    /// All the area that this level and its open children paint.
    pub fn full_bounds(&self) -> Rect {
        match &self.child {
            Some(child) => union(self.bounds, child.full_bounds()),
            None => self.bounds,
        }
    }
    fn close_child(&mut self, c: &mut ViewContext) {
        if let Some(child) = self.child.take() {
            c.invalidate(child.full_bounds());
        }
        self.child_row = None;
    }
    fn select_row(&mut self, i: usize, c: &mut ViewContext) {
        if self.child_row != Some(i) {
            self.close_child(c);
        }
        self.selected = Some(i);
        self.keep_visible(i);
        c.invalidate(self.bounds);
    }
    fn move_selection(&mut self, delta: isize, c: &mut ViewContext) {
        let n = self.model.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected.unwrap_or(if delta > 0 { n - 1 } else { 0 });
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.model.items[i].usable() {
                self.select_row(i, c);
                break;
            }
        }
    }
    /// A page of rows, without wrapping.
    fn page(&mut self, down: bool, c: &mut ViewContext) {
        let t = UiTokens::sizes();
        let rows = (self.viewport(t).height.0 / t.menu_row.0).floor().max(1.0) as usize;
        let n = self.model.items.len();
        if n == 0 {
            return;
        }
        let from = self.selected.unwrap_or(0);
        let target = if down {
            (from + rows).min(n.saturating_sub(1))
        } else {
            from.saturating_sub(rows)
        };
        let items = &self.model.items;
        let found = if down {
            (target..n)
                .find(|i| items[*i].usable())
                .or_else(|| (0..target).rev().find(|i| items[*i].usable()))
        } else {
            (0..=target)
                .rev()
                .find(|i| items[*i].usable())
                .or_else(|| (target..n).find(|i| items[*i].usable()))
        };
        if let Some(i) = found {
            self.select_row(i, c);
        }
    }
    fn viewport(&self, t: UiTokens) -> Rect {
        inset(self.bounds, 0.0, t.inset.0)
    }
    /// Rows from `first_visible` that fit whole in the bounds. The cost follows the
    /// viewport, not the length of the list.
    fn visible_rows(&self, t: UiTokens) -> Vec<(usize, Rect)> {
        let view = self.viewport(t);
        let bottom = view.y.0 + view.height.0;
        let mut y = view.y.0;
        let mut rows = Vec::new();
        for i in self.first_visible..self.model.items.len() {
            let h = self.model.row_height(i, t);
            if y + h > bottom + 0.5 {
                break;
            }
            rows.push((i, rect(self.bounds.x.0, y, self.bounds.width.0, h)));
            y += h;
        }
        rows
    }
    fn keep_visible(&mut self, i: usize) {
        let t = UiTokens::sizes();
        if i < self.first_visible {
            self.first_visible = i;
        }
        while self.first_visible < i && !self.visible_rows(t).iter().any(|(r, _)| *r == i) {
            self.first_visible += 1;
        }
    }
    fn max_first(&self, t: UiTokens) -> usize {
        let view = self.viewport(t).height.0;
        let mut used = 0.0;
        let mut first = self.model.items.len();
        for i in (0..self.model.items.len()).rev() {
            used += self.model.row_height(i, t);
            if used > view + 0.5 {
                break;
            }
            first = i;
        }
        first.min(self.model.items.len().saturating_sub(1))
    }
    fn list_thumb(&self, t: UiTokens) -> Option<ListThumb> {
        ListThumb::new(
            self.viewport(t),
            self.visible_rows(t).len(),
            self.model.items.len(),
            self.first_visible,
            self.max_first(t),
            t,
        )
    }
    fn row_at(&self, at: Point) -> Option<usize> {
        if !contains(self.bounds, at) {
            return None;
        }
        if self
            .list_thumb(UiTokens::sizes())
            .is_some_and(|thumb| contains(thumb.strip, at))
        {
            return None;
        }
        self.visible_rows(UiTokens::sizes())
            .into_iter()
            .find(|(_, r)| contains(*r, at))
            .map(|(i, _)| i)
    }
    fn open_child(&mut self, c: &mut ViewContext) {
        let Some(i) = self.selected else { return };
        let Some(model) = self
            .model
            .items
            .get(i)
            .and_then(|item| item.submenu.clone())
        else {
            return;
        };
        if self.child_row == Some(i) && self.child.is_some() {
            return;
        }
        self.close_child(c);
        let t = UiTokens::sizes();
        self.keep_visible(i);
        let anchor = self
            .visible_rows(t)
            .into_iter()
            .find(|(row, _)| *row == i)
            .map_or(self.bounds, |(_, r)| r);
        let mut child = MenuView::new(
            child_id(self.id, 1_000_000 + i as u64),
            self.model.items[i].label.clone(),
            model,
        );
        child.keyboard_opened = self.keyboard_opened;
        child.keys_used = self.keys_used;
        child.selected = child.first_enabled();
        child.is_submenu = true;
        child.work_area = self.work_area;
        let measured = self.measured.get(i).cloned().flatten();
        let (width, height) = measured.as_ref().map_or(
            (
                self.bounds.width.0,
                child.model.content_height(t) + t.inset.0 * 2.0,
            ),
            |menu| menu.size,
        );
        child.measured = measured.map_or_else(Vec::new, |menu| menu.rows);
        child.bounds = match self.work_area {
            Some(work) => place_popup(anchor, (width, height), work, true),
            None => rect(anchor.x.0 + anchor.width.0, anchor.y.0, width, height),
        };
        c.invalidate(child.bounds);
        c.invalidate(self.bounds);
        self.child = Some(Box::new(child));
        self.child_row = Some(i);
    }
    fn activate(&mut self, c: &mut ViewContext) {
        // A caller may have replaced the model, so the old row may be gone.
        let Some(item) = self.selected.and_then(|i| self.model.items.get(i)) else {
            return;
        };
        if !item.usable() {
            return;
        }
        if item.submenu.is_some() {
            self.open_child(c)
        } else if let Some(cmd) = item.command {
            self.chosen_data.clone_from(&item.data);
            c.run(cmd);
            self.end(Ended::Command, c);
        }
    }
    fn end(&mut self, reason: Ended, c: &mut ViewContext) {
        c.invalidate(self.full_bounds());
        self.child = None;
        self.child_row = None;
        self.closed = true;
        self.ended = Some(reason);
        self.pending_submenu = None;
    }
    /// Opens a submenu whose hover delay has passed, at any depth. Returns the area
    /// to repaint.
    pub fn tick(&mut self, now_us: u64) -> Option<Rect> {
        let t = UiTokens::sizes();
        if let Some((i, start)) = self.pending_submenu
            && self.selected == Some(i)
            && now_us.saturating_sub(start) >= t.submenu_delay_ms * 1000
        {
            self.pending_submenu = None;
            let mut ctx = ViewContext::default();
            self.open_child(&mut ctx);
            return Some(self.full_bounds());
        }
        self.child.as_mut().and_then(|child| child.tick(now_us))
    }
    /// Reads a child's end and closes this level to match. A command closes all
    /// levels. Going back closes only the child.
    fn after_child(&mut self, c: &mut ViewContext) {
        let Some(ended) = self.child.as_ref().and_then(|child| child.ended) else {
            return;
        };
        match ended {
            Ended::Command => {
                self.chosen_data = self
                    .child
                    .as_mut()
                    .and_then(|child| child.chosen_data.take());
                self.end(Ended::Command, c)
            }
            Ended::Back => self.close_child(c),
        }
        c.invalidate(self.bounds);
    }
    fn contains_deep(&self, at: Point) -> bool {
        contains(self.bounds, at) || self.child.as_ref().is_some_and(|c| c.contains_deep(at))
    }
    fn type_letter(&mut self, ch: char, c: &mut ViewContext) {
        let needle: String = ch.to_lowercase().collect();
        let mnemonic = self.model.items.iter().position(|item| {
            item.usable()
                && item
                    .mnemonic
                    .is_some_and(|m| m.to_lowercase().collect::<String>() == needle)
        });
        if let Some(i) = mnemonic {
            self.select_row(i, c);
            if self.keyboard_opened {
                self.activate(c);
            }
            return;
        }
        let n = self.model.items.len();
        let start = self.selected.map_or(0, |s| s + 1);
        let found = (0..n).map(|k| (start + k) % n).find(|i| {
            let item = &self.model.items[*i];
            item.usable() && item.label.to_lowercase().starts_with(&needle)
        });
        if let Some(i) = found {
            self.select_row(i, c);
        }
    }
    /// True when this arrow key leaves for the next menu on the bar: Left on the first
    /// level, or Right on a row with no submenu to open.
    pub fn arrow_moves_to_next_menu(&self, right: bool) -> bool {
        let mut level = self;
        while let Some(child) = &level.child {
            level = child;
        }
        if right {
            level
                .selected
                .and_then(|i| level.model.items.get(i))
                .is_none_or(|item| item.submenu.is_none() || !item.usable())
        } else {
            !level.is_submenu
        }
    }
    /// True while a submenu waits for its hover delay to end, so that `tick` has work.
    pub fn waiting_for_submenu(&self) -> bool {
        self.pending_submenu.is_some()
            || self
                .child
                .as_ref()
                .is_some_and(|child| child.waiting_for_submenu())
    }
    fn deepest_has_focus(&self) -> bool {
        self.child.is_none()
    }
}
impl View for MenuView {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        if self.closed {
            return Handled::No;
        }
        if matches!(e.kind, InputKind::Blur) {
            self.end(Ended::Back, c);
            return Handled::Yes;
        }
        // A second tap of Alt closes the list that the first one opened.
        if !self.is_submenu && self.alt.event(e) {
            self.end(Ended::Back, c);
            return Handled::Yes;
        }
        if alt_chord(&e.kind) {
            return Handled::No;
        }
        if let InputKind::MouseDown { at, .. } = &e.kind
            && !self.is_submenu
            && !self.contains_deep(*at)
        {
            self.end(Ended::Back, c);
            return Handled::Yes;
        }
        if let Some(child) = &mut self.child {
            let h = child.handle(e, c);
            self.after_child(c);
            if h == Handled::Yes || self.closed {
                return Handled::Yes;
            }
        }
        if matches!(e.kind, InputKind::KeyDown { .. }) {
            // Keys have taken over from the mouse, so the row needs its ring.
            self.keys_used = true;
        }
        match &e.kind {
            InputKind::KeyDown { virtual_key, .. } => match *virtual_key {
                UP => {
                    self.move_selection(-1, c);
                    Handled::Yes
                }
                DOWN => {
                    self.move_selection(1, c);
                    Handled::Yes
                }
                HOME => {
                    if let Some(i) = self.first_enabled() {
                        self.select_row(i, c);
                    }
                    Handled::Yes
                }
                END => {
                    if let Some(i) = self.last_enabled() {
                        self.select_row(i, c);
                    }
                    Handled::Yes
                }
                PAGE_UP => {
                    self.page(false, c);
                    Handled::Yes
                }
                PAGE_DOWN => {
                    self.page(true, c);
                    Handled::Yes
                }
                RIGHT => {
                    self.open_child(c);
                    Handled::Yes
                }
                LEFT if self.is_submenu => {
                    self.end(Ended::Back, c);
                    Handled::Yes
                }
                ENTER | SPACE => {
                    self.activate(c);
                    Handled::Yes
                }
                ESCAPE => {
                    self.end(Ended::Back, c);
                    Handled::Yes
                }
                _ => Handled::No,
            },
            InputKind::Char(ch) => {
                if !ch.is_control() && !ch.is_whitespace() {
                    self.type_letter(*ch, c);
                }
                Handled::Yes
            }
            InputKind::Wheel { at, delta_y, .. } if contains(self.bounds, *at) => {
                let t = UiTokens::sizes();
                let max = self.max_first(t);
                let rows = (-delta_y / WHEEL_NOTCH * 3.0).round() as isize;
                let next = (self.first_visible as isize + rows).clamp(0, max as isize) as usize;
                if next != self.first_visible {
                    self.first_visible = next;
                    self.close_child(c);
                    c.invalidate(self.bounds);
                }
                Handled::Yes
            }
            InputKind::MouseMove { at } if self.thumb_drag.is_some() => {
                if let (Some(drag), Some(thumb)) =
                    (self.thumb_drag, self.list_thumb(UiTokens::sizes()))
                {
                    let first = thumb.dragged_to(drag, at.y.0);
                    if first != self.first_visible {
                        self.first_visible = first;
                        self.close_child(c);
                        c.invalidate(self.bounds);
                    }
                }
                Handled::Yes
            }
            InputKind::MouseMove { at } if contains(self.bounds, *at) => {
                self.keys_used = false;
                let t = UiTokens::sizes();
                let hit = self.row_at(*at).filter(|i| self.model.items[*i].usable());
                if hit != self.selected {
                    self.pending_submenu = None;
                    match hit {
                        Some(i) => self.select_row(i, c),
                        None => {
                            self.close_child(c);
                            self.selected = None;
                            c.invalidate(self.bounds);
                        }
                    }
                }
                if let Some(i) = hit
                    && self.model.items[i].submenu.is_some()
                    && self.child_row != Some(i)
                    && self.pending_submenu.is_none()
                {
                    self.pending_submenu = Some((i, e.time_us));
                    c.request_timer(t.submenu_delay_ms as u32);
                }
                Handled::Yes
            }
            InputKind::MouseDown { at, .. } if contains(self.bounds, *at) => {
                // The thumb takes its own presses, and its release runs no row.
                if let Some(thumb) = self.list_thumb(UiTokens::sizes())
                    && contains(thumb.strip, *at)
                {
                    match thumb.press(at.y.0) {
                        Ok(drag) => {
                            self.thumb_drag = Some(drag);
                            c.capture_mouse(true);
                        }
                        Err(first) => {
                            self.first_visible = first;
                            self.close_child(c);
                        }
                    }
                    c.invalidate(self.bounds);
                    return Handled::Yes;
                }
                self.press_began = true;
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
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } => {
                let began = std::mem::take(&mut self.press_began);
                if !contains(self.bounds, *at) {
                    return Handled::No;
                }
                // Only the row under the release counts, and only for a press that
                // began in this menu.
                if began
                    && let Some(i) = self.row_at(*at)
                    && self.model.items[i].usable()
                {
                    self.select_row(i, c);
                    self.activate(c);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
        self.draw_own(l, theme, m);
        if let Some(child) = &self.child {
            child.draw(l, theme, m)
        }
    }
    fn layout(&mut self, r: Rect, m: &dyn TextMeasurer) {
        self.bounds = r;
        self.measured = measure_rows(&self.model, m);
    }
    fn widgets(&self) -> Option<Widget> {
        self.tree()
    }
}
impl MenuView {
    /// Paints this level only, in the coordinates of `bounds`.
    fn draw_own(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
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
        let rows = self.visible_rows(t);
        let thumb = self.list_thumb(t);
        clip(l, self.bounds);
        for (i, row) in &rows {
            let item = &self.model.items[*i];
            if item.separator {
                let y = row.y.0 + row.height.0 / 2.0;
                l.push(DrawCommand::Line {
                    from: Point {
                        x: Dip(self.bounds.x.0 + t.inset.0),
                        y: Dip(y),
                    },
                    to: Point {
                        x: Dip(self.bounds.x.0 + self.bounds.width.0 - t.inset.0),
                        y: Dip(y),
                    },
                    width: Dip(1.0),
                    color: t.popup_border,
                });
                continue;
            }
            let mut r = inset(*row, t.inset.0 / 2.0, 0.0);
            if thumb.is_some() {
                // The thumb never lies over a highlighted row.
                r.width.0 -= THUMB_ROOM;
            }
            let selected = self.selected == Some(*i);
            if selected {
                l.push(DrawCommand::RoundedRect {
                    rect: r,
                    radius: t.radius,
                    color: t.hover,
                })
            }
            let color = if !item.enabled {
                t.disabled
            } else if selected {
                t.hover_text
            } else {
                t.text
            };
            let check_area = rect(r.x.0 + t.inset.0 / 2.0, r.y.0, t.check_size.0, r.height.0);
            if item.checked {
                icon(l, m, t, check_area, CHECK_ICON, color);
            }
            let label_x = r.x.0 + t.inset.0 + t.check_size.0;
            let key_width = if item.key_text.is_empty() {
                0.0
            } else {
                text_width(m, t, &item.key_text)
            };
            let right = r.x.0 + r.width.0 - t.inset.0;
            let arrow_room = if item.submenu.is_some() {
                t.icon_size.0 + t.inset.0 / 2.0
            } else {
                0.0
            };
            let key_x = right - arrow_room - key_width;
            // A label too long for the row stops before the shortcut.
            let label_end = if key_width > 0.0 {
                key_x - t.inset.0
            } else {
                right - arrow_room
            };
            clip(l, rect(label_x, r.y.0, label_end - label_x, r.height.0));
            if self.keyboard_opened
                && let Some(mn) = item.mnemonic
                && let Some((pos, found)) = mnemonic_position(&item.label, mn)
            {
                let x = label_x + text_width(m, t, &item.label[..pos]);
                let w = text_width(m, t, &found.to_string());
                let metrics = m.measure(&item.label, t.measure_font);
                let y = r.y.0 + (r.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0 + 2.0;
                l.push(DrawCommand::Line {
                    from: Point {
                        x: Dip(x),
                        y: Dip(y),
                    },
                    to: Point {
                        x: Dip(x + w),
                        y: Dip(y),
                    },
                    width: Dip(1.0),
                    color,
                })
            }
            text_line(l, m, t, label_x, r.y.0, r.height.0, &item.label, color);
            unclip(l);
            if key_width > 0.0 {
                text_line(
                    l,
                    m,
                    t,
                    key_x,
                    r.y.0,
                    r.height.0,
                    &item.key_text,
                    if !item.enabled {
                        t.disabled
                    } else if selected {
                        t.hover_text
                    } else {
                        t.hint
                    },
                );
            }
            if item.submenu.is_some() {
                icon(
                    l,
                    m,
                    t,
                    rect(right - t.icon_size.0, r.y.0, t.icon_size.0, r.height.0),
                    SUBMENU_ICON,
                    color,
                );
            }
            // The fill alone is faint, so a keyboard user also gets a ring.
            if selected && (self.keyboard_opened || self.keys_used) && self.deepest_has_focus() {
                focus_ring_on_fill(l, r, t);
            }
        }
        unclip(l);
        if let Some(thumb) = thumb {
            thumb.paint(l, t, self.thumb_drag.is_some());
        }
    }
    fn tree(&self) -> Option<Widget> {
        let t = UiTokens::sizes();
        let rows = self.visible_rows(t);
        let mut children: Vec<_> = Vec::with_capacity(self.model.items.len());
        for (i, item) in self.model.items.iter().enumerate() {
            if item.separator {
                continue;
            }
            let selected = self.selected == Some(i);
            let bounds = rows
                .iter()
                .find(|(row, _)| *row == i)
                .map_or_else(Rect::default, |(_, r)| *r);
            let open_child = (self.child_row == Some(i))
                .then(|| self.child.as_ref().and_then(|c| c.widgets()))
                .flatten();
            let value = if !item.key_text.is_empty() {
                Some(item.key_text.clone())
            } else if item.submenu.is_some() {
                // No field says "has a submenu" yet. See the report's qe-api list.
                Some("Submenu".to_owned())
            } else {
                None
            };
            children.push(Widget {
                id: child_id(self.id, i as u64),
                bounds,
                role: WidgetRole::MenuItem,
                name: item.label.clone(),
                value,
                states: WidgetStates {
                    enabled: item.enabled,
                    focused: selected && self.child.is_none() && !self.closed,
                    selected,
                    checked: item.checked,
                    expanded: open_child.is_some(),
                    ..Default::default()
                },
                selection: None,
                range_value: None,
                description: None,
                children: open_child.into_iter().collect(),
            });
        }
        Some(Widget {
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Menu,
            name: self.name.clone(),
            value: None,
            states: WidgetStates {
                enabled: true,
                focused: self.selected.is_none() && self.child.is_none() && !self.closed,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children,
        })
    }
}
impl Control for MenuView {
    fn focusable(&self) -> bool {
        true
    }
    fn uses_key(&self, _: u16) -> bool {
        true
    }
}

/// Moves every command of a list by (-dx, -dy), from screen places to a window's own.
fn shift_list(list: &mut DisplayList, dx: f32, dy: f32) {
    let rect_of = |r: &mut Rect| {
        r.x.0 -= dx;
        r.y.0 -= dy;
    };
    let point_of = |p: &mut Point| {
        p.x.0 -= dx;
        p.y.0 -= dy;
    };
    for command in &mut list.commands {
        match command {
            DrawCommand::FillRect { rect, .. }
            | DrawCommand::StrokeRect { rect, .. }
            | DrawCommand::RoundedRect { rect, .. }
            | DrawCommand::PushClip(rect) => rect_of(rect),
            DrawCommand::FillRects { rects, .. } => rects.iter_mut().for_each(rect_of),
            DrawCommand::WavyUnderline { from, to, .. } | DrawCommand::Line { from, to, .. } => {
                point_of(from);
                point_of(to);
            }
            DrawCommand::TextRun { at, .. }
            | DrawCommand::GlyphRun { at, .. }
            | DrawCommand::Icon { at, .. } => point_of(at),
            DrawCommand::PopClip | DrawCommand::PushOpacity(_) | DrawCommand::PopOpacity => {}
        }
    }
}

/// Finds a mnemonic in a label by whole characters. Lowercasing can change a
/// string's byte length, so an offset from a lowered copy must not cut the original.
fn mnemonic_position(label: &str, mnemonic: char) -> Option<(usize, char)> {
    let wanted: Vec<char> = mnemonic.to_lowercase().collect();
    label
        .char_indices()
        .find(|(_, c)| c.to_lowercase().eq(wanted.iter().copied()))
}

/// Tracks a lone tap of Alt, which closes an open list.
#[derive(Clone, Copy, Debug, Default)]
pub struct AltKeyTracker {
    armed: bool,
}
const VK_MENU: u16 = 0x12;
const VK_LMENU: u16 = 0xa4;
const VK_RMENU: u16 = 0xa5;
fn is_alt(virtual_key: u16) -> bool {
    matches!(virtual_key, VK_MENU | VK_LMENU | VK_RMENU)
}
impl AltKeyTracker {
    /// Returns true when Alt was tapped alone. AltGr reaches Windows programs as
    /// Ctrl plus Alt, so a Ctrl flag means it is not a tap of Alt.
    pub fn event(&mut self, event: &InputEvent) -> bool {
        match &event.kind {
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                repeat,
            } if is_alt(*virtual_key) => {
                if !*repeat {
                    self.armed = !modifiers.ctrl && !modifiers.shift && !modifiers.win;
                }
                false
            }
            InputKind::KeyUp {
                virtual_key,
                modifiers,
            } if is_alt(*virtual_key) => {
                let fire = self.armed && !modifiers.ctrl;
                self.armed = false;
                fire
            }
            InputKind::KeyDown { .. }
            | InputKind::KeyUp { .. }
            | InputKind::Char(_)
            | InputKind::MouseDown { .. }
            | InputKind::Wheel { .. }
            | InputKind::Blur => {
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
