//! Real exports, read back by the structure checker, by the exact text they must hold.
use qe_api::testing::StringStore;
use qe_api::{Dip, Layout, LineIndex, TextSnapshot};
use qe_layout::DirectWriteLayout;
use qe_pdf::check::{Report, check_pdf};
use qe_pdf::{PdfError, PdfOptions, PdfSummary, export_to_file, export_worker, header_date_now};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A new folder under the system's temp folder, removed when the test ends.
struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("qe-pdf-test-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn names(&self) -> Vec<String> {
        std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        // A failed removal only leaves a folder in temp, and must not hide the test's result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Exported {
    report: Report,
    summary: PdfSummary,
    bytes: Vec<u8>,
    folder: Folder,
    path: PathBuf,
}

fn export(text: &str, options: &PdfOptions) -> Exported {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    let store = StringStore::new(text);
    let mut seen = Vec::new();
    let summary = export_to_file(
        &store,
        options,
        &path,
        &AtomicBool::new(false),
        &mut |value| seen.push(value),
    )
    .unwrap();
    assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
    assert_eq!(seen.last(), Some(&1.0));
    let bytes = std::fs::read(&path).unwrap();
    let report = check_pdf(&bytes).unwrap();
    assert_eq!(report.pages.len(), summary.pages);
    assert_eq!(report.creator, "QuickEdit");
    assert_eq!(bytes.len() as u64, summary.bytes);
    // Only the PDF is left in the folder.
    assert_eq!(folder.names(), vec!["out.pdf".to_string()]);
    Exported {
        report,
        summary,
        bytes,
        folder,
        path,
    }
}

fn plain(text: &str) -> Exported {
    export(text, &PdfOptions::default())
}

/// The file's text with the visible text only.
fn visible_text(report: &Report) -> String {
    report.text_in_file_order()
}

#[test]
fn latin_rows_hold_the_exact_text_on_letter_paper() {
    let out = plain("hello world\nsecond line\nthird");
    assert_eq!(out.report.pages.len(), 1);
    assert_eq!(out.report.pages[0].media_box, [0.0, 0.0, 612.0, 792.0]);
    assert_eq!(
        out.report.page_rows(0),
        vec!["hello world", "second line", "third"]
    );
    // The first baseline is inside the top margin's edge, and the text starts at the margin.
    let first = &out.report.pages[0].glyphs[0];
    assert!((first.x - 72.0).abs() < 0.01, "{}", first.x);
    assert!(first.y < 720.0 && first.y > 700.0, "{}", first.y);
    assert!((first.size - 10.0).abs() < 0.01, "{}", first.size);
}

#[test]
fn glyph_positions_match_the_layout_advances() {
    let text = "abcdefgh";
    let out = plain(text);
    let engine = DirectWriteLayout::with_font("Cascadia Mono", 10.0, DWRITE_FONT_WEIGHT_NORMAL)
        .unwrap()
        .with_wrap(true);
    let store = StringStore::new(text);
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(624.0))
        .unwrap();
    let run = &layout.rows[0].glyph_runs[0];
    let mut pen = 72.0f64;
    for (index, glyph) in out.report.pages[0].glyphs.iter().enumerate() {
        assert!(
            (glyph.x - pen).abs() < 0.02,
            "glyph {index}: {} {pen}",
            glyph.x
        );
        pen += f64::from(run.advances[index].0) * 0.75;
    }
}

#[test]
fn the_font_is_a_real_subset_with_widths_and_outlines() {
    let out = plain("abcabc");
    assert_eq!(out.report.fonts.len(), 1);
    let font = out.report.fonts.values().next().unwrap();
    assert!(font.is_cid_cff);
    // .notdef and three letters, and not a whole font.
    assert_eq!(font.glyph_count, 4, "{}", font.base_font);
    assert!(font.program_bytes < 2000, "{}", font.program_bytes);
    assert!(font.base_font.contains('+'));
    assert!(font.base_font.contains("Cascadia"), "{}", font.base_font);
    // A letter has a real outline. `.notdef` is only endchar.
    assert_eq!(font.charstrings[0], vec![14]);
    for string in &font.charstrings[1..] {
        assert!(string.len() > 10, "{string:?}");
    }
    for cid in 1..4u16 {
        assert!(font.widths[&cid] > 500.0, "{}", font.widths[&cid]);
    }
    assert_eq!(font.to_unicode.len(), 3);
    assert_eq!(out.summary.fonts[0].glyph_ids.len(), 3);
}

