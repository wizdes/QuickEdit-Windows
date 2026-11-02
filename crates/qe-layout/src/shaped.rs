//! The slow path: full DirectWrite shaping. Each step is linear in the text.
use crate::{
    DirectWriteLayout, LayoutError,
    fonts::{Font, dw, no_ligatures, register_face},
    segments::stop_table,
};
use qe_api::{
    Affinity, ByteOffset, CaretStop, Dip, LayoutGlyphRun, LayoutRow, LineIndex, LineLayout, Point,
    TextMetrics, VisualStop,
};
use std::{ffi::c_void, ops::Range, sync::Mutex};
use windows::{
    Win32::Graphics::DirectWrite::*,
    core::{BOOL, implement},
};

const NO_WRAP_WIDTH: f32 = 1_000_000.0;
/// DWRITE_CLUSTER_METRICS bit: the cluster runs right to left.
const IS_RTL: u16 = 1 << 4;
/// DWRITE_CLUSTER_METRICS bit: a hard break, such as VT or FF inside a line.
const IS_NEWLINE: u16 = 1 << 2;
const NBSP: u16 = 0xA0;
const OUTSIDE_THE_ROW: f32 = 4_000_000.0;
const TAB_STOP_NUDGE: f64 = 1.0 - 1e-5;

struct RawRun {
    text_position: u32,
    text_length: u32,
    font_id: u32,
    em_size: f32,
    glyph_ids: Vec<u16>,
    advances: Vec<Dip>,
    offsets: Vec<Point>,
    origin: Point,
    bidi_level: u8,
}
#[derive(Default)]
struct Collected {
    runs: Vec<RawRun>,
    error: Option<LayoutError>,
}

/// DirectWrite may pass a null pointer with a count of zero, and `from_raw_parts`
/// must never see a null pointer.
///
/// # Safety
/// A pointer that is not null must point at `count` values.
unsafe fn raw_slice<'a, T>(pointer: *const T, count: usize) -> &'a [T] {
    if pointer.is_null() || count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer, count) }
    }
}

