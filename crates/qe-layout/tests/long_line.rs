//! The done-test for long lines, with real limits. The numbers are for a release
//! build. A debug build gets a wider limit, because this code runs slower there.
use qe_api::{
    Affinity, ByteOffset, Dip, Layout, LineIndex, Point, TextPosition, TextSnapshot, Utf16Offset,
};
use qe_layout::{DirectWriteLayout, LONG_LINE_BYTES, LineCache, SEGMENT_BYTES};
use std::{
    alloc::{GlobalAlloc, Layout as AllocLayout, System},
    cell::Cell,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Mutex, MutexGuard},
    time::Instant,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

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

/// Times a call many times and prints the best, median and slowest. The pass rules
/// use the best, because other work on the PC can stop any one call. The median and
/// the slowest are what a frame really sees, so each is printed for the report.
fn timed<T>(name: &str, runs: usize, mut f: impl FnMut() -> T) -> f64 {
    let mut times: Vec<f64> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(f());
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    println!(
        "TIMED {name}: best {:.3} ms, median {:.3} ms, slowest {:.3} ms",
        times[0],
        times[runs / 2],
        times[runs - 1]
    );
    times[0]
}
fn best_of<T>(runs: usize, f: impl FnMut() -> T) -> f64 {
    timed("-", runs, f)
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
            let segment = timed(&format!("{name}, segment {index}"), TRIES, || {
                engine
                    .layout_segment(&text, line, index, Dip(900.0))
                    .unwrap()
            });
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
                    .segment_at(
                        &text,
                        ByteOffset(bytes / 2 / unit.len() * unit.len()),
                        Affinity::After,
                    )
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
                engine
                    .segment_at(&text, range.start, Affinity::After)
                    .unwrap()
                    .1,
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

/// Wrapped ASCII went from 2.5 ms at 4 KB to 631 ms at 64 KB. It must now grow with
/// the text. Shaping is timed through slices, its longest whole form.
#[test]
fn layout_time_grows_with_the_text_not_its_square() {
    let _one = one_at_a_time();
    let engine = DirectWriteLayout::new().unwrap();
    let shaped = DirectWriteLayout::new().unwrap().with_shaping(true);
    let time = |engine: &DirectWriteLayout, unit: &str, bytes: usize| {
        let text = LongLine::new(unit, bytes);
        let end = ByteOffset(text.text.len());
        best_of(TRIES, || {
            engine
                .layout_slice(&text, ByteOffset(0)..end, Dip(800.0))
                .unwrap()
        })
    };
    let ascii = "word wrap test ";
    let (small, large) = (
        time(&engine, ascii, 4096),
        time(&engine, ascii, 16 * 1024 - 64),
    );
    println!("wrapped ASCII: {small:.3} ms at 4 KB, {large:.3} ms at 16 KB");
    assert!(large < 2.0 * LIMIT_SCALE, "{large} ms");
    assert!(large < small * 8.0 + 1.0, "{small} ms then {large} ms");
    for (name, engine, unit) in [
        ("shaped ASCII", &shaped, ascii),
        ("Arabic", &engine, SAMPLES[2].1),
    ] {
        let (small, large) = (time(engine, unit, 1024), time(engine, unit, 4000));
        println!("{name}: {small:.3} ms at 1 KB, {large:.3} ms at 4 KB");
        // Four times the text. Quadratic work would be 16 times the time.
        assert!(
            large < small * 8.0 + 1.0,
            "{name}: {small} ms then {large} ms"
        );
    }
}

/// Text that costs DirectWrite more per byte, with the numbers the review measured.
const MIXED_TEXTS: [(&str, &str); 6] = [
    ("a and CJK", "a\u{6F22}"),
    ("a and Hebrew", "a\u{5E9}"),
    (
        "Japanese with words",
        "\u{65E5}\u{672C}\u{8A9E}\u{306E}\u{6587}\u{7AE0} ",
    ),
    ("CJK with spaces", "\u{6F22}\u{5B57} "),
    (
        "Arabic",
        "\u{645}\u{631}\u{62D}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645} ",
    ),
    ("Hebrew and digits", "\u{5E9}\u{5DC}\u{5D5}\u{5DD} 123 "),
];
/// These meet the 5 ms limit for one cold segment.
const WITHIN_FIVE_MS: [&str; 2] = ["Arabic", "Hebrew and digits"];
/// The rest cannot meet it at a segment size that keeps rows readable, so they rely on
/// workers. Each has a ceiling here, which only catches a new slowdown.
const CEILING_MS: f64 = 30.0;

#[test]
fn mixed_script_segments_meet_their_limits() {
    let _one = one_at_a_time();
    let engine = DirectWriteLayout::new().unwrap();
    for (name, unit) in MIXED_TEXTS {
        let text = LongLine::new(unit, 1_000_000);
        let middle = engine.segment_count(&text, LineIndex(0)).unwrap() / 2;
        let ms = timed(&format!("{name}, mixed segment"), TRIES, || {
            engine
                .layout_segment(&text, LineIndex(0), middle, Dip(800.0))
                .unwrap()
        });
        let limit = if WITHIN_FIVE_MS.contains(&name) {
            SEGMENT_LIMIT_MS
        } else {
            CEILING_MS * LIMIT_SCALE
        };
        assert!(ms < limit, "{name}: {ms} ms against {limit} ms");
    }
}

#[test]
fn only_fast_lines_stay_whole_past_the_shaping_limit() {
    let engine = DirectWriteLayout::new().unwrap();
    let shaped = DirectWriteLayout::new().unwrap().with_shaping(true);
    let line = LineIndex(0);
    let ascii = LongLine::new("word wrap test ", 16 * 1024 - 64);
    assert!(
        !engine
            .layout_line(&ascii, line, Dip(800.0))
            .unwrap()
            .partial
    );
    assert!(
        shaped
            .layout_line(&ascii, line, Dip(800.0))
            .unwrap()
            .partial
    );
    let tabs = LongLine::new("ab\tcd\t", 16 * 1024 - 64);
    assert!(!engine.is_long_line(&tabs, line).unwrap());
    let mut mixed = "x".repeat(6000);
    mixed.push('\u{6F22}');
    let mixed = LongLine {
        text: mixed,
        visited: AtomicUsize::new(0),
    };
    assert!(
        engine
            .layout_line(&mixed, line, Dip(800.0))
            .unwrap()
            .partial
    );
    let short = LongLine::new("\u{6F22}\u{5B57}", 4000);
    assert!(
        !engine
            .layout_line(&short, line, Dip(800.0))
            .unwrap()
            .partial
    );
    let over = LongLine::new("\u{6F22}\u{5B57}", 4200);
    assert!(engine.layout_line(&over, line, Dip(800.0)).unwrap().partial);
    assert!(matches!(
        engine.layout_slice_from_column(&over, ByteOffset(0)..ByteOffset(4200), Dip(800.0), 0),
        Err(qe_layout::LayoutError::SliceTooLong {
            limit: qe_layout::SHAPED_LINE_BYTES,
            ..
        })
    ));
}

// Round 3, item 5.
#[test]
fn the_caret_stays_put_across_a_segment_edge() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = LongLine::new("x", 100_000);
    let line = LineIndex(0);
    let width = Dip(400.0);
    let layout = engine.layout_segment(&text, line, 0, width).unwrap();
    let bottom = layout.rows.last().unwrap();
    let click = Point {
        x: Dip(399.0),
        y: Dip(bottom.y.0 + 1.0),
    };
    let hit = engine.hit_test_point(
        &layout,
        Point {
            x: Dip(9000.0),
            ..click
        },
    );
    assert_eq!(hit.byte, bottom.range.end);
    assert_eq!(hit.affinity, Affinity::Before);
    let (found_line, index, range) = engine.segment_at(&text, hit.byte, hit.affinity).unwrap();
    assert_eq!((found_line, index), (line, 0));
    assert_eq!(range.end, hit.byte);
    let caret = layout_caret(&engine, &text, hit);
    assert_eq!(
        caret.y, bottom.y,
        "the caret stays on the row that was clicked"
    );
    let end_x = bottom.caret_stops.last().unwrap().x;
    assert_eq!(caret.x, end_x);
    assert!(end_x.0 > 0.0);

    let (_, after, range) = engine.segment_at(&text, hit.byte, Affinity::After).unwrap();
    assert_eq!(after, 1);
    assert_eq!(range.start, hit.byte);
    let next = engine.layout_segment(&text, line, 1, width).unwrap();
    let caret = engine.caret_rect(&next, hit.byte, Affinity::After).unwrap();
    assert_eq!((caret.x, caret.y), (Dip(0.0), Dip(0.0)));
    // The first segment has nothing before it.
    let (_, first, _) = engine
        .segment_at(&text, ByteOffset(0), Affinity::Before)
        .unwrap();
    assert_eq!(first, 0);
}
fn layout_caret(engine: &DirectWriteLayout, text: &LongLine, hit: qe_api::HitTest) -> qe_api::Rect {
    let (line, index, _) = engine.segment_at(text, hit.byte, hit.affinity).unwrap();
    let layout = engine
        .layout_segment(text, line, index, Dip(400.0))
        .unwrap();
    engine.caret_rect(&layout, hit.byte, hit.affinity).unwrap()
}

