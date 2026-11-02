//! The text side of the tree: documents, and the range objects a client holds.
//!
//! A range holds UTF-16 positions, the view and document it belongs to, and the revision it
//! last saw. Each call first brings the range up to date through the editor's change log. A
//! range from another view or document, or one the log can no longer describe, is stale, and
//! its calls say UIA_E_ELEMENTNOTAVAILABLE.
#![allow(non_snake_case, non_upper_case_globals)]
use crate::{
    com::{
        bstr, gone, interface_array, invalid, none, not_enabled, not_supported,
        not_supported_value, not_yet, read_error, rectangle_array, scale_of, to_screen, to_window,
    },
    host::{UiaHost, WidgetAction},
    marshal::WindowThread,
    search::{FIND_PATIENCE, FindError, FindRequest, find_on_window, find_waiting},
    text::{
        Answer, Mover, TextDoc, TextRange, Unit, byte_of, known_len, read_text, read_text_waiting,
        utf16_of,
    },
    widgets::{CachedNodes, find_widget},
};
use qe_api::{
    ByteOffset, Dip, Point, Rect, TextSnapshot, ViewContext, Widget, WidgetId, testing::StringStore,
};
use qe_editor::{ChangesSince, EditorView, GeometryError, HostCommand};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        System::{Com::SAFEARRAY, Variant::VARIANT},
        UI::Accessibility::*,
    },
    core::{BOOL, BSTR, ComObject, IUnknownImpl, Interface, Ref, Result, implement},
};

/// What every COM object shares. After `Accessibility::shutdown`, each call fails.
pub(crate) struct Shared {
    pub(crate) window: isize,
    pub(crate) ui: Arc<WindowThread>,
    pub(crate) nodes: Mutex<Option<CachedNodes>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The editor's document.
    Editor,
    /// A painted text field's value.
    Field(WidgetId),
}

/// Which view and document, and which revision.
pub(crate) type Stamp = ((u64, u64), u64);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Tracked {
    pub(crate) range: TextRange,
    pub(crate) owner: (u64, u64),
    pub(crate) revision: u64,
}

/// One position through one change. Text before stays, text after shifts, and a position in
/// the replaced text goes to where that text began.
pub fn shift(position: usize, start: usize, old_end: usize, new_end: usize) -> usize {
    if position <= start {
        position
    } else if position >= old_end {
        position - old_end + new_end
    } else {
        start
    }
}

/// A document as COM calls see it, for the length of one call on the window's thread.
pub(crate) trait Doc: TextDoc {
    fn stamp(&self) -> Stamp;
    fn catch_up(&mut self, state: &mut Tracked) -> Result<()>;
    fn shared_snapshot(&self) -> Arc<dyn TextSnapshot>;
    fn selection(&self) -> Answer<TextRange>;
    fn caret(&self) -> Answer<usize>;
    fn read_only(&self) -> bool;
    fn select(&mut self, range: TextRange) -> Result<()>;
    fn scroll(&mut self, range: TextRange, top: bool) -> Result<()>;
    fn rects(&mut self, range: TextRange) -> Result<Vec<[f64; 4]>>;
    fn visible(&mut self) -> Result<Option<TextRange>>;
    fn at_point(&mut self, x: f64, y: f64) -> Result<TextRange>;
}

struct EditorDoc<'a> {
    view: &'a mut EditorView,
    ctx: &'a mut ViewContext,
    view_id: u64,
    window: isize,
    scale: f64,
    text: Arc<dyn TextSnapshot>,
    /// Each row on screen: its line, its first byte, and the byte after it. Kept for this one
    /// call, so a move through many lines looks once.
    rows: Option<Vec<(usize, usize, Option<usize>)>>,
}

