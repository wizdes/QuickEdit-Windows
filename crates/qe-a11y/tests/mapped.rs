//! A real mapped file, and "not yet" answers.
mod support;
use qe_a11y::{
    ReadError, TextRange,
    text::{known_len, read_text},
};
use qe_api::{
    ByteOffset, LineIndex, LookupError, StoreStatus, TextPosition, TextSnapshot, Utf16Offset,
};
use qe_editor::{EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use support::*;
use windows::{
    Win32::UI::Accessibility::*,
    core::{BSTR, Interface},
};

struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0) {
            eprintln!("could not delete {}: {error}", self.0.display());
        }
    }
}

fn mapped_view(name: &str, bytes: &[u8], delay: Option<Duration>) -> (EditorView, Temp) {
    let path = std::env::temp_dir().join(format!("qe-a11y-{}-{name}.txt", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let temp = Temp(path);
    let mut load = FileLoad::start(
        &temp.0,
        LoadOptions {
            force_read_only: true,
            ..Default::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let file = loop {
        match load.poll() {
            Some(LoadResult::Loaded(file)) => break file,
            Some(LoadResult::Failed(reason)) => panic!("{reason}"),
            Some(LoadResult::Cancelled) => panic!("cancelled"),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            None => panic!("the load timed out"),
        }
    };
    let LoadedFile::ReadOnly(mapped) = file else {
        panic!("this file should be mapped")
    };
    if let Some(delay) = delay {
        mapped.set_test_read_delay(delay);
    }
    (EditorView::from_mapped(mapped).unwrap(), temp)
}

fn settle(view: &EditorView) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !view.store_status().complete {
        assert!(Instant::now() < deadline, "the index timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn retry<T>(mut call: impl FnMut() -> windows::core::Result<T>) -> (T, usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut tries = 0;
    loop {
        tries += 1;
        match call() {
            Ok(answer) => return (answer, tries),
            Err(error) if error.code().0 == NOT_YET => {
                assert!(Instant::now() < deadline, "the answer never came");
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

#[test]
fn a_mapped_file_reads_moves_and_is_read_only() {
    let body = "a\u{1f600}e\u{301}\r\nsecond line\nthird";
    let (view, _temp) = mapped_view("small", body.as_bytes(), None);
    settle(&view);
    let window = TestWindow::new(view);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    assert_eq!(retry(|| unsafe { doc.GetText(-1) }).0, body);
    let read_only = unsafe { doc.GetAttributeValue(UIA_IsReadOnlyAttributeId) }.unwrap();
    assert!(bool::try_from(&read_only).unwrap());
    let value: IValueProvider = unsafe { document.GetPatternProvider(UIA_ValuePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert!(unsafe { value.IsReadOnly() }.unwrap().as_bool());
    let line = unsafe { doc.Clone() }.unwrap();
    retry(|| unsafe { line.ExpandToEnclosingUnit(TextUnit_Line) });
    assert_eq!(retry(|| unsafe { line.GetText(-1) }).0, "a\u{1f600}e\u{301}\r\n");
    retry(|| unsafe { line.Move(TextUnit_Line, 1) });
    assert_eq!(retry(|| unsafe { line.GetText(-1) }).0, "second line\n");
    let found = retry(|| unsafe { doc.FindText(&BSTR::from("THIRD"), false, true) }).0;
    assert_eq!(retry(|| unsafe { found.GetText(-1) }).0, "third");
}

#[test]
fn a_page_not_in_memory_says_ask_again_and_then_answers() {
    // Pages this far apart are not all in memory at once, and each read is slowed.
    let mut body = Vec::new();
    let mut middle = 0;
    for i in 0..600_000 {
        if i == 400_000 {
            middle = body.len();
        }
        body.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let (view, _temp) = mapped_view("paged", &body, Some(Duration::from_millis(5)));
    settle(&view);
    let middle = ByteOffset(middle);
    let window = TestWindow::new(view);
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    window
        .ui
        .view
        .borrow_mut()
        .select(middle..middle, &mut qe_api::ViewContext::default());
    let caret = retry(|| unsafe {
        let mut active = windows::core::BOOL(0);
        text_of(&document).GetCaretRange(&mut active)
    })
    .0;
    retry(|| unsafe { caret.ExpandToEnclosingUnit(TextUnit_Line) });
    let (text, _) = retry(|| unsafe { caret.GetText(-1) });
    assert_eq!(text, "line 400000\n");
    // A search of the whole file gets there, and no call waits long on this thread.
    let doc = document_range(&document);
    let mut worst = Duration::ZERO;
    let (found, _) = retry(|| {
        let began = Instant::now();
        let answer = unsafe { doc.FindText(&BSTR::from("line 599999"), false, false) };
        worst = worst.max(began.elapsed());
        answer
    });
    assert_eq!(retry(|| unsafe { found.GetText(-1) }).0, "line 599999");
    assert!(worst < Duration::from_millis(250), "a call took {worst:?}");
}

/// Knows only the first part of its text, as a mapped file does while its index grows.
struct PartlyKnown {
    text: String,
    known: usize,
    bytes_read: AtomicUsize,
}
impl PartlyKnown {
    fn missing<T>(&self) -> Result<T, LookupError> {
        Err(LookupError::NotYetKnown {
            indexed_up_to: ByteOffset(self.known),
        })
    }
}
impl TextSnapshot for PartlyKnown {
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
        Some(Utf16Offset(byte.0))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        Some(ByteOffset(utf16.0))
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
        match self.text.get(range.start.0..range.end.0) {
            Some(part) => {
                visitor(part);
                true
            }
            None => false,
        }
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        StoreStatus {
            known_bytes: ByteOffset(self.known),
            known_utf16: Utf16Offset(self.known),
            known_lines: 1,
            complete: false,
            failure: None,
        }
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        self.missing()
    }
    fn try_utf16_to_byte(&self, utf16: Utf16Offset) -> Result<ByteOffset, LookupError> {
        if utf16.0 <= self.known {
            Ok(ByteOffset(utf16.0))
        } else {
            self.missing()
        }
    }
    fn try_visit_chunks(
        &self,
        range: Range<ByteOffset>,
        visitor: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        self.bytes_read
            .fetch_add(range.end.0 - range.start.0, Ordering::SeqCst);
        if range.end.0 > self.known {
            return self.missing();
        }
        if self.visit_chunks(range, visitor) {
            Ok(())
        } else {
            Err(LookupError::OutOfRange)
        }
    }
}

#[test]
fn while_the_index_grows_only_the_known_part_answers() {
    let text = PartlyKnown {
        text: "abcdefghij".into(),
        known: 4,
        bytes_read: AtomicUsize::new(0),
    };
    assert_eq!(known_len(&text), Ok((4, false)));
    assert_eq!(read_text(&text, TextRange::new(0, 4), -1).unwrap(), "abcd");
    assert_eq!(read_text(&text, TextRange::new(1, 3), 1).unwrap(), "b");
    text.bytes_read.store(0, Ordering::SeqCst);
    assert_eq!(read_text(&text, TextRange::new(0, 4), 2).unwrap(), "ab");
    assert_eq!(
        text.bytes_read.load(Ordering::SeqCst),
        2,
        "a limit must not read past itself"
    );
    // Past the known part is a range error, not a wait and not a made-up answer.
    assert_eq!(
        read_text(&text, TextRange::new(0, 5), -1),
        Err(ReadError::OutOfRange)
    );
}

#[test]
fn a_range_outside_the_text_is_refused() {
    let text = qe_api::testing::StringStore::new("abc");
    assert_eq!(
        read_text(&text, TextRange::new(0, 9), -1),
        Err(ReadError::OutOfRange)
    );
    assert_eq!(
        read_text(&text, TextRange::new(2, 1), -1),
        Err(ReadError::OutOfRange)
    );
}
