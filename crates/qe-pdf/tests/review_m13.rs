//! Regressions from the first review of M13. Each test failed on the code the review saw.
use qe_api::testing::StringStore;
use qe_api::{
    ByteOffset, LineIndex, LookupError, StoreFailure, StoreStatus, TextPosition, TextSnapshot,
    Utf16Offset,
};
use qe_pdf::check::{Report, check_pdf};
use qe_pdf::{PdfError, PdfLimits, PdfOptions, export_to_file, export_worker};
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("qe-pdf-review-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn is_empty(&self) -> bool {
        std::fs::read_dir(&self.0).unwrap().next().is_none()
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        // A failed removal only leaves a folder in temp, and must not hide the result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(text: &str, options: &PdfOptions) -> (Report, Vec<u8>, Folder, PathBuf) {
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    export_to_file(
        &StringStore::new(text),
        options,
        &path,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let report = check_pdf(&bytes).unwrap();
    (report, bytes, folder, path)
}
fn text_of(text: &str) -> String {
    write(text, &PdfOptions::default()).0.text_in_file_order()
}
/// Runs `pdftotext`, and returns what it printed and what it warned. A test that calls this
/// is marked ignored, so cargo never counts it as passed on a PC with no reader.
fn pdftotext(path: &std::path::Path) -> (String, String) {
    let output = Command::new("pdftotext")
        .args(["-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .output()
        .expect("this test needs pdftotext on PATH");
    assert!(output.status.success());
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ---- Finding 1: only a whole text becomes a PDF ----

/// A text with a status of the test's choosing.
struct Pinned {
    text: StringStore,
    status: StoreStatus,
    /// A failure that this many reads of the status from now first shows.
    late: Option<(AtomicUsize, &'static str)>,
}
impl TextSnapshot for Pinned {
    fn revision(&self) -> u64 {
        self.text.revision()
    }
    fn len_bytes(&self) -> ByteOffset {
        self.text.len_bytes()
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.text.len_utf16()
    }
    fn line_count(&self) -> usize {
        self.text.line_count()
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        self.text.byte_to_utf16(byte)
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        self.text.utf16_to_byte(utf16)
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        self.text.byte_to_position(byte)
    }
    fn position_to_byte(&self, position: TextPosition) -> Option<ByteOffset> {
        self.text.position_to_byte(position)
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        self.text.line_range(line)
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, visitor: &mut dyn FnMut(&str)) -> bool {
        self.text.visit_chunks(range, visitor)
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        let mut status = self.status.clone();
        if let Some((reads, message)) = &self.late
            && reads.fetch_sub(1, Ordering::Relaxed) == 0
        {
            reads.store(0, Ordering::Relaxed);
            status.failure = Some(StoreFailure {
                message: (*message).into(),
                valid_up_to: ByteOffset(0),
            });
        }
        status
    }
    fn try_known_line(&self, line: LineIndex) -> Result<qe_api::KnownLine, LookupError> {
        self.text.try_known_line(line)
    }
}
fn status(text: &str, complete: bool, failure: Option<&str>) -> StoreStatus {
    StoreStatus {
        known_bytes: ByteOffset(text.len() / 2),
        known_utf16: Utf16Offset(text.len() / 2),
        known_lines: 1,
        complete,
        failure: failure.map(|message| StoreFailure {
            message: message.into(),
            valid_up_to: ByteOffset(0),
        }),
    }
}

#[test]
fn a_text_cut_exactly_after_a_line_end_is_refused() {
    // The worst case: every line here is whole, and only the status knows that more follows.
    let text = "one\ntwo\n";
    let folder = Folder::new();
    let result = export_to_file(
        &Pinned {
            text: StringStore::new(text),
            status: status(text, false, None),
            late: None,
        },
        &PdfOptions::default(),
        &folder.0.join("out.pdf"),
        &AtomicBool::new(false),
        &mut |_| {},
    );
    let Err(PdfError::NotComplete(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("no PDF was written"), "{message}");
    assert!(folder.is_empty());
}

#[test]
fn a_store_failure_before_or_during_the_export_stops_it() {
    let text = "one\ntwo\n";
    let folder = Folder::new();
    let result = export_to_file(
        &Pinned {
            text: StringStore::new(text),
            status: status(text, true, Some("the file changed outside QuickEdit")),
            late: None,
        },
        &PdfOptions::default(),
        &folder.0.join("out.pdf"),
        &AtomicBool::new(false),
        &mut |_| {},
    );
    let Err(PdfError::Read(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("no PDF was written"), "{message}");
    assert!(
        message.contains("the file changed outside QuickEdit"),
        "{message}"
    );
    assert!(folder.is_empty());
}

#[test]
fn a_store_that_fails_during_the_write_leaves_no_pdf() {
    let text = "one\ntwo\n";
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    std::fs::write(&path, b"the old file").unwrap();
    let result = export_to_file(
        &Pinned {
            text: StringStore::new(text),
            status: status(text, true, None),
            late: Some((AtomicUsize::new(1), "the disk went away")),
        },
        &PdfOptions::default(),
        &path,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    let Err(PdfError::Read(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("the disk went away"), "{message}");
    assert_eq!(std::fs::read(&path).unwrap(), b"the old file");
    assert_eq!(std::fs::read_dir(&folder.0).unwrap().count(), 1);
}

#[test]
fn the_worker_waits_for_a_complete_snapshot_and_reports_that_it_waits() {
    let text = "one\ntwo\n";
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    let woken = Arc::new(AtomicUsize::new(0));
    let count = woken.clone();
    let job = export_worker(
        Arc::new(Pinned {
            text: StringStore::new("one\n"),
            status: status(text, false, None),
            late: None,
        }),
        PdfOptions::default(),
        path.clone(),
        Some(Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
        })),
    )
    .unwrap();
    // It asks, and it wakes the host so that the host can answer.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !job.wants_snapshot() || woken.load(Ordering::Relaxed) == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!path.exists());
    // Another partial one changes nothing, and a whole one lets it go on.
    job.offer_snapshot(Arc::new(Pinned {
        text: StringStore::new("one\n"),
        status: status(text, false, None),
        late: None,
    }));
    assert!(job.wants_snapshot());
    // All of what this snapshot holds is known, and the wait is the first tenth of the bar.
    assert!((job.fraction() - 0.1).abs() < 0.001, "{}", job.fraction());
    job.offer_snapshot(Arc::new(StringStore::new(text)));
    while !job.is_finished() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!job.wants_snapshot());
    assert_eq!(job.fraction(), 1.0);
    job.join().unwrap();
    let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report.page_rows(0), vec!["one", "two"]);
}

#[test]
fn cancel_during_the_wait_stops_at_once() {
    let text = "one\ntwo\n";
    let folder = Folder::new();
    let job = export_worker(
        Arc::new(Pinned {
            text: StringStore::new(text),
            status: status(text, false, None),
            late: None,
        }),
        PdfOptions::default(),
        folder.0.join("out.pdf"),
        None,
    )
    .unwrap();
    job.cancel();
    assert_eq!(job.join(), Err(PdfError::Cancelled));
    assert!(folder.is_empty());
}

// ---- Finding 5: long clusters ----

#[test]
fn a_cluster_over_the_entry_limit_keeps_all_its_text_in_order() {
    for text in [
        format!("a{}", "\u{fe0f}".repeat(300)),
        format!("a{}b", "\u{301}".repeat(900)),
        "\u{1f468}\u{200d}".repeat(150),
    ] {
        let (report, ..) = write(&text, &PdfOptions::default());
        assert_eq!(report.text_in_file_order(), text);
    }
}

#[test]
#[ignore = "needs pdftotext on PATH: run with -- --ignored"]
fn another_reader_takes_every_map_entry_of_a_long_cluster() {
    for text in [
        format!("a{}", "\u{fe0f}".repeat(300)),
        "\u{1f468}\u{200d}".repeat(150),
    ] {
        let (_, _, _folder, path) = write(&text, &PdfOptions::default());
        let (_, warnings) = pdftotext(&path);
        assert!(!warnings.contains("Illegal"), "{warnings}");
    }
}

#[test]
fn no_map_entry_is_too_long_or_splits_a_character() {
    let text = "\u{1f468}\u{200d}".repeat(400);
    let (report, _, _folder, _path) = write(&text, &PdfOptions::default());
    for font in report.fonts.values() {
        for value in font.to_unicode.values() {
            let units = value.encode_utf16().count();
            assert!(units <= 63, "{units}");
            assert!(!value.contains('\u{fffd}'));
        }
    }
}

// ---- Finding 6: tabs and characters with no glyph ----

#[test]
fn tabs_reach_the_text_of_the_pdf_in_their_places() {
    for text in [
        "a\tb",
        "\ta\t\tb\t",
        "\t",
        "\u{e9}\tb\u{4e2d}",
        "x\t\u{5d0}\t",
    ] {
        let got = text_of(text);
        let mut found: Vec<char> = got.chars().collect();
        let mut want: Vec<char> = text.chars().collect();
        found.sort_unstable();
        want.sort_unstable();
        assert_eq!(found, want, "for {text:?}: {got:?}");
    }
    assert_eq!(text_of("a\tb"), "a\tb");
    assert_eq!(text_of("\ta\t\tb\t"), "\ta\t\tb\t");
}

#[test]
fn a_tab_has_no_ink_and_keeps_the_next_runs_place() {
    let (report, ..) = write("a\tb", &PdfOptions::default());
    let glyphs = &report.pages[0].glyphs;
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[1].text, "\t");
    assert!(glyphs[2].x - glyphs[0].x > 30.0);
    assert!(glyphs[1].x > glyphs[0].x && glyphs[1].x < glyphs[2].x);
    let font = report.fonts.get(&glyphs[1].font).unwrap();
    assert_eq!(font.charstrings[usize::from(glyphs[1].cid)], vec![14]);
}

#[test]
fn characters_with_no_glyph_stay_in_the_text() {
    for text in [
        "a\u{1}b\u{7f}c\u{ad}d\u{200b}e\u{feff}f",
        "\u{202e}abc\u{202c} x \u{200f}y",
    ] {
        let got = text_of(text);
        let mut found: Vec<char> = got.chars().collect();
        let mut want: Vec<char> = text.chars().collect();
        found.sort_unstable();
        want.sort_unstable();
        assert_eq!(found, want, "for {text:?}: {got:?}");
    }
}

#[test]
fn line_endings_stay_out_of_the_text() {
    assert_eq!(text_of("a\r\nb\nc\u{2028}d\u{85}e\u{2029}f\rg"), "abcdefg");
}

// ---- Finding 7: titles ----

fn header_row(report: &Report) -> Vec<&qe_pdf::check::PlacedGlyph> {
    let top = report.pages[0]
        .glyphs
        .iter()
        .map(|glyph| glyph.y)
        .fold(0.0, f64::max);
    report.pages[0]
        .glyphs
        .iter()
        .filter(|glyph| (glyph.y - top).abs() < 0.01)
        .collect()
}

#[test]
fn a_long_title_in_any_direction_stops_before_the_date() {
    for title in [
        "\u{645}\u{631}\u{62d}\u{628}\u{627} ".repeat(100),
        "\u{5e9}\u{5dc}\u{5d5}\u{5dd} ".repeat(100),
        "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def ".repeat(40),
        "long name ".repeat(50),
    ] {
        let options = PdfOptions {
            header_and_footer: true,
            title: title.clone(),
            date: "1/1/26, 1:00 PM".into(),
            ..PdfOptions::default()
        };
        let (report, ..) = write("x", &options);
        let row = header_row(&report);
        let text: String = row.iter().map(|glyph| glyph.text.as_str()).collect();
        assert!(text.contains('\u{2026}'), "{title:?}: {text:?}");
        // The date is the last 15 glyphs, and starts past the title by the Mac's 18 pt.
        let date_left = row
            .iter()
            .rev()
            .take(15)
            .map(|glyph| glyph.x)
            .fold(f64::MAX, f64::min);
        for glyph in row.iter().rev().skip(15) {
            let width = report.fonts[&glyph.font].widths[&glyph.cid] * glyph.size / 1000.0;
            assert!(glyph.x >= 71.9, "{title:?}: {}", glyph.x);
            assert!(
                glyph.x + width <= date_left - 17.0,
                "{title:?}: {} {width} {date_left}",
                glyph.x
            );
        }
        // It keeps most of the room, so the cut is not at the start.
        assert!(row.len() > 40, "{title:?}: {}", row.len());
    }
}

// ---- Finding 8: long names ----

#[test]
fn a_long_valid_file_name_exports() {
    let folder = Folder::new();
    let path = folder.0.join(format!("{}.pdf", "a".repeat(250)));
    export_to_file(
        &StringStore::new("x"),
        &PdfOptions::default(),
        &path,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(path.exists());
    assert_eq!(std::fs::read_dir(&folder.0).unwrap().count(), 1);
}

// ---- Finding 10: the checksum ----

#[test]
fn the_checker_refuses_a_wrong_checksum() {
    let (_, bytes, ..) = write("hello", &PdfOptions::default());
    // The last byte of the first stream is the last byte of its Adler-32.
    let end = bytes
        .windows(10)
        .position(|window| window == b"\nendstream")
        .unwrap();
    let mut bad = bytes.clone();
    bad[end - 1] ^= 0xFF;
    let problem = check_pdf(&bad).unwrap_err();
    assert!(problem.contains("checksum"), "{problem}");
}

// ---- Finding 14: the limits ----

fn with_limits(most_objects: usize, most_bytes: u64) -> PdfOptions {
    PdfOptions {
        limits: PdfLimits {
            most_objects,
            most_bytes,
        },
        ..PdfOptions::default()
    }
}

#[test]
fn too_many_pages_is_found_before_any_page_is_written() {
    let folder = Folder::new();
    let text: String = (0..5000).map(|n| format!("{n}\n")).collect();
    let mut calls = 0;
    let result = export_to_file(
        &StringStore::new(text),
        &with_limits(20, u64::MAX),
        &folder.0.join("out.pdf"),
        &AtomicBool::new(false),
        &mut |_| calls += 1,
    );
    let Err(PdfError::TooLong(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("too long for a PDF"), "{message}");
    assert_eq!(calls, 0, "it started writing pages");
    assert!(folder.is_empty());
}

#[test]
fn a_file_over_the_size_limit_stops_as_soon_as_it_gets_there() {
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    std::fs::write(&path, b"the old file").unwrap();
    let text: String = (0..5000).map(|n| format!("row {n}\n")).collect();
    let mut last = 0.0;
    let result = export_to_file(
        &StringStore::new(text),
        &with_limits(1 << 22, 3000),
        &path,
        &AtomicBool::new(false),
        &mut |value| last = value,
    );
    let Err(PdfError::TooLong(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("too long for a PDF"), "{message}");
    assert!(last < 0.5, "it ran on to {last}");
    assert_eq!(std::fs::read(&path).unwrap(), b"the old file");
    assert_eq!(std::fs::read_dir(&folder.0).unwrap().count(), 1);
}

// ---- Finding 6, continued: rows that wrap, and what another reader sees ----

fn sorted(text: &str) -> Vec<char> {
    let mut chars: Vec<char> = text.chars().collect();
    chars.sort_unstable();
    chars
}

#[test]
fn wrapped_rows_keep_every_space_and_tab() {
    let text = "word   \t  ".repeat(60);
    assert_eq!(sorted(&text_of(&text)), sorted(&text));
    let lines = "\t\n\n  \n\tx\t\n";
    let got = text_of(lines);
    assert_eq!(sorted(&got), sorted(&lines.replace('\n', "")));
}

#[test]
#[ignore = "needs pdftotext on PATH: run with -- --ignored"]
fn another_reader_sees_a_gap_where_a_tab_was() {
    let (_, _, _folder, path) = write("left\tright", &PdfOptions::default());
    let (text, warnings) = pdftotext(&path);
    assert_eq!(warnings, "");
    let first = text.lines().next().unwrap_or("");
    assert!(first.starts_with("left"), "{first:?}");
    assert!(first.ends_with("right"), "{first:?}");
    assert!(
        first["left".len()..first.len() - "right".len()]
            .chars()
            .all(char::is_whitespace),
        "{first:?}"
    );
    assert!(first.len() > "leftright".len(), "{first:?}");
}

// ---- The second review: text a reader would make up, and damaged files ----

const SPLIT_AND_JOINED: [&str; 3] = [
    "\u{928}\u{92e}\u{938}\u{94d}\u{924}\u{947} \u{915}\u{94d}\u{937}\u{93f} \u{915}\u{93f} \u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}",
    "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f1fa}\u{1f1f8} \u{1f44d}\u{1f3fd}",
    "e\u{301} \u{e01}\u{e33}",
];

/// The checker refuses a drawn CID with no text, because a reader such as MuPDF then returns
/// the CID's number as text. So this passing means that no reader has room to make any up.
#[test]
fn every_glyph_drawn_as_text_has_text_of_its_own() {
    for text in SPLIT_AND_JOINED {
        let (report, ..) = write(text, &PdfOptions::default());
        assert_eq!(
            sorted(&report.text_in_file_order()),
            sorted(text),
            "{text:?}"
        );
        assert!(
            report.pages[0]
                .glyphs
                .iter()
                .all(|glyph| !glyph.text.is_empty())
        );
    }
}

#[test]
fn a_page_that_draws_a_glyph_with_no_text_is_refused() {
    // Hindi's split vowels are the case in question: they draw more glyphs than characters.
    let (report, ..) = write(SPLIT_AND_JOINED[0], &PdfOptions::default());
    assert!(
        report.pages[0].form_calls > 0,
        "this text no longer proves anything, because each glyph has its own character"
    );
}

#[test]
fn format_characters_from_the_second_review_stay_in_the_text() {
    for text in [
        "a\u{200d}b\u{200c}c\u{200b}d\u{feff}e",
        "a\u{ad}b",
        "\u{2066}abc\u{2069}",
    ] {
        assert_eq!(sorted(&text_of(text)), sorted(text), "{text:?}");
    }
}

fn pdf_around(body: &str) -> Vec<u8> {
    format!("%PDF-1.7\n{body}\n%%EOF\n").into_bytes()
}

#[test]
fn a_tiny_damaged_file_gives_an_error_and_no_panic() {
    for bad in [
        pdf_around("xref\n0 18446744073709551615\n0000000000 65535 f \ntrailer\n<<>>\nstartxref\n9"),
        pdf_around("startxref\n18446744073709551615"),
        pdf_around("xref\n0 3\n0000000000 65535 f \n\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9} 00000 n \ntrailer\n<<>>\nstartxref\n9"),
        format!(
            "%PDF-1.7\n1 0 obj\n<< /Length 18446744073709551615 >>\nstream\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n<< /Size 2 >>\nstartxref\n{}\n%%EOF\n",
            "%PDF-1.7\n1 0 obj\n<< /Length 18446744073709551615 >>\nstream\n".len()
        )
        .into_bytes(),
    ] {
        assert!(check_pdf(&bad).is_err());
    }
}

/// Every byte of a real file, damaged in turn. Debug builds panic on overflow, so this
/// finds any arithmetic on a damaged number. A change the file survives is fine here.
#[test]
fn a_real_file_damaged_at_any_byte_never_panics_the_checker() {
    let options = PdfOptions {
        header_and_footer: true,
        title: "t".into(),
        date: "d".into(),
        ..PdfOptions::default()
    };
    let (_, bytes, ..) = write("a\tb", &options);
    let step = (bytes.len() / 4000).max(1);
    for at in (0..bytes.len()).step_by(step) {
        for value in [0x00, b'9', 0xFF] {
            let mut bad = bytes.clone();
            bad[at] = value;
            let _ = check_pdf(&bad);
        }
    }
}

#[test]
fn a_compressor_that_writes_a_wrong_checksum_cannot_pass_the_checker() {
    // Each stream in the file is checked, so any wrong checksum fails every PDF test.
    let (_, bytes, ..) = write("hello", &PdfOptions::default());
    let mut streams = 0;
    let mut from = 0;
    while let Some(found) = bytes[from..].windows(10).position(|w| w == b"\nendstream") {
        let end = from + found;
        let mut bad = bytes.clone();
        bad[end - 1] ^= 1;
        assert!(
            check_pdf(&bad).unwrap_err().contains("checksum"),
            "stream ending at {end}"
        );
        streams += 1;
        from = end + 10;
    }
    assert!(streams >= 3, "{streams}");
}