impl TextDoc for EditorDoc<'_> {
    fn snapshot(&self) -> &dyn TextSnapshot {
        self.text.as_ref()
    }
    fn word_at(&mut self, byte: usize) -> Answer<Range<usize>> {
        let found = self.view.word_range_at(ByteOffset(byte))?;
        Ok(found.start.0..found.end.0)
    }
    fn row_stops(&mut self, line: usize) -> Vec<usize> {
        let view = &*self.view;
        let rows = self.rows.get_or_insert_with(|| {
            // With wrap off a line is one row, even if the view shows only part of a long one.
            if !view.wraps() {
                return Vec::new();
            }
            view.visible_rows()
                .rows
                .iter()
                .map(|row| {
                    let end = view.visible_row_end(row).map(|byte| byte.0);
                    (row.line.0, row.first_byte.0, end)
                })
                .collect()
        });
        rows.iter()
            .filter(|row| row.0 == line)
            .flat_map(|row| std::iter::once(row.1).chain(row.2))
            .collect()
    }
    fn line_of(&self, byte: usize) -> Answer<usize> {
        Ok(self.view.line_of_byte(ByteOffset(byte))?.0)
    }
}

impl Doc for EditorDoc<'_> {
    fn stamp(&self) -> Stamp {
        (
            (self.view_id, self.view.document_number()),
            self.view.revision(),
        )
    }
    fn catch_up(&mut self, state: &mut Tracked) -> Result<()> {
        let (owner, revision) = self.stamp();
        if state.owner != owner {
            return Err(gone());
        }
        match self.view.changes_since(state.revision) {
            ChangesSince::Current => {}
            ChangesSince::Rescan => return Err(gone()),
            ChangesSince::Changes(changes) => {
                for change in changes {
                    let (from, to, new_end) = (
                        change.utf16.start.0,
                        change.utf16.old_end.0,
                        change.utf16.new_end.0,
                    );
                    state.range.start = shift(state.range.start, from, to, new_end);
                    state.range.end = shift(state.range.end, from, to, new_end);
                }
            }
        }
        state.revision = revision;
        Ok(())
    }
    fn shared_snapshot(&self) -> Arc<dyn TextSnapshot> {
        self.text.clone()
    }
    fn selection(&self) -> Answer<TextRange> {
        let selection = self.view.selection();
        Ok(TextRange::new(
            self.view.byte_to_utf16(selection.start)?.0,
            self.view.byte_to_utf16(selection.end)?.0,
        ))
    }
    fn caret(&self) -> Answer<usize> {
        Ok(self.view.byte_to_utf16(self.view.caret())?.0)
    }
    fn read_only(&self) -> bool {
        !self.view.can_edit()
    }
    fn select(&mut self, range: TextRange) -> Result<()> {
        let start = byte_of(self.text.as_ref(), range.start).map_err(read_error)?;
        let end = byte_of(self.text.as_ref(), range.end).map_err(read_error)?;
        self.view.run_host(
            HostCommand::Select {
                range: ByteOffset(start)..ByteOffset(end),
                reveal: true,
            },
            self.ctx,
        );
        Ok(())
    }
    /// Puts the range's start at the top of the view, or its end at the bottom. Text with a
    /// layout lands exactly. The editor has no hook that gives the height of other text, so
    /// that uses the same estimate as the scroll bar, and wrapped text can land off by the
    /// rows the estimate does not know. The selection stays.
    fn scroll(&mut self, range: TextRange, top: bool) -> Result<()> {
        let text = self.text.clone();
        // The last unit of the range, because its end may be on the next line.
        let edge = if top || range.start == range.end {
            range.start
        } else {
            range.end - 1
        };
        let byte = byte_of(text.as_ref(), edge).map_err(read_error)?;
        let area = self.view.text_area();
        let height = f64::from(area.height.0);
        let scroll = self.view.scroll_offset();
        let exact = self
            .view
            .range_rects(ByteOffset(byte)..ByteOffset(byte))
            .ok()
            .and_then(|rects| rects.first().map(|part| part.rect));
        let y = if let Some(rect) = exact {
            let row_top = scroll + f64::from(rect.y.0 - area.y.0);
            if top {
                row_top
            } else {
                row_top + f64::from(rect.height.0) - height
            }
        } else {
            let line = self
                .view
                .line_of_byte(ByteOffset(byte))
                .map_err(|e| read_error(e.into()))?;
            let lines = self.view.store_status().known_lines.max(1) as f64;
            let row = self.view.total_height() / lines;
            let row_top = row * line.0 as f64;
            if top { row_top } else { row_top + row - height }
        };
        self.view
            .run_host(HostCommand::ScrollTo(y.max(0.0)), self.ctx);
        Ok(())
    }
    /// Only text on screen has rectangles. An empty range has none, as UIA's contract says. A
    /// range off screen, or on screen with no layout yet, has none either, which is how UIA
    /// says "not visible".
    fn rects(&mut self, range: TextRange) -> Result<Vec<[f64; 4]>> {
        if range.start == range.end {
            return Ok(Vec::new());
        }
        let text = self.text.clone();
        let start = byte_of(text.as_ref(), range.start).map_err(read_error)?;
        let end = byte_of(text.as_ref(), range.end).map_err(read_error)?;
        let Ok(visible) = self.view.visible_range() else {
            return Ok(Vec::new());
        };
        let from = start.max(visible.start.0);
        let to = end.min(visible.end.0);
        if from > to || (from == to && start != end) {
            return Ok(Vec::new());
        }
        let found = match self.view.range_rects(ByteOffset(from)..ByteOffset(to)) {
            Ok(found) => found,
            Err(GeometryError::NotLaidOut | GeometryError::TooManyLines { .. }) => {
                return Ok(Vec::new());
            }
            Err(GeometryError::Lookup(error)) => return Err(read_error(error.into())),
        };
        let area = self.view.text_area();
        let mut answer = Vec::with_capacity(found.len());
        for part in found {
            if let Some(rect) = clip(part.rect, area) {
                answer.push(to_screen(self.window, self.scale, rect)?);
            }
        }
        Ok(answer)
    }
    fn visible(&mut self) -> Result<Option<TextRange>> {
        let Ok(found) = self.view.visible_range() else {
            return Ok(None);
        };
        let text = self.text.as_ref();
        Ok(Some(TextRange::new(
            utf16_of(text, found.start.0).map_err(read_error)?,
            utf16_of(text, found.end.0).map_err(read_error)?,
        )))
    }
    /// Any point gets the nearest text, as UIA's contract says.
    fn at_point(&mut self, x: f64, y: f64) -> Result<TextRange> {
        let (x, y) = to_window(self.window, self.scale, x, y)?;
        let area = self.view.text_area();
        // Just inside the far edges, because those edges belong to whatever is next.
        let x = x
            .max(area.x.0)
            .min((area.x.0 + area.width.0 - 1.0).max(area.x.0));
        let y = y
            .max(area.y.0)
            .min((area.y.0 + area.height.0 - 1.0).max(area.y.0));
        let hit = self
            .view
            .byte_at_point(Point {
                x: Dip(x),
                y: Dip(y),
            })
            .map_err(|error| match error {
                GeometryError::Lookup(error) => read_error(error.into()),
                _ => not_yet("The text here has no layout yet. Ask again soon."),
            })?;
        let at = utf16_of(self.text.as_ref(), hit.byte.0).map_err(read_error)?;
        Ok(TextRange::new(at, at))
    }
}

