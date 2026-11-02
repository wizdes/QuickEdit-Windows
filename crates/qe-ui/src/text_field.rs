use crate::control::*;
use crate::controls::{
    clip, contains, focus_ring, node, rect, state, text_line, text_width, unclip,
};
use crate::{Control, SharedClipboard, SystemClipboard, UiTokens};
use qe_api::{
    Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, MouseButton, MouseCursor, Point,
    Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId, WidgetRole,
};
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

pub type NumericValidator = fn(&str) -> bool;

/// Undo keeps at most this many steps.
pub const UNDO_MAX_STEPS: usize = 256;
/// Undo keeps at most this many bytes of removed and inserted text.
pub const UNDO_MAX_BYTES: usize = 256 * 1024;
const MAX_CARET_STOPS: usize = 512;

/// One edit, stored as the change and not as a copy of the whole text.
#[derive(Clone, Debug)]
struct UndoStep {
    at: usize,
    removed: String,
    inserted: String,
    before: (usize, usize),
    after: (usize, usize),
}
impl UndoStep {
    fn bytes(&self) -> usize {
        self.removed.len() + self.inserted.len()
    }
}

/// The caret positions of the text at the last `layout` or `draw`. `handle` has no
/// measurer, so a click looks its position up here.
#[derive(Clone, Debug, Default)]
struct CaretStops {
    text: String,
    stops: Vec<(usize, f32)>,
}

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
    pub clipboard: SharedClipboard,
    /// The last typed, pasted or input-method text that the validator refused. A
    /// caller reads it to explain the refusal. The next accepted edit clears it.
    pub rejected_input: Option<String>,
    /// The last clipboard failure, in words.
    pub clipboard_error: Option<String>,
    undo: Vec<UndoStep>,
    redo: Vec<UndoStep>,
    undo_bytes: usize,
    revision: u64,
    dragging: bool,
    scroll_x: Cell<f32>,
    cache: RefCell<CaretStops>,
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
            clipboard: Arc::new(SystemClipboard),
            rejected_input: None,
            clipboard_error: None,
            undo: vec![],
            redo: vec![],
            undo_bytes: 0,
            revision: 0,
            dragging: false,
            scroll_x: Cell::new(0.0),
            cache: RefCell::new(CaretStops::default()),
        }
    }
    pub fn with_clipboard(mut self, clipboard: SharedClipboard) -> Self {
        self.clipboard = clipboard;
        self
    }
    /// How far the text is scrolled to the left.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_x.get()
    }
    /// The number of undo steps held, for tests of the limit.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    /// The bytes of text held by undo, for tests of the limit.
    pub fn undo_held_bytes(&self) -> usize {
        self.undo_bytes
    }
    /// The caret and the other end of the selection in UTF-16 units, for UI
    /// Automation once `qe_api::Widget` can carry them.
    pub fn selection_utf16(&self) -> (usize, usize) {
        let (caret, anchor) = self.ends();
        let units = |byte: usize| self.text[..byte].encode_utf16().count();
        (units(caret), units(anchor))
    }
    /// A byte offset moved back to the start of its character and inside the text.
    /// A caller may shorten `text` at any time.
    fn clamp(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }
    fn ends(&self) -> (usize, usize) {
        (self.clamp(self.caret), self.clamp(self.anchor))
    }
    fn selected(&self) -> (usize, usize) {
        let (caret, anchor) = self.ends();
        (caret.min(anchor), caret.max(anchor))
    }
    /// Replaces a range. Refused text changes nothing and is kept for the caller.
    fn replace_range(&mut self, a: usize, b: usize, s: &str) -> bool {
        if a == b && s.is_empty() {
            return true;
        }
        let mut next = String::with_capacity(self.text.len() - (b - a) + s.len());
        next.push_str(&self.text[..a]);
        next.push_str(s);
        next.push_str(&self.text[b..]);
        if self.validator.is_some_and(|v| !v(&next)) {
            self.rejected_input = Some(s.to_owned());
            return false;
        }
        self.rejected_input = None;
        let after = a + s.len();
        let step = UndoStep {
            at: a,
            removed: self.text[a..b].to_owned(),
            inserted: s.to_owned(),
            before: self.ends(),
            after: (after, after),
        };
        self.text = next;
        self.revision += 1;
        self.caret = after;
        self.anchor = after;
        self.push_undo(step);
        self.redo.clear();
        true
    }
    fn replace_selection(&mut self, s: &str) -> bool {
        let (a, b) = self.selected();
        self.replace_range(a, b, s)
    }
    fn push_undo(&mut self, step: UndoStep) {
        self.undo_bytes += step.bytes();
        self.undo.push(step);
        while self.undo.len() > UNDO_MAX_STEPS
            || (self.undo_bytes > UNDO_MAX_BYTES && self.undo.len() > 1)
        {
            let old = self.undo.remove(0);
            self.undo_bytes -= old.bytes();
        }
    }
    /// True when a step still fits the text. A caller who changed `text` ends history.
    fn step_applies(&self, at: usize, expected: &str) -> bool {
        self.text
            .get(at..at + expected.len())
            .is_some_and(|s| s == expected)
    }
    fn undo_once(&mut self) {
        let Some(step) = self.undo.pop() else { return };
        self.undo_bytes -= step.bytes();
        if !self.step_applies(step.at, &step.inserted) {
            self.undo.clear();
            self.redo.clear();
            self.undo_bytes = 0;
            return;
        }
        self.text
            .replace_range(step.at..step.at + step.inserted.len(), &step.removed);
        self.revision += 1;
        (self.caret, self.anchor) = step.before;
        self.redo.push(step);
    }
    fn redo_once(&mut self) {
        let Some(step) = self.redo.pop() else { return };
        if !self.step_applies(step.at, &step.removed) {
            self.redo.clear();
            return;
        }
        self.text
            .replace_range(step.at..step.at + step.removed.len(), &step.inserted);
        self.revision += 1;
        (self.caret, self.anchor) = step.after;
        self.push_undo(step);
    }
    fn prev(&self, from: usize) -> usize {
        let mut cursor = GraphemeCursor::new(from, self.text.len(), true);
        cursor
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(0)
    }
    fn next(&self, from: usize) -> usize {
        let mut cursor = GraphemeCursor::new(from, self.text.len(), true);
        cursor
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(self.text.len())
    }
    fn word_prev(&self, from: usize) -> usize {
        let mut found = 0;
        for (at, word) in self.text[..from].split_word_bound_indices() {
            if word.chars().any(|c| !c.is_whitespace()) {
                found = at;
            }
        }
        found
    }
    fn word_next(&self, from: usize) -> usize {
        let mut seen_word = false;
        for (at, word) in self.text[from..].split_word_bound_indices() {
            let blank = word.chars().all(char::is_whitespace);
            if seen_word && !blank {
                return from + at;
            }
            seen_word = true;
        }
        self.text.len()
    }
    fn copy(&mut self) -> bool {
        let (a, b) = self.selected();
        if a == b {
            return false;
        }
        match self.clipboard.write_text(&self.text[a..b]) {
            Ok(()) => {
                self.clipboard_error = None;
                true
            }
            Err(error) => {
                self.clipboard_error = Some(error);
                false
            }
        }
    }
    fn paste(&mut self) {
        match self.clipboard.read_text() {
            Ok(Some(value)) => {
                self.clipboard_error = None;
                // One line of text: line breaks become spaces.
                let value = value.replace("\r\n", " ").replace(['\r', '\n'], " ");
                if !value.is_empty() {
                    self.replace_selection(&value);
                }
            }
            Ok(None) => self.clipboard_error = None,
            Err(error) => self.clipboard_error = Some(error),
        }
    }
    fn commit_composition(&mut self) {
        if let Some((value, _)) = self.composition.take()
            && !value.is_empty()
        {
            self.replace_selection(&value);
        }
    }
    fn text_origin(&self, t: UiTokens) -> f32 {
        self.bounds.x.0 + t.inset.0
    }
    fn rebuild_stops(&self, m: &dyn TextMeasurer, t: UiTokens) {
        let mut cache = self.cache.borrow_mut();
        if cache.text == self.text && !cache.stops.is_empty() {
            return;
        }
        let bounds: Vec<usize> = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(self.text.len()))
            .collect();
        // Long text measures at most MAX_CARET_STOPS stops. The rest are placed
        // between them, which can be off by a little in a click on very long text.
        let stride = (bounds.len() / MAX_CARET_STOPS).max(1);
        let mut stops = Vec::with_capacity(bounds.len());
        let mut last = (0usize, 0.0f32);
        for (n, at) in bounds.iter().enumerate() {
            if n % stride == 0 || n + 1 == bounds.len() {
                let x = text_width(m, t, &self.text[..*at]);
                if stride > 1 {
                    let span = n - last.0;
                    for k in 1..span {
                        let part = k as f32 / span as f32;
                        stops.push((bounds[last.0 + k], last.1 + (x - last.1) * part));
                    }
                }
                stops.push((*at, x));
                last = (n, x);
            }
        }
        cache.text.clone_from(&self.text);
        cache.stops = stops;
    }
    /// The caret position nearest to a point. If the text changed after the last
    /// draw, the old stops are clamped to the new text.
    fn hit_test(&self, at: Point) -> usize {
        let t = UiTokens::sizes();
        let x = at.x.0 - self.text_origin(t) + self.scroll_x.get();
        let cache = self.cache.borrow();
        let mut best = (self.text.len(), f32::MAX);
        for (byte, stop) in &cache.stops {
            let distance = (stop - x).abs();
            if distance < best.1 {
                best = (*byte, distance);
            }
        }
        self.clamp(best.0)
    }
    /// The text as shown, with an input-method preview in place of the selection.
    /// Also gives the preview's byte range.
    fn shown(&self) -> (String, Option<(usize, usize)>) {
        match &self.composition {
            Some((value, _)) => {
                let (a, b) = self.selected();
                let mut s = String::with_capacity(self.text.len() + value.len());
                s.push_str(&self.text[..a]);
                s.push_str(value);
                s.push_str(&self.text[b..]);
                (s, Some((a, a + value.len())))
            }
            None => (self.text.clone(), None),
        }
    }
    /// The caret's byte in the shown text. Input methods give it in UTF-16 units.
    fn shown_caret(&self, preview: Option<(usize, usize)>) -> usize {
        match (&self.composition, preview) {
            (Some((value, cursor)), Some((start, end))) => {
                let mut units = 0;
                for (i, ch) in value.char_indices() {
                    if units >= *cursor {
                        return start + i;
                    }
                    units += ch.len_utf16();
                }
                end
            }
            _ => self.ends().0,
        }
    }
    fn stop_drag(&mut self, c: &mut ViewContext) {
        if self.dragging {
            self.dragging = false;
            c.capture_mouse(false);
        }
    }
}
impl View for TextField {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        match &e.kind {
            InputKind::Blur => {
                self.focused = false;
                self.stop_drag(c);
                // Windows commits a preview when focus leaves, so the text is kept.
                self.commit_composition();
                c.invalidate(self.bounds);
                return Handled::Yes;
            }
            _ if !self.enabled => return Handled::No,
            _ => {}
        }
        let before = (self.caret, self.anchor, self.revision);
        let handled = match &e.kind {
            InputKind::Focus => {
                self.focused = true;
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers,
            } if contains(self.bounds, *at) => {
                self.focused = true;
                c.set_focus(self.id);
                self.caret = self.hit_test(*at);
                if !modifiers.shift {
                    self.anchor = self.caret;
                }
                self.dragging = true;
                c.capture_mouse(true);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseMove { at } => {
                if self.dragging {
                    let next = self.hit_test(*at);
                    if next != self.caret {
                        self.caret = next;
                        c.invalidate(self.bounds);
                    }
                    Handled::Yes
                } else if contains(self.bounds, *at) {
                    c.set_cursor(MouseCursor::IBeam);
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.dragging => {
                self.stop_drag(c);
                Handled::Yes
            }
            InputKind::Char(ch) if self.focused && !ch.is_control() => {
                self.replace_selection(&ch.to_string());
                Handled::Yes
            }
            InputKind::ImeStart if self.focused => {
                self.composition = Some((String::new(), 0));
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::ImeUpdate { text, cursor_utf16 } if self.focused => {
                self.composition = Some((text.clone(), *cursor_utf16));
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::ImeEnd { text } if self.focused => {
                self.composition = None;
                self.replace_selection(text);
                c.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } if self.focused => {
                // AltGr arrives as Ctrl+Alt. It types characters and is no shortcut.
                let ctrl = modifiers.ctrl && !modifiers.alt;
                let shift = modifiers.shift;
                if modifiers.alt {
                    return Handled::No;
                }
                let (caret, _) = self.ends();
                let (a, b) = self.selected();
                let has_selection = a != b;
                match (*virtual_key, ctrl, shift) {
                    (LEFT, false, _) => {
                        self.caret = if has_selection && !shift {
                            a
                        } else {
                            self.prev(caret)
                        };
                    }
                    (RIGHT, false, _) => {
                        self.caret = if has_selection && !shift {
                            b
                        } else {
                            self.next(caret)
                        };
                    }
                    (LEFT, true, _) => self.caret = self.word_prev(caret),
                    (RIGHT, true, _) => self.caret = self.word_next(caret),
                    (HOME, _, _) => self.caret = 0,
                    (END, _, _) => self.caret = self.text.len(),
                    (BACK, _, _) if has_selection => {
                        self.replace_range(a, b, "");
                    }
                    (BACK, true, _) => {
                        self.replace_range(self.word_prev(caret), caret, "");
                    }
                    (BACK, false, _) => {
                        self.replace_range(self.prev(caret), caret, "");
                    }
                    (DELETE, false, true) => {
                        if self.copy() {
                            self.replace_range(a, b, "");
                        }
                    }
                    (DELETE, _, _) if has_selection => {
                        self.replace_range(a, b, "");
                    }
                    (DELETE, true, _) => {
                        self.replace_range(caret, self.word_next(caret), "");
                    }
                    (DELETE, false, _) => {
                        self.replace_range(caret, self.next(caret), "");
                    }
                    (INSERT, true, false) | (0x43, true, false) => {
                        self.copy();
                    }
                    (INSERT, false, true) | (0x56, true, false) => self.paste(),
                    (0x58, true, false) => {
                        if self.copy() {
                            self.replace_range(a, b, "");
                        }
                    }
                    (0x41, true, false) => {
                        self.anchor = 0;
                        self.caret = self.text.len();
                    }
                    (0x5a, true, false) => self.undo_once(),
                    (0x59, true, false) | (0x5a, true, true) => self.redo_once(),
                    _ => return Handled::No,
                }
                if matches!(*virtual_key, LEFT | RIGHT | HOME | END) && !shift {
                    self.anchor = self.caret;
                }
                Handled::Yes
            }
            _ => Handled::No,
        };
        if before != (self.caret, self.anchor, self.revision) {
            c.invalidate(self.bounds);
        }
        handled
    }
    fn draw(&self, l: &mut DisplayList, theme: &Theme, m: &dyn TextMeasurer) {
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
        self.rebuild_stops(m, t);
        let (shown, preview) = self.shown();
        let inner = inset_text_area(self.bounds, t);
        let origin = self.text_origin(t);
        let caret_byte = self.shown_caret(preview);
        let caret_x = text_width(m, t, &shown[..caret_byte]);
        let total = text_width(m, t, &shown);
        // Keeps the caret in view, and shows no empty space after the text.
        let room = inner.width.0 - 1.0;
        let mut scroll = self.scroll_x.get();
        if caret_x < scroll {
            scroll = caret_x;
        } else if caret_x - scroll > room {
            scroll = caret_x - room;
        }
        scroll = scroll.min((total - room).max(0.0)).max(0.0);
        self.scroll_x.set(scroll);

        clip(l, inner);
        let box_y = self.bounds.y.0;
        let box_h = self.bounds.height.0;
        let metrics = m.measure("Ag", t.measure_font);
        let line_top = box_y + (box_h - metrics.height.0) / 2.0;
        let start = origin - scroll;
        if preview.is_none() {
            let (a, b) = self.selected();
            if a < b && self.enabled {
                let x = text_width(m, t, &self.text[..a]);
                let w = text_width(m, t, &self.text[a..b]);
                l.push(DrawCommand::FillRect {
                    rect: rect(start + x, line_top, w, metrics.height.0),
                    color: t.selected,
                });
            }
        }
        let empty = shown.is_empty();
        let ink = if empty || !self.enabled {
            t.disabled
        } else {
            t.text
        };
        let (a, b) = self.selected();
        if preview.is_none() && a < b && t.selected_text != t.text && self.enabled {
            // A system selection color needs its own text color, so the three parts
            // of the line paint apart.
            let parts = [
                (0.0, &self.text[..a], ink),
                (
                    text_width(m, t, &self.text[..a]),
                    &self.text[a..b],
                    t.selected_text,
                ),
                (text_width(m, t, &self.text[..b]), &self.text[b..], ink),
            ];
            for (x, part, color) in parts {
                text_line(l, m, t, start + x, box_y, box_h, part, color);
            }
        } else {
            text_line(
                l,
                m,
                t,
                start,
                box_y,
                box_h,
                if empty { &self.placeholder } else { &shown },
                ink,
            );
        }
        if let Some((from, to)) = preview
            && from < to
        {
            let x0 = text_width(m, t, &shown[..from]);
            let x1 = text_width(m, t, &shown[..to]);
            let y = line_top + metrics.height.0 - 1.0;
            l.push(DrawCommand::Line {
                from: Point {
                    x: Dip(start + x0),
                    y: Dip(y),
                },
                to: Point {
                    x: Dip(start + x1),
                    y: Dip(y),
                },
                width: Dip(1.0),
                color: t.text,
            });
        }
        if self.focused && self.enabled {
            let x = start + caret_x;
            l.push(DrawCommand::Line {
                from: Point {
                    x: Dip(x),
                    y: Dip(line_top),
                },
                to: Point {
                    x: Dip(x),
                    y: Dip(line_top + metrics.height.0),
                },
                width: Dip(1.0),
                color: t.text,
            });
        }
        unclip(l);
        if self.focused && self.enabled {
            focus_ring(l, self.bounds, t)
        }
    }
    fn layout(&mut self, r: Rect, m: &dyn TextMeasurer) {
        self.bounds = r;
        self.rebuild_stops(m, UiTokens::sizes());
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
fn inset_text_area(bounds: Rect, t: UiTokens) -> Rect {
    rect(
        bounds.x.0 + t.inset.0,
        bounds.y.0 + 2.0,
        bounds.width.0 - 2.0 * t.inset.0,
        bounds.height.0 - 4.0,
    )
}
impl Control for TextField {
    fn focusable(&self) -> bool {
        self.enabled
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        // Esc belongs to the input method while a preview is open.
        virtual_key == ESCAPE && self.composition.is_some()
    }
}