#[implement(IDWriteTextRenderer)]
struct RunCollector {
    collected: std::sync::Arc<Mutex<Collected>>,
}
impl IDWritePixelSnapping_Impl for RunCollector_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const c_void) -> windows::core::Result<BOOL> {
        Ok(BOOL(1))
    }
    fn GetCurrentTransform(
        &self,
        _: *const c_void,
        out: *mut DWRITE_MATRIX,
    ) -> windows::core::Result<()> {
        unsafe {
            *out = DWRITE_MATRIX {
                m11: 1.0,
                m12: 0.0,
                m21: 0.0,
                m22: 1.0,
                dx: 0.0,
                dy: 0.0,
            };
        }
        Ok(())
    }
    fn GetPixelsPerDip(&self, _: *const c_void) -> windows::core::Result<f32> {
        Ok(1.0)
    }
}
impl IDWriteTextRenderer_Impl for RunCollector_Impl {
    fn DrawGlyphRun(
        &self,
        _: *const c_void,
        x: f32,
        y: f32,
        _: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        let run = unsafe { &*run };
        let desc = unsafe { &*desc };
        let n = run.glyphCount as usize;
        let glyph_ids = unsafe { raw_slice(run.glyphIndices, n) }.to_vec();
        let advances = unsafe { raw_slice(run.glyphAdvances, n) }
            .iter()
            .copied()
            .map(Dip)
            .collect();
        let offsets = if run.glyphOffsets.is_null() {
            vec![Point::default(); n]
        } else {
            unsafe { raw_slice(run.glyphOffsets, n) }
                .iter()
                .map(|o| Point {
                    x: Dip(o.advanceOffset),
                    y: Dip(o.ascenderOffset),
                })
                .collect()
        };
        let mut collected = self.this.collected.lock().map_err(|_| {
            windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))
        })?;
        let font_id = match (*run.fontFace).as_ref().map(register_face) {
            Some(Ok(id)) => id,
            Some(Err(e)) => {
                collected.error = Some(e);
                0
            }
            None => {
                collected.error = Some(LayoutError::Poisoned("glyph run without a font face"));
                0
            }
        };
        collected.runs.push(RawRun {
            text_position: desc.textPosition,
            text_length: desc.stringLength,
            font_id,
            em_size: run.fontEmSize,
            glyph_ids,
            advances,
            offsets,
            origin: Point {
                x: Dip(x),
                y: Dip(y),
            },
            bidi_level: run.bidiLevel as u8,
        });
        Ok(())
    }
    fn DrawUnderline(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_UNDERLINE,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_STRIKETHROUGH,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawInlineObject(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: windows::core::Ref<IDWriteInlineObject>,
        _: BOOL,
        _: BOOL,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Maps each UTF-16 unit to its byte offset in `s`. The middle of a surrogate
/// pair maps to the start of its character.
fn unit_to_byte(s: &str) -> Vec<u32> {
    let mut map = Vec::with_capacity(s.len() + 1);
    for (at, ch) in s.char_indices() {
        for _ in 0..ch.len_utf16() {
            map.push(at as u32);
        }
    }
    map.push(s.len() as u32);
    map
}

/// One caret edge met while walking a row in visual order.
#[derive(Clone, Copy)]
struct Edge {
    byte: u32,
    x: f32,
    affinity: Affinity,
    /// False for an edge of a cluster with no width. A click never lands on one.
    has_width: bool,
}

/// One call for the whole text: per row and direction change, where that text
/// sits. Cluster widths then fill in each caret stop without another call.
fn hit_ranges(
    layout: &IDWriteTextLayout,
    units: u32,
) -> Result<Vec<DWRITE_HIT_TEST_METRICS>, LayoutError> {
    let mut metrics = vec![DWRITE_HIT_TEST_METRICS::default(); 16];
    let mut count = 0;
    let first =
        unsafe { layout.HitTestTextRange(0, units, 0.0, 0.0, Some(&mut metrics), &mut count) };
    if let Err(error) = first {
        if count as usize <= metrics.len() {
            return Err(dw("HitTestTextRange")(error));
        }
        metrics.resize(count as usize, DWRITE_HIT_TEST_METRICS::default());
        unsafe {
            layout
                .HitTestTextRange(0, units, 0.0, 0.0, Some(&mut metrics), &mut count)
                .map_err(dw("HitTestTextRange"))?;
        }
    }
    metrics.truncate(count as usize);
    Ok(metrics)
}

/// Lays out `s` with DirectWrite. `start_column` keeps tabs on the same stops as
/// the text before a slice: a few no-break spaces stand in for that text, and their
/// width comes off every position on their row. They also take room on that row, so
/// it stays inside `width` measured from where the slice's text would really sit. In
/// a width too narrow to hold them and one cell, they are left out and tabs start
/// from the slice's own left edge.
///
/// DirectWrite's emergency wrap can break inside a cluster, such as between the two
/// letters of a flag. When it does, the text is cut before that cluster and laid out
/// in pieces, each on its own rows, so the cluster moves whole to the next row. A
/// cluster wider than the row gets a row of its own, past the right edge. Text
/// direction and joining are worked out per piece, so a cut can change them at its
/// edge. The rows after a cut are laid out again, which costs another layout per cut.
pub(crate) fn shape(
    engine: &DirectWriteLayout,
    s: &str,
    start: ByteOffset,
    line: LineIndex,
    width: Dip,
    start_column: usize,
    partial: bool,
) -> Result<LineLayout, LayoutError> {
    let wrap = engine.wrap_enabled();
    let request = Request {
        engine,
        line,
        width,
        partial,
    };
    let mut done: Option<LineLayout> = None;
    let mut at = 0;
    let mut column = start_column;
    loop {
        let rest = &s[at..];
        let begin = ByteOffset(start.0 + at);
        let piece = match request.shape_piece(rest, begin, column, wrap)? {
            Piece::Whole(layout) => {
                at = s.len();
                layout
            }
            Piece::RowEndsInside(byte) => {
                let table = stop_table(rest);
                let before = (0..byte).rev().find(|b| table[*b]).unwrap_or(0);
                let after = (byte + 1..=rest.len())
                    .find(|b| table[*b])
                    .unwrap_or(rest.len());
                if before > 0 {
                    at += before;
                    shape(engine, &rest[..before], begin, line, width, column, partial)?
                } else {
                    at += after;
                    match request.shape_piece(&rest[..after], begin, column, false)? {
                        Piece::Whole(layout) => layout,
                        // With no wrapping, no row ends inside anything.
                        Piece::RowEndsInside(byte) => {
                            return Err(LayoutError::CutCluster(ByteOffset(begin.0 + byte)));
                        }
                    }
                }
            }
        };
        column = 0;
        done = Some(match done {
            Some(earlier) => join_rows(earlier, piece),
            None => piece,
        });
        if at >= s.len() {
            break;
        }
    }
    done.ok_or(LayoutError::DirectWrite {
        call: "GetLineMetrics",
        message: "no rows".into(),
    })
}

/// Puts `lower`'s rows under `upper`'s.
fn join_rows(mut upper: LineLayout, mut lower: LineLayout) -> LineLayout {
    let offset = upper.height.0;
    for row in &mut lower.rows {
        row.y = Dip(row.y.0 + offset);
        for run in &mut row.glyph_runs {
            run.origin.y = Dip(run.origin.y.0 + offset);
        }
    }
    upper.height = Dip(offset + lower.height.0);
    upper.rows.append(&mut lower.rows);
    upper
}

struct Request<'a> {
    engine: &'a DirectWriteLayout,
    line: LineIndex,
    width: Dip,
    partial: bool,
}
enum Piece {
    Whole(LineLayout),
    /// A row ends at this byte of the piece, which is inside a cluster.
    RowEndsInside(usize),
}

impl Request<'_> {
    fn shape_piece(
        &self,
        s: &str,
        start: ByteOffset,
        start_column: usize,
        wrap: bool,
    ) -> Result<Piece, LayoutError> {
        let (engine, line, width, partial) = (self.engine, self.line, self.width, self.partial);
        let font = engine.font();
        let lead = if s.contains('\t') {
            start_column % engine.tab_width()
        } else {
            0
        };
        let lead = if wrap && (lead + 1) as f64 * font.advance_exact > width.0 as f64 {
            0
        } else {
            lead
        };
        let mut utf16: Vec<u16> = Vec::with_capacity(s.len() + lead);
        utf16.resize(lead, NBSP);
        utf16.extend(s.encode_utf16());
        let layout_width = if wrap {
            width.0.max(1.0)
        } else {
            NO_WRAP_WIDTH
        };
        let layout = font.create_layout(&utf16, layout_width, 1_000_000.0)?;
        unsafe {
            layout
                .SetWordWrapping(if wrap {
                    DWRITE_WORD_WRAPPING_WRAP
                } else {
                    DWRITE_WORD_WRAPPING_NO_WRAP
                })
                .map_err(dw("SetWordWrapping"))?;
            // A tab exactly on a stop must go to the next stop. DirectWrite adds advances
            // in f32, so a sum can land a hair short of the stop and the tab would get no
            // width. A stop a hair closer prevents that and moves text by far under a pixel.
            layout
                .SetIncrementalTabStop(
                    (font.advance_exact * engine.tab_width() as f64 * TAB_STOP_NUDGE) as f32,
                )
                .map_err(dw("SetIncrementalTabStop"))?;
        }
        if font.fast_ok {
            turn_off_ascii_ligatures(&layout, &utf16)?;
        }
        let mut text_metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout
                .GetMetrics(&mut text_metrics)
                .map_err(dw("GetMetrics"))?;
        }
        let mut line_count = 0;
        let mut lines =
            vec![DWRITE_LINE_METRICS::default(); text_metrics.lineCount.max(1) as usize];
        unsafe {
            layout
                .GetLineMetrics(Some(&mut lines), &mut line_count)
                .map_err(dw("GetLineMetrics"))?;
        }
        lines.truncate(line_count as usize);
        let mut cluster_count = 0;
        let mut clusters = vec![DWRITE_CLUSTER_METRICS::default(); utf16.len().max(1)];
        unsafe {
            layout
                .GetClusterMetrics(Some(&mut clusters), &mut cluster_count)
                .map_err(dw("GetClusterMetrics"))?;
        }
        clusters.truncate(cluster_count as usize);

        let unit_byte = unit_to_byte(s);
        let byte_of = |unit: usize| unit_byte[unit.saturating_sub(lead).min(unit_byte.len() - 1)];
        let allowed = stop_table(s);
        let mut row_end_unit = 0;
        for metric in &lines {
            row_end_unit += metric.length as usize;
            let byte = byte_of(row_end_unit) as usize;
            if wrap && byte < s.len() && !allowed[byte] {
                return Ok(Piece::RowEndsInside(byte));
            }
        }
        // Which cluster holds each UTF-16 unit.
        let mut cluster_at = Vec::with_capacity(utf16.len() + 1);
        let mut cluster_start = Vec::with_capacity(clusters.len() + 1);
        let mut unit = 0;
        for (index, cluster) in clusters.iter().enumerate() {
            cluster_start.push(unit);
            for _ in 0..cluster.length {
                cluster_at.push(index as u32);
            }
            unit += cluster.length as usize;
        }
        cluster_start.push(unit);

        let ranges = hit_ranges(&layout, utf16.len() as u32)?;
        let mut rows = Vec::with_capacity(lines.len());
        let mut range_at = 0;
        let mut first_unit = 0;
        let mut y = 0.0f64;
        // Rows with only stand-in spaces are not rows of the text.
        let mut skipped_height = 0.0f64;
        let mut row_shifts = Vec::with_capacity(lines.len());
        for metric in &lines {
            let last_unit = first_unit + metric.length as usize;
            let row_start = byte_of(first_unit);
            let row_end = byte_of(last_unit);
            // The ranges for this row. They come in row order.
            let from = range_at;
            while range_at < ranges.len() && (ranges[range_at].textPosition as usize) < last_unit {
                range_at += 1;
            }
            if lead > 0 && last_unit <= lead {
                first_unit = last_unit;
                skipped_height += metric.height as f64;
                continue;
            }
            let mut shift = 0.0;
            let mut unit = first_unit;
            while unit < lead {
                let index = cluster_at[unit] as usize;
                shift += clusters[index].width as f64;
                unit = cluster_start[index + 1];
            }
            row_shifts.push(shift);
            let mut row_ranges: Vec<_> = ranges[from..range_at].to_vec();
            row_ranges.sort_by(|a, b| a.left.total_cmp(&b.left));
            let mut edges: Vec<Edge> = Vec::with_capacity((last_unit - first_unit) * 2 + 2);
            let mut has_rtl = false;
            for range in &row_ranges {
                let rtl = range.bidiLevel % 2 == 1;
                has_rtl |= rtl;
                let begin = range.textPosition as usize;
                let end = (begin + range.length as usize).min(last_unit);
                let run_start = edges.len();
                let sign = if rtl { -1.0 } else { 1.0 };
                let mut x = if rtl {
                    range.left as f64 + range.width as f64
                } else {
                    range.left as f64
                };
                let mut unit = begin;
                let mut lead_x = x;
                let mut lead_unit = begin;
                let mut open = false;
                while unit < end {
                    let index = cluster_at[unit] as usize;
                    // A caret never sits after a hard break on the break's own row, and
                    // DirectWrite never gives that place for a click.
                    if clusters[index]._bitfield & IS_NEWLINE != 0 {
                        unit = cluster_start[index + 1];
                        open = false;
                        continue;
                    }
                    if !open {
                        lead_x = x;
                        lead_unit = cluster_start[index].max(begin);
                        open = true;
                    }
                    has_rtl |= clusters[index]._bitfield & IS_RTL != 0;
                    unit = cluster_start[index + 1];
                    x += sign * clusters[index].width as f64;
                    let trail_byte = byte_of(unit);
                    let closes =
                        unit >= end || allowed[trail_byte as usize] || trail_byte == row_end;
                    if closes {
                        open = false;
                        if lead_unit >= lead && unit > lead_unit {
                            let has_width = x != lead_x;
                            let lead_byte = byte_of(lead_unit);
                            // A direction change can fall inside a cluster. That place is
                            // no caret stop, so a click there goes to the cluster's edges.
                            if allowed[lead_byte as usize] || lead_byte == row_start {
                                edges.push(Edge {
                                    byte: lead_byte,
                                    x: (lead_x - shift) as f32,
                                    affinity: Affinity::After,
                                    has_width,
                                });
                            }
                            if allowed[trail_byte as usize] || trail_byte == row_end {
                                edges.push(Edge {
                                    byte: trail_byte,
                                    x: (x - shift) as f32,
                                    affinity: Affinity::Before,
                                    has_width,
                                });
                            }
                        }
                    }
                }
                if rtl {
                    edges[run_start..].reverse();
                }
            }
            let ends_in_break = last_unit > first_unit
                && clusters[cluster_at[last_unit - 1] as usize]._bitfield & IS_NEWLINE != 0;
            rows.push(row_from_edges(
                start,
                row_start..row_end,
                ends_in_break,
                &edges,
                has_rtl,
                Dip(y as f32),
                Dip(metric.height),
            ));
            if edges.iter().any(|e| !e.has_width)
                && let Some(row) = rows.last_mut()
            {
                let middle = (skipped_height + y) as f32 + metric.height / 2.0;
                let answer = |x: f32| -> Result<(u32, Affinity), LayoutError> {
                    let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
                    let mut hit = DWRITE_HIT_TEST_METRICS::default();
                    unsafe {
                        layout
                            .HitTestPoint(x, middle, &mut trailing, &mut inside, &mut hit)
                            .map_err(dw("HitTestPoint"))?;
                    }
                    let unit = hit.textPosition as usize;
                    Ok(if trailing.as_bool() {
                        (byte_of(unit + hit.length as usize), Affinity::Before)
                    } else {
                        (byte_of(unit), Affinity::After)
                    })
                };
                let left = answer(-OUTSIDE_THE_ROW)?;
                let right = answer(OUTSIDE_THE_ROW)?;
                add_outer_stops(row, start, left, right);
            }
            first_unit = last_unit;
            y += metric.height as f64;
        }
        if rows.is_empty() {
            return Err(LayoutError::DirectWrite {
                call: "GetLineMetrics",
                message: "no rows".into(),
            });
        }
        let collected = std::sync::Arc::new(Mutex::new(Collected::default()));
        let renderer: IDWriteTextRenderer = RunCollector {
            collected: collected.clone(),
        }
        .into();
        unsafe {
            layout
                .Draw(None, &renderer, 0.0, 0.0)
                .map_err(dw("IDWriteTextLayout::Draw"))?;
        }
        let mut collected = collected
            .lock()
            .map_err(|_| LayoutError::Poisoned("glyph run collector"))?;
        if let Some(error) = collected.error.take() {
            return Err(error);
        }
        let placing = RunPlacing {
            unit_byte: &unit_byte,
            lead,
            skipped_height,
            row_shifts: &row_shifts,
            start,
        };
        for raw in collected.runs.drain(..) {
            placing.place(raw, &mut rows);
        }
        Ok(Piece::Whole(LineLayout {
            line,
            rows,
            height: Dip(y as f32),
            partial,
        }))
    }
}

