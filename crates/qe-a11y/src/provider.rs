//! The text side of the tree: documents, and the range objects a client holds.
//!
//! A range holds UTF-16 positions, the view and document it belongs to, and the revision it
//! last saw. Each call first brings the range up to date through the editor's change log. A
//! range from another view or document, or one the log can no longer describe, is stale, and
//! its calls say UIA_E_ELEMENTNOTAVAILABLE.
#![allow(non_snake_case, non_upper_case_globals)]
use crate::{
    com::{gone, interface_array, invalid, none, not_enabled, not_supported, not_supported_value, not_yet, read_error, rectangle_array, to_screen, to_window},
    host::{UiaHost, WidgetAction},
    marshal::OnWindowThread,
    search::{FindError, FindRequest, SearchSlot},
    text::{Answer, Mover, TextDoc, TextRange, Unit, byte_of, read_text, utf16_of},
    widgets::find_widget,
};
use qe_api::{ByteOffset, Dip, Point, Rect, TextSnapshot, ViewContext, Widget, WidgetId, testing::StringStore};
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
    core::{BOOL, BSTR, ComObject, Interface, Ref, Result, implement},
};

/// What every COM object shares. After `close`, the host is gone, and each call fails.
pub(crate) struct Shared {
    pub(crate) window: isize,
    pub(crate) host: Mutex<Option<Arc<OnWindowThread>>>,
    pub(crate) search: SearchSlot,
    pub(crate) nodes: Mutex<Option<crate::widgets::CachedNodes>>,
}

