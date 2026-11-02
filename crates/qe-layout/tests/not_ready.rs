//! A store can say "not in memory yet" where a slower one would wait. `byte_to_utf16`
//! gives `None` for that, and also for a byte inside a character, so the segment
//! code must tell the two apart. These tests use a scripted store, so the order of
//! events is the same on every run.
use qe_api::{ByteOffset, LineIndex, LookupError, TextSnapshot, Utf16Offset};
use qe_layout::{DirectWriteLayout, LayoutError};
use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One line of text. Bytes in `missing` are not in memory. The missing part arrives
/// after `arrives_after` plain lookups, so a test can place the arrival exactly.
struct Scripted {
    text: String,
    missing: Mutex<Range<usize>>,
    arrives_after: usize,
    lookups: AtomicUsize,
    /// A store that claims every byte here is inside a character.
    denied: Range<usize>,
    denied_lookups: AtomicUsize,
    /// How many times the `try_` form says "not yet" for a byte inside a character, as
    /// it can when other threads push the page out right after the plain query.
    pushed_out: AtomicUsize,
}

impl Scripted {
    fn new(text: String, missing: Range<usize>, arrives_after: usize) -> Self {
        Self {
            text,
            missing: Mutex::new(missing),
            arrives_after,
            lookups: AtomicUsize::new(0),
            denied: 0..0,
            denied_lookups: AtomicUsize::new(0),
            pushed_out: AtomicUsize::new(0),
        }
    }
    fn known(&self, range: Range<usize>) -> bool {
        let missing = self.missing.lock().unwrap();
        // A lookup at a byte needs the byte before it and the byte itself.
        range.end <= missing.start || range.start >= missing.end
    }
    fn answer(&self, byte: ByteOffset) -> Result<Utf16Offset, LookupError> {
        if self.denied.contains(&byte.0) {
            return Err(LookupError::OutOfRange);
        }
        if !self.known(byte.0..byte.0 + 1) {
            return Err(LookupError::NotYetKnown {
                indexed_up_to: ByteOffset(0),
            });
        }
        self.text
            .get(..byte.0)
            .map(|s| Utf16Offset(s.encode_utf16().count()))
            .ok_or(LookupError::OutOfRange)
    }
    fn lookups(&self) -> usize {
        self.lookups.load(Ordering::Relaxed)
    }
}

impl TextSnapshot for Scripted {
    fn revision(&self) -> u64 {
        1
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.text.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.text.encode_utf16().count())
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        let seen = self.lookups.fetch_add(1, Ordering::Relaxed) + 1;
        if self.denied.contains(&byte.0) {
            self.denied_lookups.fetch_add(1, Ordering::Relaxed);
        }
        if seen >= self.arrives_after {
            *self.missing.lock().unwrap() = 0..0;
        }
        self.answer(byte).ok()
    }
    fn utf16_to_byte(&self, _: Utf16Offset) -> Option<ByteOffset> {
        None
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<qe_api::TextPosition> {
        Some(qe_api::TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(byte.0),
        })
    }
    fn position_to_byte(&self, _: qe_api::TextPosition) -> Option<ByteOffset> {
        None
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 == 0).then_some(ByteOffset(0)..ByteOffset(self.text.len()))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        if !self.known(range.start.0..range.end.0) {
            return false;
        }
        match self.text.get(range.start.0..range.end.0) {
            Some(part) => {
                visitor(part);
                true
            }
            None => false,
        }
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn try_byte_to_utf16(&self, byte: ByteOffset) -> Result<Utf16Offset, LookupError> {
        if !self.text.is_char_boundary(byte.0)
            && self
                .pushed_out
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_ok()
        {
            return Err(LookupError::NotYetKnown {
                indexed_up_to: ByteOffset(0),
            });
        }
        self.answer(byte)
    }
}

const NEVER: usize = usize::MAX;

/// The edge of the first segment is near 2048 bytes in, and it looks at bytes from
/// 256 before that to 1024 after.
fn all_of_the_edge_window() -> Range<usize> {
    1500..4000
}

fn truth(text: &str, index: usize) -> Range<ByteOffset> {
    let engine = DirectWriteLayout::new().unwrap();
    let whole = Scripted::new(text.to_string(), 0..0, NEVER);
    engine.segment_range(&whole, LineIndex(0), index).unwrap()
}

#[test]
fn a_window_not_in_memory_is_an_error_and_not_an_overflow() {
    let text = "word ".repeat(14_000);
    let engine = DirectWriteLayout::new().unwrap();
    let store = Scripted::new(text, all_of_the_edge_window(), NEVER);
    assert!(matches!(
        engine.segment_range(&store, LineIndex(0), 1),
        Err(LayoutError::BadRange(_))
    ));
}

/// The search for an edge stops at the longest a UTF-8 character can be, even when a
/// store denies each place. Past that it is a store in trouble, and the search must stay
/// bounded on a line of any length.
#[test]
fn a_search_for_a_character_edge_looks_at_four_places() {
    let engine = DirectWriteLayout::new().unwrap();
    let mut store = Scripted::new("word ".repeat(14_000), 0..0, NEVER);
    store.denied = 1500..60_000;
    assert!(engine.segment_range(&store, LineIndex(0), 1).is_err());
    assert_eq!(store.denied_lookups.load(Ordering::Relaxed), 4);
}