struct RunPlacing<'a> {
    unit_byte: &'a [u32],
    lead: usize,
    skipped_height: f64,
    row_shifts: &'a [f64],
    start: ByteOffset,
}
impl RunPlacing<'_> {
    fn place(&self, mut raw: RawRun, rows: &mut [LayoutRow]) {
        let (lead, unit_byte, start) = (self.lead, self.unit_byte, self.start);
        let mut first = raw.text_position as usize;
        let last = first + raw.text_length as usize;
        if last <= lead {
            return;
        }
        raw.origin.y = Dip((raw.origin.y.0 as f64 - self.skipped_height) as f32);
        let at = rows.partition_point(|r| r.y.0 + r.height.0 < raw.origin.y.0);
        let row_index = at.min(rows.len() - 1);
        if first < lead {
            // This run starts with the stand-in spaces, one glyph each.
            let skip = lead - first;
            let skipped: f64 = raw.advances[..skip].iter().map(|a| a.0 as f64).sum();
            raw.glyph_ids.drain(..skip);
            raw.advances.drain(..skip);
            raw.offsets.drain(..skip);
            raw.origin.x = Dip((raw.origin.x.0 as f64 + skipped) as f32);
            first = lead;
        }
        raw.origin.x = Dip((raw.origin.x.0 as f64 - self.row_shifts[row_index]) as f32);
        let to_byte = |unit: usize| {
            ByteOffset(start.0 + unit_byte[(unit - lead).min(unit_byte.len() - 1)] as usize)
        };
        rows[row_index].glyph_runs.push(LayoutGlyphRun {
            range: to_byte(first)..to_byte(last),
            origin: raw.origin,
            bidi_level: raw.bidi_level,
            font_id: raw.font_id,
            font_em_size: Dip(raw.em_size),
            glyph_ids: raw.glyph_ids,
            advances: raw.advances,
            offsets: raw.offsets,
        });
    }
}

