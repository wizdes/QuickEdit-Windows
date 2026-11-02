use crate::{ByteOffset, Dip, LineIndex, Point, Rect, TextSnapshot};
use std::ops::Range;

/// A wrap point has one byte offset and two places on screen: the end of one row
/// and the start of the next. The same is true where text changes direction.
/// Affinity says which one the caret means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Affinity {
    /// The caret stays with the text before the byte, such as at the end of a row.
    Before,
    /// The caret stays with the text after the byte, such as at the start of a row.
    #[default]
    After,
}

/// Where a click lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitTest {
    pub byte: ByteOffset,
    pub affinity: Affinity,
}

/// One place a caret can stop on a row. A row's stops are sorted by byte.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretStop {
    pub byte: ByteOffset,
    /// The caret's x with `Affinity::After`, relative to the row's left edge.
    pub x: Dip,
    /// The caret's x with `Affinity::Before`. It differs from `x` only where the
    /// text on the two sides of this byte runs in different directions.
    pub x_before: Dip,
}

/// One caret edge in left-to-right screen order. Only rows with right-to-left
/// text carry these, because on other rows `caret_stops` is already in that order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisualStop {
    pub x: Dip,
    pub byte: ByteOffset,
    pub affinity: Affinity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutGlyphRun {
    pub range: Range<ByteOffset>,
    pub origin: Point,
    pub bidi_level: u8,
    pub font_id: u32,
    pub font_em_size: Dip,
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
    pub caret_stops: Vec<CaretStop>,
    pub visual_stops: Vec<VisualStop>,
}
/// All positions are relative to this layout's own top left, so they keep their
/// precision on very long lines.
#[derive(Clone, Debug, PartialEq)]
pub struct LineLayout {
    pub line: LineIndex,
    /// Never empty.
    pub rows: Vec<LayoutRow>,
    pub height: Dip,
    /// True when this layout holds only part of its line: a slice, or one
    /// segment of a long line.
    pub partial: bool,
}

impl LayoutRow {
    /// O(log n) on this row's own stops. It takes no lock and calls no system code.
    pub fn hit_test_x(&self, x: Dip) -> HitTest {
        if self.visual_stops.is_empty() {
            let stops = &self.caret_stops;
            let Some(last) = stops.len().checked_sub(1) else {
                return HitTest {
                    byte: self.range.start,
                    affinity: Affinity::After,
                };
            };
            let at = nearest(
                stops.partition_point(|s| s.x.0 <= x.0),
                stops.len(),
                x,
                |i| stops[i].x,
            );
            let at_row_end = at == last && last > 0;
            HitTest {
                byte: stops[at].byte,
                affinity: if at_row_end {
                    Affinity::Before
                } else {
                    Affinity::After
                },
            }
        } else {
            let stops = &self.visual_stops;
            let at = nearest(
                stops.partition_point(|s| s.x.0 <= x.0),
                stops.len(),
                x,
                |i| stops[i].x,
            );
            HitTest {
                byte: stops[at].byte,
                affinity: stops[at].affinity,
            }
        }
    }
    fn stop(&self, byte: ByteOffset) -> Option<&CaretStop> {
        let at = self
            .caret_stops
            .binary_search_by_key(&byte, |s| s.byte)
            .ok()?;
        self.caret_stops.get(at)
    }
}

/// Picks the nearer of the stops on either side of a split point. A click exactly
/// halfway goes to the right-hand stop, as DirectWrite does.
fn nearest(split: usize, len: usize, x: Dip, x_at: impl Fn(usize) -> Dip) -> usize {
    if split == 0 {
        return 0;
    }
    if split == len {
        return len - 1;
    }
    let left = x.0 - x_at(split - 1).0;
    let right = x_at(split).0 - x.0;
    if left < right { split - 1 } else { split }
}

impl LineLayout {
    /// O(log rows + log stops). It takes no lock and calls no system code.
    pub fn hit_test_point(&self, point: Point) -> HitTest {
        let at = self
            .rows
            .partition_point(|r| r.y.0 + r.height.0 <= point.y.0)
            .min(self.rows.len() - 1);
        self.rows[at].hit_test_x(point.x)
    }
    /// The caret for a byte. `None` when the byte is not a caret stop in this layout.
    pub fn caret_rect(&self, byte: ByteOffset, affinity: Affinity) -> Option<Rect> {
        let first = self.rows.partition_point(|r| r.range.end < byte);
        let next = self.rows.get(first + 1).filter(|r| r.range.start == byte);
        let row = self.rows.get(first)?;
        let (row, stop) = match (affinity, next) {
            (Affinity::After, Some(next)) => (next, next.stop(byte)?),
            // The end of a row with a hard break has no stop, so the next row holds it.
            (Affinity::Before, Some(next)) if row.stop(byte).is_none() => (next, next.stop(byte)?),
            _ => (row, row.stop(byte)?),
        };
        Some(Rect {
            x: match affinity {
                Affinity::Before => stop.x_before,
                Affinity::After => stop.x,
            },
            y: row.y,
            width: Dip(1.0),
            height: row.height,
        })
    }
}

/// Layout operates on immutable snapshots. Workers may call `layout_line`; the UI
/// thread may call hit tests on a cached `LineLayout`. Implementations must be thread-safe.
pub trait Layout: Send + Sync {
    /// An implementation may give only the first part of a very long line. It
    /// then sets `LineLayout::partial`.
    fn layout_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout>;
    /// Lays out only this slice of one line, for very long lines. The byte range
    /// must fall on character boundaries. An implementation may refuse a range
    /// that is too long or that cuts a cluster.
    fn layout_slice(
        &self,
        text: &dyn TextSnapshot,
        range: Range<ByteOffset>,
        width: Dip,
    ) -> Option<LineLayout>;
    fn hit_test_point(&self, layout: &LineLayout, point: Point) -> HitTest {
        layout.hit_test_point(point)
    }
    fn caret_rect(
        &self,
        layout: &LineLayout,
        byte: ByteOffset,
        affinity: Affinity,
    ) -> Option<Rect> {
        layout.caret_rect(byte, affinity)
    }
    /// The caret's top left with `Affinity::After`.
    fn hit_test_offset(&self, layout: &LineLayout, byte: ByteOffset) -> Option<Point> {
        self.caret_rect(layout, byte, Affinity::After)
            .map(|r| Point { x: r.x, y: r.y })
    }
}
