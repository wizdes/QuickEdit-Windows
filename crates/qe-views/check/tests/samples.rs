//! Writes sample PDFs for a person or a real reader to open, and checks each one.
use qe_api::testing::StringStore;
use qe_pdf::{PdfOptions, check::check_pdf, export_to_file};
use qe_views_check::*;
use std::sync::atomic::AtomicBool;

fn write(name: &str, text: &str, options: &PdfOptions) {
    let path = output_folder().join(name);
    let summary = export_to_file(
        &StringStore::new(text),
        options,
        &path,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report.pages.len(), summary.pages);
}

#[test]
fn sample_files_are_whole_pdfs() {
    let scripts = "Latin: The quick brown fox jumps over the lazy dog. 0123456789\n\
        Accents: caf\u{e9} na\u{ef}ve r\u{e9}sum\u{e9} and combining e\u{301}a\u{308}o\u{302} q\u{301}\u{302}\u{303}\n\
        Arabic: \u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}\n\
        Hebrew in English: abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def\n\
        CJK: \u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00}\u{4e2d}\u{6587}\n\
        Thai: \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35} Devanagari: \u{928}\u{92e}\u{938}\u{94d}\u{924}\u{947}\n\
        Emoji: \u{1f600} \u{1f44d}\u{1f3fd} \u{1f1fa}\u{1f1f8} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\n\
        \ttab at the start\tand in the middle\n\
        \n\
        A long line that wraps at the page's width, the same as Wrap to Page does on screen, so that this row and the next one break where the window shows them.\n";
    write("sample-scripts.pdf", scripts, &PdfOptions::default());
    let long: String = (1..=120).map(|n| format!("Row {n}: {scripts}")).collect();
    write(
        "sample-header.pdf",
        &long,
        &PdfOptions {
            header_and_footer: true,
            title: "Notes for the sample.txt".into(),
            date: qe_pdf::header_date_now(),
            ..PdfOptions::default()
        },
    );
    write(
        "sample-a4.pdf",
        scripts,
        &PdfOptions {
            paper_width: 595.0,
            paper_height: 842.0,
            ..PdfOptions::default()
        },
    );
}
