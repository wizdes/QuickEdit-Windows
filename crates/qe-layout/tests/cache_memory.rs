//! Finding B: the cache's own count must match what a counting allocator sees.
use qe_api::{ByteOffset, Dip, LineIndex, TextPosition, TextSnapshot, Utf16Offset};
use qe_layout::{DirectWriteLayout, LineCache};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    ops::Range,
};

/// Live Rust bytes per thread, so that tests running side by side do not mix.
struct Counting;
thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}
fn add(bytes: isize) {
    // During thread shutdown the counter may be gone, and that memory does not matter.
    let _ = LIVE.try_with(|live| live.set(live.get() + bytes));
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        add(-(layout.size() as isize));
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size as isize - layout.size() as isize);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn live() -> isize {
    LIVE.get()
}

/// A document of equal lines, with lookups a real buffer could match.
struct Document {
    lines: usize,
    line: String,
}
fn document(lines: usize, columns: usize) -> Document {
    Document {
        lines,
        line: "x".repeat(columns)
            + "
",
    }
}
impl TextSnapshot for Document {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.lines * self.line.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.lines * self.line.len())
    }
    fn line_count(&self) -> usize {
        self.lines
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        Some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, unit: Utf16Offset) -> Option<ByteOffset> {
        Some(ByteOffset(unit.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        Some(TextPosition {
            line: LineIndex(byte.0 / self.line.len()),
            column: Utf16Offset(byte.0 % self.line.len()),
        })
    }
    fn position_to_byte(&self, position: TextPosition) -> Option<ByteOffset> {
        Some(ByteOffset(
            position.line.0 * self.line.len() + position.column.0,
        ))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 < self.lines).then(|| {
            ByteOffset(line.0 * self.line.len())..ByteOffset((line.0 + 1) * self.line.len())
        })
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        let size = self.line.len();
        let mut at = range.start.0;
        while at < range.end.0 {
            let offset = at % size;
            let end = (offset + range.end.0 - at).min(size);
            visitor(&self.line[offset..end]);
            at += end - offset;
        }
        true
    }
    fn is_read_only(&self) -> bool {
        true
    }
}

fn fill(cache: &mut LineCache, engine: &DirectWriteLayout, text: &Document, lines: Range<usize>) {
    for line in lines {
        cache
            .get_or_layout(engine, text, LineIndex(line), Dip(100_000.0))
            .unwrap();
    }
}

/// What the test harness and `println!` keep on this thread: their buffers count too.
const NOISE: isize = 512;

/// The count must not be under the truth, nor far over it.
fn assert_count_is_true(cache: &LineCache, actual: isize, cap: usize, stage: &str) {
    let reported = cache.memory_bytes() as isize;
    println!("{stage}: reported {reported}, actual {actual}, cap {cap}");
    assert!(
        reported <= cap as isize,
        "{stage}: reported {reported} over cap {cap}"
    );
    assert!(
        actual <= cap as isize + NOISE,
        "{stage}: actual {actual} over cap {cap}"
    );
    assert!(
        reported + NOISE >= actual,
        "{stage}: reported {reported} under actual {actual}"
    );
    assert!(
        reported <= actual + actual / 20 + NOISE,
        "{stage}: reported {reported} far over actual {actual}"
    );
}

#[test]
fn the_count_matches_a_counting_allocator() {
    let engine = DirectWriteLayout::new().unwrap();
    let cap = 2_000_000;
    let short = document(4000, 40);
    let long = document(800, 2000);
    // Warm the font and this thread's text format, so they are not counted.
    engine
        .try_layout_line(&short, LineIndex(0), Dip(1000.0))
        .unwrap();

    let start = live();
    let mut cache = LineCache::new(cap);
    fill(&mut cache, &engine, &short, 0..4000);
    assert_count_is_true(&cache, live() - start, cap, "after filling");
    assert!(!cache.is_empty());

    cache.clear();
    let after_clear = live() - start;
    println!(
        "after clear: reported {}, actual {after_clear}",
        cache.memory_bytes()
    );
    assert!(after_clear < 4096, "clear kept {after_clear} bytes");
    assert!(cache.memory_bytes() < 4096);

    fill(&mut cache, &engine, &long, 0..800);
    assert_count_is_true(&cache, live() - start, cap, "refilled with longer lines");

    // Many tiny entries leave big tables behind. Long lines after them must still fit.
    let tiny = document(20_000, 2);
    let bigger = document(20_000, 4000);
    let mut cache = LineCache::new(cap);
    let base = live();
    fill(&mut cache, &engine, &tiny, 0..2000);
    assert_count_is_true(&cache, live() - base, cap, "tiny entries");
    let tiny_entries = cache.len();
    fill(&mut cache, &engine, &bigger, 2000..2800);
    assert_count_is_true(&cache, live() - base, cap, "long lines after tiny ones");
    assert!(
        cache.len() < tiny_entries,
        "the long lines must push out tiny ones"
    );
}

#[test]
fn a_cap_too_small_for_one_layout_holds_nothing() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = document(10, 100);
    engine
        .try_layout_line(&text, LineIndex(0), Dip(1000.0))
        .unwrap();
    let start = live();
    let mut cache = LineCache::new(1000);
    fill(&mut cache, &engine, &text, 0..10);
    assert_eq!(cache.len(), 0);
    assert!(live() - start < 1024, "{}", live() - start);
}