fn clip(rect: Rect, area: Rect) -> Option<Rect> {
    let left = rect.x.0.max(area.x.0);
    let top = rect.y.0.max(area.y.0);
    let right = (rect.x.0 + rect.width.0).min(area.x.0 + area.width.0);
    let bottom = (rect.y.0 + rect.height.0).min(area.y.0 + area.height.0);
    (right >= left && bottom > top).then_some(Rect {
        x: Dip(left),
        y: Dip(top),
        width: Dip(right - left),
        height: Dip(bottom - top),
    })
}

/// A painted text field. It has one line, so its rows and words need no editor.
struct FieldDoc {
    widget: Widget,
    host: Arc<dyn UiaHost>,
    window: isize,
    scale: f64,
    text: Arc<dyn TextSnapshot>,
    value: String,
}

fn word_class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0
    } else if crate::text::is_space(c) {
        1
    } else {
        2
    }
}

impl TextDoc for FieldDoc {
    fn snapshot(&self) -> &dyn TextSnapshot {
        self.text.as_ref()
    }
    fn word_at(&mut self, byte: usize) -> Answer<Range<usize>> {
        let chars: Vec<(usize, char)> = self.value.char_indices().collect();
        if chars.is_empty() {
            return Ok(0..0);
        }
        let index = chars
            .partition_point(|(at, _)| *at < byte)
            .min(chars.len() - 1);
        let class = word_class(chars[index].1);
        let (mut first, mut last) = (index, index + 1);
        if class != 2 {
            while first > 0 && word_class(chars[first - 1].1) == class {
                first -= 1;
            }
            while last < chars.len() && word_class(chars[last].1) == class {
                last += 1;
            }
        }
        let end = chars.get(last).map_or(self.value.len(), |(at, _)| *at);
        Ok(chars[first].0..end)
    }
    fn row_stops(&mut self, _: usize) -> Vec<usize> {
        Vec::new()
    }
}