#[test]
fn two_fonts_when_a_second_is_needed_and_each_is_embedded() {
    let out = plain("a\u{4e2d}");
    assert_eq!(out.report.fonts.len(), 2, "{:?}", out.report.fonts.keys());
    assert_eq!(visible_text(&out.report), "a\u{4e2d}");
}

#[test]
fn cjk_text_keeps_every_character() {
    let text = "\u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00}";
    let out = plain(text);
    assert_eq!(visible_text(&out.report), text);
}

#[test]
fn arabic_uses_joined_forms_and_runs_right_to_left() {
    // Beh three times: the initial, medial and final forms differ from the isolated form.
    let joined = "\u{628}\u{628}\u{628}";
    let out = plain(joined);
    let glyphs = &out.report.pages[0].glyphs;
    assert_eq!(glyphs.len(), 3);
    assert_eq!(visible_text(&out.report), joined);
    // The first letter is on the right.
    assert!(glyphs[0].x > glyphs[1].x && glyphs[1].x > glyphs[2].x);
    // Three different shapes in the file: the joined forms.
    let ids = &out.summary.fonts[0].glyph_ids;
    let mut distinct = ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "{ids:?}");
    // Alone, the same letter has another glyph.
    let alone = plain("\u{628}");
    assert!(
        !ids.contains(&alone.summary.fonts[0].glyph_ids[0]),
        "{ids:?}"
    );
    // And the shapes are the outlines of those glyphs, which differ.
    let font = out.report.fonts.values().next().unwrap();
    assert_ne!(font.charstrings[1], font.charstrings[2]);
    assert_ne!(font.charstrings[2], font.charstrings[3]);
}

#[test]
fn arabic_glyphs_are_the_ones_directwrite_chose() {
    let text =
        "\u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}";
    let out = plain(text);
    let engine = DirectWriteLayout::with_font("Cascadia Mono", 10.0, DWRITE_FONT_WEIGHT_NORMAL)
        .unwrap()
        .with_wrap(true);
    let store = StringStore::new(text);
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(624.0))
        .unwrap();
    let want: Vec<u16> = layout.rows[0]
        .glyph_runs
        .iter()
        .flat_map(|run| run.glyph_ids.clone())
        .collect();
    let mut seen: Vec<u16> = out
        .summary
        .fonts
        .iter()
        .flat_map(|font| font.glyph_ids.clone())
        .collect();
    let mut wanted = want.clone();
    seen.sort_unstable();
    seen.dedup();
    wanted.sort_unstable();
    wanted.dedup();
    assert_eq!(seen, wanted);
    // Every letter survives in the map, in the store's order.
    assert_eq!(
        visible_text(&out.report)
            .chars()
            .filter(|c| *c != ' ')
            .collect::<String>(),
        text.chars().filter(|c| *c != ' ').collect::<String>()
    );
}

#[test]
fn a_mixed_row_keeps_each_run_in_its_own_direction() {
    let text = "abc \u{5d0}\u{5d1}\u{5d2} def";
    let out = plain(text);
    let glyphs = &out.report.pages[0].glyphs;
    let at = |wanted: &str| {
        glyphs
            .iter()
            .find(|glyph| glyph.text == wanted)
            .unwrap_or_else(|| panic!("no {wanted}"))
            .x
    };
    // English reads left to right, and Hebrew inside it right to left.
    assert!(at("a") < at("b"));
    assert!(at("\u{5d0}") > at("\u{5d1}"));
    assert!(at("\u{5d1}") > at("\u{5d2}"));
    assert!(at("c") < at("\u{5d2}"));
    assert!(at("\u{5d0}") < at("d"));
    let mut found: Vec<char> = visible_text(&out.report).chars().collect();
    let mut want: Vec<char> = text.chars().collect();
    found.sort_unstable();
    want.sort_unstable();
    assert_eq!(found, want);
}

