//! Character rules that the tools share, and reads from a snapshot on character edges.
use qe_api::{ByteOffset, TextSnapshot};
use std::sync::atomic::{AtomicBool, Ordering};

/// A letter, a digit, a mark or `_`. The Mac's `Full Word` rule comes from AppKit and
/// its source does not show it, so this is the usual rule.
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_extend(c)
}

/// True for a mark that belongs to the word before it. This table is the common part of
/// Unicode's Grapheme_Extend and SpacingMark, and it is not complete. It decides words only.
/// Where a match may start or end comes from `unicode-segmentation`.
pub fn is_extend(c: char) -> bool {
    matches!(c as u32,
        0x300..=0x36f | 0x483..=0x489 | 0x591..=0x5bd | 0x5bf | 0x5c1..=0x5c2 | 0x5c4..=0x5c5
        | 0x5c7 | 0x610..=0x61a | 0x64b..=0x65f | 0x670 | 0x6d6..=0x6dc | 0x6df..=0x6e4
        | 0x6e7..=0x6e8 | 0x6ea..=0x6ed | 0x711 | 0x730..=0x74a | 0x7a6..=0x7b0
        | 0x7eb..=0x7f3 | 0x816..=0x819 | 0x81b..=0x823 | 0x825..=0x827 | 0x829..=0x82d
        | 0x859..=0x85b | 0x8d3..=0x8e1 | 0x8e3..=0x903 | 0x93a..=0x93c | 0x93e..=0x94f
        | 0x951..=0x957 | 0x962..=0x963 | 0x981..=0x983 | 0x9bc | 0x9be..=0x9cd | 0x9d7
        | 0x9e2..=0x9e3 | 0xa01..=0xa03 | 0xa3c..=0xa51 | 0xa70..=0xa71 | 0xa75
        | 0xa81..=0xa83 | 0xabc | 0xabe..=0xacd | 0xae2..=0xae3 | 0xb01..=0xb03 | 0xb3c
        | 0xb3e..=0xb57 | 0xb62..=0xb63 | 0xb82 | 0xbbe..=0xbcd | 0xbd7 | 0xc00..=0xc04
        | 0xc3c | 0xc3e..=0xc56 | 0xc62..=0xc63 | 0xc81..=0xc83 | 0xcbc | 0xcbe..=0xcd6
        | 0xce2..=0xce3 | 0xd00..=0xd03 | 0xd3b..=0xd3c | 0xd3e..=0xd57 | 0xd62..=0xd63
        | 0xd81..=0xd83 | 0xdca..=0xddf | 0xdf2..=0xdf3 | 0xe31 | 0xe34..=0xe3a
        | 0xe47..=0xe4e | 0xeb1 | 0xeb4..=0xebc | 0xec8..=0xecd | 0xf18..=0xf19 | 0xf35
        | 0xf37 | 0xf39 | 0xf3e..=0xf3f | 0xf71..=0xf84 | 0xf86..=0xf87 | 0xf8d..=0xfbc
        | 0xfc6 | 0x102b..=0x103e | 0x1056..=0x1059 | 0x105e..=0x1060 | 0x1071..=0x1074
        | 0x1082 | 0x1084..=0x1086 | 0x108d | 0x109d | 0x1160..=0x11ff | 0x135d..=0x135f
        | 0x1712..=0x1714 | 0x1732..=0x1734 | 0x17b4..=0x17d3 | 0x17dd | 0x180b..=0x180d
        | 0x18a9 | 0x1920..=0x193b | 0x1a17..=0x1a1b | 0x1a55..=0x1a7f | 0x1ab0..=0x1aff
        | 0x1b00..=0x1b04 | 0x1b34..=0x1b44 | 0x1dc0..=0x1dff | 0x200c..=0x200d
        | 0x20d0..=0x20f0 | 0x2cef..=0x2cf1 | 0x2de0..=0x2dff | 0x302a..=0x302f
        | 0x3099..=0x309a | 0xa66f..=0xa672 | 0xa674..=0xa67d | 0xa69e..=0xa69f
        | 0xa802 | 0xa806 | 0xa80b | 0xa823..=0xa827 | 0xa880..=0xa881 | 0xa8b4..=0xa8c5
        | 0xa926..=0xa92d | 0xa947..=0xa953 | 0xa980..=0xa983 | 0xa9b3..=0xa9c0
        | 0xaa29..=0xaa36 | 0xabe3..=0xabea | 0xabec..=0xabed | 0xfb1e | 0xfe00..=0xfe0f
        | 0xfe20..=0xfe2f | 0xff9e..=0xff9f | 0x1f3fb..=0x1f3ff | 0xe0020..=0xe007f
        | 0xe0100..=0xe01ef)
}

