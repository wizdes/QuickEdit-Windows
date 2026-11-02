//! Regressions from the second review of M13.
use qe_api::TextSnapshot;
use qe_api::testing::StringStore;
use qe_pdf::check::check_pdf;
use qe_pdf::{PdfError, PdfOptions, export_to_file, export_worker};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("qe-pdf-round2-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        // A failed removal only leaves a folder in temp, and must not hide the result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn export(text: &str, options: &PdfOptions, path: &Path) -> Result<qe_pdf::PdfSummary, PdfError> {
    export_to_file(
        &StringStore::new(text),
        options,
        path,
        &AtomicBool::new(false),
        &mut |_| {},
    )
}

// ---- Finding 1: the last wake ----

/// A host does nothing between wakes: it reads the job only when a wake arrives, on another
/// thread, with no polling. So each export must end on a wake that says it has finished.
#[test]
fn the_last_wake_always_finds_the_job_finished() {
    let folder = Folder::new();
    for round in 0..200 {
        let (woke, wakes) = mpsc::channel::<()>();
        let job = export_worker(
            Arc::new(StringStore::new("x")) as Arc<dyn TextSnapshot>,
            PdfOptions::default(),
            folder.0.join(format!("{round}.pdf")),
            Some(Arc::new(move || {
                // The host may have gone only after a failure, which this test reports.
                woke.send(()).unwrap();
            })),
        )
        .unwrap();
        loop {
            wakes
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("round {round}: no wake came, so a host would hang"));
            if job.is_finished() {
                break;
            }
        }
        job.join().unwrap();
    }
}

#[test]
fn a_failed_export_also_ends_on_a_finished_wake_and_gives_its_error() {
    let folder = Folder::new();
    let (woke, wakes) = mpsc::channel::<()>();
    let job = export_worker(
        Arc::new(StringStore::new("x")) as Arc<dyn TextSnapshot>,
        PdfOptions::default(),
        folder.0.join("no such folder").join("out.pdf"),
        Some(Arc::new(move || woke.send(()).unwrap())),
    )
    .unwrap();
    loop {
        wakes.recv_timeout(Duration::from_secs(30)).unwrap();
        if job.is_finished() {
            break;
        }
    }
    assert!(matches!(job.join(), Err(PdfError::Write(_))));
}

// ---- Finding 5: titles with a line separator ----

#[test]
fn a_title_with_a_line_separator_keeps_its_tail() {
    for title in [
        "before\u{2028}after",
        "before\u{2029}after",
        "before\u{85}after",
    ] {
        let folder = Folder::new();
        let path = folder.0.join("out.pdf");
        let options = PdfOptions {
            header_and_footer: true,
            title: title.into(),
            date: "d".into(),
            ..PdfOptions::default()
        };
        export("x", &options, &path).unwrap();
        let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
        let text = report.text_in_file_order();
        assert!(text.contains("before after"), "{title:?}: {text:?}");
        assert_eq!(report.title.as_deref(), Some(title));
    }
}

// ---- The first survivor: the check of the finished file ----

