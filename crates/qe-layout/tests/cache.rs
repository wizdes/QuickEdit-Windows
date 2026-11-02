//! Finding 7: what the line cache may share, how fast a get is, and what it counts.
use qe_api::{
    ByteOffset, CaretStop, Dip, LineIndex, TextChange, TextPosition, TextSnapshot, TextStore,
    Utf16Offset, testing::StringStore,
};
use qe_layout::{DirectWriteLayout, LineCache};
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
        (line.0 < self.count)
            .then(|| ByteOffset(line.0 * LINE.len())..ByteOffset((line.0 + 1) * LINE.len()))
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
            .is_some()
    );
    assert_eq!(tiny.len(), 0);
    assert_eq!(tiny.memory_bytes(), 0);
}
