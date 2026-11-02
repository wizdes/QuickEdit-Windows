//! Slow reference implementations for tests in other crates.
use crate::*;
use std::ops::Range;

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
            let mut caret_stops = content[byte_start..byte_end]
                .char_indices()
                .map(|(i, _)| ByteOffset(range.start.0 + byte_start + i))
                .collect::<Vec<_>>();
            caret_stops.push(ByteOffset(range.start.0 + byte_end));
            rows.push(LayoutRow {
                range: ByteOffset(range.start.0 + byte_start)..ByteOffset(range.start.0 + byte_end),
                y: Dip(rows.len() as f32 * self.row_height.0),
                height: self.row_height,
                glyph_runs: vec![],
                caret_stops,
            });
        }
        Some(LineLayout {
            line,
            height: Dip(rows.len() as f32 * self.row_height.0),
            rows,
        })
    }
    fn hit_test_point(&self, layout: &LineLayout, point: Point) -> ByteOffset {
        let row = ((point.y.0 / self.row_height.0).floor().max(0.0) as usize)
            .min(layout.rows.len().saturating_sub(1));
        let row = &layout.rows[row];
        let column = (point.x.0 / self.cell_width.0).round().max(0.0) as usize;
        row.caret_stops[column.min(row.caret_stops.len() - 1)]
    }
    fn caret_rect(&self, layout: &LineLayout, byte: ByteOffset) -> Option<Rect> {
        let row = layout
            .rows
            .iter()
            .find(|r| r.range.contains(&byte) || r.range.end == byte)?;
        let column = row.caret_stops.iter().position(|stop| *stop == byte)?;
        Some(Rect {
            x: Dip(column as f32 * self.cell_width.0),
            y: row.y,
            width: Dip(1.0),
            height: row.height,
        })
    }
}
