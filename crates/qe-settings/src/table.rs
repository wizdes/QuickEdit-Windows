//! The Keyboard tab's table and recorder: a command column, a 132-wide shortcut column
//! and rows 22 high. The Mac's rules are in `Shortcuts.swift` and `ShortcutRecorder.swift`.
use crate::keys::{KeyRow, Refusal, keyboard_rows};
use crate::recorder::{RecordStep, held_text, record_step};
use crate::store::SettingsStore;
use qe_api::{
    Color, CommandId, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers,
    MouseButton, Point, Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole,
    WidgetStates,
};
use qe_ui::{Control, ScrollBar, UI_FONT, UiTokens, child_id, key_text};

pub const ROW_HEIGHT: f32 = 22.0;
pub const SHORTCUT_COLUMN: f32 = 132.0;
const KEY_WIDTH: f32 = 110.0;
const BAR_WIDTH: f32 = 10.0;
const NAME_INSET: f32 = 8.0;
const KEY_INSET: f32 = 6.0;
/// A long key leaves at least this much of its row's name.
const MIN_NAME_ROOM: f32 = 60.0;
const SUB_INSET: f32 = 18.0;
const TEXT_SIZE: f32 = 12.0;
pub(crate) const TEXT_TOKEN: &str = "Segoe UI 9";
/// How far a heading's second strike sits from its first. `DrawCommand::TextRun` has no
/// weight, and the renderer finds no "Segoe UI Semibold" family, so two strikes make the
/// heavier look. A weight field in `qe-api` would replace this.
pub(crate) const HEAVY_STRIKE: f32 = 0.4;
/// The counterclockwise arrow, U+E7A7 in Segoe Fluent Icons.
const RESET_ICON: char = '\u{e7a7}';
const DOUBLE_CLICK_US: u64 = 500_000;

const BACK: u16 = 0x08;
const ENTER: u16 = 0x0d;
const SPACE: u16 = 0x20;
const PAGE_UP: u16 = 0x21;
const PAGE_DOWN: u16 = 0x22;
const END: u16 = 0x23;
const HOME: u16 = 0x24;
const UP: u16 = 0x26;
const DOWN: u16 = 0x28;
const DELETE: u16 = 0x2e;

pub const HINT_WHILE_RECORDING: &str = "Press Esc to cancel, or Delete to remove the shortcut.";
pub const TYPE_SHORTCUT: &str = "Type shortcut";
pub const CHANGED_MARK: &str = " (changed)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Name,
    Key,
    Reset,
}