fn damage_the_table(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    let at = bytes
        .windows(6)
        .rposition(|window| window == b"\nxref\n")
        .unwrap()
        + 1;
    // The first real entry follows "xref", the size line and the free entry.
    let size_line = bytes[at + 5..]
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap();
    let first = at + 5 + size_line + 1 + 20;
    // Its offset now points one digit away.
    bytes[first + 9] = if bytes[first + 9] == b'9' {
        b'8'
    } else {
        bytes[first + 9] + 1
    };
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_file_damaged_before_the_move_never_replaces_the_old_one() {
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    std::fs::write(&path, b"the old file").unwrap();
    let options = PdfOptions {
        after_write: Some(damage_the_table),
        ..PdfOptions::default()
    };
    let result = export("x", &options, &path);
    let Err(PdfError::Check(message)) = result else {
        panic!("{result:?}");
    };
    assert!(message.contains("not where it should be"), "{message}");
    assert_eq!(std::fs::read(&path).unwrap(), b"the old file");
    assert_eq!(std::fs::read_dir(&folder.0).unwrap().count(), 1);
}

// ---- The second survivor: blocks in the map ----

#[test]
fn a_font_with_many_glyphs_keeps_each_map_block_to_100_entries() {
    let text: String = (0x4e00u32..0x4e00 + 350)
        .filter_map(char::from_u32)
        .collect();
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    export(&text, &PdfOptions::default(), &path).unwrap();
    // The check refuses a bigger block, and one font here needs more than one block.
    let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
    let most = report
        .fonts
        .values()
        .map(|font| font.to_unicode.len())
        .max();
    assert!(most > Some(100), "{most:?}");
}

// ---- Finding 4: what real readers give ----

const PDFTOTEXT: &str = r"C:\Program Files\Git\ucrt64\bin\pdftotext.exe";

fn read_with_pdftotext(path: &Path, mode: &[&str]) -> String {
    let program = if Path::new(PDFTOTEXT).exists() {
        PDFTOTEXT
    } else {
        "pdftotext"
    };
    let output = Command::new(program)
        .args(["-enc", "UTF-8"])
        .args(mode)
        .arg(path)
        .arg("-")
        .output()
        .expect("this test needs pdftotext");
    assert!(output.status.success());
    let warnings = String::from_utf8_lossy(&output.stderr);
    assert!(warnings.is_empty(), "{warnings}");
    String::from_utf8_lossy(&output.stdout).replace(['\u{c}', '\r', '\n'], "")
}

fn exported(text: &str) -> (Folder, PathBuf) {
    let folder = Folder::new();
    let path = folder.0.join("out.pdf");
    export(text, &PdfOptions::default(), &path).unwrap();
    (folder, path)
}

const DEVANAGARI: &str = "\u{915}\u{94d}\u{937}\u{924}\u{94d}\u{930}\u{93f}\u{92f}";
const THAI: &str = "\u{e01}\u{e49}\u{e33} \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}";
const POINTED_HEBREW: &str = "\u{5e9}\u{5c1}\u{5b8}\u{5dc}\u{5d5}\u{5b9}\u{5dd}";
const HEBREW: &str = "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def";
const ARABIC: &str =
    "\u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}";

/// Without the two direction marks that xpdf puts around right-to-left text.
fn no_direction_marks(text: &str) -> String {
    text.replace(['\u{202b}', '\u{202c}'], "")
}
fn sorted(text: &str) -> Vec<char> {
    let mut chars: Vec<char> = text.chars().collect();
    chars.sort_unstable();
    chars
}

#[test]
#[ignore = "needs pdftotext: run with -- --ignored"]
fn pdftotext_gives_devanagari_and_thai_exactly() {
    let hindi =
        "\u{928}\u{92e}\u{938}\u{94d}\u{924}\u{947} \u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}";
    let repeated = hindi.repeat(6);
    for text in [
        DEVANAGARI,
        hindi,
        repeated.as_str(),
        THAI,
        "cafe\u{301} e\u{302}x",
    ] {
        let (_folder, path) = exported(text);
        for mode in [&[][..], &["-raw"][..]] {
            assert_eq!(
                read_with_pdftotext(&path, mode).trim_end(),
                text,
                "mode {mode:?}"
            );
        }
    }
}

#[test]
#[ignore = "needs pdftotext: run with -- --ignored"]
fn pdftotext_gives_right_to_left_text_in_its_default_mode() {
    for text in [HEBREW, ARABIC] {
        let (_folder, path) = exported(text);
        let found = no_direction_marks(&read_with_pdftotext(&path, &[]));
        // xpdf moves a space at a change of direction, which the README says.
        assert_eq!(found.replace(' ', ""), text.replace(' ', ""));
    }
    // Pointed Hebrew is exact in raw mode. In its default mode, xpdf moves the marks.
    let (_folder, path) = exported(POINTED_HEBREW);
    assert_eq!(
        read_with_pdftotext(&path, &["-raw"]).trim_end(),
        POINTED_HEBREW
    );
    let found = no_direction_marks(&read_with_pdftotext(&path, &[]));
    assert_eq!(sorted(found.trim_end()), sorted(POINTED_HEBREW));
}

#[test]
#[ignore = "needs pdftotext: run with -- --ignored"]
fn pdftotext_keeps_every_mark_up_to_8_on_one_letter() {
    for marks in [2, 8] {
        let text = format!("a{}b", "\u{301}".repeat(marks));
        let (_folder, path) = exported(&text);
        for mode in [&[][..], &["-raw"][..]] {
            // xpdf adds a space after the stack, which the README says.
            let found = read_with_pdftotext(&path, mode).replace(' ', "");
            assert_eq!(found.trim_end(), text, "mode {mode:?}");
        }
    }
}

/// Past 8 marks on a letter, the file's own map stays exact, and this reader loses marks. This
/// test holds both facts, so a change in either shows up. The README promises no more.
#[test]
#[ignore = "needs pdftotext: run with -- --ignored"]
fn pdftotext_keeps_the_letters_of_a_long_cluster_and_the_map_keeps_it_all() {
    let text = format!("a{}b", "\u{301}".repeat(200));
    let (_folder, path) = exported(&text);
    let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report.text_in_file_order(), text);
    for mode in [&[][..], &["-raw"][..]] {
        let found = read_with_pdftotext(&path, mode);
        assert!(found.contains('a') && found.contains('b'), "mode {mode:?}");
        let marks = found.matches('\u{301}').count();
        assert!((1..=200).contains(&marks), "mode {mode:?}: {marks}");
    }
}