// Round 3, item 6.
#[test]
fn flags_are_whole_at_any_offset() {
    let engine = DirectWriteLayout::new().unwrap();
    let line = LineIndex(0);
    let flag = "\u{1F1FA}\u{1F1F8}";
    for prefix in [
        "", "a", "ab", "abc", "abcd", "abcde", "abcdefg", "\u{6F22}", "x y z ",
    ] {
        let text = LongLine {
            text: prefix.to_string() + &flag.repeat(4000),
            visited: AtomicUsize::new(0),
        };
        let on_flag_edge = |at: usize| {
            at == text.text.len()
                || at <= prefix.len()
                || (at - prefix.len()).is_multiple_of(flag.len())
        };
        let count = engine.segment_count(&text, line).unwrap();
        for index in 0..count {
            let range = engine.segment_range(&text, line, index).unwrap();
            assert!(
                on_flag_edge(range.start.0) || index == 0,
                "{prefix:?} {index}: {range:?}"
            );
            assert!(on_flag_edge(range.end.0), "{prefix:?} {index}: {range:?}");
            let stops = engine
                .layout_segment(&text, line, index, Dip(900.0))
                .unwrap();
            for row in &stops.rows {
                for stop in &row.caret_stops {
                    assert!(
                        stop.byte.0 < prefix.len() || on_flag_edge(stop.byte.0),
                        "{stop:?}"
                    );
                }
            }
        }
        let good = prefix.len() + 800 * flag.len();
        let slice = ByteOffset(good)..ByteOffset(good + 10 * flag.len());
        assert!(
            engine
                .layout_slice_from_column(&text, slice, Dip(900.0), 0)
                .is_ok(),
            "{prefix:?}: a slice on true flag edges is refused"
        );
        let cut = ByteOffset(good + 4)..ByteOffset(good + 12);
        assert!(
            engine
                .layout_slice_from_column(&text, cut, Dip(900.0), 0)
                .is_err(),
            "{prefix:?}: a slice inside a flag is taken"
        );
    }
}

