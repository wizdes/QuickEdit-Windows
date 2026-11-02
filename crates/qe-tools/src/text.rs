//! Character rules that the tools share, and reads from a snapshot on character edges.
use qe_api::{ByteOffset, TextSnapshot};

/// A letter, a digit, a mark or `_`. The Mac's `Full Word` rule comes from AppKit and
/// its source does not show it, so this is the usual rule.
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_extend(c)
}

/// True for a character that joins the character before it. This table is the common
/// part of Unicode's Grapheme_Extend, because `unicode-segmentation` is not in this
/// crate's `Cargo.lock` entry.
pub fn is_extend(c: char) -> bool {
    matches!(c as u32,
        0x300..=0x36f | 0x483..=0x489 | 0x591..=0x5bd | 0x5bf | 0x5c1..=0x5c2 | 0x5c4..=0x5c5
        | 0x5c7 | 0x610..=0x61a | 0x64b..=0x65f | 0x670 | 0x6d6..=0x6dc | 0x6df..=0x6e4
        | 0x6e7..=0x6e8 | 0x6ea..=0x6ed | 0x711 | 0x730..=0x74a | 0x7a6..=0x7b0
        | 0x900..=0x903 | 0x93a..=0x93c | 0x93e..=0x94f | 0x951..=0x957 | 0x962..=0x963
        | 0x981..=0x983 | 0x9bc | 0x9be..=0x9cd | 0xa01..=0xa03 | 0xa3c..=0xa4d
        | 0xe31 | 0xe34..=0xe3a | 0xe47..=0xe4e | 0xeb1 | 0xeb4..=0xebc | 0xec8..=0xecd
        | 0x1160..=0x11ff | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x200c..=0x200d
        | 0x20d0..=0x20f0 | 0x302a..=0x302f | 0x3099..=0x309a | 0xfe00..=0xfe0f
        | 0xfe20..=0xfe2f | 0xff9e..=0xff9f | 0x1f3fb..=0x1f3ff | 0xe0020..=0xe007f
        | 0xe0100..=0xe01ef)
}

fn is_regional(c: char) -> bool {
    matches!(c as u32, 0x1f1e6..=0x1f1ff)
}

/// True when a match may start or end at `at`: it is a character edge, and it is not
/// inside a CRLF, a base with its marks, an emoji sequence or a flag.
pub fn is_cluster_edge(text: &str, at: usize) -> bool {
    if at == 0 || at == text.len() {
        return true;
    }
    let (Some(head), Some(tail)) = (text.get(..at), text.get(at..)) else {
        return false;
    };
    let (Some(before), Some(after)) = (head.chars().next_back(), tail.chars().next()) else {
        return false;
    };
    if before == '\r' && after == '\n' {
        return false;
    }
    if is_extend(after) || before == '\u{200d}' {
        return false;
    }
    if is_regional(before) && is_regional(after) {
        // Flags are pairs, so an odd count before this edge stands inside a flag.
        let run = head.chars().rev().take_while(|c| is_regional(*c)).count();
        return run % 2 == 0;
    }
    true
}

/// Full case folding with no locale, one character at a time. `ß` folds to `ss`, and `ς`
/// to `σ`. Dotless `ı` stays apart from `i`.
pub fn fold_char(c: char, out: &mut impl FnMut(char)) {
    if c.is_ascii() {
        out(c.to_ascii_lowercase());
    } else if c == '\u{131}' {
        out(c);
    } else if c == '\u{1e9e}' {
        out('s');
        out('s');
    } else {
        for upper in c.to_uppercase() {
            for lower in upper.to_lowercase() {
                out(lower);
            }
        }
    }
}

/// The nearest character edge at or before `at`. A worker calls this, because a mapped
/// store may read one byte from disk to answer.
pub fn align_down(snapshot: &dyn TextSnapshot, at: usize) -> usize {
    let at = at.min(readable_len(snapshot));
    for back in 0..4 {
        let Some(place) = at.checked_sub(back) else {
            break;
        };
        if snapshot.visit_chunks(ByteOffset(place)..ByteOffset(place), &mut |_| {}) {
            return place;
        }
    }
    at.saturating_sub(3)
}

/// The nearest character edge at or after `at`, within the text.
pub fn align_up(snapshot: &dyn TextSnapshot, at: usize) -> usize {
    let len = readable_len(snapshot);
    let at = at.min(len);
    for ahead in 0..4 {
        let place = (at + ahead).min(len);
        if snapshot.visit_chunks(ByteOffset(place)..ByteOffset(place), &mut |_| {}) {
            return place;
        }
    }
    len
}

/// The store's own words for a failed read, or a plain sentence.
pub fn read_failure(snapshot: &dyn TextSnapshot) -> String {
    match snapshot.status().failure {
        Some(failure) => format!("The file could not be read: {}", failure.message),
        None => "The text could not be read.".to_string(),
    }
}

/// The bytes this snapshot can read. A snapshot of a mapped file reads only what was indexed
/// when it was taken, and `len_bytes` says the size of the whole file.
pub fn readable_len(snapshot: &dyn TextSnapshot) -> usize {
    let status = snapshot.status();
    if status.complete {
        snapshot.len_bytes().0
    } else {
        status.known_bytes.0.min(snapshot.len_bytes().0)
    }
}
