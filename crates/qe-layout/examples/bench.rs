use qe_api::{
    ByteOffset, Dip, Layout, LineIndex, TextPosition, TextSnapshot, Utf16Offset,
    testing::StringStore,
};
use qe_layout::{DirectWriteLayout, LineCache};
use std::ops::Range;
use std::time::Instant;

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
        (byte.0 <= self.bytes.len()).then_some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        (utf16.0 <= self.bytes.len()).then_some(ByteOffset(utf16.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        (byte.0 <= self.bytes.len()).then_some(TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(byte.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        (pos.line.0 == 0 && pos.column.0 <= self.bytes.len()).then_some(ByteOffset(pos.column.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        (line.0 == 0).then_some(ByteOffset(0)..ByteOffset(self.bytes.len()))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        if let Some(s) = self.bytes.get(range.start.0..range.end.0) {
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
fn measure(name: &str, mut f: impl FnMut()) {
    let now = Instant::now();
    f();
    println!("{name}: {:.3} ms", now.elapsed().as_secs_f64() * 1000.0);
}
fn main() {
    let engine = DirectWriteLayout::new().expect("font");
    println!("font: {}", engine.font_family());
    let ascii = StringStore::new("The quick brown fox jumps over the lazy dog 12345");
    let complex = StringStore::new("Latin مرحبا שלום नमस्ते 漢字 👩‍💻");
    engine
        .layout_line(&ascii, LineIndex(0), Dip(1000.0))
        .expect("warm font");
    measure("ASCII line", || {
        std::hint::black_box(engine.layout_line(&ascii, LineIndex(0), Dip(1000.0)));
    });
    measure("slow line", || {
        std::hint::black_box(engine.layout_line(&complex, LineIndex(0), Dip(1000.0)));
    });
    let screen = StringStore::new(
        (0..40)
            .map(|i| format!("Screen line {i:02} The quick brown fox\n"))
            .collect::<String>(),
    );
    let mut cache = LineCache::new(1_000_000);
    measure("40 lines cold", || {
        for i in 0..40 {
            std::hint::black_box(cache.get_or_layout(&engine, &screen, LineIndex(i), Dip(1000.0)));
        }
    });
    measure("40 lines warm", || {
        for i in 0..40 {
            std::hint::black_box(cache.get_or_layout(&engine, &screen, LineIndex(i), Dip(1000.0)));
        }
    });
    let long = LongLine {
        bytes: "x".repeat(100_000_000),
    };
    measure("100 MB line slice", || {
        std::hint::black_box(engine.layout_slice(
            &long,
            ByteOffset(50_000_000)..ByteOffset(50_000_200),
            Dip(1000.0),
        ));
    });
}