fn right_edge_of_ink(row: &qe_api::LayoutRow) -> f32 {
    row.glyph_runs
        .iter()
        .map(|run| run.origin.x.0 + run.advances.iter().map(|a| a.0).sum::<f32>())
        .fold(0.0, f32::max)
}

// Round 3, item 7.
#[test]
fn rows_stay_inside_the_width_on_both_routes() {
    let width = 300.0;
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
        let line = LineIndex(0);
        let text = LongLine::new("ab\tcd\tef", 40_000);
        let slice = engine
            .layout_slice_from_column(&text, ByteOffset(0)..ByteOffset(800), Dip(width), 5)
            .unwrap();
        assert!(slice.rows.len() > 3);
        for row in &slice.rows {
            let right = right_edge_of_ink(row);
            assert!(right <= width + 0.01, "{family} slice row reaches {right}");
        }
        let plain =
            engine.layout_slice_from_column(&text, ByteOffset(0)..ByteOffset(800), Dip(width), 0);
        assert!(plain.unwrap().rows.len() <= slice.rows.len() + 1);
        for index in 1..6 {
            let layout = engine
                .layout_segment(&text, line, index, Dip(width))
                .unwrap();
            for row in &layout.rows {
                let right = right_edge_of_ink(row);
                assert!(
                    right <= width + 0.01,
                    "{family} segment {index} row reaches {right}"
                );
            }
        }
    }
}

// Round 3, item 8.
#[test]
fn estimates_are_close_for_any_text() {
    let line = LineIndex(0);
    for wrap in [true, false] {
        let engine = DirectWriteLayout::new().unwrap().with_wrap(wrap);
        for (name, unit) in [
            ("ASCII", "word wrap test "),
            ("CJK", "\u{6F22}\u{5B57}\u{304B}\u{306A}"),
            ("emoji", "\u{1F600}\u{1F4BB} "),
            ("tabs", "ab\tcd\t"),
            ("Arabic", SAMPLES[2].1),
            ("a and CJK", "a\u{6F22}"),
        ] {
            let text = LongLine::new(unit, 120_000);
            let map = engine.long_line_map(&text, line, Dip(800.0)).unwrap();
            let count = engine.segment_count(&text, line).unwrap();
            assert_eq!(map.len(), count);
            let real: f64 = (0..count)
                .map(|s| {
                    let layout = engine.layout_segment(&text, line, s, Dip(800.0)).unwrap();
                    engine.segment_extent(&layout).0 as f64
                })
                .sum();
            let ratio = real / map.total_height();
            println!("wrap {wrap}, {name}: real over estimate {ratio:.3}");
            assert!((0.9..=1.1).contains(&ratio), "wrap {wrap}, {name}: {ratio}");
        }
    }
}

