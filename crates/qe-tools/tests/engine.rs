#![allow(clippy::single_range_in_vec_init)]
//! The find engine alone, on a store whose pieces and pace a test chooses, and on a real
//! mapped file.
use qe_api::{ByteOffset, LineIndex, StoreStatus, TextPosition, TextSnapshot, Utf16Offset};
use qe_editor::{EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
use qe_tools::{
    Direction, FindJob, FindOptions, JobState, MatchKind, Matcher, QUEUE_LIMIT, ScanEnd, find_step,
    find_text, first_match, last_match, scan_forward,
};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A store that gives its text in pieces of a chosen size, counts what it gives, and can
/// stop at a byte until a test lets it go on.
struct Pieces {
    text: String,
    piece: usize,
    given: AtomicUsize,
    biggest: AtomicUsize,
    hold_at: Option<usize>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    status_incomplete: bool,
}
impl Pieces {
    fn new(text: &str, piece: usize) -> Self {
        Self {
            text: text.to_string(),
            piece,
            given: AtomicUsize::new(0),
            biggest: AtomicUsize::new(0),
            hold_at: None,
            gate: Arc::new((Mutex::new(false), Condvar::new())),
            status_incomplete: false,
        }
    }
    fn open_gate(gate: &(Mutex<bool>, Condvar)) {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }
}
impl TextSnapshot for Pieces {
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
    fn byte_to_utf16(&self, _: ByteOffset) -> Option<Utf16Offset> {
        None
    }
    fn utf16_to_byte(&self, _: Utf16Offset) -> Option<ByteOffset> {
        None
    }
    fn byte_to_position(&self, _: ByteOffset) -> Option<TextPosition> {
        None
    }
    fn position_to_byte(&self, _: TextPosition) -> Option<ByteOffset> {
        None
    }
    fn line_range(&self, _: LineIndex) -> Option<Range<ByteOffset>> {
        None
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        StoreStatus {
            known_bytes: ByteOffset(if self.status_incomplete {
                0
            } else {
                self.text.len()
            }),
            known_utf16: Utf16Offset(0),
            known_lines: 1,
            complete: !self.status_incomplete,
            failure: None,
        }
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        self.visit_chunks_while(range, &mut |s| {
            f(s);
            true
        })
    }
    fn visit_chunks_while(
        &self,
        range: Range<ByteOffset>,
        f: &mut dyn FnMut(&str) -> bool,
    ) -> bool {
        let (start, end) = (range.start.0, range.end.0);
        if self.text.get(start..end).is_none() {
            return false;
        }
        let mut at = start;
        while at < end {
            if let Some(hold) = self.hold_at
                && at >= hold
            {
                let (lock, wait) = &*self.gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wait.wait(open).unwrap();
                }
            }
            let mut stop = (at + self.piece).min(end);
            while !self.text.is_char_boundary(stop) {
                stop += 1;
            }
            let part = &self.text[at..stop];
            self.given.fetch_add(part.len(), Ordering::Relaxed);
            self.biggest.fetch_max(part.len(), Ordering::Relaxed);
            if !f(part) {
                return true;
            }
            at = stop;
        }
        true
    }
}

static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

fn options(ignore_case: bool, kind: MatchKind) -> FindOptions {
    FindOptions {
        ignore_case,
        wrap: true,
        kind,
    }
}
fn scan_all(snapshot: &dyn TextSnapshot, query: &str, o: FindOptions) -> Vec<Range<usize>> {
    let matcher = Matcher::new(query, o).unwrap();
    let mut out = Vec::new();
    let end = scan_forward(
        snapshot,
        &matcher,
        ByteOffset(0)..snapshot.len_bytes(),
        &NOT_CANCELLED,
        &mut |m| {
            out.push(m.start.0..m.end.0);
            true
        },
    );
    assert_eq!(end, ScanEnd::Finished);
    out
}

#[test]
fn case_rules_table() {
    let any = FindOptions::default();
    let exact = options(false, MatchKind::Contains);
    let cases: &[(&str, &str, FindOptions, &[Range<usize>])] = &[
        ("1 Fox 2 fox 3 FOX", "fox", any, &[2..5, 8..11, 14..17]),
        ("1 Fox 2 fox 3 FOX", "fox", exact, &[8..11]),
        ("aaa", "aa", any, &[0..2]),
        ("", "a", any, &[]),
        // Full folding: one `ß` is two letters, and half of it never matches.
        ("Stra\u{df}e", "STRASSE", any, &[0..7]),
        ("STRASSE", "stra\u{df}e", any, &[0..7]),
        ("\u{df}", "s", any, &[]),
        ("\u{1e9e}", "ss", any, &[0..3]),
        // Sigma in all three forms.
        ("\u{3a3}\u{3c3}\u{3c2}", "\u{3c3}", any, &[0..2, 2..4, 4..6]),
        // Kelvin sign and ligature.
        ("\u{212a}", "k", any, &[0..3]),
        ("\u{fb01}x", "FI", any, &[0..3]),
        // Turkish letters: no locale. Dotless i is its own letter, and dotted capital I
        // folds to i with a dot, which is not plain i.
        ("\u{131}", "i", any, &[]),
        ("\u{130}", "i\u{307}", any, &[0..2]),
        ("\u{130}", "i", any, &[]),
    ];
    for (text, query, o, want) in cases {
        assert_eq!(&find_text(text, query, *o), want, "{query:?} in {text:?}");
    }
}