fn is_regional(c: char) -> bool {
    matches!(c as u32, 0x1f1e6..=0x1f1ff)
}
/// A flag letter is 4 bytes: F0 9F 87 and one more.
fn regional_at(bytes: &[u8], at: usize) -> bool {
    matches!(bytes.get(at..at + 4), Some([0xf0, 0x9f, 0x87, 0xa6..=0xbf]))
}

/// Counts flag letters before a place, for the rule that flags are pairs. It remembers its
/// last answer, so a scan that moves forward through a run never walks the run again, and it
/// carries the count across a buffer's start.
#[derive(Clone, Debug, Default)]
pub(crate) struct FlagRun {
    /// True when an odd number of flag letters stands just before the buffer's first byte.
    base_odd: bool,
    known: Option<(usize, bool)>,
}
impl FlagRun {
    pub(crate) fn new(base_odd: bool) -> Self {
        Self {
            base_odd,
            known: None,
        }
    }
    /// True when an odd number of flag letters stands just before `at`.
    fn odd_before(&mut self, text: &str, at: usize) -> bool {
        let bytes = text.as_bytes();
        if let Some((place, odd)) = self.known
            && place <= at
            && (at - place).is_multiple_of(4)
            && (place..at).step_by(4).all(|i| regional_at(bytes, i))
        {
            let answer = odd ^ ((at - place) / 4 % 2 == 1);
            self.known = Some((at, answer));
            return answer;
        }
        // A place a little before the last answer, as a match's start after its end.
        if let Some((place, odd)) = self.known
            && at < place
            && (place - at).is_multiple_of(4)
            && (at..place).step_by(4).all(|i| regional_at(bytes, i))
        {
            return odd ^ ((place - at) / 4 % 2 == 1);
        }
        let mut first = at;
        while first >= 4 && regional_at(bytes, first - 4) {
            first -= 4;
        }
        let mut odd = (at - first) / 4 % 2 == 1;
        if first == 0 {
            odd ^= self.base_odd;
        }
        self.known = Some((at, odd));
        odd
    }
    /// The buffer is about to lose its first `cut` bytes.
    pub(crate) fn rebase(&mut self, text: &str, cut: usize) {
        self.base_odd = self.odd_before(text, cut);
        self.known = None;
    }
    pub(crate) fn is_edge(&mut self, text: &str, at: usize) -> bool {
        if at == 0 || at == text.len() {
            return true;
        }
        let (Some(head), Some(tail)) = (text.get(..at), text.get(at..)) else {
            return false;
        };
        let (Some(before), Some(after)) = (head.chars().next_back(), tail.chars().next()) else {
            return false;
        };
        if is_regional(before) && is_regional(after) {
            return !self.odd_before(text, at);
        }
        joins_nothing(text, at)
    }
}

/// The full Unicode rule. Text before the buffer can only matter in rare emoji sequences, and
/// the buffer always keeps some bytes before a candidate.
fn joins_nothing(text: &str, at: usize) -> bool {
    use unicode_segmentation::GraphemeCursor;
    GraphemeCursor::new(at, text.len(), true)
        .is_boundary(text, 0)
        .unwrap_or(true)
}

/// True when a match may start or end at `at` in a whole text: it is a character edge, and it
/// is not inside a CRLF, a base with its marks, an emoji sequence or a flag.
pub fn is_cluster_edge(text: &str, at: usize) -> bool {
    FlagRun::new(false).is_edge(text, at)
}

/// True when an odd number of flag letters stands just before `at` in the snapshot. It runs
/// once for each scan, and it reads only as far back as the run goes.
pub(crate) fn flags_odd_before(
    snapshot: &dyn TextSnapshot,
    at: usize,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let mut high = at;
    let mut count = 0_usize;
    let mut step = 256;
    let mut text = String::new();
    while high > 0 && !cancel.load(Ordering::Acquire) {
        let low = align_down(snapshot, high.saturating_sub(step));
        text.clear();
        if !snapshot.visit_chunks(ByteOffset(low)..ByteOffset(high), &mut |part| {
            text.push_str(part)
        }) {
            return Err(read_failure(snapshot));
        }
        let run = text.chars().rev().take_while(|c| is_regional(*c)).count();
        count += run;
        if run * 4 < text.len() || low == 0 {
            break;
        }
        high = low;
        step = (step * 2).min(64 * 1024);
    }
    Ok(count % 2 == 1)
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

/// The nearest character edge at or after `at`, within what the snapshot can read.
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
