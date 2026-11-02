//! The done-test for long lines, with real limits. The numbers are for a release
//! build. A debug build gets a wider limit, because this code runs slower there.
use qe_api::{ByteOffset, Dip, Layout, LineIndex, Point, TextPosition, TextSnapshot, Utf16Offset};
use qe_layout::{DirectWriteLayout, LONG_LINE_BYTES, LineCache, SEGMENT_BYTES};
use std::{
    alloc::{GlobalAlloc, Layout as AllocLayout, System},
    cell::Cell,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Mutex, MutexGuard},
    time::Instant,
};

/// Counts what Rust code allocates on each thread, so another test's big string
/// does not count. DirectWrite's own memory is not counted.
struct Counting;
thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}
fn add_allocated(bytes: usize) {
    // During thread shutdown the counter may be gone, and that memory does not matter.
    let _ = ALLOCATED.try_with(|count| count.set(count.get() + bytes));
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
        add_allocated(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: AllocLayout, new_size: usize) -> *mut u8 {
        add_allocated(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;
/// Timed tests run one at a time, so that they do not disturb each other's counts.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// One failed test must not fail the others through a broken lock.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|broken| broken.into_inner())
}

/// The PC may be busy with other work. The best of this many tries shows what the
/// code costs, and the limit stays the real limit.
const TRIES: usize = 40;

const LIMIT_SCALE: f64 = if cfg!(debug_assertions) { 6.0 } else { 1.0 };
const SEGMENT_LIMIT_MS: f64 = 5.0 * LIMIT_SCALE;
const HIT_LIMIT_MS: f64 = 1.0 * LIMIT_SCALE;
const MB100: usize = 100_000_000;

struct LongLine {
    text: String,
    visited: AtomicUsize,
}
impl LongLine {
    fn new(unit: &str, bytes: usize) -> Self {
        Self {
            text: unit.repeat(bytes / unit.len()),
            visited: AtomicUsize::new(0),
        }
    }
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
        self.text
            .is_char_boundary(byte.0)
            .then_some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, unit: Utf16Offset) -> Option<ByteOffset> {
        self.text
            .is_char_boundary(unit.0)
            .then_some(ByteOffset(unit.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.text.is_char_boundary(byte.0).then_some(TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(byte.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        (pos.line.0 == 0).then_some(ByteOffset(pos.column.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 == 0).then_some(ByteOffset(0)..ByteOffset(self.text.len()))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        match self.text.get(range.start.0..range.end.0) {
            Some(s) => {
                self.visited.fetch_add(s.len(), Ordering::Relaxed);
                visitor(s);
                true
            }
            None => false,
        }
    }
    fn is_read_only(&self) -> bool {
        true
    }
}

fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..runs {
        let start = Instant::now();
        std::hint::black_box(f());
        best = best.min(start.elapsed().as_secs_f64() * 1000.0);
    }
    best
}

const SAMPLES: [(&str, &str); 3] = [
    ("ASCII words", "word wrap test "),
    ("ASCII one word", "x"),
    (
        "Arabic",
        "\u{645}\u{631}\u{62D}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645} ",
    ),
];

#[test]
fn a_100_mb_line_meets_every_limit() {
    let _one = one_at_a_time();
    let engine = DirectWriteLayout::new().unwrap();
    for (name, unit) in SAMPLES {
        let text = LongLine::new(unit, MB100);
        let line = LineIndex(0);
        assert!(engine.is_long_line(&text, line).unwrap());
        let count = engine.segment_count(&text, line).unwrap();
        assert!(count > 20_000, "{name}: {count}");
        for index in [0, 1, count / 2, count - 1] {
            let visited = text.visited.load(Ordering::Relaxed);
            let segment = best_of(TRIES, || {
                engine
                    .layout_segment(&text, line, index, Dip(900.0))
                    .unwrap()
            });
            println!("{name}, segment {index}: {segment:.3} ms");
            assert!(
                segment < SEGMENT_LIMIT_MS,
                "{name} segment {index}: {segment} ms"
            );
            let per_call = (text.visited.load(Ordering::Relaxed) - visited) / TRIES;
            assert!(
                per_call < 3 * SEGMENT_BYTES,
                "{name}: read {per_call} bytes per layout"
            );

            let layout = engine
                .layout_segment(&text, line, index, Dip(900.0))
                .unwrap();
            assert!(layout.partial);
            let point = Point {
                x: Dip(450.0),
                y: Dip(layout.height.0 / 2.0),
            };
            let hit = best_of(TRIES, || engine.hit_test_point(&layout, point));
            println!("{name}, hit test: {hit:.4} ms");
            assert!(hit < HIT_LIMIT_MS, "{name} hit test: {hit} ms");
            let target = engine.hit_test_point(&layout, point);
            let caret = best_of(TRIES, || {
                engine.caret_rect(&layout, target.byte, target.affinity)
            });
            assert!(caret < HIT_LIMIT_MS, "{name} caret: {caret} ms");
            assert!(
                engine
                    .caret_rect(&layout, target.byte, target.affinity)
                    .is_some()
            );
        }
    }
}

#[test]
fn no_call_allocates_in_proportion_to_the_line() {
    let _one = one_at_a_time();
    let engine = DirectWriteLayout::new().unwrap();
    for (name, unit) in SAMPLES {
        let measure = |bytes: usize| {
            let text = LongLine::new(unit, bytes);
            let line = LineIndex(0);
            let count = engine.segment_count(&text, line).unwrap();
            let at = |f: &mut dyn FnMut()| {
                let before = ALLOCATED.get();
                f();
                ALLOCATED.get() - before
            };
            let middle = count / 2;
            let mut biggest = 0;
            biggest = biggest.max(at(&mut || {
                engine.layout_line(&text, line, Dip(900.0)).unwrap();
            }));
            biggest = biggest.max(at(&mut || {
                engine
                    .layout_segment(&text, line, middle, Dip(900.0))
                    .unwrap();
            }));
            biggest = biggest.max(at(&mut || {
                engine.long_line_map(&text, line, Dip(900.0)).unwrap();
            }));
            biggest = biggest.max(at(&mut || {
                engine
                    .segment_at(&text, ByteOffset(bytes / 2 / unit.len() * unit.len()))
                    .unwrap();
            }));
            let layout = engine
                .layout_segment(&text, line, middle, Dip(900.0))
                .unwrap();
            biggest = biggest.max(at(&mut || {
                engine.hit_test_point(&layout, Point::default());
            }));
            biggest
        };
        let small = measure(1_000_000);
        let large = measure(MB100);
        println!("{name}: most Rust memory in one call: {small} bytes at 1 MB, {large} at 100 MB");
        assert!(large < 2 * 1024 * 1024, "{name}: {large} bytes");
        assert!(
            large < small * 2,
            "{name}: {small} at 1 MB and {large} at 100 MB"
        );
    }
}

#[test]
fn layout_line_gives_only_the_first_segment_of_a_long_line() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = LongLine::new("x", MB100);
    let layout = engine.layout_line(&text, LineIndex(0), Dip(900.0)).unwrap();
    assert!(layout.partial);
    let end = layout.rows.last().unwrap().range.end.0;
    assert!((SEGMENT_BYTES..=2 * SEGMENT_BYTES).contains(&end), "{end}");
    assert!(text.visited.load(Ordering::Relaxed) < 3 * SEGMENT_BYTES);
    // A line right at the threshold is still whole.
    let edge = LongLine::new("x", LONG_LINE_BYTES);
    let whole = engine.layout_line(&edge, LineIndex(0), Dip(900.0)).unwrap();
    assert!(!whole.partial);
    assert_eq!(whole.rows.last().unwrap().range.end.0, LONG_LINE_BYTES);
    let over = LongLine::new("x", LONG_LINE_BYTES + 1);
    assert!(
        engine
            .layout_line(&over, LineIndex(0), Dip(900.0))
            .unwrap()
            .partial
    );
    assert!(
        engine
            .layout_slice(&text, ByteOffset(0)..ByteOffset(MB100), Dip(900.0))
            .is_none()
    );
}

#[test]
fn segments_cover_the_line_without_gaps_and_cut_no_cluster() {
    let engine = DirectWriteLayout::new().unwrap();
    for unit in [
        "plain words ",
        "x",
        "\u{1F1FA}\u{1F1F8}",
        "\u{1F469}\u{200D}\u{1F4BB}",
        "e\u{301}",
        "\u{1100}\u{1161}\u{11A8}",
        "\u{644}\u{627}",
        "1\u{FE0F}\u{20E3}",
    ] {
        let text = LongLine::new(unit, 300_000);
        let line = LineIndex(0);
        let count = engine.segment_count(&text, line).unwrap();
        let mut at = 0;
        for index in 0..count {
            let range = engine.segment_range(&text, line, index).unwrap();
            assert_eq!(range.start.0, at, "{unit:?} segment {index}");
            assert!(range.end.0 > range.start.0, "{unit:?} segment {index}");
            assert!(range.end.0 - range.start.0 <= SEGMENT_BYTES + 1024);
            assert!(
                unit.contains(' ') || range.end.0.is_multiple_of(unit.len()),
                "{unit:?}: segment {index} ends inside a cluster at {}",
                range.end.0
            );
            assert_eq!(
                engine.segment_at(&text, range.start).unwrap().1,
                index,
                "{unit:?}"
            );
            at = range.end.0;
        }
        assert_eq!(at, text.text.len());
    }
}

#[test]
fn one_cluster_longer_than_a_segment_still_gives_bounded_segments() {
    let engine = DirectWriteLayout::new().unwrap();
    let mut s = String::from("a");
    s.push_str(&"\u{301}".repeat(100_000));
    let text = LongLine {
        text: s,
        visited: AtomicUsize::new(0),
    };
    let line = LineIndex(0);
    let count = engine.segment_count(&text, line).unwrap();
    for index in 0..count {
        let range = engine.segment_range(&text, line, index).unwrap();
        assert!(range.end.0 - range.start.0 <= SEGMENT_BYTES + 1024);
    }
    assert!(
        engine
            .layout_segment(&text, line, count / 2, Dip(900.0))
            .is_ok()
    );
}

#[test]
fn tabs_stay_aligned_across_segments() {
    let engine = DirectWriteLayout::new().unwrap().with_wrap(false);
    let cell = engine.cell_width().0;
    let unit = "ab\tcd\tefg\t";
    let text = LongLine::new(unit, 200_000);
    let line = LineIndex(0);
    for index in 1..5 {
        let layout = engine
            .layout_segment(&text, line, index, Dip(900.0))
            .unwrap();
        let start = layout.rows[0].range.start.0;
        // In the whole line, the start of each unit sits on a tab stop.
        let unit_start = start.next_multiple_of(unit.len());
        let stop = layout.rows[0]
            .caret_stops
            .iter()
            .find(|s| s.byte.0 == unit_start)
            .unwrap();
        let first = layout.rows[0].caret_stops[1].x.0 - layout.rows[0].caret_stops[0].x.0;
        assert!(first > 0.0);
        let column = (start - start / unit.len() * unit.len()) as isize;
        let lead = whole_line_column(unit, column as usize) % 8;
        let x_on_stops = (stop.x.0 + lead as f32 * cell) / (8.0 * cell);
        assert!(
            (x_on_stops - x_on_stops.round()).abs() < 0.001,
            "segment {index}: {x_on_stops}"
        );
    }
}
fn whole_line_column(unit: &str, bytes: usize) -> usize {
    let mut column = 0;
    for byte in unit.as_bytes()[..bytes].iter() {
        column = if *byte == b'\t' {
            (column / 8 + 1) * 8
        } else {
            column + 1
        };
    }
    column
}

/// The steps the crate documentation tells the editor to follow.
#[test]
fn scrolling_a_100_mb_line_as_documented() {
    let _one = one_at_a_time();
    let engine = DirectWriteLayout::new().unwrap();
    let text = LongLine::new("word wrap test ", MB100);
    let line = LineIndex(0);
    let width = Dip(800.0);
    let mut map = engine.long_line_map(&text, line, width).unwrap();
    assert!(map.memory_bytes() < 4096, "{}", map.memory_bytes());
    let mut cache = LineCache::new(16 * 1024 * 1024);
    let estimate = map.total_height();
    let window = 800.0;
    let mut scroll = estimate / 2.0;
    let mut steps = Vec::new();
    for _ in 0..60 {
        let start = Instant::now();
        let mut index = map.scroll_to_line(scroll).0;
        let mut top = map.line_to_scroll(LineIndex(index));
        while top < scroll + window && index < map.len() {
            let layout = cache
                .get_or_layout_segment(&engine, &text, line, index, width)
                .unwrap();
            map.set(LineIndex(index), engine.segment_extent(&layout))
                .unwrap();
            top = map.line_to_scroll(LineIndex(index)) + layout.height.0 as f64;
            index += 1;
        }
        steps.push(start.elapsed().as_secs_f64() * 1000.0);
        scroll += window;
    }
    steps.sort_by(f64::total_cmp);
    let (middle, slowest) = (steps[steps.len() / 2], steps[steps.len() - 1]);
    println!("scroll step: median {middle:.3} ms, slowest {slowest:.3} ms");
    assert!(middle < 3.0 * SEGMENT_LIMIT_MS, "{middle} ms");
    assert!(cache.memory_bytes() <= 16 * 1024 * 1024);
    let corrected = map.total_height();
    assert!(
        (corrected - estimate).abs() / estimate < 0.2,
        "estimate {estimate}, after measuring {corrected}"
    );
    let warm = best_of(TRIES, || {
        cache
            .get_or_layout_segment(
                &engine,
                &text,
                line,
                map.scroll_to_line(scroll - window).0,
                width,
            )
            .unwrap()
    });
    assert!(warm < 0.5, "warm segment: {warm} ms");
}

/// Wrapped ASCII went from 2.5 ms at 4 KB to 631 ms at 64 KB. It must now grow
/// with the text, and the fast path must stay well under what shaping costs.
#[test]
fn layout_time_grows_with_the_text_not_its_square() {
    let _one = one_at_a_time();
    let engine = DirectWriteLayout::new().unwrap();
    let shaped = DirectWriteLayout::new().unwrap().with_shaping(true);
    let arabic = SAMPLES[2].1;
    let cases: [(&str, &DirectWriteLayout, &str, f64); 3] = [
        ("wrapped ASCII", &engine, "word wrap test ", 2.0),
        ("shaped ASCII", &shaped, "word wrap test ", 25.0),
        ("Arabic", &engine, arabic, 40.0),
    ];
    for (name, engine, unit, limit_ms) in cases {
        let time = |bytes: usize| {
            let text = LongLine::new(unit, bytes);
            best_of(TRIES, || {
                engine.layout_line(&text, LineIndex(0), Dip(800.0)).unwrap()
            })
        };
        let (small, large) = (time(4096), time(16 * 1024 - 64));
        println!("{name}: {small:.3} ms at 4 KB, {large:.3} ms at 16 KB");
        assert!(
            large < limit_ms * LIMIT_SCALE,
            "{name}: {large} ms at 16 KB"
        );
        // Four times the text. Quadratic work would be 16 times the time.
        assert!(
            large < small * 8.0 + 1.0,
            "{name}: {small} ms then {large} ms"
        );
    }
}
