//! Wrap points for printable ASCII in a fixed-width font, without DirectWrite.
//!
//! The rules follow the Unicode line breaking rules (UAX 14) as DirectWrite
//! applies them. `wrap_matches_directwrite` in `tests/agreement.rs` compares the
//! result with DirectWrite on random text.

/// How many cells fit in a width. DirectWrite adds advances up in `f32` and a row
/// that lands a hair over the width does not fit, so this adds them the same way.
/// `needed` is the most cells the text could take. A width far past it needs no sum.
pub(crate) fn columns_that_fit(advance: f32, width: f32, needed: usize) -> usize {
    if !(advance > 0.0 && width > 0.0) {
        return 0;
    }
    let estimate = (width as f64 / advance as f64) as usize;
    if estimate > needed.saturating_add(1) {
        return estimate;
    }
    // The sum drifts by far less than one cell in the sizes a window can have.
    let mut columns = estimate.saturating_sub(1);
    let mut sum = 0.0f32;
    for _ in 0..columns {
        sum += advance;
    }
    while sum + advance <= width {
        sum += advance;
        columns += 1;
    }
    columns
}

const fn set(bytes: &[u8]) -> u128 {
    let mut out = 0;
    let mut at = 0;
    while at < bytes.len() {
        out |= 1 << bytes[at];
        at += 1;
    }
    out
}
const LETTERS: u128 = set(b"#&*<=>@A^_`~");
const OPENERS: u128 = set(b"([{");
const NUMBER_SIGNS: u128 = set(b"$%+\\");
/// What may start a new row after each kind of character. `A` stands for every
/// letter and `0` for every digit. These sets come from DirectWrite's break flags
/// for each pair, because it applies an older form of the Unicode rules.
const AFTER_LETTER: u128 = NUMBER_SIGNS;
const AFTER_CLOSE_PAREN: u128 = OPENERS;
const AFTER_INFIX: u128 = NUMBER_SIGNS | OPENERS;
const AFTER_HYPHEN: u128 = LETTERS | NUMBER_SIGNS | OPENERS;
const AFTER_BANG: u128 = AFTER_HYPHEN | set(b"0");
const AFTER_CLOSE_BRACE: u128 = LETTERS | OPENERS | set(b"0");
const AFTER_SPACE: u128 = AFTER_BANG | set(b"\t\"'-|");

fn group(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' | b'B'..=b'Z' => b'A',
        b'1'..=b'9' => b'0',
        other => other,
    }
}
fn may_follow(byte: u8) -> u128 {
    match byte {
        b' ' => AFTER_SPACE,
        b'\t' | b'!' | b'?' | b'|' => AFTER_BANG,
        b'-' | b'/' => AFTER_HYPHEN,
        b',' | b'.' | b':' | b';' => AFTER_INFIX,
        b')' | b']' => AFTER_CLOSE_PAREN,
        b'}' => AFTER_CLOSE_BRACE,
        b'"' | b'\'' | b'(' | b'[' | b'{' | b'0'..=b'9' => 0,
        _ => AFTER_LETTER,
    }
}

/// For each byte, whether a row may end right after it.
pub(crate) fn break_points(bytes: &[u8]) -> Vec<bool> {
    let mut out = vec![false; bytes.len()];
    let mut before_spaces = b' ';
    for (at, pair) in bytes.windows(2).enumerate() {
        let (a, b) = (pair[0], group(pair[1]));
        if a != b' ' {
            before_spaces = a;
        }
        let held = a == b' '
            && (OPENERS >> before_spaces & 1 == 1
                || (matches!(before_spaces, b'"' | b'\'') && OPENERS >> b & 1 == 1));
        out[at] = !held && may_follow(a) >> b & 1 == 1;
    }
    out
}

/// Where each row ends, in bytes from the start of the text. The text holds no
/// tabs, so each byte is one cell.
pub(crate) fn wrap_rows(bytes: &[u8], breaks: &[bool], max_columns: usize) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut last_break = None;
        let mut at = start;
        let mut end = bytes.len();
        while at < bytes.len() {
            // Spaces may hang past the right edge.
            if bytes[at] != b' ' && at - start + 1 > max_columns {
                end = last_break.unwrap_or(at.max(start + 1));
                break;
            }
            if breaks[at] {
                last_break = Some(at + 1);
            }
            at += 1;
        }
        rows.push(end);
        start = end;
    }
    if rows.is_empty() {
        rows.push(0);
    }
    rows
}

/// Cells a row of these bytes takes. Spaces may hang past the edge, so they do not count.
fn cells_without_hanging_spaces(bytes: &[u8], start: usize, end: usize) -> usize {
    let mut last = end;
    while last > start && bytes[last - 1] == b' ' {
        last -= 1;
    }
    last - start
}

/// How many rows at a segment's end `fill_last_row` may choose again. Each gives up a
/// word or less, so this many rows hide any short row and the work stays bounded.
const ROWS_TO_REFILL: usize = 12;
/// A last row this close to full reads as a natural one, and costs nothing to keep.
const CLOSE_ENOUGH_CELLS: usize = 8;
/// What a row cut in the middle of a word costs, against one that ends at a break.
const CUT_WORD_COST: u64 = 1 << 20;

