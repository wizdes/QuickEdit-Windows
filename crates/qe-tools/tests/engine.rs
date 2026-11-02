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

/// The model: each start in order, checked with plain string calls.
fn slow_model(text: &str, query: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if text.is_char_boundary(at)
            && text[at..].starts_with(query)
            && qe_tools::is_cluster_edge(text, at)
            && qe_tools::is_cluster_edge(text, at + query.len())
        {
            out.push(at..at + query.len());
            at += query.len();
        } else {
            at += 1;
        }
    }
    out
}

#[test]
fn seeded_search_matches_a_slow_model_at_every_piece_size() {
    let parts = [
        "a",
        "A",
        "b",
        "e\u{301}",
        "\u{1f600}",
        "\r\n",
        " ",
        "\u{df}",
        "\u{4e2d}",
        "ab",
        "\n",
    ];
    let mut seed = 0x9e37_79b9_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    let mut compared = 0;
    for round in 0..60 {
        let mut text = String::new();
        for _ in 0..(40 + round * 8) {
            text.push_str(parts[next() % parts.len()]);
        }
        let piece = [1, 2, 3, 5, 7, 16, 4096][round % 7];
        let store = Pieces::new(&text, piece);
        for query in [
            "a",
            "b",
            "ab",
            "aa",
            "e\u{301}",
            "\u{1f600}",
            "\r\n",
            "\u{4e2d}a",
        ] {
            let want = slow_model(&text, query);
            let exact = options(false, MatchKind::Contains);
            assert_eq!(
                find_text(&text, query, exact),
                want,
                "{query:?} in {text:?}"
            );
            assert_eq!(scan_all(&store, query, exact), want, "pieces of {piece}");
            compared += 1;
        }
        // Ignoring case, a scan over pieces agrees with one over the whole string.
        for query in ["A", "AB", "SS", "\u{df}"] {
            let any = FindOptions::default();
            assert_eq!(scan_all(&store, query, any), find_text(&text, query, any));
            compared += 1;
        }
    }
    assert!(compared >= 700);
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
struct Pinned {
    text: Arc<String>,
    known: usize,
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
            failure: None,
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
    let snapshot = Pinned { text, known: 10 };
    let matcher = Matcher::new("needle", FindOptions::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let from = ByteOffset(8)..ByteOffset(8);
    let found = find_step(&snapshot, &matcher, from, Direction::Forward, true, &cancel).unwrap();
    assert_eq!(
        found, None,
        "the match behind the caret waits for the whole text"
    );
}
