//! The done-test's third part: the PDF's pages have the same line breaks as Wrap to Page.
use qe_api::{ByteOffset, ViewContext};
use qe_pdf::{PdfOptions, check::check_pdf, export_to_file};
use qe_views::Paper;
use qe_views_check::*;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

/// Every row's first byte, from the editor in Wrap to Page, walking the whole document.
fn rows_on_screen(text: &str, paper: Paper, zoom: f32) -> BTreeSet<usize> {
    rows_on_screen_in(text, paper, zoom, None)
}
fn rows_on_screen_in(text: &str, paper: Paper, zoom: f32, family: Option<&str>) -> BTreeSet<usize> {
    let mut scene = Scene::new(
        qe_editor::EditorView::from_text(text).unwrap(),
        1600.0,
        1200.0,
        paper,
    );
    let mut ctx = ViewContext::default();
    scene
        .frame
        .set_page_wrap(true, &mut scene.editor, &scene.measure, &mut ctx)
        .unwrap();
    if let Some(family) = family {
        scene
            .frame
            .set_base_font(family, 10.0, &mut scene.editor, &scene.measure, &mut ctx)
            .unwrap();
    }
    if zoom != 1.0 {
        scene
            .frame
            .set_zoom(zoom, &mut scene.editor, &scene.measure, &mut ctx)
            .unwrap();
    }
    scene.settle(&mut Vec::new());
    let mut rows = BTreeSet::new();
    let mut position = 0.0;
    loop {
        let mut ctx = ViewContext::default();
        scene.editor.set_scroll_position(position, &mut ctx);
        scene.settle(&mut Vec::new());
        let visible = scene.editor.visible_rows();
        assert!(visible.complete);
        for row in &visible.rows {
            rows.insert(row.first_byte.0);
        }
        // Heights are learned as lines lay out, so the end is read again each time.
        if scene.editor.scroll_offset() >= scene.editor.max_scroll_position() {
            return rows;
        }
        position = scene.editor.scroll_offset() + 600.0;
    }
}

fn rows_in_pdf(text: &str, paper: Paper) -> (BTreeSet<usize>, usize) {
    rows_in_pdf_in(text, paper, "Cascadia Mono")
}
fn rows_in_pdf_in(text: &str, paper: Paper, family: &str) -> (BTreeSet<usize>, usize) {
    let store = qe_api::testing::StringStore::new(text);
    let folder = output_folder().join(format!(
        "breaks-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("pages.pdf");
    let options = PdfOptions {
        paper_width: paper.width,
        paper_height: paper.height,
        keep_rows: true,
        font_family: family.into(),
        ..PdfOptions::default()
    };
    let summary = export_to_file(
        &store,
        &options,
        &path,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report.pages.len(), summary.pages);
    std::fs::remove_dir_all(&folder).unwrap();
    (
        summary
            .rows
            .iter()
            .map(|(_, byte): &(usize, ByteOffset)| byte.0)
            .collect(),
        summary.pages,
    )
}

fn compare(screen: &BTreeSet<usize>, pdf: &BTreeSet<usize>, what: &str) {
    let only_screen: Vec<_> = screen.difference(pdf).take(10).collect();
    let only_pdf: Vec<_> = pdf.difference(screen).take(10).collect();
    assert!(
        only_screen.is_empty() && only_pdf.is_empty(),
        "{what}: {} rows on screen, {} in the PDF; only on screen {only_screen:?}; only in the PDF {only_pdf:?}",
        screen.len(),
        pdf.len()
    );
}

#[test]
fn every_line_break_of_a_mixed_document_matches_on_letter() {
    let text = mixed_document();
    let screen = rows_on_screen(&text, Paper::LETTER, 1.0);
    let (pdf, pages) = rows_in_pdf(&text, Paper::LETTER);
    assert!(pages >= 2, "{pages}");
    assert!(pdf.len() > 150, "{}", pdf.len());
    compare(&screen, &pdf, "Letter");
}

#[test]
fn every_line_break_matches_on_a4_with_its_narrower_width() {
    let text = mixed_document();
    let screen = rows_on_screen(&text, Paper::A4, 1.0);
    let (pdf, _) = rows_in_pdf(&text, Paper::A4);
    compare(&screen, &pdf, "A4");
    // A4's width really differs from Letter's.
    let (letter, _) = rows_in_pdf(&text, Paper::LETTER);
    assert_ne!(pdf, letter);
}

#[test]
fn a_zoomed_page_keeps_the_breaks_of_the_pdf() {
    let text = mixed_document();
    let (pdf, _) = rows_in_pdf(&text, Paper::LETTER);
    let screen = rows_on_screen(&text, Paper::LETTER, 2.0);
    compare(&screen, &pdf, "200%");
}

#[test]
fn a_very_long_line_breaks_the_same_way() {
    // Past the size at which a line is cut into segments.
    let text = format!("{}\nend\n", "abcdefghij klmno ".repeat(20_000));
    let screen = rows_on_screen(&text, Paper::LETTER, 1.0);
    let (pdf, pages) = rows_in_pdf(&text, Paper::LETTER);
    assert!(pages > 5);
    compare(&screen, &pdf, "a long line");
}

/// In a font of even widths, a small change of width can fall between two columns. In a
/// proportional font, nearly any change moves some break.
#[test]
fn a_proportional_font_breaks_the_same_way() {
    // Words in a repeatable random order, so that each row ends at another distance from
    // the edge, and a break moves when the width moves by 4 dips.
    let words = [
        "ill", "wax", "mimic", "WWW", "minimum", "iii", "MMM", "tiny", "Wonder", "a", "of",
    ];
    let mut steps = Steps(0x1357_9BDF_2468);
    let text: String = (0..300)
        .map(|_| {
            let mut line: Vec<&str> = Vec::new();
            for _ in 0..60 {
                line.push(words[steps.next(words.len() as u64) as usize]);
            }
            line.join(" ") + "\n"
        })
        .collect();
    let screen = rows_on_screen_in(&text, Paper::LETTER, 1.0, Some("Segoe UI"));
    let (pdf, _) = rows_in_pdf_in(&text, Paper::LETTER, "Segoe UI");
    assert!(pdf.len() > 160, "{}", pdf.len());
    compare(&screen, &pdf, "Segoe UI");
}