// Round 3, item 9.
#[test]
fn a_long_line_never_ends_with_an_empty_segment() {
    let engine = DirectWriteLayout::new().unwrap();
    let line = LineIndex(0);
    for tail in 0..80usize {
        for base in [18_440usize, 20_480 - 40] {
            let mut s = "x".repeat(base + tail);
            s.push(' ');
            let text = LongLine {
                text: s,
                visited: AtomicUsize::new(0),
            };
            let count = engine.segment_count(&text, line).unwrap();
            let last = engine.segment_range(&text, line, count - 1).unwrap();
            assert!(last.end > last.start, "{}: {last:?}", text.text.len());
            assert_eq!(last.end.0, text.text.len());
            let layout = engine
                .layout_segment(&text, line, count - 1, Dip(800.0))
                .unwrap();
            assert!(layout.rows.iter().all(|r| r.range.end > r.range.start));
        }
    }
}

// Round 3, item 11.
#[test]
fn with_wrap_off_a_long_line_is_one_row_tall() {
    let engine = DirectWriteLayout::new().unwrap().with_wrap(false);
    let text = LongLine::new("word wrap test ", 100_000);
    let line = LineIndex(0);
    let map = engine.long_line_map(&text, line, Dip(800.0)).unwrap();
    assert!(
        map.total_height() > 100_000.0,
        "the map holds widths: {}",
        map.total_height()
    );
    let height = engine.long_line_height(&map).0;
    let layout = engine.layout_segment(&text, line, 0, Dip(800.0)).unwrap();
    assert!((height - layout.height.0).abs() < 0.01, "{height}");
    let wrapped = DirectWriteLayout::new().unwrap();
    let map = wrapped.long_line_map(&text, line, Dip(800.0)).unwrap();
    assert_eq!(
        wrapped.long_line_height(&map),
        Dip(map.total_height() as f32)
    );
}

// Round 3, item 10.
#[test]
fn an_edit_keeps_measured_sizes() {
    let engine = DirectWriteLayout::new().unwrap();
    let line = LineIndex(0);
    let width = Dip(800.0);
    let mut store = qe_api::testing::StringStore::new("word wrap test ".repeat(4000));
    let mut map = engine.long_line_map(&store, line, width).unwrap();
    let measured = Dip(1234.5);
    map.set(LineIndex(3), measured).unwrap();
    map.set(LineIndex(20), measured).unwrap();
    let count = map.len();
    use qe_api::TextStore;
    store
        .replace(ByteOffset(100)..ByteOffset(100), &"more ".repeat(3000))
        .unwrap();
    engine.update_long_line_map(&mut map, &store, line).unwrap();
    assert!(map.len() > count);
    assert_eq!(map.len(), engine.segment_count(&store, line).unwrap());
    assert_eq!(map.line_height(LineIndex(3)), Some(measured));
    assert_eq!(map.line_height(LineIndex(20)), Some(measured));
    let end = store.len_bytes().0;
    store
        .replace(ByteOffset(30_000)..ByteOffset(end), "")
        .unwrap();
    engine.update_long_line_map(&mut map, &store, line).unwrap();
    assert_eq!(map.len(), engine.segment_count(&store, line).unwrap());
    assert_eq!(map.line_height(LineIndex(3)), Some(measured));
}

/// Round 3, item 8: the crate documentation's rule keeps the view still.
#[test]
fn correcting_a_segment_above_keeps_the_view_still() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = LongLine::new("\u{6F22}\u{5B57}\u{304B}\u{306A}", 200_000);
    let line = LineIndex(0);
    let mut map = engine.long_line_map(&text, line, Dip(800.0)).unwrap();
    let top_segment = LineIndex(40);
    let mut scroll = map.line_to_scroll(top_segment) + 12.5;
    for above in [5, 39, 2, 17] {
        let before = map.line_to_scroll(top_segment);
        map.set(
            LineIndex(above),
            Dip(map.line_height(LineIndex(above)).unwrap().0 + 77.25),
        )
        .unwrap();
        let after = map.line_to_scroll(top_segment);
        scroll += after - before;
        assert_eq!(map.scroll_to_line(scroll), top_segment);
        assert!((scroll - map.line_to_scroll(top_segment) - 12.5).abs() < 1e-6);
    }
}

