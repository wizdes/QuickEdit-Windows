//! The message bar above the text. A warning stays here until the person deals with it: Esc or
//! a click dismisses a note, and a failed autosave stays until a copy succeeds. Long text wraps,
//! and Esc pages through what does not fit. The bar costs nothing while there is nothing to say.
use crate::app::{App, Effect};
use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Rect, TextMeasurer, Theme, ViewContext};
use qe_api::{Widget, WidgetId, WidgetRole, WidgetStates};
use qe_docs::{DocId, WindowId};

const FONT: &str = "Segoe UI";
const FONT_POINTS: f32 = 9.0;
const LINE: f32 = 18.0;
const BASELINE: f32 = 13.0;
const PAD: f32 = 4.0;
const LEFT: f32 = 8.0;
const NOTICE_WIDGET: u64 = 19_100;
const MORE: &str = "More below. Esc shows it.";

/// The wrapped lines for one set of messages at one width, kept until either changes.
#[derive(Default)]
pub struct BarLayout {
    width: f32,
    max_lines: usize,
    texts: Vec<String>,
    lines: Vec<String>,
    /// Lines already read, skipped by Esc.
    first: usize,
    /// Where the bar starts, in the window.
    top: f32,
}

/// Breaks one message into lines of at most `width`. A word too long for a line, such as a
/// path, breaks where it must, so that its end still shows.
fn wrap(text: &str, width: f32, measure: &dyn TextMeasurer) -> Vec<String> {
    let token = format!("{FONT} {FONT_POINTS}");
    let width_of = |piece: &str| measure.measure(piece, &token).width.0;
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let joined = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if width_of(&joined) <= width {
                line = joined;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut rest = word;
            while width_of(rest) > width {
                let mut cut = rest.len();
                let mut low = 1;
                while low < cut {
                    let middle = (low + cut).div_ceil(2);
                    let at = (0..=middle)
                        .rev()
                        .find(|at| rest.is_char_boundary(*at))
                        .unwrap_or(0);
                    if at > 0 && width_of(&rest[..at]) <= width {
                        low = middle;
                    } else {
                        cut = middle - 1;
                    }
                }
                let at = (1..=low.max(1))
                    .rev()
                    .find(|at| rest.is_char_boundary(*at))
                    .or_else(|| rest.char_indices().nth(1).map(|(at, _)| at))
                    .unwrap_or(rest.len());
                lines.push(rest[..at].to_owned());
                rest = &rest[at..];
            }
            line = rest.to_owned();
        }
        lines.push(line);
    }
    lines
}

/// Paints the bar at the top of `area`.
pub fn draw(list: &mut DisplayList, area: Rect, lines: &[String], theme: &Theme) {
    if lines.is_empty() {
        return;
    }
    let height = bar_height(lines.len());
    list.push(DrawCommand::FillRect {
        rect: Rect {
            height: Dip(height.min(area.height.0)),
            ..area
        },
        color: theme.text_background,
    });
    list.push(DrawCommand::FillRect {
        rect: Rect {
            y: Dip(area.y.0 + height - 1.0),
            height: Dip(1.0),
            ..area
        },
        color: Color {
            a: 255,
            ..theme.warning
        },
    });
    for (at, line) in lines.iter().enumerate() {
        list.push(DrawCommand::TextRun {
            at: Point {
                x: Dip(area.x.0 + LEFT),
                y: Dip(area.y.0 + PAD + at as f32 * LINE + BASELINE),
            },
            text: line.clone(),
            font: FONT.into(),
            font_size: Dip(FONT_POINTS * 96.0 / 72.0),
            color: theme.warning,
        });
    }
}

fn bar_height(lines: usize) -> f32 {
    if lines == 0 {
        0.0
    } else {
        lines as f32 * LINE + PAD * 2.0
    }
}

impl App {
    /// What this document's bar says, most urgent first. Nothing allocates when it is empty.
    fn notice_texts(&self, doc: DocId) -> Vec<String> {
        let mut texts = Vec::new();
        if self.autosave_failing.contains(&doc)
            && let Some(failure) = self.store.autosave_failure(doc)
        {
            texts.push(format!(
                "Unsaved text has no safe copy, so a crash would lose it. Save it to keep it. {failure}"
            ));
        }
        if let Some(warning) = self.store.view(doc).and_then(|view| view.warning()) {
            texts.push(warning.to_owned());
        }
        if let Some(ui) = self.docs.get(&doc) {
            texts.extend(ui.notices.iter().cloned());
        }
        texts
    }

    /// `area` less the bar, whose lines are worked out here when anything has changed.
    pub fn text_area(&mut self, doc: DocId, area: Rect) -> Rect {
        let texts = self.notice_texts(doc);
        let measure = self.measure.clone();
        let Some(ui) = self.docs.get_mut(&doc) else {
            return area;
        };
        if texts.is_empty() && ui.bar.texts.is_empty() {
            return area;
        }
        let width = (area.width.0 - LEFT * 2.0).max(40.0);
        let max_lines = ((area.height.0 * 2.0 / 3.0 / LINE) as usize).max(1);
        let bar = &mut ui.bar;
        bar.top = area.y.0;
        if bar.width != width || bar.texts != texts || bar.max_lines != max_lines {
            if bar.texts != texts {
                bar.first = 0;
            }
            bar.width = width;
            bar.max_lines = max_lines;
            bar.lines = texts
                .iter()
                .flat_map(|text| wrap(text, width, measure.as_ref()))
                .collect();
            bar.texts = texts;
        }
        let height = bar_height(shown_count(bar)).min(area.height.0);
        Rect {
            y: Dip(area.y.0 + height),
            height: Dip(area.height.0 - height),
            ..area
        }
    }

