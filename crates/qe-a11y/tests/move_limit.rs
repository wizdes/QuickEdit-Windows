//! The limit on moves, with the real limit. This program has no test window, because a test
//! window widens the limit for tests that check exact counts.
use qe_a11y::{
    TextRange,
    text::{Answer, Mover, TextDoc, Unit},
};
use qe_api::{TextSnapshot, testing::StringStore};
use std::{ops::Range, time::Duration};

/// Text whose word lookup is slow, as a page of a mapped file can be.
struct SlowWords {
    text: StringStore,
    value: String,
    pause: Duration,
}
impl SlowWords {
    fn new(value: String, pause: Duration) -> Self {
        Self {
            text: StringStore::new(value.clone()),
            value,
            pause,
        }
    }
}
impl TextDoc for SlowWords {
    fn snapshot(&self) -> &dyn TextSnapshot {
        &self.text
    }
    fn word_at(&mut self, byte: usize) -> Answer<Range<usize>> {
        std::thread::sleep(self.pause);
        let bytes = self.value.as_bytes();
        let blank = |b: u8| b == b' ';
        let at = byte.min(bytes.len().saturating_sub(1));
        let (mut start, mut end) = (at, at + 1);
        while start > 0 && blank(bytes[start - 1]) == blank(bytes[at]) {
            start -= 1;
        }
        while end < bytes.len() && blank(bytes[end]) == blank(bytes[at]) {
            end += 1;
        }
        Ok(start..end)
    }
    fn row_stops(&mut self, _: usize) -> Vec<usize> {
        Vec::new()
    }
}

/// `count` words of two letters and a blank, so word number `n` starts at `n * 3`.
fn words(count: usize) -> String {
    "ab ".repeat(count)
}

#[test]
fn a_word_move_stops_at_the_limit_and_returns_the_count_it_reached() {
    let mut doc = SlowWords::new(words(2000), Duration::from_millis(2));
    let mut mover = Mover { doc: &mut doc };
    let (range, moved) = mover
        .move_range(TextRange::new(0, 0), Unit::Word, 1000)
        .unwrap();
    assert!(moved > 0 && moved < 64, "moved {moved}");
    assert_eq!(
        range,
        TextRange::new(moved as usize * 3, moved as usize * 3)
    );

    let (range, moved) = mover
        .move_range(TextRange::new(3000, 3000), Unit::Word, -1000)
        .unwrap();
    assert!(moved < 0 && moved > -64, "moved {moved}");
    let at = 3000 - moved.unsigned_abs() as usize * 3;
    assert_eq!(range, TextRange::new(at, at));

    let (range, moved) = mover
        .move_range(TextRange::new(0, 3), Unit::Word, 1000)
        .unwrap();
    assert!(moved > 0 && moved < 64, "moved {moved}");
    let at = moved as usize * 3;
    assert_eq!(range, TextRange::new(at, at + 3), "still one word long");

    let (range, moved) = mover
        .move_endpoint(TextRange::new(0, 0), false, Unit::Word, 1000)
        .unwrap();
    assert!(moved > 0 && moved < 64, "moved {moved}");
    assert_eq!(range, TextRange::new(0, moved as usize * 3));
}

#[test]
fn a_move_makes_one_step_even_when_the_limit_has_already_passed() {
    let mut doc = SlowWords::new(words(5), Duration::from_millis(20));
    let mut mover = Mover { doc: &mut doc };
    // Finding the start of the word already takes longer than the limit.
    let (range, moved) = mover
        .move_range(TextRange::new(3, 6), Unit::Word, 1)
        .unwrap();
    assert_eq!((range, moved), (TextRange::new(6, 9), 1));
}