impl Doc for FieldDoc {
    fn stamp(&self) -> Stamp {
        let mut hasher = DefaultHasher::new();
        self.value.hash(&mut hasher);
        ((self.widget.id.0, 0), hasher.finish())
    }
    /// A field has no change log, so after a change a range keeps its numbers, inside the
    /// new text.
    fn catch_up(&mut self, state: &mut Tracked) -> Result<()> {
        let (owner, revision) = self.stamp();
        if state.owner != owner {
            return Err(gone());
        }
        let len = known_len(self.text.as_ref()).map_err(read_error)?.0;
        state.range.start = state.range.start.min(len);
        state.range.end = state.range.end.min(len);
        state.revision = revision;
        Ok(())
    }
    fn shared_snapshot(&self) -> Arc<dyn TextSnapshot> {
        self.text.clone()
    }
    fn selection(&self) -> Answer<TextRange> {
        let len = known_len(self.text.as_ref())?.0;
        let selection = self.widget.selection.clone().unwrap_or(len..len);
        Ok(TextRange::new(
            selection.start.min(len),
            selection.end.min(len),
        ))
    }
    fn caret(&self) -> Answer<usize> {
        Ok(self.selection()?.end)
    }
    fn read_only(&self) -> bool {
        self.widget.states.read_only
    }
    fn select(&mut self, range: TextRange) -> Result<()> {
        if !self.widget.states.enabled {
            return Err(not_enabled("This field is turned off."));
        }
        self.host
            .action(self.widget.id, WidgetAction::SelectText(range))
    }
    fn scroll(&mut self, _: TextRange, _: bool) -> Result<()> {
        Ok(())
    }
    /// The field does not report where its characters are, so any text in it answers with
    /// the field's whole box.
    fn rects(&mut self, range: TextRange) -> Result<Vec<[f64; 4]>> {
        if range.start == range.end {
            return Ok(Vec::new());
        }
        Ok(vec![to_screen(
            self.window,
            self.scale,
            self.widget.bounds,
        )?])
    }
    fn visible(&mut self) -> Result<Option<TextRange>> {
        let len = known_len(self.text.as_ref()).map_err(read_error)?.0;
        Ok(Some(TextRange::new(0, len)))
    }
    /// `Widget` does not say where a field's characters are, so this is an estimate: the same
    /// share of the text as the point is of the box. The README lists this for M14.
    fn at_point(&mut self, x: f64, y: f64) -> Result<TextRange> {
        let _ = y;
        let (x, _) = to_window(self.window, self.scale, x, 0.0)?;
        let text = self.text.as_ref();
        let len = known_len(text).map_err(read_error)?.0;
        let share =
            ((x - self.widget.bounds.x.0) / self.widget.bounds.width.0.max(1.0)).clamp(0.0, 1.0);
        let unit = (f64::from(share) * len as f64).round() as usize;
        let byte = byte_of(text, unit.min(len)).map_err(read_error)?;
        let at = utf16_of(text, byte).map_err(read_error)?;
        Ok(TextRange::new(at, at))
    }
}