/// Puts DirectWrite's answers for a click past each end of a row at the ends of its
/// visual order, when they are caret stops.
fn add_outer_stops(
    row: &mut LayoutRow,
    start: ByteOffset,
    left: (u32, Affinity),
    right: (u32, Affinity),
) {
    let stop = |(byte, affinity): (u32, Affinity), x: Dip| {
        let byte = ByteOffset(start.0 + byte as usize);
        row.caret_stops
            .binary_search_by_key(&byte, |s| s.byte)
            .is_ok()
            .then_some(VisualStop { x, byte, affinity })
    };
    let (low, high) = match (row.visual_stops.first(), row.visual_stops.last()) {
        (Some(first), Some(last)) => (first.x, last.x),
        _ => (Dip(0.0), Dip(0.0)),
    };
    let (first, last) = (stop(left, low), stop(right, high));
    row.visual_stops.extend(last);
    if let Some(first) = first {
        row.visual_stops.insert(0, first);
    }
}

fn row_from_edges(
    start: ByteOffset,
    row_bytes: Range<u32>,
    ends_in_break: bool,
    edges: &[Edge],
    has_rtl: bool,
    y: Dip,
    height: Dip,
) -> LayoutRow {
    let absolute = |byte: u32| ByteOffset(start.0 + byte as usize);
    let (row_start, row_end) = (row_bytes.start, row_bytes.end);
    let mut logical: Vec<Edge> = edges.to_vec();
    if !logical.windows(2).all(|p| p[0].byte <= p[1].byte) {
        logical.sort_by_key(|e| e.byte);
    }
    let mut stops: Vec<CaretStop> = Vec::with_capacity(logical.len() / 2 + 2);
    for edge in &logical {
        let byte = absolute(edge.byte);
        let x = Dip(edge.x);
        match stops.last_mut() {
            Some(stop) if stop.byte == byte => match edge.affinity {
                Affinity::Before => stop.x_before = x,
                Affinity::After => stop.x = x,
            },
            _ => stops.push(CaretStop {
                byte,
                x,
                x_before: x,
            }),
        }
    }
    // A row always has a stop at each end, even if nothing was drawn there.
    let right = stops
        .iter()
        .map(|s| s.x_before.0.max(s.x.0))
        .fold(0.0, f32::max);
    if stops.first().is_none_or(|s| s.byte != absolute(row_start)) {
        let x = stops.first().map_or(Dip(0.0), |s| s.x);
        stops.insert(
            0,
            CaretStop {
                byte: absolute(row_start),
                x,
                x_before: x,
            },
        );
    }
    if !ends_in_break && stops.last().is_none_or(|s| s.byte != absolute(row_end)) {
        stops.push(CaretStop {
            byte: absolute(row_end),
            x: Dip(right),
            x_before: Dip(right),
        });
    }
    // A click inside a row never lands on a cluster with no width, such as U+200F. Its
    // edges tie with their neighbors and would hide them from the search, so they stay
    // out of the visual order. They keep their caret stops. `shape` adds the row's two
    // outer stops afterward, from DirectWrite's own answer.
    let has_empty_cluster = edges.iter().any(|e| !e.has_width);
    let visual_stops = if has_rtl || has_empty_cluster {
        let mut visual: Vec<VisualStop> = edges
            .iter()
            .filter(|e| e.has_width)
            .map(|e| VisualStop {
                x: Dip(e.x),
                byte: absolute(e.byte),
                affinity: e.affinity,
            })
            .collect();
        // The edges are already in screen order, and the search needs sorted x. Rounding
        // in the sums can put an edge a hair before the one to its left. Holding it at
        // that edge keeps the order. Sorting would swap them and lose a run's edge.
        let mut right = f32::NEG_INFINITY;
        for stop in &mut visual {
            right = right.max(stop.x.0);
            stop.x = Dip(right);
        }
        visual
    } else {
        Vec::new()
    };
    LayoutRow {
        range: absolute(row_start)..absolute(row_end),
        y,
        height,
        glyph_runs: Vec::new(),
        caret_stops: stops,
        visual_stops,
    }
}