/// The table. It changes the store itself, and the store tells everyone else.
pub struct ShortcutTable {
    pub id: WidgetId,
    store: SettingsStore,
    rows: Vec<KeyRow>,
    pub bounds: Rect,
    selected: usize,
    first_visible: usize,
    pub focused: bool,
    recording: Option<CommandId>,
    held: Modifiers,
    refusal: Option<Refusal>,
    announcement: Option<String>,
    hover: Option<(usize, Part)>,
    /// The key that ended a recording. Its repeats must not reach the dialog, or a held
    /// Esc would go on to close the window.
    swallow: Option<u16>,
    bar: ScrollBar,
    last_click: Option<(u64, usize)>,
}
impl ShortcutTable {
    pub fn new(id: WidgetId, store: SettingsStore) -> Self {
        let rows = keyboard_rows();
        let selected = rows
            .iter()
            .position(|row| matches!(row, KeyRow::Command { .. }))
            .unwrap_or(0);
        let mut bar = ScrollBar::new(child_id(id, u64::MAX), 0.0, 0.0);
        bar.name = "Keyboard shortcuts scroll bar".into();
        bar.line = ROW_HEIGHT;
        Self {
            id,
            store,
            rows,
            bounds: Rect::default(),
            selected,
            first_visible: 0,
            focused: false,
            recording: None,
            held: Modifiers::default(),
            refusal: None,
            announcement: None,
            hover: None,
            swallow: None,
            bar,
            last_click: None,
        }
    }
    pub fn rows(&self) -> &[KeyRow] {
        &self.rows
    }
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }
    pub fn recording_command(&self) -> Option<CommandId> {
        self.recording
    }
    pub fn selected_command(&self) -> Option<CommandId> {
        self.command_at(self.selected)
    }
    pub fn refusal(&self) -> Option<&Refusal> {
        self.refusal.as_ref()
    }
    pub fn first_visible(&self) -> usize {
        self.first_visible
    }
    /// The line under the table, and whether it is a warning. A refusal comes first.
    pub fn message(&self) -> Option<(String, bool)> {
        if let Some(refused) = &self.refusal {
            return Some((refused.message(), true));
        }
        self.recording
            .map(|_| (HINT_WHILE_RECORDING.to_owned(), false))
    }
    /// What a screen reader should say now. Reading clears it.
    pub fn take_announcement(&mut self) -> Option<String> {
        self.announcement.take()
    }
    fn command_at(&self, row: usize) -> Option<CommandId> {
        match self.rows.get(row) {
            Some(KeyRow::Command { command, .. }) => Some(*command),
            _ => None,
        }
    }
    fn name_at(&self, row: usize) -> &str {
        match &self.rows[row] {
            KeyRow::Menu(name) | KeyRow::Submenu(name) => name,
            KeyRow::Command { name, .. } => name,
        }
    }
    /// The row of a command, or `None` if it has no row.
    pub fn row_of(&self, command: CommandId) -> Option<usize> {
        self.rows.iter().position(
            |row| matches!(row, KeyRow::Command { command: held, .. } if *held == command),
        )
    }
    fn capacity(&self) -> usize {
        (((self.bounds.height.0 - ROW_HEIGHT).max(0.0) / ROW_HEIGHT).floor() as usize).max(1)
    }
    fn max_first(&self) -> usize {
        self.rows.len().saturating_sub(self.capacity())
    }
    fn visible(&self) -> std::ops::Range<usize> {
        let first = self.first_visible.min(self.rows.len());
        first..(first + self.capacity()).min(self.rows.len())
    }
    fn sync_bar(&mut self) {
        let capacity = self.capacity();
        self.bar.content = self.rows.len() as f32 * ROW_HEIGHT;
        self.bar.viewport = capacity as f32 * ROW_HEIGHT;
        self.bar.offset = self.first_visible as f32 * ROW_HEIGHT;
        self.bar.bounds = Rect {
            x: Dip(self.bounds.x.0 + self.bounds.width.0 - BAR_WIDTH),
            y: Dip(self.bounds.y.0 + ROW_HEIGHT),
            width: Dip(BAR_WIDTH),
            height: Dip((self.bounds.height.0 - ROW_HEIGHT).max(0.0)),
        };
    }
    fn row_rect(&self, row: usize) -> Rect {
        Rect {
            x: self.bounds.x,
            y: Dip(self.bounds.y.0 + ROW_HEIGHT * (row as f32 - self.first_visible as f32 + 1.0)),
            width: Dip(self.bounds.width.0 - BAR_WIDTH),
            height: Dip(ROW_HEIGHT),
        }
    }
    fn column_x(&self) -> f32 {
        self.bounds.x.0 + self.bounds.width.0 - BAR_WIDTH - SHORTCUT_COLUMN
    }
    fn key_rect(&self, row: Rect) -> Rect {
        Rect {
            x: Dip(self.column_x()),
            y: row.y,
            width: Dip(KEY_WIDTH),
            height: row.height,
        }
    }
    fn reset_rect(&self, row: Rect) -> Rect {
        Rect {
            x: Dip(self.column_x() + KEY_WIDTH),
            y: row.y,
            width: Dip(SHORTCUT_COLUMN - KEY_WIDTH),
            height: row.height,
        }
    }
    fn hit(&self, at: Point) -> Option<(usize, Part)> {
        for row in self.visible() {
            let r = self.row_rect(row);
            if inside(r, at) {
                let part = if inside(self.key_rect(r), at) {
                    Part::Key
                } else if inside(self.reset_rect(r), at) {
                    Part::Reset
                } else {
                    Part::Name
                };
                return Some((row, part));
            }
        }
        None
    }
    fn invalidate(&self, c: &mut ViewContext) {
        c.invalidate(self.bounds);
    }
    fn select(&mut self, row: usize, down: bool, c: &mut ViewContext) {
        let last = self.rows.len().saturating_sub(1);
        let row = row.min(last);
        let forward = (row..=last).find(|i| self.command_at(*i).is_some());
        let backward = (0..=row).rev().find(|i| self.command_at(*i).is_some());
        let Some(found) = (if down {
            forward.or(backward)
        } else {
            backward.or(forward)
        }) else {
            return;
        };
        if found != self.selected {
            self.refusal = None;
        }
        self.selected = found;
        self.scroll_to_selected();
        self.invalidate(c);
    }
    fn scroll_to_selected(&mut self) {
        let capacity = self.capacity();
        let row = self.selected;
        if row < self.first_visible {
            self.first_visible = row;
        } else if row >= self.first_visible + capacity {
            self.first_visible = row + 1 - capacity;
        }
        // A section's title stays on screen with its first row.
        if row == self.first_visible && row > 0 && self.command_at(row - 1).is_none() {
            self.first_visible = row - 1;
        }
        self.first_visible = self.first_visible.min(self.max_first());
        self.sync_bar();
    }
    /// Starts recording a new key for the command.
    pub fn start_recording(&mut self, command: CommandId, c: &mut ViewContext) {
        let Some(row) = self.row_of(command) else {
            return;
        };
        self.selected = row;
        self.recording = Some(command);
        self.held = Modifiers::default();
        self.refusal = None;
        self.swallow = None;
        self.scroll_to_selected();
        self.announcement = Some(format!(
            "Recording a new shortcut for {}. {HINT_WHILE_RECORDING}",
            self.name_at(row)
        ));
        self.invalidate(c);
    }
    /// Ends recording with no change. The refusal goes with it.
    pub fn end_recording(&mut self, c: &mut ViewContext) {
        if self.recording.take().is_some() {
            self.held = Modifiers::default();
            self.refusal = None;
            self.invalidate(c);
        }
    }
    fn record(
        &mut self,
        virtual_key: u16,
        modifiers: Modifiers,
        repeat: bool,
        c: &mut ViewContext,
    ) {
        let Some(command) = self.recording else {
            return;
        };
        let name = self.name_at(self.selected).to_owned();
        match record_step(virtual_key, modifiers, repeat) {
            RecordStep::Ignore => {}
            RecordStep::Held(held) => {
                self.held = held;
                self.invalidate(c);
            }
            RecordStep::Cancel => {
                self.swallow = Some(virtual_key);
                self.end_recording(c);
            }
            RecordStep::Remove => {
                self.swallow = Some(virtual_key);
                self.store.clear_key(command);
                self.end_recording(c);
                self.announcement = Some(format!("{name} has no shortcut now."));
            }
            RecordStep::WindowsKey(key) => {
                self.held = Modifiers::default();
                self.refuse(Refusal::Windows(key), c);
            }
            RecordStep::Candidate(key) => {
                self.held = Modifiers::default();
                match self.store.set_key(command, key) {
                    Ok(()) => {
                        self.swallow = Some(virtual_key);
                        self.end_recording(c);
                        self.announcement =
                            Some(format!("The shortcut for {name} is now {}.", key_text(key)));
                    }
                    Err(refused) => self.refuse(refused, c),
                }
            }
        }
    }
    fn refuse(&mut self, refused: Refusal, c: &mut ViewContext) {
        self.announcement = Some(refused.message());
        self.refusal = Some(refused);
        self.invalidate(c);
    }
    /// Delete on a row, or its reset arrow. Another command may hold the default key.
    fn reset_row(&mut self, row: usize, c: &mut ViewContext) {
        let Some(command) = self.command_at(row) else {
            return;
        };
        if !self.store.is_key_changed(command) {
            return;
        }
        self.recording = None;
        match self.store.reset_key(command) {
            Ok(()) => {
                self.refusal = None;
                let key = command
                    .default_key()
                    .map_or_else(|| "none".to_owned(), key_text);
                self.announcement = Some(format!(
                    "{} is back to its default shortcut, {key}.",
                    self.name_at(row)
                ));
                self.invalidate(c);
            }
            Err(refused) => self.refuse(refused, c),
        }
    }
    /// Goes back to the table's defaults after Restore, with nothing being recorded.
    pub fn after_restore(&mut self, c: &mut ViewContext) {
        self.recording = None;
        self.held = Modifiers::default();
        self.refusal = None;
        self.invalidate(c);
    }
    fn key_down(
        &mut self,
        virtual_key: u16,
        modifiers: Modifiers,
        repeat: bool,
        c: &mut ViewContext,
    ) -> Handled {
        if self.recording.is_some() {
            self.record(virtual_key, modifiers, repeat, c);
            return Handled::Yes;
        }
        if self.swallow == Some(virtual_key) && repeat {
            return Handled::Yes;
        }
        self.swallow = None;
        if !self.focused || modifiers.alt {
            return Handled::No;
        }
        let page = self.capacity();
        let plain = !modifiers.ctrl && !modifiers.shift;
        match virtual_key {
            UP => self.select(self.selected.saturating_sub(1), false, c),
            DOWN => self.select(self.selected + 1, true, c),
            PAGE_UP => self.select(self.selected.saturating_sub(page), false, c),
            PAGE_DOWN => self.select(self.selected + page, true, c),
            HOME => self.select(0, true, c),
            END => self.select(self.rows.len().saturating_sub(1), false, c),
            ENTER | SPACE if plain && !repeat => {
                if let Some(command) = self.selected_command() {
                    self.start_recording(command, c);
                }
            }
            DELETE | BACK if plain && !repeat => self.reset_row(self.selected, c),
            _ => return Handled::No,
        }
        Handled::Yes
    }
    fn wheel(&mut self, delta_y: f32, c: &mut ViewContext) {
        let rows = (-delta_y / 120.0 * 3.0).round() as isize;
        let next = (self.first_visible as isize + rows).clamp(0, self.max_first() as isize);
        self.first_visible = next as usize;
        self.sync_bar();
        self.invalidate(c);
    }
    fn after_bar(&mut self, c: &mut ViewContext) {
        let first = (self.bar.offset / ROW_HEIGHT).round() as usize;
        if first != self.first_visible {
            self.first_visible = first.min(self.max_first());
            self.invalidate(c);
        }
    }
    fn mouse_down(&mut self, event: &InputEvent, at: Point, c: &mut ViewContext) -> Handled {
        if inside(self.bar.bounds, at) && self.bar.content > self.bar.viewport {
            let answer = self.bar.handle(event, c);
            self.after_bar(c);
            return answer;
        }
        let Some((row, part)) = self.hit(at) else {
            return Handled::Yes;
        };
        let Some(command) = self.command_at(row) else {
            return Handled::Yes;
        };
        if row != self.selected {
            self.refusal = None;
        }
        self.selected = row;
        self.invalidate(c);
        let double = self
            .last_click
            .replace((event.time_us, row))
            .is_some_and(|(time, was)| {
                was == row && event.time_us.saturating_sub(time) < DOUBLE_CLICK_US
            });
        match part {
            Part::Reset if self.store.is_key_changed(command) => self.reset_row(row, c),
            Part::Key => self.start_recording(command, c),
            _ if double => self.start_recording(command, c),
            _ => {}
        }
        Handled::Yes
    }
    /// Text in the table's size, centered on its row.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        l: &mut DisplayList,
        m: &dyn TextMeasurer,
        area: Rect,
        x: f32,
        text: &str,
        color: Color,
        semibold: bool,
    ) {
        if text.is_empty() {
            return;
        }
        let metrics = m.measure(text, TEXT_TOKEN);
        let y = Dip(area.y.0 + (area.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0);
        let strikes: &[f32] = if semibold {
            &[0.0, HEAVY_STRIKE]
        } else {
            &[0.0]
        };
        for shift in strikes {
            l.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(x + shift),
                    y,
                },
                text: text.into(),
                font: UI_FONT.into(),
                font_size: Dip(TEXT_SIZE),
                color,
            });
        }
    }
    fn line(&self, l: &mut DisplayList, r: Rect, color: Color) {
        let y = r.y.0 + r.height.0 - 0.5;
        l.push(DrawCommand::Line {
            from: Point { x: r.x, y: Dip(y) },
            to: Point {
                x: Dip(r.x.0 + r.width.0),
                y: Dip(y),
            },
            width: Dip(1.0),
            color,
        });
    }
    fn draw_row(
        &self,
        l: &mut DisplayList,
        theme: &Theme,
        m: &dyn TextMeasurer,
        t: UiTokens,
        row: usize,
    ) {
        let r = self.row_rect(row);
        l.push(DrawCommand::PushClip(r));
        let (name_x, section_ink) = match &self.rows[row] {
            KeyRow::Menu(_) => (NAME_INSET, Some(t.text)),
            KeyRow::Submenu(_) => (SUB_INSET, Some(t.hint)),
            KeyRow::Command { indented, .. } => {
                (if *indented { SUB_INSET } else { NAME_INSET }, None)
            }
        };
        if let Some(ink) = section_ink {
            if matches!(self.rows[row], KeyRow::Menu(_)) {
                section_fill(l, r, t);
            }
            self.text(l, m, r, r.x.0 + name_x, self.name_at(row), ink, true);
            self.line(l, r, theme.border);
            l.push(DrawCommand::PopClip);
            return;
        }
        let Some(command) = self.command_at(row) else {
            l.push(DrawCommand::PopClip);
            return;
        };
        let selected = row == self.selected;
        let hovered = self.hover.is_some_and(|(at, _)| at == row);
        let mut ink = t.text;
        let mut dim = t.hint;
        if selected {
            l.push(DrawCommand::FillRect {
                rect: r,
                color: t.selected,
            });
            l.push(DrawCommand::RoundedRect {
                rect: Rect {
                    x: Dip(r.x.0 + 2.0),
                    y: Dip(r.y.0 + 4.0),
                    width: Dip(3.0),
                    height: Dip(r.height.0 - 8.0),
                },
                radius: Dip(1.5),
                color: t.selected_mark,
            });
            ink = t.selected_text;
            dim = t.selected_text;
        } else if hovered {
            l.push(DrawCommand::FillRect {
                rect: r,
                color: t.hover,
            });
            ink = t.hover_text;
            dim = t.hover_text;
        }
        let changed = self.store.is_key_changed(command);
        let name = self.name_at(row);
        let key_area = self.key_rect(r);
        // A key too long for its cell grows to the left over the name's room, so two long
        // keys never look the same. A recording row shows no key.
        let key_shown = self
            .store
            .key_of(command)
            .filter(|_| self.recording != Some(command));
        let key_words = key_shown.map(key_text).unwrap_or_default();
        let key_width = m.measure(&key_words, TEXT_TOKEN).width.0;
        let key_x = if key_width > key_area.width.0 - KEY_INSET {
            (key_area.x.0 + key_area.width.0 - key_width - 1.0).max(r.x.0 + name_x + MIN_NAME_ROOM)
        } else {
            key_area.x.0 + KEY_INSET
        };
        let room_right = key_x.min(self.column_x()) - 2.0;
        let name_area = Rect {
            x: r.x,
            y: r.y,
            width: Dip((room_right - r.x.0).max(0.0)),
            height: r.height,
        };
        l.push(DrawCommand::PushClip(name_area));
        // The mark stays whole, and the name gives way with an ellipsis.
        let mark_width = if changed {
            m.measure(CHANGED_MARK, TEXT_TOKEN).width.0
        } else {
            0.0
        };
        let name_room = room_right - r.x.0 - name_x - mark_width;
        let name_words = fit_with_ellipsis(name, name_room, m);
        self.text(l, m, r, r.x.0 + name_x, &name_words, ink, false);
        if changed {
            let after = r.x.0 + name_x + m.measure(&name_words, TEXT_TOKEN).width.0;
            self.text(l, m, r, after, CHANGED_MARK, dim, false);
        }
        l.push(DrawCommand::PopClip);
        if self.recording == Some(command) {
            let held = self.held != Modifiers::default();
            let box_r = Rect {
                x: Dip(key_area.x.0 + 1.0),
                y: Dip(r.y.0 + 2.0),
                width: Dip(key_area.width.0 - 4.0),
                height: Dip(r.height.0 - 4.0),
            };
            l.push(DrawCommand::RoundedRect {
                rect: box_r,
                radius: t.radius,
                color: t.background,
            });
            l.push(DrawCommand::StrokeRect {
                rect: box_r,
                radius: t.radius,
                width: t.focus_width,
                color: t.focus,
            });
            let text = if held {
                held_text(self.held)
            } else {
                TYPE_SHORTCUT.to_owned()
            };
            let color = if held { t.text } else { t.hint };
            self.text(l, m, r, key_area.x.0 + 6.0, &text, color, false);
        } else if key_shown.is_some() {
            let clip = Rect {
                x: Dip(key_x.min(key_area.x.0)),
                y: key_area.y,
                width: Dip(key_area.x.0 + key_area.width.0 - key_x.min(key_area.x.0)),
                height: key_area.height,
            };
            l.push(DrawCommand::PushClip(clip));
            let words = fit_with_ellipsis(&key_words, clip.width.0, m);
            self.text(l, m, r, key_x, &words, ink, false);
            l.push(DrawCommand::PopClip);
        }
        if changed {
            let area = self.reset_rect(r);
            let glyph = RESET_ICON.to_string();
            let metrics = m.measure(&glyph, t.icon_measure_font);
            l.push(DrawCommand::Icon {
                at: Point {
                    x: Dip(area.x.0 + (area.width.0 - metrics.width.0) / 2.0),
                    y: Dip(area.y.0
                        + (area.height.0 - metrics.height.0) / 2.0
                        + metrics.baseline.0),
                },
                icon_id: format!("{:04X}", RESET_ICON as u32),
                font_size: t.icon_size,
                color: ink,
            });
        }
        self.line(l, r, theme.border);
        if self.focused && selected && self.recording.is_none() {
            l.push(DrawCommand::StrokeRect {
                rect: Rect {
                    x: Dip(r.x.0 + 1.0),
                    y: Dip(r.y.0 + 1.0),
                    width: Dip(r.width.0 - 2.0),
                    height: Dip(r.height.0 - 2.0),
                },
                radius: t.radius,
                width: t.focus_width,
                color: t.focus_on_selected,
            });
        }
        l.push(DrawCommand::PopClip);
    }
}
/// The text as it is, or cut to fit with an ellipsis, so a cut always shows.
fn fit_with_ellipsis(text: &str, width: f32, m: &dyn TextMeasurer) -> String {
    if m.measure(text, TEXT_TOKEN).width.0 <= width {
        return text.to_owned();
    }
    let mut kept = text.to_owned();
    while kept.pop().is_some() {
        let candidate = format!("{}\u{2026}", kept.trim_end());
        if m.measure(&candidate, TEXT_TOKEN).width.0 <= width {
            return candidate;
        }
    }
    "\u{2026}".to_owned()
}
/// The soft band behind a heading row. High contrast has no soft color: its hover color
/// is the highlight, which white heading text cannot be read on, so it has no band.
fn section_fill(l: &mut DisplayList, r: Rect, t: UiTokens) {
    if !t.high_contrast {
        l.push(DrawCommand::FillRect {
            rect: r,
            color: t.hover,
        });
    }
}
fn inside(r: Rect, p: Point) -> bool {
    p.x.0 >= r.x.0 && p.y.0 >= r.y.0 && p.x.0 < r.x.0 + r.width.0 && p.y.0 < r.y.0 + r.height.0
}
impl View for ShortcutTable {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                self.invalidate(c);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.end_recording(c);
                self.swallow = None;
                self.bar.handle(e, c);
                self.invalidate(c);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                repeat,
            } => self.key_down(*virtual_key, *modifiers, *repeat, c),
            InputKind::KeyUp {
                virtual_key,
                modifiers,
            } => {
                if self.swallow == Some(*virtual_key) {
                    self.swallow = None;
                }
                if self.recording.is_none() {
                    return Handled::No;
                }
                // A key's release is not a press: only a modifier's release counts.
                if let RecordStep::Held(held) = record_step(*virtual_key, *modifiers, false) {
                    self.held = held;
                    self.invalidate(c);
                }
                Handled::Yes
            }
            InputKind::Char(_) if self.recording.is_some() => Handled::Yes,
            InputKind::MouseMove { at } => {
                let answer = self.bar.handle(e, c);
                if self.bar.dragging {
                    self.after_bar(c);
                    return Handled::Yes;
                }
                let now = if inside(self.bounds, *at) {
                    self.hit(*at)
                } else {
                    None
                };
                if now != self.hover {
                    self.hover = now;
                    self.invalidate(c);
                }
                if now.is_some() { Handled::Yes } else { answer }
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } if inside(self.bounds, *at) => self.mouse_down(e, *at, c),
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.bar.dragging => {
                let answer = self.bar.handle(e, c);
                self.after_bar(c);
                answer
            }
            InputKind::Wheel { at, delta_y, .. } if inside(self.bounds, *at) => {
                self.wheel(*delta_y, c);
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
        l.push(DrawCommand::PushClip(self.bounds));
        let head = Rect {
            x: self.bounds.x,
            y: self.bounds.y,
            width: self.bounds.width,
            height: Dip(ROW_HEIGHT),
        };
        section_fill(l, head, t);
        self.text(l, m, head, head.x.0 + NAME_INSET, "Command", t.text, true);
        self.text(l, m, head, self.column_x() + 6.0, "Shortcut", t.text, true);
        self.line(l, head, theme.border);
        l.push(DrawCommand::PopClip);
        for row in self.visible() {
            self.draw_row(l, theme, m, t, row);
        }
        l.push(DrawCommand::StrokeRect {
            rect: self.bounds,
            radius: t.radius,
            width: Dip(1.0),
            color: t.border,
        });
        self.bar.draw(l, theme, m);
    }
    fn layout(&mut self, bounds: Rect, _: &dyn TextMeasurer) {
        self.bounds = bounds;
        self.first_visible = self.first_visible.min(self.max_first());
        self.sync_bar();
    }
    fn widgets(&self) -> Option<Widget> {
        let mut children = vec![header_node(self)];
        let visible = self.visible();
        for row in 0..self.rows.len() {
            children.push(row_node(self, row, visible.contains(&row)));
        }
        if self.bar.content > self.bar.viewport
            && let Some(bar) = self.bar.widgets()
        {
            children.push(bar);
        }
        let commands = self
            .rows
            .iter()
            .filter(|row| matches!(row, KeyRow::Command { .. }))
            .count();
        Some(Widget {
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Table,
            name: "Keyboard shortcuts".into(),
            value: Some(format!("{commands} commands")),
            states: enabled(false),
            selection: None,
            range_value: None,
            description: Some(
                "Press Enter to record a new shortcut for the selected row, or Delete to reset it."
                    .into(),
            ),
            children,
        })
    }
}
fn enabled(focused: bool) -> WidgetStates {
    WidgetStates {
        enabled: true,
        focused,
        ..Default::default()
    }
}
fn plain_node(
    id: WidgetId,
    bounds: Rect,
    role: WidgetRole,
    name: String,
    value: Option<String>,
    states: WidgetStates,
    children: Vec<Widget>,
) -> Widget {
    Widget {
        id,
        bounds,
        role,
        name,
        value,
        states,
        selection: None,
        range_value: None,
        description: None,
        children,
    }
}
fn header_node(table: &ShortcutTable) -> Widget {
    let bounds = Rect {
        x: table.bounds.x,
        y: table.bounds.y,
        width: table.bounds.width,
        height: Dip(ROW_HEIGHT),
    };
    let cell = |slot: u64, name: &str| {
        plain_node(
            child_id(table.id, slot),
            bounds,
            WidgetRole::Cell,
            name.into(),
            None,
            enabled(false),
            vec![],
        )
    };
    plain_node(
        child_id(table.id, 1),
        bounds,
        WidgetRole::Row,
        "Column headings".into(),
        None,
        enabled(false),
        vec![cell(2, "Command"), cell(3, "Shortcut")],
    )
}
fn row_node(table: &ShortcutTable, row: usize, visible: bool) -> Widget {
    let bounds = if visible {
        table.row_rect(row)
    } else {
        Rect::default()
    };
    let base = 16 + row as u64 * 4;
    let name = table.name_at(row).to_owned();
    let Some(command) = table.command_at(row) else {
        return plain_node(
            child_id(table.id, base),
            bounds,
            WidgetRole::Row,
            name,
            None,
            WidgetStates {
                enabled: true,
                section: true,
                ..Default::default()
            },
            vec![],
        );
    };
    let selected = row == table.selected;
    let changed = table.store.is_key_changed(command);
    let recording = table.recording == Some(command);
    let key_value = if recording {
        TYPE_SHORTCUT.to_owned()
    } else {
        table
            .store
            .key_of(command)
            .map_or_else(|| "none".to_owned(), key_text)
    };
    let (key_area, reset_area) = if visible {
        (table.key_rect(bounds), table.reset_rect(bounds))
    } else {
        (Rect::default(), Rect::default())
    };
    let mut children = vec![
        plain_node(
            child_id(table.id, base + 1),
            bounds,
            WidgetRole::Cell,
            name.clone(),
            changed.then(|| "changed".to_owned()),
            enabled(false),
            vec![],
        ),
        plain_node(
            child_id(table.id, base + 2),
            key_area,
            WidgetRole::Button,
            format!("Shortcut for {name}"),
            Some(key_value),
            enabled(table.focused && selected),
            vec![],
        ),
    ];
    if changed {
        children.push(plain_node(
            child_id(table.id, base + 3),
            reset_area,
            WidgetRole::Button,
            format!("Reset {name} to its default shortcut"),
            None,
            enabled(false),
            vec![],
        ));
    }
    plain_node(
        child_id(table.id, base),
        bounds,
        WidgetRole::Row,
        name,
        None,
        WidgetStates {
            enabled: true,
            selected,
            ..Default::default()
        },
        children,
    )
}
impl Control for ShortcutTable {
    fn focusable(&self) -> bool {
        true
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        self.recording.is_some()
            || self.swallow == Some(virtual_key)
            || matches!(virtual_key, ENTER | SPACE)
    }
    fn draw_base(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        self.draw(list, theme, measure);
    }
}