/// Runs `body` with this source's document, on the window's thread.
pub(crate) fn with_doc<T: 'static>(
    shared: &Arc<Shared>,
    source: Source,
    body: impl FnOnce(&mut dyn Doc) -> Result<T> + 'static,
) -> Result<T> {
    let window = shared.window;
    shared.ui.on_ui(move |host| {
        let scale = scale_of(window, host.dpi())?;
        match source {
            Source::Editor => {
                let view_id = host.view_id().ok_or_else(gone)?;
                let mut body = Some(body);
                let mut out = None;
                let reached = host.with_editor(&mut |view, ctx| {
                    let text = view.worker_snapshot();
                    let mut doc = EditorDoc {
                        view,
                        ctx,
                        view_id,
                        window,
                        scale,
                        text,
                        rows: None,
                    };
                    if let Some(body) = body.take() {
                        out = Some(body(&mut doc));
                    }
                });
                if !reached {
                    return Err(not_yet("The editor is busy. Ask again soon."));
                }
                out.unwrap_or_else(|| Err(not_yet("The editor is busy. Ask again soon.")))
            }
            Source::Field(id) => {
                let root = host.widgets().ok_or_else(gone)?;
                let widget = find_widget(&root, id).ok_or_else(gone)?.clone();
                let value = widget.value.clone().unwrap_or_default();
                let mut doc = FieldDoc {
                    widget,
                    host: host.clone(),
                    window,
                    scale,
                    text: Arc::new(StringStore::new(value.clone())),
                    value,
                };
                body(&mut doc)
            }
        }
    })
}

pub(crate) fn make_range(
    shared: &Arc<Shared>,
    source: Source,
    stamp: Stamp,
    range: TextRange,
    enclosing: IRawElementProviderSimple,
) -> ITextRangeProvider {
    let (owner, revision) = stamp;
    let made: ITextRangeProvider = ComObject::new(TextRangeObject {
        shared: shared.clone(),
        source,
        enclosing,
        state: Mutex::new(Tracked {
            range,
            owner,
            revision,
        }),
    })
    .to_interface();
    crate::abi::range::protect(&made);
    made
}

fn unit_of(unit: TextUnit) -> Result<Unit> {
    match unit {
        TextUnit_Character => Ok(Unit::Character),
        TextUnit_Word => Ok(Unit::Word),
        TextUnit_Line => Ok(Unit::Line),
        TextUnit_Paragraph => Ok(Unit::Paragraph),
        TextUnit_Format | TextUnit_Page | TextUnit_Document => Ok(Unit::Document),
        _ => Err(invalid("This text unit does not exist.")),
    }
}

fn moves_start(endpoint: TextPatternRangeEndpoint) -> Result<bool> {
    match endpoint {
        TextPatternRangeEndpoint_Start => Ok(true),
        TextPatternRangeEndpoint_End => Ok(false),
        _ => Err(invalid("This range end does not exist.")),
    }
}

#[implement(ITextRangeProvider)]
struct TextRangeObject {
    shared: Arc<Shared>,
    source: Source,
    enclosing: IRawElementProviderSimple,
    state: Mutex<Tracked>,
}

impl TextRangeObject {
    fn tracked(&self) -> Result<Tracked> {
        self.state.lock().map(|state| *state).map_err(|_| gone())
    }
    fn fresh(&self, stamp: Stamp, range: TextRange) -> ITextRangeProvider {
        make_range(
            &self.shared,
            self.source,
            stamp,
            range,
            self.enclosing.clone(),
        )
    }
}

