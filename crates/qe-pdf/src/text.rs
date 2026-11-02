//! Works out which text each glyph of a run stands for.
//!
//! A `LayoutGlyphRun` has glyphs and a byte range, and no cluster map. The row's caret stops
//! mark the cluster edges, with their x places. So the walk takes clusters in byte order
//! and glyphs in logical order, gives each cluster the glyphs its width holds, and keeps
//! a zero-width glyph with the cluster before it. All of the run's text is always given to
//! some glyph, so a wrong guess can change where a mark's text sits and never loses text.
use qe_api::{LayoutGlyphRun, LayoutRow};
use std::ops::Range;

/// For each glyph of the run, the text it stands for. Most are one character.
pub fn glyph_texts(run: &LayoutGlyphRun, row: &LayoutRow, text: &str) -> Vec<String> {
    let count = run.glyph_ids.len();
    let mut out = vec![String::new(); count];
    if count == 0 {
        return out;
    }
    let base = run.range.start.0;
    let clusters = clusters_of(run, row);
    let (mut glyph, mut used, mut wanted) = (0, 0f64, 0f64);
    for (at, (range, width)) in clusters.iter().enumerate() {
        let piece = text
            .get(range.start - base..range.end - base)
            .unwrap_or_default();
        wanted += width;
        let first = glyph;
        if at + 1 == clusters.len() {
            glyph = count;
        } else {
            while glyph < count && (used < wanted - 0.01 || (glyph == first && *width > 0.01)) {
                used += f64::from(run.advances[glyph].0);
                glyph += 1;
            }
            // Marks with no width stay with the letter before them.
            while glyph < count && glyph > first && run.advances[glyph].0 == 0.0 {
                glyph += 1;
            }
        }
        if first == glyph {
            // A cluster that drew nothing, such as a joiner, goes with the glyph before.
            out[first.saturating_sub(1).min(count - 1)].push_str(piece);
        } else {
            share_out(piece, &mut out[first..glyph]);
        }
    }
    out
}

/// When a cluster has as many glyphs as characters, each glyph gets its own character,
/// such as a letter and its mark. Otherwise the first glyph stands for all of it, such as
/// a ligature, and the others stand for nothing.
fn share_out(piece: &str, glyphs: &mut [String]) {
    if piece.chars().count() == glyphs.len() {
        for (slot, character) in glyphs.iter_mut().zip(piece.chars()) {
            slot.push(character);
        }
    } else if let Some(slot) = glyphs.first_mut() {
        slot.push_str(piece);
    }
}

/// The clusters of a run, in byte order: their byte ranges and their widths.
fn clusters_of(run: &LayoutGlyphRun, row: &LayoutRow) -> Vec<(Range<usize>, f64)> {
    let (start, end) = (run.range.start, run.range.end);
    let from = row.caret_stops.partition_point(|stop| stop.byte < start);
    let to = row.caret_stops.partition_point(|stop| stop.byte <= end);
    let stops = row.caret_stops.get(from..to).unwrap_or(&[]);
    let mut clusters = Vec::with_capacity(stops.len());
    let mut last_byte = start.0;
    for pair in stops.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        // The edge after a byte belongs to this run, and so does the edge before the next.
        let width = (f64::from(b.x_before.0) - f64::from(a.x.0)).abs();
        if a.byte.0 > last_byte {
            clusters.push((last_byte..a.byte.0, 0.0));
        }
        clusters.push((a.byte.0..b.byte.0, width));
        last_byte = b.byte.0;
    }
    if last_byte < end.0 {
        clusters.push((last_byte..end.0, 0.0));
    }
    if clusters.is_empty() {
        clusters.push((start.0..end.0, 0.0));
    }
    clusters
}