/// On a worker's snapshot the plain query waits for the page, so "not in memory" must not
/// be an error there. A line that fails this way stays failed until its list is cleared.
#[test]
fn a_store_whose_plain_query_waits_always_gets_an_answer() {
    let text = "word ".repeat(14_000);
    let engine = DirectWriteLayout::new().unwrap();
    for index in 0..3 {
        // The page arrives on the first plain query, as it would for a store that waits.
        let store = Scripted::new(text.clone(), all_of_the_edge_window(), 1);
        assert_eq!(
            engine.segment_range(&store, LineIndex(0), index).unwrap(),
            truth(&text, index)
        );
    }
}

/// The plain query says "inside a character", then other threads push the page out, and
/// the `try_` form says "not yet". One round of that must not fail the line.
#[test]
fn a_page_pushed_out_between_the_two_queries_is_not_an_error() {
    let text = "語 x".repeat(14_000);
    let engine = DirectWriteLayout::new().unwrap();
    for index in 1..4 {
        let store = Scripted::new(text.clone(), 0..0, NEVER);
        store.pushed_out.store(1, Ordering::Relaxed);
        assert_eq!(
            engine.segment_range(&store, LineIndex(0), index).unwrap(),
            truth(&text, index)
        );
    }
}

#[test]
fn a_page_that_never_arrives_costs_a_few_lookups_whatever_the_line_length() {
    let engine = DirectWriteLayout::new().unwrap();
    for bytes in [70_000, 7_000_000] {
        let store = Scripted::new("word ".repeat(bytes / 5), all_of_the_edge_window(), NEVER);
        assert!(engine.segment_range(&store, LineIndex(0), 1).is_err());
        assert!(
            store.lookups() < 16,
            "{} lookups on a line of {bytes} bytes",
            store.lookups()
        );
    }
}

/// A page can arrive between any two lookups. The answer is then either "not yet" or
/// the true edges, and never edges that depend on when it came.
#[test]
fn a_page_that_arrives_during_the_search_never_moves_an_edge() {
    let engine = DirectWriteLayout::new().unwrap();
    for text in ["word ".repeat(14_000), "語 x".repeat(14_000)] {
        for index in 0..3 {
            let expected = truth(&text, index);
            for arrival in 1..600 {
                let store = Scripted::new(text.clone(), all_of_the_edge_window(), arrival);
                match engine.segment_range(&store, LineIndex(0), index) {
                    Ok(range) => assert_eq!(range, expected, "arrival {arrival}, index {index}"),
                    Err(LayoutError::BadRange(_)) => {}
                    Err(e) => panic!("{e}"),
                }
            }
        }
    }
}

/// A window can sit in memory while the start of the run of flags before it does not.
/// Then nobody knows how the flags pair up, and a guess cuts a flag in two.
#[test]
fn a_flag_run_whose_start_is_not_in_memory_is_not_a_guess() {
    let flag = "\u{1F1EB}\u{1F1F7}";
    // Five letters first. The first character edge at or after 2048 - 256 then falls
    // in the middle of a flag, and only the look-back can tell.
    let text = format!("aaaaa{}", flag.repeat(6000));
    let engine = DirectWriteLayout::new().unwrap();
    let expected = truth(&text, 1);
    // The window starts about 256 bytes before the edge near 2048, so this is before it.
    let store = Scripted::new(text, 1..1000, NEVER);
    let got = engine.segment_range(&store, LineIndex(0), 1);
    assert!(
        matches!(got, Err(LayoutError::BadRange(_))),
        "{got:?}, truth {expected:?}"
    );
}

/// The line ending is 3 bytes here. Its page arrives after the search for an edge has
/// taken its first byte for the inside of a character, which used to carry the search
/// to the end of the line and count the line ending as text.
#[test]
fn a_page_that_arrives_during_the_line_ending_search_keeps_the_ending_out() {
    let text = format!("{}\u{2028}", "word ".repeat(14_000));
    let engine = DirectWriteLayout::new().unwrap();
    let whole = Scripted::new(text.clone(), 0..0, NEVER);
    let last = engine.segment_count(&whole, LineIndex(0)).unwrap() - 1;
    let expected = truth(&text, last);
    assert_eq!(expected.end.0, text.len() - 3);
    for arrival in 1..10 {
        let store = Scripted::new(text.clone(), text.len() - 3..text.len(), arrival);
        match engine.segment_range(&store, LineIndex(0), last) {
            Ok(range) => assert_eq!(range, expected, "arrival {arrival}"),
            Err(LayoutError::BadRange(_)) => {}
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_byte_inside_a_character_is_still_only_a_step_to_the_next_edge() {
    // The nominal edge falls inside these characters at some offset.
    for pad in 0..4 {
        let text = format!("{}{}", "x".repeat(pad), "語".repeat(3000));
        let engine = DirectWriteLayout::new().unwrap();
        let store = Scripted::new(text.clone(), 0..0, NEVER);
        let range = engine.segment_range(&store, LineIndex(0), 1).unwrap();
        assert!(text.is_char_boundary(range.start.0));
        assert!(text.is_char_boundary(range.end.0));
        assert!(range.start.0 >= 2048 && range.start.0 < 2048 + 4);
    }
}