impl TextRangeObject_Impl {
    /// Brings the range up to date on the window's thread, runs `body`, and keeps what `body`
    /// left in the range. Everything `body` uses travels with it, so a caller that gives up
    /// on a busy window leaves nothing behind.
    fn run<T: 'static>(
        &self,
        body: impl FnOnce(&TextRangeObject, &mut dyn Doc, &mut TextRange) -> Result<T> + 'static,
    ) -> Result<T> {
        let this = self.to_object();
        let shared = self.shared.clone();
        with_doc(&shared, self.source, move |doc| {
            let mut state = this.tracked()?;
            doc.catch_up(&mut state)?;
            let answer = body(&this, doc, &mut state.range)?;
            *this.state.lock().map_err(|_| gone())? = state;
            Ok(answer)
        })
    }
    /// This range's value in a call that also needs another range of the same document.
    fn with_other<T: 'static>(
        &self,
        other: Ref<ITextRangeProvider>,
        body: impl FnOnce(&mut dyn Doc, &mut TextRange, TextRange) -> Result<T> + 'static,
    ) -> Result<T> {
        let other = other
            .cloned()
            .ok_or_else(|| invalid("No range was given."))?;
        self.run(move |this, doc, range| {
            let other = other
                .cast_object_ref::<TextRangeObject>()
                .map_err(|_| invalid("This range is from another provider."))?;
            if !Arc::ptr_eq(&this.shared, &other.shared) || this.source != other.source {
                return Err(invalid("This range is from another document."));
            }
            let mut state = other.tracked()?;
            doc.catch_up(&mut state)?;
            *other.state.lock().map_err(|_| gone())? = state;
            body(doc, range, state.range)
        })
    }
    fn attribute(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        match id {
            UIA_IsReadOnlyAttributeId => {
                let read_only = self.run(|_, doc, _| Ok(doc.read_only()))?;
                Ok(VARIANT::from(read_only))
            }
            UIA_FontNameAttributeId | UIA_FontSizeAttributeId if self.source == Source::Editor => {
                self.run(|_, _, _| Ok(()))?;
                match self.shared.ui.on_ui(|host| Ok(host.font()))? {
                    Some((family, _)) if id == UIA_FontNameAttributeId => {
                        Ok(VARIANT::from(family.as_str()))
                    }
                    Some((_, points)) => Ok(VARIANT::from(points)),
                    None => not_supported_value(),
                }
            }
            _ => {
                self.run(|_, _, _| Ok(()))?;
                not_supported_value()
            }
        }
    }
}

fn endpoint_of(range: TextRange, start: bool) -> usize {
    if start { range.start } else { range.end }
}