#[test]
fn matches_do_not_split_composed_characters() {
    let any = FindOptions::default();
    // A base with its mark, and emoji sequences, stay whole.
    assert_eq!(find_text("e\u{301}", "e", any), Vec::<Range<usize>>::new());
    assert_eq!(find_text("e\u{301} e", "e", any), vec![4..5]);
    assert_eq!(find_text("e\u{301}x", "e\u{301}", any), vec![0..3]);
    let family = "\u{1f468}\u{200d}\u{1f469}";
    assert!(find_text(family, "\u{1f468}", any).is_empty());
    assert!(find_text(family, "\u{1f469}", any).is_empty());
    assert_eq!(find_text("\u{1f44d}\u{1f3fd}", "\u{1f44d}", any).len(), 0);
    // A CRLF is one character, so LF alone does not match inside it.
    assert!(find_text("a\r\nb", "\n", any).is_empty());
    assert_eq!(find_text("a\r\nb", "\r\n", any), vec![1..3]);
    // Flags are pairs: US then GB, and the middle pair is not a flag.
    let flags = "\u{1f1fa}\u{1f1f8}\u{1f1ec}\u{1f1e7}";
    assert_eq!(
        find_text(flags, "\u{1f1f8}\u{1f1ec}", any),
        Vec::<Range<usize>>::new()
    );
    assert_eq!(find_text(flags, "\u{1f1ec}\u{1f1e7}", any), vec![8..16]);
}

#[test]
fn match_kinds_table() {
    let text = "cat catalog bobcat _cat cat_ cat. \u{e9}cat";
    let at = |kind| find_text(text, "cat", options(true, kind));
    let word = |r: &Range<usize>| r.start;
    assert_eq!(at(MatchKind::Contains).len(), 7);
    assert_eq!(
        at(MatchKind::FullWord).iter().map(word).collect::<Vec<_>>(),
        vec![0, 29]
    );
    assert_eq!(
        at(MatchKind::StartsWith)
            .iter()
            .map(word)
            .collect::<Vec<_>>(),
        vec![0, 4, 24, 29]
    );
    assert_eq!(
        at(MatchKind::EndsWith).iter().map(word).collect::<Vec<_>>(),
        vec![0, 15, 20, 29, 36]
    );
}

/// One piece of the seeded text: one whole character as a reader sees it, and what it folds
/// to, both written here by hand. So the model below uses no rule from the crate.
const TOKENS: &[(&str, &str)] = &[
    ("a", "a"),
    ("A", "a"),
    ("b", "b"),
    ("s", "s"),
    ("S", "s"),
    ("\u{df}", "ss"),
    ("\u{1e9e}", "ss"),
    ("k", "k"),
    ("\u{212a}", "k"),
    ("\u{3a3}", "\u{3c3}"),
    ("\u{3c3}", "\u{3c3}"),
    ("\u{3c2}", "\u{3c3}"),
    ("\u{131}", "\u{131}"),
    ("e", "e"),
    ("i", "i"),
    ("I", "i"),
    ("e\u{301}", "e\u{301}"),
    ("\u{1f600}", "\u{1f600}"),
    ("\u{1f468}\u{200d}\u{1f469}", "\u{1f468}\u{200d}\u{1f469}"),
    ("\u{1f1fa}\u{1f1f8}", "\u{1f1fa}\u{1f1f8}"),
    ("\r\n", "\r\n"),
    (" ", " "),
    ("\n", "\n"),
    ("\u{4e2d}", "\u{4e2d}"),
];

