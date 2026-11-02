use qe_api::{
    ByteOffset, Dip, Layout, LineIndex, TextPosition, TextSnapshot, TextStore, Utf16Offset,
    testing::StringStore,
};
use qe_layout::{DirectWriteLayout, HeightMap, LineCache, font_face, slice_around};
use std::{
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
};

struct LongLine {
    text: String,
    visited: AtomicUsize,
}
impl TextSnapshot for LongLine {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.text.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.text.len())
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        (byte.0 <= self.text.len()).then_some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, unit: Utf16Offset) -> Option<ByteOffset> {
        (unit.0 <= self.text.len()).then_some(ByteOffset(unit.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        (byte.0 <= self.text.len()).then_some(TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(byte.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        (pos.line.0 == 0 && pos.column.0 <= self.text.len()).then_some(ByteOffset(pos.column.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 == 0).then_some(ByteOffset(0)..ByteOffset(self.text.len()))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        if let Some(s) = self.text.get(range.start.0..range.end.0) {
            self.visited.fetch_add(s.len(), Ordering::Relaxed);
            visitor(s);
            true
        } else {
            false
        }
    }
    fn is_read_only(&self) -> bool {
        true
    }
}

#[test]
fn ascii_fast_path_and_caret_round_trip() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = StringStore::new("abc\tdef");
    let result = engine
        .layout_line(&store, LineIndex(0), Dip(1000.0))
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].glyph_runs[0].glyph_ids.len(), 7);
    for stop in &result.rows[0].caret_stops {
        let point = engine.hit_test_offset(&result, *stop).unwrap();
        assert_eq!(engine.hit_test_point(&result, point), *stop);
    }
    let tab = &result.rows[0].glyph_runs[0].advances[3];
    assert!(tab.0 > engine.cell_width().0 * 4.0);
}

#[test]
fn ascii_fast_and_shaped_positions_agree() {
    let fast = DirectWriteLayout::new().unwrap();
    let slow = DirectWriteLayout::new().unwrap().with_shaping(true);
    let store = StringStore::new("Hello, world 123");
    let a = fast.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
    let b = slow.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
    for stop in &a.rows[0].caret_stops {
        let x_a = fast.hit_test_offset(&a, *stop).unwrap().x.0;
        let x_b = slow.hit_test_offset(&b, *stop).unwrap().x.0;
        assert!((x_a - x_b).abs() < 0.05, "{stop:?}: {x_a} vs {x_b}");
    }
    let width_a: f32 = a.rows[0].glyph_runs[0].advances.iter().map(|v| v.0).sum();
    let width_b: f32 = b.rows[0]
        .glyph_runs
        .iter()
        .flat_map(|r| &r.advances)
        .map(|v| v.0)
        .sum();
    assert!((width_a - width_b).abs() < 0.05, "{width_a} vs {width_b}");
}

#[test]
fn slow_scripts_have_caret_round_trips() {
    let engine = DirectWriteLayout::new().unwrap();
    for sample in [
        "aé café",
        "مرحبا",
        "שלום",
        "नमस्ते",
        "漢字かな",
        "👩‍💻",
        "abc שלום 123",
    ] {
        let store = StringStore::new(sample);
        let result = engine
            .layout_line(&store, LineIndex(0), Dip(1000.0))
            .unwrap();
        assert!(!result.rows.is_empty(), "{sample}");
        assert!(
            result
                .rows
                .iter()
                .any(|r| r.glyph_runs.iter().any(|g| !g.glyph_ids.is_empty())),
            "no shaped glyphs for {sample}"
        );
        for run in result.rows.iter().flat_map(|r| &r.glyph_runs) {
            assert!(font_face(run.font_id).is_some());
        }
        for row in &result.rows {
            for stop in &row.caret_stops {
                let point = engine.hit_test_offset(&result, *stop).unwrap();
                assert_eq!(
                    engine.hit_test_point(&result, point),
                    *stop,
                    "{sample} {stop:?}"
                );
            }
        }
    }
}

#[test]
fn wrapping_long_words_and_trailing_spaces() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = StringStore::new("one two abcdefghijklmnopqrstuvwxyz   ");
    let result = engine
        .layout_line(&store, LineIndex(0), Dip(engine.cell_width().0 * 8.0))
        .unwrap();
    assert!(result.rows.len() >= 4);
    assert_eq!(
        result.rows.last().unwrap().range.end,
        ByteOffset(store.len_bytes().0)
    );
}