impl ITextRangeProvider_Impl for TextRangeObject_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        self.run(|this, doc, range| Ok(this.fresh(doc.stamp(), *range)))
    }
    fn Compare(&self, other: Ref<ITextRangeProvider>) -> Result<BOOL> {
        self.with_other(other, |_, range, other| Ok(BOOL::from(*range == other)))
    }
    fn CompareEndpoints(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let mine = moves_start(endpoint)?;
        let theirs = moves_start(other_endpoint)?;
        self.with_other(other, move |_, range, other| {
            let left = endpoint_of(*range, mine);
            let right = endpoint_of(other, theirs);
            let distance = i32::try_from(left.abs_diff(right)).unwrap_or(i32::MAX);
            Ok(if left < right { -distance } else { distance })
        })
    }
    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        let unit = unit_of(unit)?;
        self.run(move |_, doc, range| {
            *range = Mover { doc: &mut *doc }
                .unit_at(unit, range.start)
                .map_err(read_error)?;
            Ok(())
        })
    }
    /// Plain text has one format, so the answer is this whole range or nothing.
    fn FindAttribute(
        &self,
        id: UIA_TEXTATTRIBUTE_ID,
        value: &VARIANT,
        _backward: BOOL,
    ) -> Result<ITextRangeProvider> {
        let wanted = self.attribute(id)?;
        let same = match id {
            UIA_IsReadOnlyAttributeId => {
                matches!((bool::try_from(&wanted), bool::try_from(value)), (Ok(a), Ok(b)) if a == b)
            }
            UIA_FontNameAttributeId => matches!(
                (BSTR::try_from(&wanted), BSTR::try_from(value)),
                (Ok(a), Ok(b)) if a == b
            ),
            UIA_FontSizeAttributeId => matches!(
                (f64::try_from(&wanted), f64::try_from(value)),
                (Ok(a), Ok(b)) if a == b
            ),
            _ => false,
        };
        if !same {
            return Err(none());
        }
        self.run(|this, doc, range| Ok(this.fresh(doc.stamp(), *range)))
    }
    fn FindText(
        &self,
        query: &BSTR,
        backward: BOOL,
        ignore_case: BOOL,
    ) -> Result<ITextRangeProvider> {
        if query.is_empty() {
            return Err(invalid("The text to find is empty."));
        }
        // Text with a lone surrogate cannot be in a document, which is all valid text.
        let Ok(query) = String::from_utf16(query) else {
            self.run(|_, _, _| Ok(()))?;
            return Err(none());
        };
        // The window's thread only hands over a snapshot. The search then runs on this thread.
        let (text, stamp, range) =
            self.run(|_, doc, range| Ok((doc.shared_snapshot(), doc.stamp(), *range)))?;
        let request = FindRequest {
            range,
            query,
            backward: backward.as_bool(),
            ignore_case: ignore_case.as_bool(),
        };
        let answer = if self.shared.ui.is_window_thread() {
            find_on_window(text.as_ref(), &request)
        } else {
            find_waiting(text.as_ref(), &request, FIND_PATIENCE)
        };
        match answer {
            Ok(Some(found)) => Ok(make_range(
                &self.shared,
                self.source,
                stamp,
                found,
                self.enclosing.clone(),
            )),
            Ok(None) => Err(none()),
            Err(FindError::TooLong) => Err(not_yet(
                "The search hit its limit, so the answer is not known.",
            )),
            Err(FindError::Read(error)) => Err(read_error(error)),
        }
    }
    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        self.attribute(id)
    }
    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        let rectangles = self.run(|_, doc, range| doc.rects(*range))?;
        rectangle_array(&rectangles)
    }
    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        // A closed provider or a stale range has no element to give.
        self.run(|_, _, _| Ok(()))?;
        Ok(self.enclosing.clone())
    }
    fn GetText(&self, limit: i32) -> Result<BSTR> {
        if limit < -1 {
            return Err(invalid("The limit must be -1 or more."));
        }
        // The window's thread only hands over a snapshot. A client's read then runs on this
        // thread, where it can wait for disk and hold nothing up.
        let (text, range) = self.run(|_, doc, range| Ok((doc.shared_snapshot(), *range)))?;
        let read = if self.shared.ui.is_window_thread() {
            read_text(text.as_ref(), range, limit)
        } else {
            read_text_waiting(text.as_ref(), range, limit)
        };
        bstr(&read.map_err(read_error)?)
    }
    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        let unit = unit_of(unit)?;
        self.run(move |_, doc, range| {
            let (next, moved) = Mover { doc: &mut *doc }
                .move_range(*range, unit, count)
                .map_err(read_error)?;
            *range = next;
            Ok(moved)
        })
    }
    fn MoveEndpointByUnit(
        &self,
        endpoint: TextPatternRangeEndpoint,
        unit: TextUnit,
        count: i32,
    ) -> Result<i32> {
        let unit = unit_of(unit)?;
        let start = moves_start(endpoint)?;
        self.run(move |_, doc, range| {
            let (next, moved) = Mover { doc: &mut *doc }
                .move_endpoint(*range, start, unit, count)
                .map_err(read_error)?;
            *range = next;
            Ok(moved)
        })
    }
    fn MoveEndpointByRange(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let start = moves_start(endpoint)?;
        let theirs = moves_start(other_endpoint)?;
        self.with_other(other, move |_, range, other| {
            let at = endpoint_of(other, theirs);
            if start {
                range.start = at;
                range.end = range.end.max(at);
            } else {
                range.end = at;
                range.start = range.start.min(at);
            }
            Ok(())
        })
    }
    fn Select(&self) -> Result<()> {
        self.run(|_, doc, range| doc.select(*range))
    }
    fn AddToSelection(&self) -> Result<()> {
        self.run(|_, _, _| Ok(()))?;
        Err(not_supported("This text has one selection only."))
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        self.run(|_, _, _| Ok(()))?;
        Err(not_supported("This text has one selection only."))
    }
    fn ScrollIntoView(&self, align_top: BOOL) -> Result<()> {
        let top = align_top.as_bool();
        self.run(move |_, doc, range| doc.scroll(*range, top))
    }
    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        self.run(|_, _, _| Ok(()))?;
        interface_array::<IRawElementProviderSimple>(&[])
    }
}
