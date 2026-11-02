use qe_a11y::provider::DocumentProvider;
use qe_a11y::{GET_TEXT_CAP, ReadError, TextModel, TextRange};
use qe_api::{ByteOffset, LineIndex, LookupError, TextPosition, TextSnapshot, Utf16Offset};
use qe_editor::EditorView;
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use windows::{
    Win32::UI::Accessibility::{ITextProvider2, UIA_TextPatternId},
    core::Interface,
};

#[test]
fn unicode_and_limit_keep_utf16_positions() {
    let view = EditorView::from_text("a😀e\u{301}\r\nb").unwrap();
    let model = TextModel::new(view.worker_snapshot());
    assert_eq!(model.len_utf16().unwrap(), 8);
    assert_eq!(model.read(TextRange::new(1, 7), 2).unwrap(), "😀");
    assert_eq!(model.read(TextRange::new(1, 3), 1).unwrap(), "");
    assert_eq!(model.read(TextRange::new(1, 7), 0).unwrap(), "");
    assert_eq!(
        model.read(TextRange::new(1, 7), -1).unwrap(),
        "😀e\u{301}\r\n"
    );
}

#[test]
fn empty_document_and_invalid_positions() {
    let view = EditorView::from_text("").unwrap();
    let model = TextModel::new(view.worker_snapshot());
    assert_eq!(model.read(TextRange::new(0, 0), -1).unwrap(), "");
    assert_eq!(
        model.read(TextRange::new(0, 1), -1),
        Err(ReadError::OutOfRange)
    );
}

#[test]
fn ranges_after_edit_use_new_document_bounds() {
    let mut view = EditorView::from_text("abcdef").unwrap();
    let stale = TextRange::new(2, 6);
    let mut ctx = qe_api::ViewContext::default();
    view.replace_range(qe_api::ByteOffset(1)..qe_api::ByteOffset(6), "x", &mut ctx);
    let model = TextModel::new(view.worker_snapshot());
    assert_eq!(model.read(stale, -1), Err(ReadError::OutOfRange));
}

#[test]
fn unrestricted_read_is_capped() {
    let text = "x".repeat(GET_TEXT_CAP + 100);
    let view = EditorView::from_text(&text).unwrap();
    let model = TextModel::new(view.worker_snapshot());
    let answer = model.read(TextRange::new(0, text.len()), -1).unwrap();
    assert_eq!(answer.len(), GET_TEXT_CAP);
    assert_eq!(model.read(TextRange::new(0, text.len()), 3).unwrap(), "xxx");
}

struct UnknownMapped;
impl TextSnapshot for UnknownMapped {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(512 * 1024 * 1024)
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(0)
    }
    fn line_count(&self) -> usize {
        0
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
    fn visit_chunks(&self, _: Range<ByteOffset>, _: &mut dyn FnMut(&str)) -> bool {
        false
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        Err(LookupError::NotYetKnown {
            indexed_up_to: ByteOffset(0),
        })
    }
}

#[test]
fn mapped_not_yet_known_is_reported_without_reading() {
    let model = TextModel::new(Arc::new(UnknownMapped));
    assert_eq!(
        model.read(TextRange::new(0, 1), 1),
        Err(ReadError::NotYetKnown)
    );
}

#[test]
fn find_text_uses_utf16_positions_and_respects_direction() {
    let view = EditorView::from_text("a😀b e\u{301} 😀b").unwrap();
    let model = TextModel::new(view.worker_snapshot());
    let all = TextRange::new(0, model.len_utf16().unwrap());
    assert_eq!(
        model.find_text(all, "😀b", false, false).unwrap(),
        Some(TextRange::new(1, 4))
    );
    assert_eq!(
        model.find_text(all, "😀B", true, true).unwrap(),
        Some(TextRange::new(8, 11))
    );
    assert_eq!(model.find_text(all, "missing", false, false).unwrap(), None);
}

struct HugeSnapshot {
    most_read: Arc<AtomicUsize>,
}
impl TextSnapshot for HugeSnapshot {
    fn revision(&self) -> u64 {
        0
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(512 * 1024 * 1024)
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(512 * 1024 * 1024)
    }
    fn line_count(&self) -> usize {
        1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        (byte.0 <= self.len_bytes().0).then_some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, offset: Utf16Offset) -> Option<ByteOffset> {
        (offset.0 <= self.len_utf16().0).then_some(ByteOffset(offset.0))
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
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        let size = range.end.0.saturating_sub(range.start.0);
        self.most_read.fetch_max(size, Ordering::SeqCst);
        if size > GET_TEXT_CAP {
            return false;
        }
        visitor(&"x".repeat(size));
        true
    }
    fn is_read_only(&self) -> bool {
        panic!("a virtual mapped snapshot must not answer a plain read-only query on this thread")
    }
}

#[test]
fn virtual_512_mib_file_never_reads_past_get_text_limit() {
    let most_read = Arc::new(AtomicUsize::new(0));
    let model = TextModel::new(Arc::new(HugeSnapshot {
        most_read: most_read.clone(),
    }));
    let whole = TextRange::new(0, 512 * 1024 * 1024);
    assert_eq!(model.read(whole, 1).unwrap(), "x");
    assert_eq!(most_read.load(Ordering::SeqCst), 1);
    assert_eq!(model.read(whole, -1).unwrap().len(), GET_TEXT_CAP);
    assert_eq!(most_read.load(Ordering::SeqCst), GET_TEXT_CAP);
    assert_eq!(
        model.find_text(whole, "x", false, false),
        Err(ReadError::TooLong)
    );
    let provider = DocumentProvider::new(model, TextRange::new(0, 0), true);
    let pattern = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }.unwrap();
    let text: ITextProvider2 = pattern.cast().unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    let started = std::time::Instant::now();
    assert_eq!(unsafe { range.GetText(1) }.unwrap().to_string(), "x");
    eprintln!("virtual 512 MiB COM GetText(1): {:?}", started.elapsed());
    assert_eq!(most_read.load(Ordering::SeqCst), GET_TEXT_CAP);
}