/// The app draws plain fixed-pitch text, so ligatures are off for ASCII on both
/// paths. Text in other scripts keeps its own features.
fn turn_off_ascii_ligatures(layout: &IDWriteTextLayout, utf16: &[u16]) -> Result<(), LayoutError> {
    let typography = no_ligatures()?;
    let mut at = 0;
    while at < utf16.len() {
        if utf16[at] >= 0x80 {
            at += 1;
            continue;
        }
        let begin = at;
        while at < utf16.len() && utf16[at] < 0x80 {
            at += 1;
        }
        unsafe {
            layout
                .SetTypography(
                    &typography,
                    DWRITE_TEXT_RANGE {
                        startPosition: begin as u32,
                        length: (at - begin) as u32,
                    },
                )
                .map_err(dw("SetTypography"))?;
        }
    }
    Ok(())
}

/// The real size of `text` as DirectWrite lays it out, including fallback fonts
/// and line breaks.
pub(crate) fn measure_text(font: &Font, text: &str) -> Result<TextMetrics, LayoutError> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let layout = font.create_layout(&utf16, NO_WRAP_WIDTH, 1_000_000.0)?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe {
        layout.GetMetrics(&mut metrics).map_err(dw("GetMetrics"))?;
    }
    let mut lines = vec![DWRITE_LINE_METRICS::default(); metrics.lineCount.max(1) as usize];
    let mut count = 0;
    unsafe {
        layout
            .GetLineMetrics(Some(&mut lines), &mut count)
            .map_err(dw("GetLineMetrics"))?;
    }
    Ok(TextMetrics {
        width: Dip(metrics.widthIncludingTrailingWhitespace),
        height: Dip(metrics.height),
        baseline: Dip(lines[0].baseline),
    })
}