/// With no outside program: which runs carry their whole text, and that it is right.
#[test]
fn a_left_to_right_run_that_is_not_plain_carries_its_whole_text() {
    let spans = |text: &str| {
        let (_folder, path) = exported(text);
        let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
        report.pages[0].actual_text.clone()
    };
    assert_eq!(spans(DEVANAGARI), vec![DEVANAGARI]);
    assert_eq!(spans(THAI).concat().replace(' ', ""), THAI.replace(' ', ""));
    // Plain text needs none. A right-to-left run comes out backward in xpdf with one.
    assert!(spans("plain words").is_empty());
    assert!(spans(ARABIC).is_empty());
    assert!(spans(POINTED_HEBREW).is_empty());
    // Up to 8 marks on one letter carries the text, and more does not.
    let eight = format!("a{}b", "\u{301}".repeat(8));
    assert_eq!(spans(&eight).concat(), eight.trim_end_matches('b'));
    assert!(spans(&format!("a{}b", "\u{301}".repeat(9))).is_empty());
}

/// Writes the reader cases for a person or a script to read. Run by hand with a folder.
#[test]
#[ignore = "by hand: set QE_READER_CASES to a folder"]
fn write_the_reader_cases() {
    let folder = PathBuf::from(std::env::var("QE_READER_CASES").unwrap());
    let long = format!("a{}b", "\u{301}".repeat(200));
    let eight = format!("a{}b", "\u{301}".repeat(8));
    let nine = format!("a{}b", "\u{301}".repeat(9));
    let hindi = "\u{928}\u{92e}\u{938}\u{94d}\u{924}\u{947}".repeat(8);
    let one = "cafe\u{301} e\u{302}x";
    let more = [
        "\u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}",
        "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def",
        "plain words and caf\u{e9}",
        "a\u{301}\u{301}\u{301}\u{301}\u{301}b",
        "xe\u{301}\u{301}y word",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f44d}\u{1f3fd}",
    ];
    let cases = [DEVANAGARI, THAI, POINTED_HEBREW]
        .into_iter()
        .chain(more)
        .chain([
            long.as_str(),
            eight.as_str(),
            nine.as_str(),
            hindi.as_str(),
            one,
        ]);
    for (number, text) in cases.enumerate() {
        let path = folder.join(format!("case-{number:02}.pdf"));
        export(text, &PdfOptions::default(), &path).unwrap();
        std::fs::write(path.with_extension("txt"), text).unwrap();
    }
}

// ---- A break that survived: where marks go in right-to-left text ----

/// DirectWrite's offset points along the run's direction, so in right-to-left text a positive
/// offset moves a glyph left. The expected places here come straight from the layout.
#[test]
fn a_mark_in_right_to_left_text_sits_where_layout_put_it() {
    use qe_api::{Dip, LineIndex};
    use qe_layout::DirectWriteLayout;
    use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
    let store = StringStore::new(POINTED_HEBREW);
    let engine = DirectWriteLayout::with_font("Cascadia Mono", 10.0, DWRITE_FONT_WEIGHT_NORMAL)
        .unwrap()
        .with_wrap(true)
        .with_tab_width(8)
        .with_full_rows_at(Dip(624.0));
    let layout = engine
        .layout_segment(&store, LineIndex(0), 0, Dip(624.0))
        .unwrap();
    let run = layout.rows[0]
        .glyph_runs
        .iter()
        .find(|run| run.bidi_level & 1 == 1 && run.offsets.iter().any(|o| o.x.0 != 0.0))
        .expect("this text must have a mark moved sideways, or this test proves nothing");
    let (_folder, path) = exported(POINTED_HEBREW);
    let report = check_pdf(&std::fs::read(&path).unwrap()).unwrap();
    let glyphs = &report.pages[0].glyphs;
    assert_eq!(glyphs.len(), run.glyph_ids.len());
    let scale = 0.75;
    let mut pen = 72.0 + f64::from(run.origin.x.0) * scale;
    for (index, glyph) in glyphs.iter().enumerate() {
        pen -= f64::from(run.advances[index].0) * scale;
        let want = pen - f64::from(run.offsets[index].x.0) * scale;
        assert!(
            (glyph.x - want).abs() < 0.01,
            "glyph {index}: {} and {want}",
            glyph.x
        );
    }
}