/// Leftmost matches with no overlap, on whole tokens only: the tokens from a start must spell
/// the query, and the last token must end with it. `folded` compares what they fold to.
fn token_model(tokens: &[usize], query: &str, folded: bool) -> Vec<Range<usize>> {
    let spell = |t: usize| if folded { TOKENS[t].1 } else { TOKENS[t].0 };
    let mut starts = Vec::with_capacity(tokens.len() + 1);
    let mut at = 0;
    for &t in tokens {
        starts.push(at);
        at += TOKENS[t].0.len();
    }
    starts.push(at);
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let mut left = query;
        let mut j = i;
        while !left.is_empty() && j < tokens.len() {
            match left.strip_prefix(spell(tokens[j])) {
                Some(rest) => {
                    left = rest;
                    j += 1;
                }
                None => break,
            }
        }
        if left.is_empty() {
            out.push(starts[i]..starts[j]);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn seeded_search_matches_an_independent_model_past_the_buffer_turnover() {
    let mut seed = 0x9e37_79b9_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    let exact_queries = [
        "a",
        "b",
        "ab",
        "e\u{301}",
        "\u{1f600}",
        "\r\n",
        "\u{1f1fa}\u{1f1f8}",
        "\u{4e2d}a",
    ];
    // Each is what it folds to, as the model compares.
    let folded_queries = [
        "a", "ss", "k", "\u{3c3}", "as", "\u{131}", "i", "\r\n", "e\u{301}",
    ];
    let mut compared = 0;
    let mut matches = 0;
    let mut longest = 0;
    for round in 0..4 {
        // Over 200 KB, so the 64 KiB buffer turns over a few times.
        let mut tokens = Vec::new();
        let mut text = String::new();
        while text.len() < 200_000 {
            let t = next() % TOKENS.len();
            tokens.push(t);
            text.push_str(TOKENS[t].0);
        }
        longest = longest.max(text.len());
        let piece = [3, 4096, 65_536, 7][round % 4];
        let store = Pieces::new(&text, piece);
        for query in exact_queries {
            let want = token_model(&tokens, query, false);
            let exact = options(false, MatchKind::Contains);
            matches += want.len();
            assert_eq!(
                scan_all(&store, query, exact),
                want,
                "{query:?}, piece {piece}"
            );
            assert_eq!(
                find_text(&text, query, exact),
                want,
                "{query:?}, whole string"
            );
            compared += 2;
        }
        for query in folded_queries {
            let want = token_model(&tokens, query, true);
            let any = FindOptions::default();
            matches += want.len();
            assert_eq!(
                scan_all(&store, query, any),
                want,
                "{query:?}, piece {piece}"
            );
            compared += 1;
        }
    }
    assert!(longest > 2 * 64 * 1024);
    assert!(
        compared >= 90 && matches > 10_000,
        "{compared} compared, {matches} matches"
    );
}

// ---- Flags, which are pairs, across every kind of edge ----

fn flags_text(prefix: &str, pairs: usize) -> String {
    format!("{prefix}{}", "\u{1f1f8}\u{1f1fa}".repeat(pairs))
}

#[test]
fn flags_at_a_buffer_edge_give_no_false_match_and_replace_all_leaves_them() {
    // The first letter of this query's pair is the second letter of each flag.
    let query = "\u{1f1fa}\u{1f1f8}";
    let exact = options(false, MatchKind::Contains);
    for prefix in ["", "\u{1f600}", "x", "xy", "xyz"] {
        let text = flags_text(prefix, 12_000);
        for piece in [3, 4096, 65_536] {
            let store = Pieces::new(&text, piece);
            assert!(
                scan_all(&store, query, exact).is_empty(),
                "{prefix:?} piece {piece}"
            );
        }
        let view = EditorView::from_text(&text).unwrap();
        let snapshot = view.worker_snapshot();
        assert!(scan_all(snapshot.as_ref(), query, exact).is_empty());
        let matcher = Matcher::new(query, exact).unwrap();
        let ready = qe_tools::prepare_replace_all(snapshot.as_ref(), &matcher, "X", &NOT_CANCELLED);
        assert!(
            ready.unwrap().is_none(),
            "Replace All finds nothing to change"
        );
        let back = last_match(
            snapshot.as_ref(),
            &matcher,
            ByteOffset(0),
            snapshot.len_bytes(),
            &NOT_CANCELLED,
        );
        assert_eq!(back.unwrap(), None, "backward agrees");
    }
}

#[test]
fn a_real_flag_past_a_buffer_edge_is_found_and_a_scan_from_inside_a_run_counts_right() {
    let exact = options(false, MatchKind::Contains);
    let us = "\u{1f1fa}\u{1f1f8}";
    // 65,000 plain bytes, flags past the 64 KiB edge, then one rare US flag.
    let text = format!(
        "{}{}{us}",
        "x".repeat(65_000),
        "\u{1f1ec}\u{1f1e7}".repeat(150)
    );
    let place = text.len() - us.len();
    let store = Pieces::new(&text, 32_769);
    assert_eq!(scan_all(&store, us, exact), vec![place..place + us.len()]);

    // One flag letter later, the same letters are not a flag.
    let text = format!(
        "{}\u{1f1ec}{}{us}",
        "x".repeat(65_000),
        "\u{1f1ec}\u{1f1e7}".repeat(150)
    );
    let store = Pieces::new(&text, 32_769);
    assert!(scan_all(&store, us, exact).is_empty());

    // A scan that starts deep inside a run reads back to learn where the pairs fall.
    let text = "\u{1f1f8}\u{1f1fa}".repeat(1000);
    let store = Pieces::new(&text, 4096);
    let matcher = Matcher::new("\u{1f1f8}\u{1f1fa}", exact).unwrap();
    let from = ByteOffset(4 * 1000); // on a pair's first letter
    let found = first_match(
        &store,
        &matcher,
        from..ByteOffset(text.len()),
        &NOT_CANCELLED,
    );
    assert_eq!(found.unwrap().unwrap().start, from);
    let matcher = Matcher::new(us, exact).unwrap();
    let found = first_match(
        &store,
        &matcher,
        ByteOffset(4 * 1001)..ByteOffset(text.len()),
        &NOT_CANCELLED,
    );
    assert_eq!(found.unwrap(), None, "from a pair's second letter");
}

#[test]
fn a_long_run_of_flags_is_searched_in_time_that_follows_its_length() {
    // Walking the whole run for each candidate took seconds on 96 KB. 2 MB here would take
    // far longer than this limit, even on a loaded PC in a debug build.
    let text = "\u{1f1f8}\u{1f1fa}".repeat(250_000);
    let store = Pieces::new(&text, 64 * 1024);
    let started = Instant::now();
    assert!(
        scan_all(
            &store,
            "\u{1f1fa}\u{1f1f8}",
            options(false, MatchKind::Contains)
        )
        .is_empty()
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

// ---- Wrap around, with the caret inside the only match ----

#[test]
fn wrap_finds_the_only_match_with_the_caret_inside_it() {
    let store = Pieces::new("xneedley", 3);
    let matcher = Matcher::new("needle", FindOptions::default()).unwrap();
    let at = ByteOffset(3)..ByteOffset(3);
    for direction in [Direction::Forward, Direction::Backward] {
        let found = find_step(
            &store,
            &matcher,
            at.clone(),
            direction,
            true,
            &NOT_CANCELLED,
        );
        let (found, wrapped) = found.unwrap().unwrap();
        assert_eq!(found, ByteOffset(1)..ByteOffset(7), "{direction:?}");
        assert!(wrapped);
    }
}

// ---- The bytes kept past a candidate ----

#[test]
fn a_mark_right_after_a_match_at_every_offset_near_the_buffer_edge_is_seen() {
    // Each offset, so that one match ends right where the 64 KiB buffer ends.
    let exact = options(false, MatchKind::Contains);
    for offset in 65_520..65_545 {
        let text = format!("{}e\u{301}{}", "z".repeat(offset), "y".repeat(70_000));
        for piece in [4096, 65_536] {
            let store = Pieces::new(&text, piece);
            assert!(
                scan_all(&store, "e", exact).is_empty(),
                "offset {offset}, piece {piece}"
            );
        }
    }
    let word = options(false, MatchKind::FullWord);
    for offset in 65_520..65_545 {
        let text = format!("{}cats{}", " ".repeat(offset), " ".repeat(70_000));
        let store = Pieces::new(&text, 4096);
        assert!(scan_all(&store, "cat", word).is_empty(), "offset {offset}");
    }
}

#[test]
fn what_stands_before_a_match_is_kept_when_the_buffer_turns_over() {
    // The buffer keeps some bytes back and drops those before them, so each offset near that
    // place has a match whose left neighbor must still be read.
    let word = options(false, MatchKind::FullWord);
    let exact = options(false, MatchKind::Contains);
    for offset in 65_420..65_540 {
        let text = format!("{}xcat{}", " ".repeat(offset), " ".repeat(70_000));
        let store = Pieces::new(&text, 4096);
        assert!(scan_all(&store, "cat", word).is_empty(), "offset {offset}");
        let text = format!("{}\r\n{}", " ".repeat(offset), " ".repeat(70_000));
        let store = Pieces::new(&text, 4096);
        assert!(scan_all(&store, "\n", exact).is_empty(), "offset {offset}");
    }
}

#[test]
fn a_match_across_a_piece_edge_and_a_block_edge_is_found() {
    // The scan's own buffer turns over at 64 KiB, so put matches on each side of that edge,
    // at each byte offset around it, with store pieces that split the match too.
    for offset in 65_400..65_600 {
        let mut text = "x".repeat(offset);
        text.push_str("needle");
        text.push_str(&"y".repeat(70_000));
        text.push_str("NEEDLE");
        let second = text.len() - 6;
        for piece in [3, 4096, 65_536] {
            let store = Pieces::new(&text, piece);
            assert_eq!(
                scan_all(&store, "needle", FindOptions::default()),
                vec![offset..offset + 6, second..second + 6],
                "offset {offset}, piece {piece}"
            );
        }
    }
}

#[test]
fn context_at_a_block_edge_still_keeps_the_rules() {
    // A mark right after the piece that ends a match, and a word letter right before it.
    for piece in [1, 2, 3, 4096] {
        let mut text = "z".repeat(70_000);
        text.push_str(" e\u{301} ecat");
        let store = Pieces::new(&text, piece);
        let exact = options(false, MatchKind::Contains);
        assert_eq!(scan_all(&store, "e", exact).len(), 1, "the one in ecat");
        let word = options(false, MatchKind::FullWord);
        assert!(scan_all(&store, "cat", word).is_empty());
    }
}

#[test]
fn the_scan_holds_a_bounded_buffer_on_big_text() {
    // 64 MiB in pieces of 64 KiB. Memory is measured in release elsewhere. Here the store
    // proves that no piece is bigger than asked and that all of the text went through.
    let text = "lorem ipsum dolor sit amet\n".repeat(2_500_000);
    let store = Pieces::new(&text, 64 * 1024);
    let found = scan_all(&store, "AMET", FindOptions::default());
    assert_eq!(found.len(), 2_500_000);
    assert!(store.biggest.load(Ordering::Relaxed) <= 64 * 1024 + 3);
}

#[test]
fn step_forward_wraps_and_backward_wraps() {
    let text = "1 dog 2 dog 3 dog";
    let store = Pieces::new(text, 4);
    let matcher = Matcher::new("dog", FindOptions::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let step = |from: Range<usize>, direction, wrap| {
        find_step(
            &store,
            &matcher,
            ByteOffset(from.start)..ByteOffset(from.end),
            direction,
            wrap,
            &cancel,
        )
        .unwrap()
        .map(|(m, wrapped)| (m.start.0, wrapped))
    };
    assert_eq!(step(2..5, Direction::Forward, true), Some((8, false)));
    assert_eq!(step(14..17, Direction::Forward, true), Some((2, true)));
    assert_eq!(step(14..17, Direction::Forward, false), None);
    assert_eq!(step(14..17, Direction::Backward, true), Some((8, false)));
    assert_eq!(step(2..5, Direction::Backward, true), Some((14, true)));
    assert_eq!(step(2..5, Direction::Backward, false), None);
}

#[test]
fn backward_tries_each_start_and_crosses_block_edges() {
    let cancel = AtomicBool::new(false);
    let store = Pieces::new("aaa", 1);
    let matcher = Matcher::new("aa", FindOptions::default()).unwrap();
    let found = last_match(&store, &matcher, ByteOffset(0), ByteOffset(3), &cancel).unwrap();
    assert_eq!(found, Some(ByteOffset(1)..ByteOffset(3)));
    // A match sits across each possible edge of the 256 KiB backward block.
    let block = 256 * 1024;
    let needle = Matcher::new("needle", FindOptions::default()).unwrap();
    let tail = 1000;
    for shift in [0, 1, 3, 5, 6, 63, 64, 65, 70, 100] {
        let place = 2 * block - tail - shift;
        let mut text = "x".repeat(place);
        text.push_str("needle");
        text.push_str(&"y".repeat(2 * block));
        let store = Pieces::new(&text, 4096);
        let got = last_match(
            &store,
            &needle,
            ByteOffset(0),
            ByteOffset(text.len()),
            &cancel,
        )
        .unwrap();
        assert_eq!(
            got,
            Some(ByteOffset(place)..ByteOffset(place + 6)),
            "shift {shift}"
        );
    }
    // A match past the limit does not count.
    let text = "ab ab";
    let store = Pieces::new(text, 2);
    let ab = Matcher::new("ab", FindOptions::default()).unwrap();
    assert_eq!(
        last_match(&store, &ab, ByteOffset(0), ByteOffset(4), &cancel).unwrap(),
        Some(ByteOffset(0)..ByteOffset(2))
    );
}

#[test]
fn seeded_backward_steps_walk_the_same_matches_as_forward() {
    let any = FindOptions::default();
    let cancel = AtomicBool::new(false);
    let mut text = String::new();
    let mut seed = 77_u64;
    for _ in 0..60_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        text.push_str(["word ", "x", "\u{4e2d}", "e\u{301} ", "\n"][(seed >> 40) as usize % 5]);
    }
    let store = Pieces::new(&text, 4096);
    let forward = scan_all(&store, "word", any);
    assert!(forward.len() > 5000);
    let matcher = Matcher::new("word", any).unwrap();
    let mut end = text.len();
    let mut seen = Vec::new();
    while let Some(found) =
        last_match(&store, &matcher, ByteOffset(0), ByteOffset(end), &cancel).unwrap()
    {
        seen.push(found.start.0..found.end.0);
        end = found.start.0;
    }
    seen.reverse();
    assert_eq!(seen, forward);
}

#[test]
fn first_match_stops_reading_early() {
    let text = format!("hit{}", "x".repeat(40 * 1024 * 1024));
    let store = Pieces::new(&text, 64 * 1024);
    let matcher = Matcher::new("hit", FindOptions::default()).unwrap();
    let found = first_match(
        &store,
        &matcher,
        ByteOffset(0)..store.len_bytes(),
        &NOT_CANCELLED,
    );
    assert_eq!(found.unwrap(), Some(ByteOffset(0)..ByteOffset(3)));
    assert!(
        store.given.load(Ordering::Relaxed) < 1024 * 1024,
        "it read {} bytes",
        store.given.load(Ordering::Relaxed)
    );
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "waited for {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn results_come_as_they_are_found_with_a_wake() {
    let mut text = "needle ".repeat(20);
    let first_part = text.len();
    text.push_str(&"x".repeat(200_000));
    text.push_str("needle");
    let mut store = Pieces::new(&text, 4096);
    // The worker stops at this byte, like a page still on its way from disk.
    store.hold_at = Some(100_000);
    let gate = store.gate.clone();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let matcher = Matcher::new("needle", FindOptions::default()).unwrap();
    let job = FindJob::start(
        Arc::new(store),
        matcher,
        ByteOffset(0)..ByteOffset(text.len()),
        QUEUE_LIMIT,
        Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
    );
    wait_until("the first wake", || wakes.load(Ordering::SeqCst) >= 1);
    wait_until("the first part's matches", || job.count() == 20);
    assert_eq!(job.state(), JobState::Running, "it is held in the middle");
    let early = job.drain();
    assert_eq!(early.len(), 20);
    assert!(early.iter().all(|m| m.end.0 <= first_part));
    Pieces::open_gate(&gate);
    wait_until("the end", || job.state() == JobState::Finished);
    assert_eq!(job.count(), 21);
    assert_eq!(job.drain().len(), 1);
    assert!(wakes.load(Ordering::SeqCst) >= 2, "the end wakes too");
}

#[test]
fn a_held_worker_cancels_and_a_new_query_starts_at_once() {
    let text = format!("{}needle", "x".repeat(200_000));
    let mut store = Pieces::new(&text, 4096);
    store.hold_at = Some(100_000);
    let gate = store.gate.clone();
    let store = Arc::new(store);
    let matcher = Matcher::new("needle", FindOptions::default()).unwrap();
    let old = FindJob::start(
        store.clone(),
        matcher.clone(),
        ByteOffset(0)..ByteOffset(text.len()),
        QUEUE_LIMIT,
        None,
    );
    // The old one is waiting for its page. The new one has its own thread, so it does not
    // wait for the old one. Cancelling marks the old one, and it stops at its next piece.
    let reached = store.clone();
    wait_until("the worker to reach its wait", || {
        reached.given.load(Ordering::Relaxed) >= 96_000
    });
    old.cancel();
    let other = Arc::new(Pieces::new("a needle", 4));
    let new = FindJob::start(
        other,
        matcher,
        ByteOffset(0)..ByteOffset(8),
        QUEUE_LIMIT,
        None,
    );
    wait_until("the new query", || new.state() == JobState::Finished);
    assert_eq!(new.count(), 1);
    assert_eq!(old.state(), JobState::Running);
    Pieces::open_gate(&gate);
    wait_until("the old query to stop", || {
        old.state() == JobState::Cancelled
    });
    assert_eq!(old.count(), 0, "it stopped before it reached the match");
}

#[test]
fn the_queue_is_bounded_and_the_count_is_not() {
    let text = "a".repeat(100_000);
    let job = FindJob::start(
        Arc::new(Pieces::new(&text, 4096)),
        Matcher::new("a", FindOptions::default()).unwrap(),
        ByteOffset(0)..ByteOffset(text.len()),
        1000,
        None,
    );
    wait_until("the end", || job.state() == JobState::Finished);
    assert_eq!(job.count(), 100_000);
    assert_eq!(job.dropped(), 99_000);
    assert_eq!(job.drain().len(), 1000);
}

#[test]
fn wakes_stay_few_when_there_are_many_matches() {
    let text = "a".repeat(4_000_000);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let job = FindJob::start(
        Arc::new(Pieces::new(&text, 4096)),
        Matcher::new("a", FindOptions::default()).unwrap(),
        ByteOffset(0)..ByteOffset(text.len()),
        0,
        Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
    );
    wait_until("the end", || job.state() == JobState::Finished);
    // Nobody drained, so only the first match and the end woke the host.
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert_eq!(job.count(), 4_000_000);
}

struct Failing;
impl TextSnapshot for Failing {
    fn revision(&self) -> u64 {
        1
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(100)
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(100)
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, _: ByteOffset) -> Option<Utf16Offset> {
        None
    }
    fn utf16_to_byte(&self, _: Utf16Offset) -> Option<ByteOffset> {
        None
    }
    fn byte_to_position(&self, _: ByteOffset) -> Option<TextPosition> {
        None
    }
    fn position_to_byte(&self, _: TextPosition) -> Option<ByteOffset> {
        None
    }
    fn line_range(&self, _: LineIndex) -> Option<Range<ByteOffset>> {
        None
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, _: &mut dyn FnMut(&str)) -> bool {
        range.start == range.end
    }
}

#[test]
fn a_read_failure_says_so() {
    let matcher = Matcher::new("x", FindOptions::default()).unwrap();
    let job = FindJob::start(
        Arc::new(Failing),
        matcher,
        ByteOffset(0)..ByteOffset(100),
        10,
        None,
    );
    wait_until("the end", || job.state() != JobState::Running);
    assert!(matches!(job.state(), JobState::Failed(m) if m.contains("could not be read")));
}

#[test]
fn an_empty_query_matches_nothing() {
    assert!(Matcher::new("", FindOptions::default()).is_none());
    assert!(find_text("abc", "", FindOptions::default()).is_empty());
}

pub fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("qe-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}
fn open_mapped(path: &std::path::Path) -> EditorView {
    let quiet: qe_tools::Wake = Arc::new(|| {});
    let options = LoadOptions {
        force_read_only: true,
        ..Default::default()
    };
    let mut load = FileLoad::start(path, options, quiet).unwrap();
    let start = Instant::now();
    let loaded = loop {
        if let Some(result) = load.poll() {
            break result;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "the load hung");
        std::thread::sleep(Duration::from_millis(1));
    };
    let LoadResult::Loaded(LoadedFile::ReadOnly(store)) = loaded else {
        panic!("the file must open read-only");
    };
    let mut view = EditorView::from_text("").unwrap();
    view.adopt(LoadedFile::ReadOnly(store)).unwrap();
    view
}

#[test]
fn a_mapped_file_is_searched_with_pieces_and_across_their_edges() {
    let path = temp_path("mapped.txt");
    let page = 64 * 1024;
    // The store's page is 64 KiB. Put one match across that edge, with multibyte text too.
    let mut text = "\u{4e2d}".repeat((page - 3) / 3);
    let edge = text.len();
    text.push_str("needle");
    text.push_str(&"plain line\r\n".repeat(30_000));
    text.push_str("NEEDLE at the end");
    std::fs::write(&path, &text).unwrap();
    let view = open_mapped(&path);
    // A snapshot reads only what is indexed, so wait for all of it. Searching while it grows has
    // its own tests.
    let start = Instant::now();
    while !view.store_status().complete {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the index did not end"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let snapshot = view.worker_snapshot();
    assert!(snapshot.is_read_only());
    let found = scan_all(snapshot.as_ref(), "needle", FindOptions::default());
    let last = text.len() - "NEEDLE at the end".len();
    assert_eq!(found, vec![edge..edge + 6, last..last + 6]);
    // A step back from the end finds the last, by the same reads.
    let matcher = Matcher::new("needle", FindOptions::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let back = last_match(
        snapshot.as_ref(),
        &matcher,
        ByteOffset(0),
        ByteOffset(last),
        &cancel,
    )
    .unwrap();
    assert_eq!(back, Some(ByteOffset(edge)..ByteOffset(edge + 6)));
    drop(snapshot);
    drop(view);
    std::fs::remove_file(&path).unwrap();
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

/// As a mapped snapshot does: it reads only up to `known`, and past that it gives nothing.
fn stop_failure() -> qe_api::StoreFailure {
    qe_api::StoreFailure {
        message: "a bad byte".to_string(),
        valid_up_to: ByteOffset(0),
    }
}
struct Pinned {
    text: Arc<String>,
    known: usize,
    stopped: bool,
}
impl TextSnapshot for Pinned {
    fn revision(&self) -> u64 {
        1
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.text.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(0)
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, _: ByteOffset) -> Option<Utf16Offset> {
        None
    }
    fn utf16_to_byte(&self, _: Utf16Offset) -> Option<ByteOffset> {
        None
    }
    fn byte_to_position(&self, _: ByteOffset) -> Option<TextPosition> {
        None
    }
    fn position_to_byte(&self, _: TextPosition) -> Option<ByteOffset> {
        None
    }
    fn line_range(&self, _: LineIndex) -> Option<Range<ByteOffset>> {
        None
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        StoreStatus {
            known_bytes: ByteOffset(self.known),
            known_utf16: Utf16Offset(0),
            known_lines: 1,
            complete: self.known == self.text.len(),
            failure: self.stopped.then(stop_failure),
        }
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        if range.end.0 > self.known {
            return false;
        }
        match self.text.get(range.start.0..range.end.0) {
            Some(part) => {
                f(part);
                true
            }
            None => false,
        }
    }
}

#[test]
fn a_search_goes_on_as_the_index_grows_and_finds_a_match_across_its_edge() {
    let mut text = "x".repeat(1000);
    let first = text.len();
    text.push_str("needle");
    text.push_str(&"y".repeat(1000));
    let second = text.len();
    text.push_str("needle");
    let text = Arc::new(text);
    let snap = |known: usize| -> Arc<dyn TextSnapshot> {
        Arc::new(Pinned {
            text: text.clone(),
            known,
            stopped: false,
        })
    };
    // The first index edge falls inside the first match.
    let job = FindJob::start(
        snap(first + 3),
        Matcher::new("needle", FindOptions::default()).unwrap(),
        ByteOffset(0)..ByteOffset(text.len()),
        QUEUE_LIMIT,
        None,
    );
    wait_until("the worker to ask for more", || {
        job.waiting_past().is_some()
    });
    assert_eq!(job.waiting_past(), Some(first as u64 + 3));
    assert_eq!(job.state(), JobState::Running, "no failure, and no end");
    assert_eq!(job.count(), 0);
    job.offer_snapshot(snap(second + 2));
    wait_until("the first match", || job.count() == 1);
    wait_until("it to ask again", || {
        job.waiting_past() == Some(second as u64 + 2)
    });
    job.offer_snapshot(snap(text.len()));
    wait_until("the end", || job.state() == JobState::Finished);
    assert_eq!(job.count(), 2);
    let found: Vec<usize> = job.drain().iter().map(|m| m.start.0).collect();
    assert_eq!(found, vec![first, second]);
}

#[test]
fn a_waiting_search_cancels_at_once() {
    let text = Arc::new("x".repeat(1000));
    let job = FindJob::start(
        Arc::new(Pinned {
            text: text.clone(),
            known: 10,
            stopped: false,
        }),
        Matcher::new("needle", FindOptions::default()).unwrap(),
        ByteOffset(0)..ByteOffset(1000),
        QUEUE_LIMIT,
        None,
    );
    wait_until("the worker to ask for more", || {
        job.waiting_past().is_some()
    });
    job.cancel();
    wait_until("it to stop", || job.state() == JobState::Cancelled);
}

#[test]
fn a_step_does_not_wrap_or_say_not_found_while_indexing() {
    let text = Arc::new("needle xxxxxxxxxxxxxxx".to_string());
    let snapshot = Pinned {
        text,
        known: 10,
        stopped: false,
    };
    let matcher = Matcher::new("needle", FindOptions::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let from = ByteOffset(8)..ByteOffset(8);
    let found = find_step(&snapshot, &matcher, from, Direction::Forward, true, &cancel).unwrap();
    assert_eq!(
        found, None,
        "the match behind the caret waits for the whole text"
    );
}

// ---- Second review, finding 1: Find Previous at a block edge ----

/// Every start that matches, in order, overlaps included, with the word rules worked out on
/// whole tokens. It uses no rule from the crate.
fn all_starts_model(tokens: &[usize], query: &str, kind: MatchKind) -> Vec<Range<usize>> {
    let word = |t: usize| {
        TOKENS[t]
            .0
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
    };
    let mut starts = Vec::with_capacity(tokens.len() + 1);
    let mut at = 0;
    for &t in tokens {
        starts.push(at);
        at += TOKENS[t].0.len();
    }
    starts.push(at);
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        let mut left = query;
        let mut j = i;
        while !left.is_empty() && j < tokens.len() {
            match left.strip_prefix(TOKENS[tokens[j]].0) {
                Some(rest) => {
                    left = rest;
                    j += 1;
                }
                None => break,
            }
        }
        if !left.is_empty() {
            continue;
        }
        let before = i > 0 && word(tokens[i - 1]);
        let after = j < tokens.len() && word(tokens[j]);
        let good = match kind {
            MatchKind::Contains => true,
            MatchKind::StartsWith => !before,
            MatchKind::EndsWith => !after,
            MatchKind::FullWord => !before && !after,
        };
        if good {
            out.push(starts[i]..starts[j]);
        }
    }
    out
}

#[test]
fn backward_steps_match_a_plain_model_at_random_places() {
    let mut seed = 0x51_7c_c1_b7_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    let queries = [
        ("a", MatchKind::FullWord),
        ("ab", MatchKind::FullWord),
        ("a", MatchKind::EndsWith),
        ("b", MatchKind::StartsWith),
        ("e", MatchKind::Contains),
        ("\r", MatchKind::Contains),
        ("\n", MatchKind::Contains),
        ("\u{1f1fa}", MatchKind::Contains),
        ("\u{1f1fa}\u{1f1f8}", MatchKind::Contains),
        ("\u{4e2d}a", MatchKind::Contains),
    ];
    let mut compared = 0;
    let mut found = 0;
    for piece in [4096, 7] {
        let mut tokens = Vec::new();
        let mut text = String::new();
        while text.len() < 120_000 {
            let t = next() % TOKENS.len();
            tokens.push(t);
            text.push_str(TOKENS[t].0);
        }
        let mut places = Vec::with_capacity(tokens.len() + 1);
        let mut at = 0;
        for &t in &tokens {
            places.push(at);
            at += TOKENS[t].0.len();
        }
        places.push(at);
        let store = Pieces::new(&text, piece);
        for (query, kind) in queries {
            let want = all_starts_model(&tokens, query, kind);
            let matcher = Matcher::new(query, options(false, kind)).unwrap();
            for step in 0..120 {
                let limit = if step == 0 {
                    text.len()
                } else {
                    places[next() % places.len()]
                };
                let got = last_match(
                    &store,
                    &matcher,
                    ByteOffset(0),
                    ByteOffset(limit),
                    &NOT_CANCELLED,
                )
                .unwrap()
                .map(|r| r.start.0..r.end.0);
                let expect = want.iter().rfind(|r| r.end <= limit).cloned();
                found += usize::from(expect.is_some());
                assert_eq!(
                    got, expect,
                    "{query:?} {kind:?} up to {limit}, piece {piece}"
                );
                compared += 1;
            }
        }
    }
    assert!(
        compared >= 2000 && found > 500,
        "{compared} compared, {found} found"
    );
}

#[test]
fn find_previous_selects_no_false_match_at_a_block_edge() {
    let exact = options(false, MatchKind::Contains);
    let word = options(false, MatchKind::FullWord);
    let cases = [
        ("foobar ", "foo", word),
        ("e\u{301} ", "e", exact),
        ("x\r\n", "\r", exact),
        ("\u{1f1f8}\u{1f1fa}", "\u{1f1fa}\u{1f1f8}", exact),
    ];
    for (unit, query, kind) in cases {
        for pad in 0..unit.len() {
            let text = format!("{}{}", unit.repeat(200_000 / unit.len()), " ".repeat(pad));
            let store = Pieces::new(&text, 4096);
            let matcher = Matcher::new(query, kind).unwrap();
            let got = last_match(
                &store,
                &matcher,
                ByteOffset(0),
                ByteOffset(text.len()),
                &NOT_CANCELLED,
            );
            assert_eq!(got.unwrap(), None, "{query:?}, pad {pad}");
        }
    }
}

#[test]
fn backward_blocks_keep_a_match_on_their_edge_and_what_stands_before_it() {
    let word = options(false, MatchKind::FullWord);
    let exact = options(false, MatchKind::Contains);
    let len = 40_000;
    let edge = len - 16 * 1024;
    let needle = Matcher::new("needle", exact).unwrap();
    let cat = Matcher::new("cat", word).unwrap();
    for offset in edge - 100..edge + 90 {
        let mut text = " ".repeat(len);
        text.replace_range(offset..offset + 6, "needle");
        let store = Pieces::new(&text, 4096);
        let got = last_match(
            &store,
            &needle,
            ByteOffset(0),
            ByteOffset(len),
            &NOT_CANCELLED,
        );
        let got = got.unwrap().map(|r| r.start.0);
        assert_eq!(got, Some(offset), "the edge cuts this match");

        let mut text = " ".repeat(len);
        text.replace_range(offset..offset + 4, "xcat");
        let store = Pieces::new(&text, 4096);
        let got = last_match(&store, &cat, ByteOffset(0), ByteOffset(len), &NOT_CANCELLED);
        assert_eq!(
            got.unwrap(),
            None,
            "the block must keep the x before it, at {offset}"
        );
    }
}

#[test]
fn a_match_that_runs_past_the_end_of_the_range_is_refused() {
    let store = Pieces::new("xx abcdef yy", 3);
    let matcher = Matcher::new("abcd", options(false, MatchKind::Contains)).unwrap();
    let cut = first_match(
        &store,
        &matcher,
        ByteOffset(0)..ByteOffset(5),
        &NOT_CANCELLED,
    );
    assert_eq!(cut.unwrap(), None);
    let whole = first_match(
        &store,
        &matcher,
        ByteOffset(0)..ByteOffset(7),
        &NOT_CANCELLED,
    );
    assert_eq!(whole.unwrap(), Some(ByteOffset(3)..ByteOffset(7)));
}

#[test]
fn a_waiting_search_ends_with_what_can_be_read_when_the_index_stops() {
    // The match stands in the last bytes, which wait for the text after them.
    let mut text = "y".repeat(5000);
    text.push_str("needle");
    let known = text.len() + 4;
    text.push_str(&"y".repeat(5000));
    let text = Arc::new(text);
    let job = FindJob::start(
        Arc::new(Pinned {
            text: text.clone(),
            known,
            stopped: false,
        }),
        Matcher::new("needle", FindOptions::default()).unwrap(),
        ByteOffset(0)..ByteOffset(text.len()),
        QUEUE_LIMIT,
        None,
    );
    wait_until("the worker to ask for more", || {
        job.waiting_past().is_some()
    });
    assert_eq!(job.count(), 0);
    let stopped = |failed: bool| -> Arc<dyn TextSnapshot> {
        Arc::new(Pinned {
            text: text.clone(),
            known,
            stopped: failed,
        })
    };
    // No more text and no failure: the worker goes on waiting.
    let mut offered = false;
    job.feed_with(&stopped(false).status(), || {
        offered = true;
        stopped(false)
    });
    assert!(
        !offered,
        "an offer here would wake the worker for nothing, again and again"
    );
    assert_eq!(job.waiting_past(), Some(known as u64));
    assert_eq!(job.state(), JobState::Running);
    // The index stops: it judges the last bytes and ends.
    job.feed_with(&stopped(true).status(), || stopped(true));
    wait_until("the end", || job.state() == JobState::Finished);
    assert_eq!(job.count(), 1);
}