    /// The lines on show now.
    pub fn notice_lines(&self, doc: DocId) -> Vec<String> {
        let Some(ui) = self.docs.get(&doc) else {
            return Vec::new();
        };
        let bar = &ui.bar;
        let count = shown_count(bar);
        let mut lines: Vec<String> = bar
            .lines
            .iter()
            .skip(bar.first)
            .take(count)
            .cloned()
            .collect();
        if bar.lines.len() - bar.first.min(bar.lines.len()) > count
            && let Some(last) = lines.last_mut()
        {
            MORE.clone_into(last);
        }
        lines
    }

    /// A message for this document. An identical one on show is not said twice.
    pub fn add_notice(&mut self, doc: DocId, text: String) {
        let Some(ui) = self.doc_ui(doc) else {
            self.waiting_notes.push(text);
            return;
        };
        if ui.notices.contains(&text) {
            return;
        }
        ui.notices.push(text.clone());
        self.notices_changed(doc);
        if let Some(window) = self.store.window_of(doc)
            && self
                .windows
                .get(&window)
                .is_some_and(|win| win.shown == Some(doc) && win.slot.current().is_some())
        {
            self.effects.push(Effect::Announce(window, text));
        }
    }

    /// Lays the window out again and repaints, when this document is on show.
    pub fn notices_changed(&mut self, doc: DocId) {
        let Some(window) = self.store.window_of(doc) else {
            return;
        };
        let Some(win) = self.windows.get(&window) else {
            return;
        };
        if win.shown != Some(doc) {
            return;
        }
        let content = win.content;
        self.relayout(window);
        let mut ctx = ViewContext::default();
        ctx.invalidate(content);
        self.apply(window, ctx.requests);
    }

    /// Esc or a click on the bar: the next page of a long message, or else the end of the
    /// first note. A failed autosave cannot be dismissed. False when there was nothing to do.
    pub fn dismiss_notice(&mut self, doc: DocId) -> bool {
        let Some(ui) = self.docs.get_mut(&doc) else {
            return false;
        };
        let count = shown_count(&ui.bar);
        if ui.bar.first + count < ui.bar.lines.len() {
            ui.bar.first += count.saturating_sub(1).max(1);
        } else if ui.notices.is_empty() {
            if ui.bar.first == 0 {
                return false;
            }
            ui.bar.first = 0;
        } else {
            ui.notices.remove(0);
        }
        self.notices_changed(doc);
        true
    }

    /// True when this point in the window is on the bar of the document on show.
    pub fn notice_hit(&self, window: WindowId, doc: DocId, at: Point) -> bool {
        let (Some(win), Some(ui)) = (self.windows.get(&window), self.docs.get(&doc)) else {
            return false;
        };
        if win.shown != Some(doc) {
            return false;
        }
        let count = shown_count(&ui.bar);
        count > 0 && at.y.0 >= ui.bar.top && at.y.0 < ui.bar.top + bar_height(count)
    }

    /// A failed copy has been written after all, or the text no longer needs one.
    pub fn refresh_autosave_failures(&mut self) {
        if self.autosave_failing.is_empty() {
            return;
        }
        let ended: Vec<DocId> = self
            .autosave_failing
            .iter()
            .copied()
            .filter(|doc| self.store.autosave_failure(*doc).is_none())
            .collect();
        for doc in ended {
            self.autosave_failing.remove(&doc);
            self.update_tab(doc);
            self.notices_changed(doc);
        }
        // A later failure can say something new, such as another folder.
        let failing: Vec<DocId> = self.autosave_failing.iter().copied().collect();
        for doc in failing {
            let same = self.docs.get(&doc).is_none_or(|ui| {
                self.store
                    .autosave_failure(doc)
                    .is_some_and(|failure| ui.bar.texts.iter().any(|text| text.contains(&failure)))
            });
            if !same {
                self.notices_changed(doc);
            }
        }
    }

    /// What a screen reader reads of the bar.
    pub fn notice_widgets(&self, doc: DocId, area: Rect) -> Vec<Widget> {
        let texts = self.notice_texts(doc);
        texts
            .into_iter()
            .enumerate()
            .map(|(at, name)| Widget {
                id: WidgetId(NOTICE_WIDGET + at as u64),
                bounds: Rect {
                    height: Dip(LINE),
                    ..area
                },
                role: WidgetRole::Label,
                name,
                value: None,
                states: WidgetStates {
                    enabled: true,
                    ..Default::default()
                },
                selection: None,
                range_value: None,
                description: None,
                char_boxes: Vec::new(),
                access_key: None,
                children: Vec::new(),
            })
            .collect()
    }
}

fn shown_count(bar: &BarLayout) -> usize {
    bar.lines.len().saturating_sub(bar.first).min(bar.max_lines)
}
