//! Finding 7: what the line cache may share, how fast a get is, and what it counts.
use qe_api::{
    ByteOffset, CaretStop, Dip, LineIndex, TextChange, TextPosition, TextSnapshot, TextStore,
    Utf16Offset, testing::StringStore,
};
use qe_layout::{CacheTicket, DirectWriteLayout, LineCache};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

/// One hundred thousand short lines, with lookups a real buffer could match. It
/// counts reads, so a test can see that a warm get reads no text.
struct Lines {
    count: usize,
    revision: u64,
    reads: AtomicUsize,
    /// Bytes of text above the first line, so a test can move every line.
    shift: usize,
}
const LINE: &str = "The quick brown fox\n";
impl TextSnapshot for Lines {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.count * LINE.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.count * LINE.len())
    }
    fn line_count(&self) -> usize {
        self.count
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        Some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, unit: Utf16Offset) -> Option<ByteOffset> {
        Some(ByteOffset(unit.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        Some(TextPosition {
            line: LineIndex(byte.0 / LINE.len()),
            column: Utf16Offset(byte.0 % LINE.len()),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        Some(ByteOffset(pos.line.0 * LINE.len() + pos.column.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 < self.count).then(|| {
            ByteOffset(self.shift + line.0 * LINE.len())
                ..ByteOffset(self.shift + (line.0 + 1) * LINE.len())
        })
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let (from, to) = (range.start.0, range.end.0);
        let mut at = from;
        while at < to {
            let offset = at % LINE.len();
            let end = (offset + to - at).min(LINE.len());
            visitor(&LINE[offset..end]);
            at += end - offset;
        }
        true
    }
    fn is_read_only(&self) -> bool {
        true
    }
}
fn lines(count: usize, revision: u64) -> Lines {
    Lines {
        count,
        revision,
        reads: AtomicUsize::new(0),
        shift: 0,
    }
}

#[test]
fn each_engine_setting_gets_its_own_entry() {
    let wrap = DirectWriteLayout::new().unwrap();
    let no_wrap = DirectWriteLayout::new().unwrap().with_wrap(false);
    let shaped = DirectWriteLayout::new().unwrap().with_shaping(true);
    let wide_tabs = DirectWriteLayout::new().unwrap().with_tab_width(4);
    let text = StringStore::new("\tend ".to_string() + &"word ".repeat(100));
    let mut cache = LineCache::new(10_000_000);
    let width = Dip(wrap.cell_width().0 * 33.0);
    let line = LineIndex(0);
    let a = cache.get_or_layout(&wrap, &text, line, width).unwrap();
    let b = cache.get_or_layout(&no_wrap, &text, line, width).unwrap();
    assert_ne!(a.rows.len(), b.rows.len());
    assert_eq!(a, Arc::new(wrap_fresh(&wrap, &text, width)));
    let c = cache.get_or_layout(&shaped, &text, line, width).unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    let d = cache.get_or_layout(&wide_tabs, &text, line, width).unwrap();
    assert_ne!(a, d);
    assert_eq!(cache.len(), 4);
    assert!(Arc::ptr_eq(
        &a,
        &cache.get_or_layout(&wrap, &text, line, width).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &d,
        &cache.get_or_layout(&wide_tabs, &text, line, width).unwrap()
    ));
    // Another width is another layout, but only when the engine wraps.
    let other = Dip(width.0 * 2.0);
    assert!(!Arc::ptr_eq(
        &a,
        &cache.get_or_layout(&wrap, &text, line, other).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &b,
        &cache.get_or_layout(&no_wrap, &text, line, other).unwrap()
    ));
}
fn wrap_fresh(
    engine: &DirectWriteLayout,
    text: &dyn TextSnapshot,
    width: Dip,
) -> qe_api::LineLayout {
    engine.try_layout_line(text, LineIndex(0), width).unwrap()
}

#[test]
fn a_warm_get_reads_no_text_and_is_fast_with_many_entries() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = lines(100_000, 1);
    let mut cache = LineCache::new(usize::MAX);
    for i in 0..100_000 {
        cache
            .get_or_layout(&engine, &text, LineIndex(i), Dip(1000.0))
            .unwrap();
    }
    assert_eq!(cache.len(), 100_000);
    let reads = text.reads.load(Ordering::Relaxed);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        for i in 50_000..50_040 {
            std::hint::black_box(
                cache
                    .get_or_layout(&engine, &text, LineIndex(i), Dip(1000.0))
                    .unwrap(),
            );
        }
        best = best.min(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!("40 warm gets with 100,000 entries: {best:.4} ms");
    assert_eq!(
        text.reads.load(Ordering::Relaxed),
        reads,
        "a warm get read text"
    );
    assert!(best < 1.0, "was 10.28 ms before: {best} ms");
}

fn change(replaced: Range<usize>, new_len: usize, delta: isize, revision: u64) -> TextChange {
    TextChange {
        replaced: ByteOffset(replaced.start)..ByteOffset(replaced.end),
        new_len,
        line_count_delta: delta,
        revision,
    }
}

#[test]
fn invalidate_does_not_depend_on_the_entry_count() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = lines(100_000, 1);
    let mut cache = LineCache::new(usize::MAX);
    for i in 0..100_000 {
        cache
            .get_or_layout(&engine, &text, LineIndex(i), Dip(1000.0))
            .unwrap();
    }
    let mut revision = 1;
    let mut count = 100_000isize;
    let mut times = Vec::new();
    for edit in 0..200 {
        let old = lines(count as usize, revision);
        revision += 1;
        // Half the edits keep the line count, half press Enter.
        let delta = (edit % 2) as isize;
        let at = (10 + edit * 100) * LINE.len();
        let start = Instant::now();
        cache.invalidate(&old, &change(at..at, 1, delta, revision));
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        count += delta;
    }
    // A busy PC can stop any one call, so the limit is on the median.
    times.sort_by(f64::total_cmp);
    let (middle, slowest) = (times[100], times[199]);
    println!("invalidate with 100,000 entries: median {middle:.4} ms, slowest {slowest:.4} ms");
    assert!(middle < 0.2, "{middle} ms");
    assert_eq!(cache.len(), 100_000 - 200);
}

#[test]
fn edits_above_a_line_move_it_without_new_layout() {
    let engine = DirectWriteLayout::new().unwrap();
    let mut store = StringStore::new("first\nsecond\nthird\nfourth\nfifth");
    let mut cache = LineCache::new(1_000_000);
    let width = Dip(1000.0);
    for i in 0..5 {
        cache
            .get_or_layout(&engine, &store, LineIndex(i), width)
            .unwrap();
    }
    let check = |cache: &mut LineCache, store: &StringStore| {
        for i in 0..store.line_count() {
            let got = cache
                .get_or_layout(&engine, store, LineIndex(i), width)
                .unwrap();
            let fresh = engine.try_layout_line(store, LineIndex(i), width).unwrap();
            assert_eq!(*got, fresh, "line {i} of {:?}", store.as_str());
        }
    };
    let mut edit = |cache: &mut LineCache, range: Range<usize>, new: &str| {
        let old = store.snapshot();
        let change = store
            .replace(ByteOffset(range.start)..ByteOffset(range.end), new)
            .unwrap();
        cache.invalidate(old.as_ref(), &change);
        check(cache, &store);
        store.clone()
    };
    edit(&mut cache, 2..2, "XX");
    edit(&mut cache, 6..6, "one\ntwo\n");
    edit(&mut cache, 0..7, "");
    let now = edit(&mut cache, 3..9, "");
    assert!(now.line_count() >= 2);
    edit(&mut cache, 0..0, "a\r");
    // CR then LF join into one line ending, which changes the line before.
    edit(&mut cache, 2..2, "\nb");
    let last = now.len_bytes().0;
    assert!(last > 0);
    for round in 0..600 {
        // More line-count edits than the cache keeps pieces for.
        edit(&mut cache, 0..0, "z\n");
        if round % 50 == 0 {
            edit(&mut cache, 0..2, "");
        }
    }
}

#[test]
fn a_revision_the_cache_did_not_hear_about_empties_it() {
    let engine = DirectWriteLayout::new().unwrap();
    let mut store = StringStore::new("abc\ndef");
    let mut cache = LineCache::new(100_000);
    let width = Dip(1000.0);
    let before = cache
        .get_or_layout(&engine, &store, LineIndex(1), width)
        .unwrap();
    store.replace(ByteOffset(4)..ByteOffset(5), "XY").unwrap();
    let after = cache
        .get_or_layout(&engine, &store, LineIndex(1), width)
        .unwrap();
    assert_ne!(before, after, "no old layout after an edit nobody reported");
}

#[test]
fn memory_counts_everything_and_stays_under_the_cap() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = lines(1000, 1);
    let mut unlimited = LineCache::new(usize::MAX);
    let mut least = 0;
    for i in 0..100 {
        let layout = unlimited
            .get_or_layout(&engine, &text, LineIndex(i), Dip(1000.0))
            .unwrap();
        for row in &layout.rows {
            least += row.caret_stops.len() * size_of::<CaretStop>();
            for run in &row.glyph_runs {
                least += run.glyph_ids.len() * size_of::<u16>()
                    + run.advances.len() * size_of::<Dip>()
                    + run.offsets.len() * size_of::<qe_api::Point>();
            }
        }
    }
    assert!(
        unlimited.memory_bytes() > least,
        "{} <= {least}",
        unlimited.memory_bytes()
    );

    let cap = unlimited.memory_bytes() / 4;
    let mut capped = LineCache::new(cap);
    for i in 0..1000 {
        capped
            .get_or_layout(&engine, &text, LineIndex(i), Dip(1000.0))
            .unwrap();
        assert!(capped.memory_bytes() <= cap);
    }
    assert!(capped.len() >= 20, "{}", capped.len());
    // The oldest line went first, and the newest stayed.
    let reads = text.reads.load(Ordering::Relaxed);
    capped
        .get_or_layout(&engine, &text, LineIndex(999), Dip(1000.0))
        .unwrap();
    assert_eq!(text.reads.load(Ordering::Relaxed), reads);
    capped
        .get_or_layout(&engine, &text, LineIndex(0), Dip(1000.0))
        .unwrap();
    assert!(text.reads.load(Ordering::Relaxed) > reads);

    // A layout bigger than the cap is returned and not kept.
    let mut tiny = LineCache::new(10);
    assert!(
        tiny.get_or_layout(&engine, &text, LineIndex(0), Dip(1000.0))
            .is_ok()
    );
    assert_eq!(tiny.len(), 0);
    assert!(tiny.memory_bytes() < 1024, "{}", tiny.memory_bytes());
}

// Round 3, item 2.
#[test]
fn a_worker_lays_out_ahead_and_the_ui_thread_never_shapes() {
    let engine = Arc::new(DirectWriteLayout::new().unwrap());
    let store: Arc<dyn TextSnapshot> = Arc::new(StringStore::new(
        (0..40)
            .map(|i| format!("\u{6F22}\u{5B57} line {i} \u{645}\u{631}\n"))
            .collect::<String>(),
    ));
    let width = Dip(800.0);
    let mut cache = LineCache::new(10_000_000);
    let (to_worker, jobs) = std::sync::mpsc::channel::<CacheTicket>();
    let (results, from_worker) = std::sync::mpsc::channel();
    let worker = {
        let (engine, store) = (engine.clone(), store.clone());
        std::thread::spawn(move || {
            for ticket in jobs {
                let layout = ticket.layout(&engine, store.as_ref()).unwrap();
                results.send((ticket, layout)).unwrap();
            }
        })
    };
    for line in 0..40 {
        let line = LineIndex(line);
        assert!(cache.cached(&engine, store.as_ref(), line, width).is_none());
        to_worker
            .send(cache.ticket(&engine, store.as_ref(), line, width).unwrap())
            .unwrap();
    }
    assert!(cache.is_empty(), "a miss must not shape or store anything");
    drop(to_worker);
    for (ticket, layout) in from_worker {
        assert!(cache.insert(&ticket, layout));
    }
    worker.join().unwrap();
    assert_eq!(cache.len(), 40);
    for line in 0..40 {
        let line = LineIndex(line);
        let got = cache.cached(&engine, store.as_ref(), line, width).unwrap();
        assert_eq!(
            *got,
            engine.try_layout_line(store.as_ref(), line, width).unwrap()
        );
    }
}

#[test]
fn a_late_worker_result_is_kept_only_if_it_is_still_true() {
    let engine = DirectWriteLayout::new().unwrap();
    let width = Dip(1000.0);
    let mut store = StringStore::new("one\ntwo\nthree\nfour");
    let mut cache = LineCache::new(1_000_000);
    let start = store.snapshot();
    let line = |n| LineIndex(n);
    let edited_line = cache
        .ticket(&engine, start.as_ref(), line(1), width)
        .unwrap();
    let moved_line = cache
        .ticket(&engine, start.as_ref(), line(2), width)
        .unwrap();
    let unmoved_line = cache
        .ticket(&engine, start.as_ref(), line(0), width)
        .unwrap();
    let edited = edited_line.layout(&engine, start.as_ref()).unwrap();
    let moved = moved_line.layout(&engine, start.as_ref()).unwrap();
    let unmoved = unmoved_line.layout(&engine, start.as_ref()).unwrap();

    // An edit in line 1 that also adds a line, so line 2 moves to line 3.
    let change = store.replace(ByteOffset(5)..ByteOffset(5), "X\nY").unwrap();
    cache.invalidate(start.as_ref(), &change);

    assert!(!cache.insert(&edited_line, edited), "line 1 changed");
    assert!(cache.insert(&moved_line, moved), "line 2 only moved");
    assert!(cache.insert(&unmoved_line, unmoved));
    for n in 0..store.line_count() {
        if let Some(got) = cache.cached(&engine, &store, line(n), width) {
            assert_ne!(n, 1);
            assert_ne!(n, 2);
            assert_eq!(
                *got,
                engine.try_layout_line(&store, line(n), width).unwrap()
            );
        }
    }
    assert!(cache.cached(&engine, &store, line(3), width).is_some());
    assert!(cache.cached(&engine, &store, line(0), width).is_some());

    // The same line edited with no change in the line count.
    let before = store.snapshot();
    let ticket = cache
        .ticket(&engine, before.as_ref(), line(0), Dip(500.0))
        .unwrap();
    let layout = ticket.layout(&engine, before.as_ref()).unwrap();
    let change = store.replace(ByteOffset(0)..ByteOffset(1), "Q").unwrap();
    cache.invalidate(before.as_ref(), &change);
    assert!(!cache.insert(&ticket, layout));

    // A ticket from a snapshot older than the cache is never trusted.
    let old = cache
        .ticket(&engine, start.as_ref(), line(0), width)
        .unwrap();
    let layout = old.layout(&engine, start.as_ref()).unwrap();
    assert!(!cache.insert(&old, layout));

    // The wrong snapshot is an error, not a wrong layout.
    assert!(matches!(
        ticket.layout(&engine, start.as_ref()),
        Err(qe_layout::LayoutError::StaleTicket { .. })
    ));

    // A cache that started over knows nothing about older tickets.
    let current = store.snapshot();
    let ticket = cache
        .ticket(&engine, current.as_ref(), line(0), width)
        .unwrap();
    let layout = ticket.layout(&engine, current.as_ref()).unwrap();
    cache.clear();
    assert!(!cache.insert(&ticket, layout));
}

#[test]
fn an_older_snapshot_never_wipes_newer_entries() {
    let engine = DirectWriteLayout::new().unwrap();
    let width = Dip(1000.0);
    let mut store = StringStore::new("abc\ndef\nghi");
    let old = store.snapshot();
    let mut cache = LineCache::new(100_000);
    let change = store.replace(ByteOffset(0)..ByteOffset(1), "x").unwrap();
    cache.invalidate(old.as_ref(), &change);
    let kept = cache
        .get_or_layout(&engine, &store, LineIndex(1), width)
        .unwrap();
    assert_eq!(cache.len(), 1);

    let from_old = cache
        .get_or_layout(&engine, old.as_ref(), LineIndex(0), width)
        .unwrap();
    assert_eq!(from_old.rows[0].range.end, ByteOffset(3));
    assert!(
        cache
            .cached(&engine, old.as_ref(), LineIndex(1), width)
            .is_none()
    );
    assert_eq!(
        cache.len(),
        1,
        "an older snapshot stores nothing and wipes nothing"
    );
    let again = cache.cached(&engine, &store, LineIndex(1), width).unwrap();
    assert!(Arc::ptr_eq(&kept, &again));

    // A repeated report of an edit the cache already knows changes nothing.
    cache.invalidate(old.as_ref(), &change);
    assert_eq!(cache.len(), 1);
}

// Round 3, item 10.
#[test]
fn an_edit_in_a_long_line_keeps_the_segments_before_it() {
    let engine = DirectWriteLayout::new().unwrap();
    let width = Dip(800.0);
    let line = LineIndex(0);
    let mut store = StringStore::new("word wrap test ".repeat(8000));
    let mut cache = LineCache::new(100_000_000);
    let count = engine.segment_count(&store, line).unwrap();
    let held: Vec<_> = (0..count)
        .map(|s| {
            cache
                .get_or_layout_segment(&engine, &store, line, s, width)
                .unwrap()
        })
        .collect();
    let old = store.snapshot();
    let at = engine.segment_range(&store, line, 30).unwrap().start.0 + 100;
    let change = store
        .replace(ByteOffset(at)..ByteOffset(at), "typed")
        .unwrap();
    cache.invalidate(old.as_ref(), &change);
    for (s, kept) in held.iter().enumerate().take(28) {
        let got = cache
            .cached_segment(&engine, &store, line, s, width)
            .unwrap();
        assert!(Arc::ptr_eq(kept, &got), "segment {s} was laid out again");
    }
    for s in 30..count.min(50) {
        assert!(
            cache
                .cached_segment(&engine, &store, line, s, width)
                .is_none(),
            "segment {s}"
        );
    }
    let new_count = engine.segment_count(&store, line).unwrap();
    for s in 0..new_count {
        let got = cache
            .get_or_layout_segment(&engine, &store, line, s, width)
            .unwrap();
        assert_eq!(
            *got,
            engine.layout_segment(&store, line, s, width).unwrap(),
            "segment {s}"
        );
    }
    // An edit that leaves the line short drops everything, because it stays whole.
    let old = store.snapshot();
    let end = store.len_bytes().0;
    let change = store.replace(ByteOffset(50)..ByteOffset(end), "").unwrap();
    cache.invalidate(old.as_ref(), &change);
    assert!(cache.is_empty());
    assert!(
        !cache
            .get_or_layout(&engine, &store, line, width)
            .unwrap()
            .partial
    );
}

// Round 3, item 12.
#[test]
fn the_first_get_after_an_edit_above_costs_little() {
    let engine = DirectWriteLayout::new().unwrap();
    let width = Dip(2000.0);
    let long_row = "x".repeat(120);
    let mut store = StringStore::new(format!("{long_row}\n").repeat(41));
    let mut cache = LineCache::new(100_000_000);
    for i in 1..41 {
        cache
            .get_or_layout(&engine, &store, LineIndex(i), width)
            .unwrap();
    }
    let old = store.snapshot();
    let change = store
        .replace(ByteOffset(0)..ByteOffset(0), "typed")
        .unwrap();
    cache.invalidate(old.as_ref(), &change);
    for i in 1..41 {
        let got = cache.cached(&engine, &store, LineIndex(i), width).unwrap();
        assert_eq!(got.rows[0].range.start.0, 5 + i * 121);
    }
}

/// The first get after an edit above rewrites every offset in the layout, and it copies
/// the layout if a caller still holds it. This holds the cost of that to a known size.
#[test]
fn moving_a_screen_of_cached_lines_stays_inside_a_frame() {
    let engine = DirectWriteLayout::new().unwrap();
    let width = Dip(100_000.0);
    let count = 40;
    for holding in [false, true] {
        let mut best = f64::MAX;
        for _ in 0..20 {
            let mut cache = LineCache::new(usize::MAX);
            let text = lines(count, 1);
            let held: Vec<_> = (0..count)
                .map(|i| {
                    cache
                        .get_or_layout(&engine, &text, LineIndex(i), width)
                        .unwrap()
                })
                .collect();
            let kept = if holding { held } else { Vec::new() };
            // An edit far below, so these lines stay cached, then a move of them all.
            let far = 1000 * LINE.len();
            cache.invalidate(&lines(2000, 1), &change(far..far, 0, 0, 2));
            let mut moved = lines(count, 2);
            moved.shift = 5;
            let start = Instant::now();
            for i in 0..count {
                let got = cache.cached(&engine, &moved, LineIndex(i), width).unwrap();
                assert_eq!(got.rows[0].range.start.0, 5 + i * LINE.len());
            }
            best = best.min(start.elapsed().as_secs_f64() * 1000.0);
            drop(kept);
        }
        println!("first get of 40 moved lines, layouts held {holding}: {best:.4} ms");
        assert!(best < 2.0, "{best} ms");
    }
}

// Round 3, item 16.
#[test]
fn random_edits_keep_every_cached_line_correct() {
    let engine = DirectWriteLayout::new().unwrap();
    let width = Dip(300.0);
    let mut store = StringStore::new("first line\nsecond\r\nthird \u{5D0}\u{5D1}\n\u{6F22}\nlast");
    let mut cache = LineCache::new(200_000);
    let mut seed = 77u64;
    let mut next = move |below: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % below
    };
    let inserts = [
        "",
        "x",
        "\n",
        "\r",
        "\r\n",
        "ab\ncd",
        "\u{5D0}",
        "\u{6F22}\n",
        " word ",
    ];
    for round in 0..3000 {
        let text = store.as_str().to_string();
        let edge = |n: usize| {
            let mut at = n % (text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        let a = edge(next(text.len() + 1));
        let b = edge(a + next(6));
        let old = store.snapshot();
        let change = store
            .replace(
                ByteOffset(a.min(b))..ByteOffset(a.max(b)),
                inserts[next(inserts.len())],
            )
            .unwrap();
        cache.invalidate(old.as_ref(), &change);
        if store.len_bytes().0 > 400 {
            let old = store.snapshot();
            let change = store
                .replace(
                    ByteOffset(0)
                        ..ByteOffset(
                            200.min(
                                store
                                    .as_str()
                                    .char_indices()
                                    .map(|(i, _)| i)
                                    .find(|i| *i >= 200)
                                    .unwrap_or(0),
                            ),
                        ),
                    "",
                )
                .unwrap();
            cache.invalidate(old.as_ref(), &change);
        }
        for _ in 0..4 {
            let line = LineIndex(next(store.line_count()));
            let got = cache.get_or_layout(&engine, &store, line, width).unwrap();
            let fresh = engine.try_layout_line(&store, line, width).unwrap();
            assert_eq!(
                *got,
                fresh,
                "round {round} line {line:?}: {:?}",
                store.as_str()
            );
        }
    }
}

#[test]
fn a_cache_get_returns_the_reason_for_a_failure() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = StringStore::new("short");
    let mut cache = LineCache::new(10_000);
    assert_eq!(
        cache
            .get_or_layout(&engine, &store, LineIndex(4), Dip(100.0))
            .unwrap_err(),
        qe_layout::LayoutError::NoSuchLine(LineIndex(4))
    );
    assert_eq!(
        cache
            .get_or_layout_segment(&engine, &store, LineIndex(0), 3, Dip(100.0))
            .unwrap_err(),
        qe_layout::LayoutError::NoSuchSegment {
            line: LineIndex(0),
            index: 3
        }
    );
}