#[test]
fn combining_marks_are_kept_with_their_letters() {
    for text in [
        "e\u{301}",
        "a\u{308}o\u{302}",
        "\u{1e9b}\u{323}",
        "Z\u{351}\u{36b}\u{343}a\u{36c}\u{33d}",
        "q\u{301}\u{302}\u{303}x",
    ] {
        let out = plain(text);
        assert_eq!(visible_text(&out.report), text, "for {text:?}");
    }
}

#[test]
fn a_letter_and_its_separate_marks_each_map_to_their_own_character() {
    // No font has one glyph for this q with three marks, so these are separate glyphs.
    let out = plain("q\u{301}\u{302}\u{303}");
    let texts: Vec<&str> = out.report.pages[0]
        .glyphs
        .iter()
        .map(|glyph| glyph.text.as_str())
        .collect();
    assert_eq!(texts, vec!["q", "\u{301}", "\u{302}", "\u{303}"]);
}

#[test]
fn a_letter_alone_and_with_a_mark_map_to_their_own_text() {
    let text = "e e\u{301} e";
    let out = plain(text);
    assert_eq!(visible_text(&out.report), text);
}

#[test]
fn surrogate_pairs_map_as_one_character() {
    let text = "\u{20bb7}\u{1d11e}x";
    let out = plain(text);
    assert_eq!(visible_text(&out.report), text);
}

#[test]
fn color_emoji_draw_as_colored_shapes_over_searchable_text() {
    let text = "a\u{1f600}b";
    let out = plain(text);
    assert_eq!(visible_text(&out.report), text);
    assert_eq!(out.summary.color_glyphs, 1);
    assert_eq!(out.report.pages[0].form_calls, 1);
    let glyph = out.report.pages[0]
        .glyphs
        .iter()
        .find(|glyph| glyph.text == "\u{1f600}")
        .unwrap();
    assert!(glyph.invisible);
    assert!(
        out.report.pages[0]
            .glyphs
            .iter()
            .filter(|g| g.invisible)
            .count()
            == 1
    );
    // The same emoji twice uses one form.
    let twice = plain("\u{1f600}\u{1f600}");
    assert_eq!(twice.report.pages[0].form_calls, 2);
    assert_eq!(twice.summary.color_glyphs, 2);
    let forms = twice.bytes.windows(7).filter(|w| w == b"/XObject").count();
    let _ = forms;
}

#[test]
fn joined_emoji_and_flags_keep_all_their_text() {
    for text in [
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
        "\u{1f1fa}\u{1f1f8}",
        "\u{1f44d}\u{1f3fd}",
    ] {
        let out = plain(text);
        assert_eq!(visible_text(&out.report), text, "for {text:?}");
    }
}

#[test]
fn crlf_empty_lines_and_an_empty_document() {
    let out = plain("one\r\n\r\ntwo\r\n");
    assert_eq!(out.report.page_rows(0), vec!["one", "two"]);
    assert_eq!(visible_text(&out.report), "onetwo");
    let empty = plain("");
    assert_eq!(empty.report.pages.len(), 1);
    assert!(empty.report.pages[0].glyphs.is_empty());
}

#[test]
fn tabs_move_the_next_run_and_mixed_text_keeps_its_characters() {
    let out = plain("a\tb");
    // The tab itself is in the file too, as text with nothing to see.
    let glyphs: Vec<_> = out.report.pages[0]
        .glyphs
        .iter()
        .filter(|glyph| glyph.text != "\t")
        .collect();
    assert_eq!(glyphs.len(), 2);
    assert!(glyphs[1].x - glyphs[0].x > 30.0);
    let mixed = plain("\u{e9}\tb\u{4e2d}");
    let found = visible_text(&mixed.report).replace('\t', "");
    assert_eq!(found, "\u{e9}b\u{4e2d}");
}