/// A long line is cut into segments, and each is wrapped alone. Wrapped greedily, a
/// segment ends in a short row, which looks like a line break in the middle of a
/// paragraph.
///
/// So the last rows of the segment are chosen together, to share out the space that the
/// short row had, and the segment ends in a full row. A wrong-looking row needs more space
/// than any one word, so this minimizes the sum of each row's empty cells, squared. The
/// result depends only on this segment's bytes, so the work is bounded on a line of any
/// length: about `ROWS_TO_REFILL` rows times the cells on a row.
pub(crate) fn fill_last_row(
    bytes: &[u8],
    breaks: &[bool],
    max_columns: usize,
    rows_end: &mut Vec<usize>,
) {
    if rows_end.len() < 2 || max_columns == 0 {
        return;
    }
    let last = rows_end.len() - 1;
    let short = cells_without_hanging_spaces(bytes, rows_end[last - 1], rows_end[last]);
    if short + CLOSE_ENOUGH_CELLS >= max_columns {
        return;
    }
    let kept = rows_end.len().saturating_sub(ROWS_TO_REFILL);
    let begin = if kept == 0 { 0 } else { rows_end[kept - 1] };
    let end = bytes.len();
    let room = end - begin;
    // For each byte from `begin`, the lowest cost of rows that end there, and the row
    // start that gave it.
    let mut cost = vec![u64::MAX; room + 1];
    let mut from = vec![0_usize; room + 1];
    cost[0] = 0;
    for start in 0..room {
        if cost[start] == u64::MAX {
            continue;
        }
        let mut found_break = false;
        let mut stop = start + 1;
        while stop <= room {
            let used = cells_without_hanging_spaces(bytes, begin + start, begin + stop);
            if used > max_columns {
                break;
            }
            if stop == room || breaks[begin + stop - 1] {
                found_break = true;
                let empty = (max_columns - used) as u64;
                let total = cost[start] + empty * empty;
                if total < cost[stop] {
                    cost[stop] = total;
                    from[stop] = start;
                }
            }
            stop += 1;
        }
        if !found_break {
            // One word fills the row, so the row cuts it, as the plain wrap does.
            let cut = (start + max_columns).min(room);
            let total = cost[start] + CUT_WORD_COST;
            if total < cost[cut] {
                cost[cut] = total;
                from[cut] = start;
            }
        }
    }
    if cost[room] == u64::MAX {
        return;
    }
    let mut ends = Vec::new();
    let mut at = room;
    while at > 0 {
        ends.push(begin + at);
        at = from[at];
    }
    rows_end.truncate(kept);
    rows_end.extend(ends.into_iter().rev());
}

#[cfg(test)]
mod tests {
    use super::break_points;
    use crate::fonts::{dw, font};
    use windows::Win32::Graphics::DirectWrite::*;

    /// DirectWrite reports where it may wrap, so the table can be checked directly.
    fn directwrite_breaks(text: &[u8]) -> Vec<bool> {
        let font = font("Cascadia Mono", 13.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
        let units: Vec<u16> = text.iter().map(|b| *b as u16).collect();
        let layout = font.create_layout(&units, 1e6, 1e6).unwrap();
        let mut clusters = vec![DWRITE_CLUSTER_METRICS::default(); units.len()];
        let mut count = 0;
        unsafe {
            layout
                .GetClusterMetrics(Some(&mut clusters), &mut count)
                .map_err(dw("GetClusterMetrics"))
                .unwrap();
        }
        let mut out = vec![false; units.len()];
        let mut at = 0;
        for cluster in &clusters[..count as usize] {
            at += cluster.length as usize;
            out[at - 1] = cluster._bitfield & 1 != 0;
        }
        out
    }

    #[test]
    fn break_points_match_directwrite() {
        let mut seed = 11u64;
        let mut next = move |below: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % below
        };
        let printable: Vec<u8> = (0x20..0x7Fu8).chain(Some(b'\t')).collect();
        let mut wrong = std::collections::BTreeMap::<(u8, u8, u8), (bool, u32)>::new();
        for _ in 0..200 {
            let text: Vec<u8> = (0..2000)
                .map(|_| printable[next(printable.len())])
                .collect();
            let want = directwrite_breaks(&text);
            let got = break_points(&text);
            // The last character's flag says nothing about a next character.
            for at in 1..text.len() - 1 {
                if want[at] != got[at] {
                    let entry = wrong
                        .entry((text[at - 1], text[at], text[at + 1]))
                        .or_insert((want[at], 0));
                    entry.1 += 1;
                }
            }
        }
        let sample: Vec<String> = wrong
            .iter()
            .take(60)
            .map(|((p, a, b), (want, n))| {
                format!(
                    "{:?}{:?}{:?}={want}x{n}",
                    *p as char, *a as char, *b as char
                )
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "{} wrong: {}",
            wrong.len(),
            sample.join(" ")
        );
    }
}