/// The DirectWrite cluster edges of `s` in this font, as byte offsets.
pub(crate) fn cluster_edges(font: &Font, s: &str) -> Result<Vec<usize>, LayoutError> {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    if utf16.is_empty() {
        return Ok(vec![0]);
    }
    let layout = font.create_layout(&utf16, NO_WRAP_WIDTH, 1_000_000.0)?;
    let mut count = 0;
    let mut clusters = vec![DWRITE_CLUSTER_METRICS::default(); utf16.len()];
    unsafe {
        layout
            .GetClusterMetrics(Some(&mut clusters), &mut count)
            .map_err(dw("GetClusterMetrics"))?;
    }
    clusters.truncate(count as usize);
    let map = unit_to_byte(s);
    let mut unit = 0;
    let mut edges = vec![0];
    for cluster in &clusters {
        unit += cluster.length as usize;
        edges.push(map[unit] as usize);
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::raw_slice;

    #[test]
    fn a_null_pointer_with_no_glyphs_is_an_empty_slice() {
        let empty: &[u16] = unsafe { raw_slice(std::ptr::null(), 0) };
        assert!(empty.is_empty());
        let still_empty: &[u16] = unsafe { raw_slice(std::ptr::null(), 3) };
        assert!(still_empty.is_empty());
        let values = [1u16, 2];
        assert_eq!(unsafe { raw_slice(values.as_ptr(), 2) }, &values);
    }
}
