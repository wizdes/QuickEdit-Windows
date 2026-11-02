use crate::{ByteOffset, Dip, LineIndex, Point, Rect, TextSnapshot};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutGlyphRun {
    pub range: Range<ByteOffset>,
    pub origin: Point,
    pub bidi_level: u8,
    pub font_id: u32,
    pub glyph_ids: Vec<u16>,
    pub advances: Vec<Dip>,
    pub offsets: Vec<Point>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutRow {
    pub range: Range<ByteOffset>,
    pub y: Dip,
    pub height: Dip,
    pub glyph_runs: Vec<LayoutGlyphRun>,
    pub caret_stops: Vec<ByteOffset>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LineLayout {
    pub line: LineIndex,
    pub rows: Vec<LayoutRow>,
    pub height: Dip,
}

/// Layout operates on immutable snapshots. Workers may call `layout_line`; the UI
/// thread may call hit tests on a cached `LineLayout`. Implementations must be thread-safe.
pub trait Layout: Send + Sync {
    fn layout_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout>;
    /// Lays out only this slice of one line, for very long lines. The byte range
    /// must fall on character boundaries.
    fn layout_slice(
        &self,
        text: &dyn TextSnapshot,
        range: Range<ByteOffset>,
        width: Dip,
    ) -> Option<LineLayout>;
    fn hit_test_point(&self, layout: &LineLayout, point: Point) -> ByteOffset;
    fn caret_rect(&self, layout: &LineLayout, byte: ByteOffset) -> Option<Rect>;
    fn hit_test_offset(&self, layout: &LineLayout, byte: ByteOffset) -> Option<Point> {
        self.caret_rect(layout, byte)
            .map(|r| Point { x: r.x, y: r.y })
    }
}