#[test]
fn tab_respects_wrap_width() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = StringStore::new("a\tb");
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(engine.cell_width().0 * 4.0))
        .unwrap();
    assert!(layout.rows.len() > 1);
    assert_eq!(layout.rows.last().unwrap().range.end, ByteOffset(3));
}

#[test]
fn hebrew_caret_positions_follow_visual_order() {
    let engine = DirectWriteLayout::new().unwrap();
    let sample = "שלום";
    let store = StringStore::new(sample);
    let result = engine
        .layout_line(&store, LineIndex(0), Dip(1000.0))
        .unwrap();
    let first = engine.hit_test_offset(&result, ByteOffset(0)).unwrap().x.0;
    let last = engine
        .hit_test_offset(&result, ByteOffset(sample.len()))
        .unwrap()
        .x
        .0;
    assert!(first > last, "first={first} last={last}");
}

#[test]
fn slice_does_not_read_the_whole_line() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = LongLine {
        text: "x".repeat(100_000_000),
        visited: AtomicUsize::new(0),
    };
    let start = std::time::Instant::now();
    let result = engine
        .layout_slice(
            &store,
            ByteOffset(50_000_000)..ByteOffset(50_000_100),
            Dip(1000.0),
        )
        .unwrap();
    println!(
        "100 MB line slice: {:.3} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(result.rows[0].range.start, ByteOffset(50_000_000));
    assert_eq!(result.rows[0].range.end, ByteOffset(50_000_100));
    assert_eq!(store.visited.load(Ordering::Relaxed), 100);
}

#[test]
fn slice_helper_keeps_joined_emoji_together() {
    let store = StringStore::new("abc 👩‍💻 xyz");
    let range = slice_around(&store, ByteOffset(8), 16).unwrap();
    let selected = &store.as_str()[range.start.0..range.end.0];
    assert!(selected.contains("👩‍💻"), "{selected}");
}

#[test]
fn cache_changes_one_line_and_honors_cap() {
    let engine = DirectWriteLayout::new().unwrap();
    let mut store = StringStore::new("abc\ndef\nghi");
    let mut cache = LineCache::new(1000);
    for i in 0..3 {
        cache
            .get_or_layout(&engine, &store, LineIndex(i), Dip(1000.0))
            .unwrap();
    }
    assert!(cache.memory_bytes() <= 1000);
    let old = store.snapshot();
    let change = store.replace(ByteOffset(4)..ByteOffset(5), "X").unwrap();
    cache.invalidate(old.as_ref(), &change);
    assert_eq!(cache.len(), 2);
}

#[test]
fn cache_shifts_unchanged_lines_after_newline_insert() {
    let engine = DirectWriteLayout::new().unwrap();
    let mut store = StringStore::new("first\nsecond\nthird");
    let mut cache = LineCache::new(10_000);
    for i in 0..3 {
        cache
            .get_or_layout(&engine, &store, LineIndex(i), Dip(1000.0))
            .unwrap();
    }
    let old = store.snapshot();
    let change = store
        .replace(ByteOffset(6)..ByteOffset(6), "new\n")
        .unwrap();
    cache.invalidate(old.as_ref(), &change);
    let shifted = cache
        .get_or_layout(&engine, &store, LineIndex(3), Dip(1000.0))
        .unwrap();
    assert_eq!(shifted.rows[0].range.start, ByteOffset(17));
    assert!(engine.hit_test_offset(&shifted, ByteOffset(17)).is_some());
}

#[test]
fn height_map_matches_plain_vec() {
    let mut map = HeightMap::new(1000, Dip(12.0));
    let mut plain = vec![12.0f32; 1000];
    let mut seed = 7u64;
    for _ in 0..2000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let i = ((seed >> 32) as usize) % 1000;
        let h = ((seed & 31) + 1) as f32;
        map.set(LineIndex(i), Dip(h));
        plain[i] = h;
        let n = ((seed >> 16) as usize) % 1001;
        let expected: f32 = plain[..n].iter().sum();
        assert!((map.line_to_scroll(LineIndex(n)).0 - expected).abs() < 0.05);
        let scroll = expected - 0.1;
        let target = plain
            .iter()
            .scan(0.0, |sum, v| {
                *sum += *v;
                Some(*sum)
            })
            .take_while(|sum| *sum <= scroll)
            .count();
        assert_eq!(map.scroll_to_line(Dip(scroll)), LineIndex(target));
    }
}