/// With `with_full_rows_at`, a long line wraps with no short row where segments meet.
#[test]
fn full_rows_leave_no_short_row_at_a_segment_edge() {
    let width = Dip(820.0);
    let engine = DirectWriteLayout::new().unwrap().with_full_rows_at(width);
    let cell = engine.cell_width().0;
    let cells = (width.0 / cell).floor() as usize;
    for (name, text, slack) in [
        (
            "prose",
            "The quick brown fox jumps over the lazy dog. ".repeat(3000),
            14,
        ),
        ("no spaces", "x".repeat(120_000), 0),
        (
            "long words",
            format!("{} ", "abcdefghijklmnopqrstuvwxyz0123456789").repeat(4000),
            // Rows hold whole words of 36. An odd count leaves one row with one word.
            68,
        ),
    ] {
        let store = LongLine::new(&text, text.len());
        assert!(engine.is_long_line(&store, LineIndex(0)).unwrap());
        let count = engine.segment_count(&store, LineIndex(0)).unwrap();
        assert!(count > 10);
        let mut next_start = ByteOffset(0);
        for index in 0..count {
            let layout = engine
                .layout_segment(&store, LineIndex(0), index, width)
                .unwrap();
            for (at, row) in layout.rows.iter().enumerate() {
                assert_eq!(
                    row.range.start, next_start,
                    "{name} segment {index} row {at}"
                );
                next_start = row.range.end;
                let used = text[row.range.start.0..row.range.end.0].trim_end().len();
                let last_of_line = index + 1 == count && at + 1 == layout.rows.len();
                assert!(
                    last_of_line || used + slack >= cells,
                    "{name} segment {index} row {at} holds {used} of {cells}"
                );
                assert!(
                    used <= cells,
                    "{name} segment {index} row {at} holds {used}"
                );
            }
        }
        assert_eq!(next_start.0, text.len(), "{name}: every byte is in one row");
    }
}

/// The result depends on the segment's text and the width alone, so any segment lays
/// out in the same time and to the same rows, whatever came before it.
#[test]
fn full_rows_do_not_depend_on_which_segment_came_first() {
    let width = Dip(620.0);
    let engine = DirectWriteLayout::new().unwrap().with_full_rows_at(width);
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(3000);
    let store = LongLine::new(&text, text.len());
    let far = engine.segment_count(&store, LineIndex(0)).unwrap() - 2;
    let first = engine
        .layout_segment(&store, LineIndex(0), far, width)
        .unwrap();
    let _ = engine
        .layout_segment(&store, LineIndex(0), 0, width)
        .unwrap();
    let again = engine
        .layout_segment(&store, LineIndex(0), far, width)
        .unwrap();
    assert_eq!(first, again);
}

/// The plain engine keeps its edges, so a caller who never asks sees no change.
#[test]
fn full_rows_are_only_for_who_asks() {
    let plain = DirectWriteLayout::new().unwrap();
    let store = LongLine::new("x", 100_000);
    let range = plain.segment_range(&store, LineIndex(0), 1).unwrap();
    assert_eq!(range.start.0, qe_layout::SEGMENT_BYTES);
}

/// Choosing the last rows again stays inside the budget for one segment, and it does not
/// depend on the length of the line.
#[test]
fn full_rows_stay_inside_the_segment_budget() {
    let _guard = one_at_a_time();
    let width = Dip(820.0);
    let engine = DirectWriteLayout::new().unwrap().with_full_rows_at(width);
    let store = LongLine::new("The quick brown fox jumps over the lazy dog. ", MB100);
    let count = engine.segment_count(&store, LineIndex(0)).unwrap();
    let mut worst = 0.0_f64;
    for index in [0, 1, count / 2, count - 2] {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let started = Instant::now();
            engine
                .layout_segment(&store, LineIndex(0), index, width)
                .unwrap();
            best = best.min(started.elapsed().as_secs_f64() * 1000.0);
        }
        worst = worst.max(best);
    }
    eprintln!("a segment with its rows chosen again: {worst:.3} ms at worst");
    assert!(worst < SEGMENT_LIMIT_MS, "{worst} ms");
}
