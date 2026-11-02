//! Layout timing. Run it with `cargo run -p qe-layout --release --example bench --locked`.
//! Each number is a median over several runs, after the font has loaded.
use qe_api::{
    ByteOffset, Dip, Layout, LineIndex, Point, TextPosition, TextSnapshot, Utf16Offset,
    testing::StringStore,
};
use qe_layout::{DirectWriteLayout, LineCache};
use std::ops::Range;
use std::time::Instant;

const RUNS: usize = 15;
/// A frame at 60 Hz.
const FRAME_MS: f64 = 16.7;

struct LongLine {
    bytes: String,
}
impl TextSnapshot for LongLine {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.bytes.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.bytes.len())
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.bytes
            .is_char_boundary(byte.0)
            .then_some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        self.bytes
            .is_char_boundary(utf16.0)
            .then_some(ByteOffset(utf16.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.bytes.is_char_boundary(byte.0).then_some(TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(byte.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        (pos.line.0 == 0).then_some(ByteOffset(pos.column.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 == 0).then_some(ByteOffset(0)..ByteOffset(self.bytes.len()))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        match self.bytes.get(range.start.0..range.end.0) {
            Some(s) => {
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

fn median(name: &str, mut f: impl FnMut()) -> f64 {
    let mut times: Vec<f64> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let middle = times[RUNS / 2];
    println!(
        "{name}: median {middle:.3} ms, slowest {:.3} ms",
        times[RUNS - 1]
    );
    middle
}

fn screen(line: &str, rows: usize) -> StringStore {
    StringStore::new((0..rows).map(|_| format!("{line}\n")).collect::<String>())
}

/// One cold screen with a fresh cache, then the same screen warm.
fn screens(name: &str, engine: &DirectWriteLayout, text: &StringStore, width: f32) -> f64 {
    let rows = text.line_count().min(40);
    let cold = median(&format!("{name}: {rows} lines cold"), || {
        let mut cache = LineCache::new(16_000_000);
        for i in 0..rows {
            std::hint::black_box(cache.get_or_layout(engine, text, LineIndex(i), Dip(width)));
        }
    });
    let mut cache = LineCache::new(16_000_000);
    for i in 0..rows {
        cache.get_or_layout(engine, text, LineIndex(i), Dip(width));
    }
    median(&format!("{name}: {rows} lines warm"), || {
        for i in 0..rows {
            std::hint::black_box(cache.get_or_layout(engine, text, LineIndex(i), Dip(width)));
        }
    });
    cold
}

fn main() {
    let engine = DirectWriteLayout::new().expect("font");
    println!("font: {}", engine.font_family());
    let ascii = StringStore::new("The quick brown fox jumps over the lazy dog 12345");
    let complex = StringStore::new("Latin مرحبا שלום नमस्ते 漢字 👩‍💻");
    engine
        .layout_line(&ascii, LineIndex(0), Dip(1000.0))
        .expect("warm font");
    engine
        .layout_line(&complex, LineIndex(0), Dip(1000.0))
        .expect("warm fallback fonts");
    median("ASCII line", || {
        std::hint::black_box(engine.layout_line(&ascii, LineIndex(0), Dip(1000.0)));
    });
    median("slow line", || {
        std::hint::black_box(engine.layout_line(&complex, LineIndex(0), Dip(1000.0)));
    });

    screens(
        "ASCII screen",
        &engine,
        &screen("Screen line 00 The quick brown fox", 40),
        1000.0,
    );
    screens(
        "CJK screen",
        &engine,
        &screen(&"漢字かな".repeat(10), 40),
        1000.0,
    );
    screens(
        "Arabic screen",
        &engine,
        &screen(&"مرحبا بالعالم ".repeat(4), 40),
        1000.0,
    );
    screens(
        "mixed screen",
        &engine,
        &screen("abc مرحبا 123 שלום def 漢字 ghi", 40),
        1000.0,
    );

    println!("--- wide CJK page-down: 39 lines of 120 characters, frame {FRAME_MS} ms");
    let wide = screen(&"漢字かな".repeat(30), 39);
    let cell = engine.cell_width().0;
    for (name, width) in [
        ("no wrap needed", 1_000_000.0),
        ("wrapped at 120 cells", cell * 120.0),
    ] {
        let page = screens(&format!("wide CJK, {name}"), &engine, &wide, width);
        println!(
            "wide CJK, {name}: {}",
            if page < FRAME_MS {
                "inside one frame"
            } else {
                "OVER one frame"
            }
        );
    }

    println!("--- wide CJK page laid out ahead by worker threads");
    for workers in [2usize, 4, 8] {
        let page = median(&format!("wide CJK, 39 lines on {workers} threads"), || {
            std::thread::scope(|scope| {
                for worker in 0..workers {
                    let (engine, wide) = (&engine, &wide);
                    scope.spawn(move || {
                        for i in (worker..39).step_by(workers) {
                            std::hint::black_box(engine.layout_line(
                                wide,
                                LineIndex(i),
                                Dip(1_000_000.0),
                            ));
                        }
                    });
                }
            });
        });
        println!(
            "{workers} threads: {}",
            if page < FRAME_MS {
                "inside one frame"
            } else {
                "OVER one frame"
            }
        );
    }

    println!("--- long lines");
    for (name, unit) in [
        ("ASCII words", "word wrap test "),
        ("ASCII one word", "x"),
        ("Arabic", "مرحبا بالعالم "),
        ("CJK", "漢字かな"),
    ] {
        for kb in [4usize, 16] {
            let text = LongLine {
                bytes: unit.repeat(kb * 1024 / unit.len()),
            };
            median(&format!("{name}: one line of {kb} KB, wrapped"), || {
                std::hint::black_box(engine.layout_line(&text, LineIndex(0), Dip(800.0)));
            });
        }
        let text = LongLine {
            bytes: unit.repeat(100_000_000 / unit.len()),
        };
        let middle = engine.segment_count(&text, LineIndex(0)).expect("line") / 2;
        median(&format!("{name}: 100 MB line, one segment"), || {
            std::hint::black_box(engine.layout_segment(&text, LineIndex(0), middle, Dip(800.0)))
                .expect("segment");
        });
        let layout = engine
            .layout_segment(&text, LineIndex(0), middle, Dip(800.0))
            .expect("segment");
        median(&format!("{name}: 100 MB line, 1000 hit tests"), || {
            for i in 0..1000 {
                std::hint::black_box(engine.hit_test_point(
                    &layout,
                    Point {
                        x: Dip((i % 800) as f32),
                        y: Dip(layout.height.0 / 2.0),
                    },
                ));
            }
        });
    }
}
