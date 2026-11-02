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
        let glyph_ids = unsafe { std::slice::from_raw_parts(run.glyphIndices, n) }.to_vec();
        let advances = unsafe { std::slice::from_raw_parts(run.glyphAdvances, n) }
            .iter()
            .copied()
            .map(Dip)
            .collect();
        let offsets = if run.glyphOffsets.is_null() {
            vec![Point::default(); n]
        } else {
            unsafe { std::slice::from_raw_parts(run.glyphOffsets, n) }
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
/// the text before a slice: a few no-break spaces stand in for that text, and
/// their width comes off every position on the first row.
pub(crate) fn shape(
    engine: &DirectWriteLayout,
    s: &str,
    start: ByteOffset,
    line: LineIndex,
    width: Dip,
    start_column: usize,
    partial: bool,
) -> Result<LineLayout, LayoutError> {
    let font = engine.font();
    let lead = if s.contains('\t') {
        start_column % engine.tab_width()
    } else {
        0
    };
    let mut utf16: Vec<u16> = Vec::with_capacity(s.len() + lead);
    utf16.resize(lead, NBSP);
    utf16.extend(s.encode_utf16());
    let lead_width = lead as f64 * font.advance_exact;
    let wrap = engine.wrap_enabled();
    let layout_width = if wrap {
        (width.0 as f64 + lead_width).max(1.0) as f32
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
    let mut lines = vec![DWRITE_LINE_METRICS::default(); text_metrics.lineCount.max(1) as usize];
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
    for (row_index, metric) in lines.iter().enumerate() {
        let last_unit = first_unit + metric.length as usize;
        let row_start = byte_of(first_unit);
        let row_end = byte_of(last_unit);
        let shift = if row_index == 0 { lead_width } else { 0.0 };
        // The ranges for this row. They come in row order.
        let from = range_at;
        while range_at < ranges.len() && (ranges[range_at].textPosition as usize) < last_unit {
            range_at += 1;
        }
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
                x += sign * clusters[index].width as f64;
                has_rtl |= clusters[index]._bitfield & IS_RTL != 0;
                unit = cluster_start[index + 1];
                let trail_byte = byte_of(unit);
                let closes = unit >= end || allowed[trail_byte as usize] || trail_byte == row_end;
                if closes {
                    open = false;
                    if lead_unit >= lead && unit > lead_unit {
                        edges.push(Edge {
                            byte: byte_of(lead_unit),
                            x: (lead_x - shift) as f32,
                            affinity: Affinity::After,
                        });
                        edges.push(Edge {
                            byte: trail_byte,
                            x: (x - shift) as f32,
                            affinity: Affinity::Before,
                        });
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
    for raw in collected.runs.drain(..) {
        place_run(raw, &mut rows, &unit_byte, lead, lead_width, start);
    }
    Ok(LineLayout {
        line,
        rows,
        height: Dip(y as f32),
        partial,
    })
}

fn place_run(
    mut raw: RawRun,
    rows: &mut [LayoutRow],
    unit_byte: &[u32],
    lead: usize,
    lead_width: f64,
    start: ByteOffset,
) {
    let at = rows.partition_point(|r| r.y.0 + r.height.0 < raw.origin.y.0);
    let row_index = at.min(rows.len() - 1);
    let mut first = raw.text_position as usize;
    let last = first + raw.text_length as usize;
    if last <= lead {
        return;
    }
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
    if row_index == 0 && lead > 0 {
        raw.origin.x = Dip((raw.origin.x.0 as f64 - lead_width) as f32);
    }
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
    let visual_stops = if has_rtl {
        edges
            .iter()
            .map(|e| VisualStop {
                x: Dip(e.x),
                byte: absolute(e.byte),
                affinity: e.affinity,
            })
            .collect()
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