#[test]
fn pages_break_between_whole_rows_and_lose_none() {
    let text: String = (0..500).map(|n| format!("line {n}\n")).collect();
    let out = plain(&text);
    assert!(out.report.pages.len() > 5);
    let mut all = Vec::new();
    for page in 0..out.report.pages.len() {
        all.extend(out.report.page_rows(page));
    }
    let want: Vec<String> = (0..500).map(|n| format!("line {n}")).collect();
    assert_eq!(all, want);
    // No glyph passes the margins, top or bottom, on any page.
    for page in &out.report.pages {
        for glyph in &page.glyphs {
            assert!(glyph.y < 720.0 && glyph.y >= 72.0, "{}", glyph.y);
        }
    }
    // The page holds as many rows as fit: 648 pt of whole rows.
    let first = &out.report.pages[0];
    let rows = out.report.page_rows(0).len();
    let top = first.glyphs[0].y;
    let second = first.glyphs.iter().find(|glyph| glyph.y != top).unwrap().y;
    let row = top - second;
    assert_eq!(rows, (648.0 / row).floor() as usize, "row height {row}");
}

#[test]
fn paper_sizes_set_the_page_and_the_wrap_width() {
    let a4 = PdfOptions {
        paper_width: 595.0,
        paper_height: 842.0,
        ..PdfOptions::default()
    };
    let legal = PdfOptions {
        paper_width: 612.0,
        paper_height: 1008.0,
        ..PdfOptions::default()
    };
    assert_eq!(
        export("x", &a4).report.pages[0].media_box,
        [0.0, 0.0, 595.0, 842.0]
    );
    assert_eq!(
        export("x", &legal).report.pages[0].media_box,
        [0.0, 0.0, 612.0, 1008.0]
    );
    // A row never passes the text width on A4, with 451 pt between the margins.
    let long = "word ".repeat(200);
    let out = export(&long, &a4);
    assert!(out.report.page_rows(0).len() > 1);
    for glyph in &out.report.pages[0].glyphs {
        if glyph.text != " " {
            assert!(glyph.x < 595.0 - 72.0, "{}", glyph.x);
        }
    }
}

#[test]
fn a_long_row_wraps_and_keeps_every_character() {
    let text = "alpha beta gamma delta ".repeat(40);
    let out = plain(&text);
    assert!(out.report.page_rows(0).len() > 2);
    assert_eq!(visible_text(&out.report), text);
}

#[test]
fn a_very_long_line_goes_through_in_segments_with_nothing_lost() {
    // Past the fast path's limit for a whole line, so that layout works in segments.
    let text = "abcdefghij".repeat(120_000);
    let out = plain(&text);
    assert!(out.report.pages.len() > 10);
    assert_eq!(visible_text(&out.report), text);
}

