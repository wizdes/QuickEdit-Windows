//! Wrap points for printable ASCII in a fixed-width font, without DirectWrite.
//!
//! The rules follow the Unicode line breaking rules (UAX 14) as DirectWrite
//! applies them. `wrap_matches_directwrite` in `tests/wrap.rs` compares the
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
