//! Slow reference implementations for tests in other crates.
use crate::*;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct StringStore {
    text: String,
    revision: u64,
    read_only: bool,
}

impl StringStore {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            revision: 0,
            read_only: false,
        }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn set_read_only(&mut self, value: bool) {
        self.read_only = value;
    }
    fn starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        let mut iter = self.text.char_indices().peekable();
        while let Some((at, c)) = iter.next() {
            let end = if c == '\r' && iter.peek().is_some_and(|(_, next)| *next == '\n') {
                iter.next().map_or(at + 1, |(i, _)| i + 1)
            } else {
                at + c.len_utf8()
            };
            if matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}') {
                starts.push(end);
            }
        }
        starts
    }
}

impl TextSnapshot for StringStore {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.text.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.text.encode_utf16().count())
    }
    fn line_count(&self) -> usize {
        self.starts().len()
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.text
            .get(..byte.0)
            .map(|s| Utf16Offset(s.encode_utf16().count()))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        let mut units = 0;
        for (index, c) in self.text.char_indices() {
            if units == utf16.0 {
                return Some(ByteOffset(index));
            }
            units += c.len_utf16();
            if units > utf16.0 {
                return None;
            }
        }
        (units == utf16.0).then_some(ByteOffset(self.text.len()))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.text.get(..byte.0)?;
        let starts = self.starts();
        let line = starts.partition_point(|start| *start <= byte.0) - 1;
        let column = self.text[starts[line]..byte.0].encode_utf16().count();
        Some(TextPosition {
            line: LineIndex(line),
            column: Utf16Offset(column),
        })
    }
    fn position_to_byte(&self, position: TextPosition) -> Option<ByteOffset> {
        let range = self.line_range(position.line)?;
        let relative = StringStore::new(&self.text[range.start.0..range.end.0]);
        let offset = relative.utf16_to_byte(position.column)?;
        Some(ByteOffset(range.start.0 + offset.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        let starts = self.starts();
        let start = *starts.get(line.0)?;
        let end = starts.get(line.0 + 1).copied().unwrap_or(self.text.len());
        Some(ByteOffset(start)..ByteOffset(end))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        if let Some(slice) = self.text.get(range.start.0..range.end.0) {
            visitor(slice);
            true
        } else {
            false
        }
    }
    fn is_read_only(&self) -> bool {
        self.read_only
    }
}

impl TextStore for StringStore {
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        Arc::new(self.clone())
    }
    fn replace(&mut self, range: Range<ByteOffset>, text: &str) -> Result<TextChange, EditError> {
        if self.read_only {
            return Err(EditError::ReadOnly);
        }
        if self.text.get(range.start.0..range.end.0).is_none() {
            return Err(EditError::InvalidRange);
        }
        let before = self.line_count();
        self.text.replace_range(range.start.0..range.end.0, text);
        self.revision += 1;
        Ok(TextChange {
            replaced: range,
            new_len: text.len(),
            line_count_delta: self.line_count() as isize - before as isize,
            revision: self.revision,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FixedWidthLayout {
    pub cell_width: Dip,
    pub row_height: Dip,
}

impl Layout for FixedWidthLayout {
    fn layout_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout> {
        let range = text.line_range(line)?;
        let mut s = String::new();
        text.visit_chunks(range.clone(), &mut |chunk| s.push_str(chunk));
        let content = s.trim_end_matches(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}']);
        Some(self.layout_content(content, range.start, line, width))
    }
    fn layout_slice(
        &self,
        text: &dyn TextSnapshot,
        range: Range<ByteOffset>,
        width: Dip,
    ) -> Option<LineLayout> {
        if range.start > range.end || range.end > text.len_bytes() {
            return None;
        }
        let line = text.byte_to_position(range.start)?.line;
        if text.byte_to_position(range.end)?.line != line {
            return None;
        }
        let mut content = String::new();
        if !text.visit_chunks(range.clone(), &mut |chunk| content.push_str(chunk)) {
            return None;
        }
        if content
            .chars()
            .any(|c| matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}'))
        {
            return None;
        }
        Some(self.layout_content(&content, range.start, line, width))
    }
}

impl FixedWidthLayout {
    fn layout_content(
        &self,
        content: &str,
        start_offset: ByteOffset,
        line: LineIndex,
        width: Dip,
    ) -> LineLayout {
        let columns = content.chars().count();
        let per_row = if self.cell_width.0 > 0.0 {
            (width.0 / self.cell_width.0).floor().max(1.0) as usize
        } else {
            1
        };
        let mut rows = Vec::new();
        for start in (0..columns.max(1)).step_by(per_row) {
            let end = (start + per_row).min(columns);
            let byte_start = content
                .char_indices()
                .nth(start)
                .map_or(content.len(), |(i, _)| i);
            let byte_end = content
                .char_indices()
                .nth(end)
                .map_or(content.len(), |(i, _)| i);
            let stop = |column: usize, byte: usize| {
                let x = Dip(column as f32 * self.cell_width.0);
                CaretStop {
                    byte: ByteOffset(start_offset.0 + byte),
                    x,
                    x_before: x,
                }
            };
            let mut caret_stops = content[byte_start..byte_end]
                .char_indices()
                .enumerate()
                .map(|(column, (i, _))| stop(column, byte_start + i))
                .collect::<Vec<_>>();
            caret_stops.push(stop(end - start, byte_end));
            rows.push(LayoutRow {
                range: ByteOffset(start_offset.0 + byte_start)
                    ..ByteOffset(start_offset.0 + byte_end),
                y: Dip(rows.len() as f32 * self.row_height.0),
                height: self.row_height,
                glyph_runs: vec![],
                caret_stops,
                visual_stops: vec![],
            });
        }
        LineLayout {
            line,
            height: Dip(rows.len() as f32 * self.row_height.0),
            rows,
            partial: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FixedTextMeasurer {
    pub char_width: Dip,
    pub height: Dip,
    pub baseline: Dip,
}

impl TextMeasurer for FixedTextMeasurer {
    fn measure(&self, text: &str, _font: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(text.chars().count() as f32 * self.char_width.0),
            height: self.height,
            baseline: self.baseline,
        }
    }
}