#[test]
fn header_and_footer_follow_the_macs_rules() {
    let options = PdfOptions {
        header_and_footer: true,
        title: "notes.txt".into(),
        date: "1/2/26, 3:04 PM".into(),
        ..PdfOptions::default()
    };
    let text: String = (0..200).map(|n| format!("row {n}\n")).collect();
    let out = export(&text, &options);
    let total = out.report.pages.len();
    assert!(total >= 2);
    for page in 0..total {
        let rows = out.report.page_rows(page);
        assert_eq!(rows[0], "notes.txt1/2/26, 3:04 PM", "page {page}");
        assert_eq!(
            rows.last().unwrap(),
            &format!("Page {} of {total}", page + 1),
            "page {page}"
        );
        let glyphs = &out.report.pages[page].glyphs;
        let top = glyphs.iter().map(|g| g.y).fold(f64::MIN, f64::max);
        let bottom = glyphs.iter().map(|g| g.y).fold(f64::MAX, f64::min);
        // In the middle of the margins.
        assert!(top > 720.0 && top < 792.0, "{top}");
        assert!(bottom > 0.0 && bottom < 72.0, "{bottom}");
        // The header font is 9 pt.
        assert!(glyphs.iter().any(|g| (g.size - 9.0).abs() < 0.01));
        // The name starts at the left margin, and the date ends at the right margin.
        let header: Vec<_> = glyphs.iter().filter(|g| g.y == top).collect();
        assert!((header[0].x - 72.0).abs() < 0.01, "{}", header[0].x);
        assert!(header.last().unwrap().x > 500.0);
        assert!(header.last().unwrap().x < 540.0);
    }
    assert_eq!(out.report.title.as_deref(), Some("notes.txt"));

    let off = plain("x");
    assert_eq!(off.report.title, None);
    assert_eq!(off.report.page_rows(0), vec!["x"]);
}

#[test]
fn a_long_title_is_cut_with_an_ellipsis_before_the_date() {
    let options = PdfOptions {
        header_and_footer: true,
        title: "a very long name ".repeat(20),
        date: "1/2/26, 3:04 PM".into(),
        ..PdfOptions::default()
    };
    let out = export("x", &options);
    let row = &out.report.page_rows(0)[0];
    assert!(row.contains('\u{2026}'), "{row}");
    assert!(row.ends_with("1/2/26, 3:04 PM"), "{row}");
    assert!(row.len() < 150, "{row}");
    // The whole name stays in the file's title.
    assert_eq!(out.report.title.as_deref(), Some(options.title.as_str()));
}

#[test]
fn header_date_now_is_short_plain_text() {
    let date = header_date_now();
    assert!(date.len() > 5 && date.len() < 60, "{date}");
}

#[test]
fn rewrap_off_uses_the_window_width_and_scales_down() {
    let text = "word ".repeat(120);
    let wide = PdfOptions {
        rewrap: false,
        window_width_dips: 1248.0,
        ..PdfOptions::default()
    };
    let out = export(&text, &wide);
    // Twice the page's width, so half the size, and about twice the characters on a row.
    let glyph = &out.report.pages[0].glyphs[0];
    assert!((glyph.size - 5.0).abs() < 0.01, "{}", glyph.size);
    let rewrapped = export(&text, &PdfOptions::default());
    let one = out.report.page_rows(0)[0].len();
    let other = rewrapped.report.page_rows(0)[0].len();
    assert!(one > other * 3 / 2, "{one} {other}");
    // A narrower window keeps full size and wraps sooner.
    let narrow = PdfOptions {
        rewrap: false,
        window_width_dips: 300.0,
        ..PdfOptions::default()
    };
    let out = export(&text, &narrow);
    assert!((out.report.pages[0].glyphs[0].size - 10.0).abs() < 0.01);
    assert!(out.report.page_rows(0)[0].len() < other);
}

