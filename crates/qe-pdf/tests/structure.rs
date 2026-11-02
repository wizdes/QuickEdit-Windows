use qe_api::{ByteOffset, Dip, Layout, LineIndex, TextPosition, TextSnapshot, Utf16Offset};
use qe_pdf::{PdfOptions, build_pdf, check_pdf, export_worker};
use std::ops::Range;
use std::sync::{Arc, atomic::AtomicBool};

struct Source(&'static str);
impl TextSnapshot for Source {
    fn revision(&self) -> u64 {
        1
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.0.len())
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.0.encode_utf16().count())
    }
    fn line_count(&self) -> usize {
        self.0.lines().count().max(1)
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.0
            .get(..byte.0)
            .map(|s| Utf16Offset(s.encode_utf16().count()))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        Some(ByteOffset(utf16.0))
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        Some(TextPosition {
            line: LineIndex(0),
            column: Utf16Offset(byte.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        Some(ByteOffset(pos.column.0))
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        let mut start = 0;
        for (n, part) in self.0.split_inclusive('\n').enumerate() {
            let end = start + part.len();
            if n == line.0 {
                return Some(ByteOffset(start)..ByteOffset(end));
            }
            start = end;
        }
        if self.0.is_empty() && line.0 == 0 {
            Some(ByteOffset(0)..ByteOffset(0))
        } else {
            None
        }
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        if let Some(s) = self.0.get(range.start.0..range.end.0) {
            visitor(s);
            true
        } else {
            false
        }
    }
    fn is_read_only(&self) -> bool {
        false
    }
}

#[test]
fn empty_and_crlf_pages_have_valid_offsets() {
    let empty: Arc<dyn TextSnapshot> = Arc::new(Source(""));
    let bytes = build_pdf(
        empty,
        &PdfOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(check_pdf(&bytes).unwrap(), 1);
    let source: Arc<dyn TextSnapshot> = Arc::new(Source("one\r\ntwo"));
    let bytes = build_pdf(
        source,
        &PdfOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(check_pdf(&bytes).unwrap(), 1);
    assert!(bytes.windows(6).any(|w| w == b"/Font "));
}

#[test]
fn cancelled_job_writes_nothing() {
    let source: Arc<dyn TextSnapshot> = Arc::new(Source("hello"));
    assert!(
        build_pdf(
            source,
            &PdfOptions::default(),
            &AtomicBool::new(true),
            |_| {}
        )
        .is_err()
    );
}

#[test]
fn page_count_matches_layout_and_corruption_is_found() {
    let long: &'static str = Box::leak("hello\n".repeat(90).into_boxed_str());
    let source: Arc<dyn TextSnapshot> = Arc::new(Source(long));
    let mut options = PdfOptions::default();
    options.paper_height = 200.0;
    let mut progress = 0;
    let mut bytes = build_pdf(source, &options, &AtomicBool::new(false), |n| progress = n).unwrap();
    assert_eq!(progress, 90);
    assert!(check_pdf(&bytes).unwrap() > 1);
    let at = bytes
        .windows(20)
        .position(|w| w == b"0000000000 65535 f \n")
        .unwrap()
        + 20;
    bytes[at] = b'9';
    assert!(check_pdf(&bytes).is_err());
}

#[test]
fn unsupported_unicode_is_named() {
    let source: Arc<dyn TextSnapshot> = Arc::new(Source("a😀b"));
    let error = build_pdf(
        source,
        &PdfOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.contains("cannot embed"));
}

#[test]
fn pdf_breaks_follow_page_layout_width() {
    let opts = PdfOptions::default();
    let layout = qe_layout::DirectWriteLayout::new().unwrap();
    for count in 1..200 {
        let text: &'static str = Box::leak("m".repeat(count).into_boxed_str());
        let source: Arc<dyn TextSnapshot> = Arc::new(Source(text));
        let expected = layout
            .layout_line(
                source.as_ref(),
                LineIndex(0),
                Dip(opts.paper_width - 2.0 * opts.margin),
            )
            .unwrap()
            .rows
            .len();
        let bytes = build_pdf(source, &opts, &AtomicBool::new(false), |_| {}).unwrap();
        let actual = String::from_utf8(bytes).unwrap().matches(" Tj ET").count();
        assert_eq!(actual, expected, "at {count} characters");
    }
}

#[test]
fn cancelled_worker_leaves_no_target_or_temp() {
    let source: Arc<dyn TextSnapshot> = Arc::new(Source("hello"));
    let folder = std::env::temp_dir().join(format!(
        "qe-pdf-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&folder).unwrap();
    let target = folder.join("cancel.pdf");
    let cancel = Arc::new(AtomicBool::new(true));
    let job = export_worker(
        source,
        PdfOptions::default(),
        target.clone(),
        cancel,
        |_| {},
    );
    assert!(job.join().unwrap().is_err());
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    std::fs::remove_dir(folder).unwrap();
}