impl Shared {
    pub(crate) fn host(&self) -> Result<Arc<OnWindowThread>> {
        self.host
            .lock()
            .map_err(|_| gone())?
            .as_ref()
            .cloned()
            .ok_or_else(gone)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The editor's document.
    Editor,
    /// A painted text field's value.
    Field(WidgetId),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Tracked {
    pub(crate) range: TextRange,
    /// Which view, and which document in it.
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

/// A document as COM calls see it, for the length of one call.
pub(crate) trait Doc: TextDoc {
    fn stamp(&self) -> ((u64, u64), u64);
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
    text: Arc<dyn TextSnapshot>,
    rows: Option<Vec<qe_editor::VisibleRow>>,
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
        let rows = self.rows.get_or_insert_with(|| view.visible_rows().rows);
        rows.iter()
            .filter(|row| row.line.0 == line)
            .map(|row| row.first_byte.0)
            .collect()
    }
    fn line_of(&self, byte: usize) -> Answer<usize> {
        Ok(self.view.line_of_byte(ByteOffset(byte))?.0)
    }
}

impl Doc for EditorDoc<'_> {
    fn stamp(&self) -> ((u64, u64), u64) {
        ((self.view_id, self.view.document_number()), self.view.revision())
    }
    fn catch_up(&mut self, state: &mut Tracked) -> Result<()> {
        let (owner, revision) = self.stamp();
        if state.owner != owner {
            return Err(gone());
        }
        match self.view.changes_since(state.revision) {
            ChangesSince::Current => {}
            // A mapped file cannot be edited, so its positions stay good when its index grows.
            ChangesSince::Rescan if self.text.is_read_only() => {}
            ChangesSince::Rescan => return Err(gone()),
            ChangesSince::Changes(changes) => {
                for change in changes {
                    let (from, to, new_end) =
                        (change.utf16.start.0, change.utf16.old_end.0, change.utf16.new_end.0);
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
    /// The editor has no hook that scrolls to a byte and keeps the selection, so this uses
    /// the same estimate the scroll bar does: lines with no layout count at their estimate.
    fn scroll(&mut self, range: TextRange, top: bool) -> Result<()> {
        let start = byte_of(self.text.as_ref(), range.start).map_err(read_error)?;
        if let Ok(visible) = self.view.visible_range()
            && (visible.start.0..visible.end.0).contains(&start)
        {
            return Ok(());
        }
        let line = self.view.line_of_byte(ByteOffset(start)).map_err(|e| read_error(e.into()))?;
        let lines = self.view.store_status().known_lines.max(1);
        let along = self.view.total_height() * line.0 as f64 / lines as f64;
        let height = f64::from(self.view.text_area().height.0);
        let y = if top { along } else { (along - height / 2.0).max(0.0) };
        self.view.run_host(HostCommand::ScrollTo(y), self.ctx);
        Ok(())
    }
    /// Only text on screen has rectangles. A range off screen, or on screen with no layout
    /// yet, gives none, which is how UIA says "not visible".
    fn rects(&mut self, range: TextRange) -> Result<Vec<[f64; 4]>> {
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
            Err(GeometryError::NotLaidOut | GeometryError::TooManyLines { .. }) => return Ok(Vec::new()),
            Err(GeometryError::Lookup(error)) => return Err(read_error(error.into())),
        };
        let area = self.view.text_area();
        let mut answer = Vec::with_capacity(found.len());
        for part in found {
            if let Some(rect) = clip(part.rect, area) {
                answer.push(to_screen(self.window, rect)?);
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
    fn at_point(&mut self, x: f64, y: f64) -> Result<TextRange> {
        let (x, y) = to_window(self.window, x, y)?;
        let area = self.view.text_area();
        let inside = x >= area.x.0
            && y >= area.y.0
            && x < area.x.0 + area.width.0
            && y < area.y.0 + area.height.0;
        if !inside {
            return Err(invalid("This point is outside the text."));
        }
        let hit = self
            .view
            .byte_at_point(Point { x: Dip(x), y: Dip(y) })
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
    fn stamp(&self) -> ((u64, u64), u64) {
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
        let len = crate::text::known_len(self.text.as_ref()).map_err(read_error)?.0;
        state.range.start = state.range.start.min(len);
        state.range.end = state.range.end.min(len);
        state.revision = revision;
        Ok(())
    }
    fn shared_snapshot(&self) -> Arc<dyn TextSnapshot> {
        self.text.clone()
    }
    fn selection(&self) -> Answer<TextRange> {
        let len = crate::text::known_len(self.text.as_ref())?.0;
        let selection = self.widget.selection.clone().unwrap_or(len..len);
        Ok(TextRange::new(selection.start.min(len), selection.end.min(len)))
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
    fn rects(&mut self, _: TextRange) -> Result<Vec<[f64; 4]>> {
        Ok(vec![to_screen(self.window, self.widget.bounds)?])
    }
    fn visible(&mut self) -> Result<Option<TextRange>> {
        let len = crate::text::known_len(self.text.as_ref()).map_err(read_error)?.0;
        Ok(Some(TextRange::new(0, len)))
    }
    fn at_point(&mut self, _: f64, _: f64) -> Result<TextRange> {
        Err(not_supported("A field does not report where its characters are."))
    }
}

/// Runs `body` with this source's document, on the UI thread, with the host's permission.
pub(crate) fn with_doc<T>(
    shared: &Shared,
    source: Source,
    body: impl FnOnce(&mut dyn Doc) -> Result<T>,
) -> Result<T> {
    let host = shared.host()?;
    match source {
        Source::Editor => {
            let view_id = host.call_now(|h| h.view_id().ok_or_else(gone))?;
            let mut body = Some(body);
            let mut out = None;
            let window = shared.window;
            let reached = host.with_editor(&mut |view, ctx| {
                let text = view.worker_snapshot();
                let mut doc = EditorDoc {
                    view,
                    ctx,
                    view_id,
                    window,
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
            let root = host.widgets_now()?;
            let widget = find_widget(&root, id).ok_or_else(gone)?.clone();
            let value = widget.value.clone().unwrap_or_default();
            let mut doc = FieldDoc {
                widget,
                host,
                window: shared.window,
                text: Arc::new(StringStore::new(value.clone())),
                value,
            };
            body(&mut doc)
        }
    }
}

pub(crate) fn make_range(
    shared: &Arc<Shared>,
    source: Source,
    doc: &dyn Doc,
    range: TextRange,
    enclosing: IRawElementProviderSimple,
) -> ITextRangeProvider {
    let (owner, revision) = doc.stamp();
    ComObject::new(TextRangeObject {
        shared: shared.clone(),
        source,
        enclosing,
        state: Mutex::new(Tracked {
            range,
            owner,
            revision,
        }),
    })
    .to_interface()
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
    /// Brings the range up to date, runs `body`, and keeps what `body` left in the range.
    fn run<T>(&self, body: impl FnOnce(&mut dyn Doc, &mut TextRange) -> Result<T>) -> Result<T> {
        with_doc(&self.shared, self.source, |doc| {
            let mut state = self.tracked()?;
            doc.catch_up(&mut state)?;
            let answer = body(doc, &mut state.range)?;
            *self.state.lock().map_err(|_| gone())? = state;
            Ok(answer)
        })
    }
    /// The other range's text range, up to date. It must belong to this same document.
    fn other_range(&self, doc: &mut dyn Doc, other: Ref<ITextRangeProvider>) -> Result<TextRange> {
        let other = other
            .ok()
            .map_err(|_| invalid("No range was given."))?
            .cast_object_ref::<TextRangeObject>()
            .map_err(|_| invalid("This range is from another provider."))?;
        if !Arc::ptr_eq(&self.shared, &other.shared) || self.source != other.source {
            return Err(invalid("This range is from another document."));
        }
        let mut state = other.tracked()?;
        doc.catch_up(&mut state)?;
        *other.state.lock().map_err(|_| gone())? = state;
        Ok(state.range)
    }
}

fn endpoint_of(range: TextRange, start: bool) -> usize {
    if start { range.start } else { range.end }
}

impl ITextRangeProvider_Impl for TextRangeObject_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        self.run(|doc, range| {
            Ok(make_range(
                &self.shared,
                self.source,
                &*doc,
                *range,
                self.enclosing.clone(),
            ))
        })
    }
    fn Compare(&self, other: Ref<ITextRangeProvider>) -> Result<BOOL> {
        self.run(|doc, range| {
            let other = self.other_range(doc, other)?;
            Ok(BOOL::from(*range == other))
        })
    }
    fn CompareEndpoints(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let mine = moves_start(endpoint)?;
        let theirs = moves_start(other_endpoint)?;
        self.run(|doc, range| {
            let other = self.other_range(doc, other)?;
            let left = endpoint_of(*range, mine);
            let right = endpoint_of(other, theirs);
            let distance = i32::try_from(left.abs_diff(right)).unwrap_or(i32::MAX);
            Ok(if left < right { -distance } else { distance })
        })
    }
    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        let unit = unit_of(unit)?;
        self.run(|doc, range| {
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
        let same = match (bool::try_from(&wanted), bool::try_from(value)) {
            (Ok(a), Ok(b)) if id == UIA_IsReadOnlyAttributeId => a == b,
            _ => match id {
                UIA_FontNameAttributeId => matches!(
                    (BSTR::try_from(&wanted), BSTR::try_from(value)),
                    (Ok(a), Ok(b)) if a == b
                ),
                UIA_FontSizeAttributeId => matches!(
                    (f64::try_from(&wanted), f64::try_from(value)),
                    (Ok(a), Ok(b)) if a == b
                ),
                _ => false,
            },
        };
        if !same {
            return Err(none());
        }
        self.run(|doc, range| {
            Ok(make_range(
                &self.shared,
                self.source,
                &*doc,
                *range,
                self.enclosing.clone(),
            ))
        })
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
            return Err(none());
        };
        self.run(|doc, range| {
            let ((view, document), revision) = doc.stamp();
            let request = FindRequest {
                identity: (view, document, revision),
                range: *range,
                query,
                backward: backward.as_bool(),
                ignore_case: ignore_case.as_bool(),
            };
            let found = self
                .shared
                .search
                .find(&doc.shared_snapshot(), request)
                .map_err(|error| match error {
                    FindError::Searching => {
                        not_yet("The search is still running. Ask again soon.")
                    }
                    FindError::Read(error) => read_error(error),
                })?;
            match found {
                Some(found) => Ok(make_range(
                    &self.shared,
                    self.source,
                    &*doc,
                    found,
                    self.enclosing.clone(),
                )),
                None => Err(none()),
            }
        })
    }
    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        self.attribute(id)
    }
    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        self.run(|doc, range| rectangle_array(&doc.rects(*range)?))
    }
    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        // A closed provider has no element to give.
        self.shared.host()?;
        Ok(self.enclosing.clone())
    }
    fn GetText(&self, limit: i32) -> Result<BSTR> {
        if limit < -1 {
            return Err(invalid("The limit must be -1 or more."));
        }
        self.run(|doc, range| {
            let text = read_text(doc.snapshot(), *range, limit).map_err(read_error)?;
            Ok(BSTR::from(text.as_str()))
        })
    }
    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        let unit = unit_of(unit)?;
        self.run(|doc, range| {
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
        self.run(|doc, range| {
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
        self.run(|doc, range| {
            let at = endpoint_of(self.other_range(doc, other)?, theirs);
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
        self.run(|doc, range| doc.select(*range))
    }
    fn AddToSelection(&self) -> Result<()> {
        Err(not_supported("This text has one selection only."))
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(not_supported("This text has one selection only."))
    }
    fn ScrollIntoView(&self, align_top: BOOL) -> Result<()> {
        self.run(|doc, range| doc.scroll(*range, align_top.as_bool()))
    }
    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        self.shared.host()?;
        interface_array::<IRawElementProviderSimple>(&[])
    }
}

impl TextRangeObject {
    fn attribute(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        match id {
            UIA_IsReadOnlyAttributeId => self.run(|doc, _| Ok(VARIANT::from(doc.read_only()))),
            UIA_FontNameAttributeId | UIA_FontSizeAttributeId if self.source == Source::Editor => {
                let host = self.shared.host()?;
                self.run(|_, _| Ok(()))?;
                match host.font() {
                    Some((family, _)) if id == UIA_FontNameAttributeId => {
                        Ok(VARIANT::from(family.as_str()))
                    }
                    Some((_, points)) => Ok(VARIANT::from(points)),
                    None => not_supported_value(),
                }
            }
            _ => {
                self.shared.host()?;
                not_supported_value()
            }
        }
    }
}