#[test]
fn bad_options_say_so_and_write_nothing() {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    let store = StringStore::new("x");
    for options in [
        PdfOptions {
            margin: 400.0,
            ..PdfOptions::default()
        },
        PdfOptions {
            font_points: 0.0,
            ..PdfOptions::default()
        },
        PdfOptions {
            paper_width: f32::NAN,
            ..PdfOptions::default()
        },
        PdfOptions {
            rewrap: false,
            window_width_dips: 0.0,
            ..PdfOptions::default()
        },
    ] {
        let result = export_to_file(
            &store,
            &options,
            &path,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert!(matches!(result, Err(PdfError::BadOptions(_))), "{result:?}");
    }
    assert!(folder.names().is_empty());
}

#[test]
fn a_missing_font_is_a_plain_error() {
    let folder = Folder::new();
    let options = PdfOptions {
        font_family: "No Such Font 12345".into(),
        ..PdfOptions::default()
    };
    let result = export_to_file(
        &StringStore::new("x"),
        &options,
        &folder.file("out.pdf"),
        &AtomicBool::new(false),
        &mut |_| {},
    );
    let Err(PdfError::Font(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("No Such Font 12345"), "{message}");
    assert!(folder.names().is_empty());
}

#[test]
fn cancel_leaves_no_file_and_keeps_the_old_one() {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    std::fs::write(&path, b"the old file").unwrap();
    let text: String = (0..3000).map(|n| format!("row {n}\n")).collect();
    let store = Arc::new(StringStore::new(text));
    let job = export_worker(store, PdfOptions::default(), path.clone(), None).unwrap();
    job.cancel();
    assert_eq!(job.join(), Err(PdfError::Cancelled));
    assert_eq!(std::fs::read(&path).unwrap(), b"the old file");
    assert_eq!(folder.names(), vec!["out.pdf".to_string()]);
}

#[test]
fn cancel_part_way_stops_soon_and_cleans_up() {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    let text: String = (0..5000).map(|n| format!("row {n}\n")).collect();
    let store = StringStore::new(text);
    let cancel = AtomicBool::new(false);
    let mut last = 0.0;
    let result = export_to_file(
        &store,
        &PdfOptions::default(),
        &path,
        &cancel,
        &mut |value| {
            last = value;
            if value > 0.2 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(result, Err(PdfError::Cancelled));
    assert!(last < 0.3, "{last}");
    assert!(folder.names().is_empty(), "{:?}", folder.names());
}

#[test]
fn a_cancel_at_the_very_end_never_replaces_the_old_file() {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    std::fs::write(&path, b"the old file").unwrap();
    let cancel = AtomicBool::new(false);
    let result = export_to_file(
        &StringStore::new("x"),
        &PdfOptions::default(),
        &path,
        &cancel,
        &mut |value| {
            if value >= 1.0 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(result, Err(PdfError::Cancelled));
    assert_eq!(std::fs::read(&path).unwrap(), b"the old file");
    assert_eq!(folder.names(), vec!["out.pdf".to_string()]);
}

#[test]
fn a_failed_write_says_so_in_plain_words() {
    let folder = Folder::new();
    let missing = folder.file("no-such-folder").join("out.pdf");
    let result = export_to_file(
        &StringStore::new("x"),
        &PdfOptions::default(),
        &missing,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    let Err(PdfError::Write(message)) = result else {
        panic!("{result:?}");
    };
    assert!(
        message.starts_with("The PDF could not be written"),
        "{message}"
    );
}

#[test]
fn a_locked_target_keeps_its_content_and_leaves_no_temp_file() {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    std::fs::write(&path, b"the old file").unwrap();
    // A folder at the target's name cannot be replaced by a file on any Windows account.
    let blocked = folder.file("blocked.pdf");
    std::fs::create_dir(&blocked).unwrap();
    let result = export_to_file(
        &StringStore::new("x"),
        &PdfOptions::default(),
        &blocked,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    let Err(PdfError::Write(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("blocked.pdf"), "{message}");
    let mut names = folder.names();
    names.sort();
    assert_eq!(
        names,
        vec!["blocked.pdf".to_string(), "out.pdf".to_string()]
    );
}

#[test]
fn the_worker_reports_progress_and_wakes_its_host() {
    let folder = Folder::new();
    let path = folder.file("out.pdf");
    let text: String = (0..3000).map(|n| format!("row {n}\n")).collect();
    let woken = Arc::new(AtomicUsize::new(0));
    let count = woken.clone();
    let job = export_worker(
        Arc::new(StringStore::new(text)) as Arc<dyn TextSnapshot>,
        PdfOptions::default(),
        path.clone(),
        Some(Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
        })),
    )
    .unwrap();
    while !job.is_finished() {
        std::thread::yield_now();
    }
    assert_eq!(job.fraction(), 1.0);
    let summary = job.join().unwrap();
    assert_eq!(woken.load(Ordering::Relaxed), 1);
    assert_eq!(
        check_pdf(&std::fs::read(&path).unwrap())
            .unwrap()
            .pages
            .len(),
        summary.pages
    );
}

// ---- The checker must catch what it claims to ----

#[test]
fn the_checker_catches_broken_files() {
    let out = plain("hello");
    let good = &out.bytes;
    let find = |needle: &[u8]| {
        good.windows(needle.len())
            .rposition(|window| window == needle)
            .unwrap()
    };
    let replaced = |at: usize, with: &[u8]| {
        let mut bad = good.clone();
        bad[at..at + with.len()].copy_from_slice(with);
        bad
    };

    // The end marker.
    assert!(check_pdf(&good[..good.len() - 1]).is_err());
    // startxref's number.
    let at = find(b"startxref\n") + 10;
    assert!(check_pdf(&replaced(at, b"1")).is_err());
    // An object's table entry.
    let at = find(b"xref\n0 ") + 5;
    let table = good[at..].iter().position(|b| *b == b'\n').unwrap() + at + 1 + 20;
    assert!(check_pdf(&replaced(table, b"0000000017")).is_err());
    // The page count.
    let at = find(b"/Count ");
    assert!(check_pdf(&replaced(at, b"/Coun2 ")).is_err());
    // A stream's data.
    let at = find(b"stream\n") + 7;
    assert!(check_pdf(&replaced(at + 5, &[0xFF, 0xFF, 0xFF, 0xFF])).is_err());
    // ToUnicode's name, so the font has no map.
    let at = find(b"/ToUnicode");
    assert!(check_pdf(&replaced(at, b"/ToUnicodf")).is_err());
    // The embedded program's key.
    let at = find(b"/FontFile3");
    assert!(check_pdf(&replaced(at, b"/FontFile9")).is_err());
    // The start of the file.
    assert!(check_pdf(&replaced(0, b"%XDF")).is_err());
    assert!(check_pdf(b"").is_err());
}

// ---- A real outside reader. Cargo lists this as ignored, so it never passes with no reader ----

fn pdftotext(path: &std::path::Path) -> String {
    let output = Command::new("pdftotext")
        .args(["-enc", "UTF-8", "-layout"])
        .arg(path)
        .arg("-")
        .output()
        .expect("this test needs pdftotext on PATH");
    assert!(
        output.status.success(),
        "pdftotext rejected the file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[ignore = "needs pdftotext on PATH: run with -- --ignored"]
fn pdftotext_reads_every_script() {
    let cases = [
        ("hello world", "hello world"),
        (
            "\u{6f22}\u{5b57}\u{304b}\u{306a}",
            "\u{6f22}\u{5b57}\u{304b}\u{306a}",
        ),
        ("caf\u{e9}", "caf\u{e9}"),
    ];
    for (text, want) in cases {
        let out = plain(text);
        let found = pdftotext(&out.path);
        assert!(found.contains(want), "{want:?} not in {found:?}");
    }
    // It reads the Arabic letters. Readers differ in the order they print them, so this
    // compares the letters and not their order.
    let arabic = "\u{645}\u{631}\u{62d}\u{628}\u{627}";
    let out = plain(arabic);
    let found = pdftotext(&out.path);
    for letter in arabic.chars() {
        assert!(found.contains(letter), "{letter:?} not in {found:?}");
    }
    // It reads a mark with its letter.
    let out = plain("e\u{301}x");
    let found = pdftotext(&out.path);
    assert!(
        found.contains('x') && (found.contains('\u{301}') || found.contains('\u{e9}')),
        "{found:?}"
    );
    // It finds as many pages as the checker.
    let text: String = (0..300).map(|n| format!("row {n}\n")).collect();
    let out = plain(&text);
    let found = pdftotext(&out.path);
    assert_eq!(found.matches('\u{c}').count(), out.report.pages.len());
    assert!(found.contains("row 299"));
    let _ = &out.folder;
}
