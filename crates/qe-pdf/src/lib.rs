//! PDF export with no outside program and no printer, on a worker.
//!
//! The pages are laid out by `qe-layout` with the same font and width as Wrap to Page, so
//! the line breaks are the same. Each glyph is the id DirectWrite chose, placed at the
//! advance DirectWrite gave. The fonts are embedded subsets, and a ToUnicode map makes the
//! text selectable and searchable. The README states the limits and the design.
mod cff;
pub mod check;
pub mod deflate;
mod fonts;
mod page;
mod text;
mod writer;

use fonts::FontSet;
use page::{HeaderText, PageBuilder};
use qe_api::{ByteOffset, Dip, LayoutRow, LineIndex, TextSnapshot};
use qe_layout::DirectWriteLayout;
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use writer::PdfFile;

/// A point of paper is this many dips.
pub(crate) const DIPS_PER_POINT: f32 = 96.0 / 72.0;
/// PDF readers commonly stop at this many objects (PDF 1.7, Annex C).
const MOST_OBJECTS: usize = 8_388_607;

/// What to export. The sizes are in points unless a name says dips.
#[derive(Clone, Debug)]
pub struct PdfOptions {
    pub paper_width: f32,
    pub paper_height: f32,
    pub margin: f32,
    /// The editor's font at 100%. Zoom does not change a PDF, as on the Mac.
    pub font_family: String,
    pub font_points: f32,
    pub tab_width: usize,
    /// "Print header and footer", `NumberPagesWhenPrinting`, off by default.
    pub header_and_footer: bool,
    /// True for Wrap to Page and for "Rewrap contents to fit page": the text wraps to the
    /// page. False wraps it to the window's width and scales down to fit, as on the Mac.
    pub rewrap: bool,
    /// The window's text width. It is used only when `rewrap` is false.
    pub window_width_dips: f32,
    /// The document's name, for the header and the file's title.
    pub title: String,
    /// The date as the header shows it. `header_date_now` makes the usual one.
    pub date: String,
    /// Keeps each row's first byte in the summary, for tests that compare line breaks.
    pub keep_rows: bool,
}

impl Default for PdfOptions {
    /// Letter, 72 pt margins, Cascadia Mono 10 pt, no header, rewrap on.
    fn default() -> Self {
        Self {
            paper_width: 612.0,
            paper_height: 792.0,
            margin: 72.0,
            font_family: "Cascadia Mono".into(),
            font_points: 10.0,
            tab_width: 8,
            header_and_footer: false,
            rewrap: true,
            window_width_dips: 624.0,
            title: String::new(),
            date: String::new(),
            keep_rows: false,
        }
    }
}

/// Why no PDF was written. The text of each one is for the person using the app.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfError {
    Cancelled,
    BadOptions(String),
    /// A line could not be laid out.
    Layout {
        line: usize,
        message: String,
    },
    /// A font could not be read or embedded.
    Font(String),
    /// The file could not be written.
    Write(String),
    /// The finished file failed its own check, so it did not replace anything.
    Check(String),
    /// The document needs more pages than a PDF can hold.
    TooLong(String),
    /// The text could not be read.
    Read(String),
}
impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "The PDF export was cancelled."),
            Self::BadOptions(message)
            | Self::Font(message)
            | Self::Write(message)
            | Self::TooLong(message)
            | Self::Read(message) => write!(f, "{message}"),
            Self::Layout { line, message } => {
                write!(f, "Line {} could not be laid out: {message}", line + 1)
            }
            Self::Check(message) => {
                write!(
                    f,
                    "QuickEdit stopped before the PDF was finished: {message}"
                )
            }
        }
    }
}
impl std::error::Error for PdfError {}

/// One font in the file, for a test or a report.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSummary {
    pub postscript_name: String,
    pub glyph_ids: Vec<u16>,
}

/// What a finished export did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfSummary {
    pub pages: usize,
    pub bytes: u64,
    pub fonts: Vec<FontSummary>,
    /// (page index, first byte of the row), with `keep_rows`.
    pub rows: Vec<(usize, ByteOffset)>,
    /// The number of color glyphs drawn as colored shapes.
    pub color_glyphs: u64,
}

fn check_options(options: &PdfOptions) -> Result<(), PdfError> {
    let good = |value: f32| value.is_finite() && value >= 0.0;
    if !good(options.paper_width)
        || !good(options.paper_height)
        || !good(options.margin)
        || options.paper_width <= 2.0 * options.margin
        || options.paper_height <= 2.0 * options.margin
    {
        return Err(PdfError::BadOptions(
            "The PDF's paper is too small for its margins.".into(),
        ));
    }
    if !good(options.font_points) || options.font_points <= 0.0 {
        return Err(PdfError::BadOptions(
            "The PDF's font size is not a usable number.".into(),
        ));
    }
    if !options.rewrap && (!good(options.window_width_dips) || options.window_width_dips <= 0.0) {
        return Err(PdfError::BadOptions(
            "The PDF's window width is not a usable number.".into(),
        ));
    }
    Ok(())
}

/// The width to wrap at in dips, and the scale that makes it fit the page. Wider text
/// scales down, and narrower text keeps its size, as `PDFRenderer.Pagination` does.
pub fn wrap_width_and_scale(options: &PdfOptions) -> (f32, f32) {
    let page = (options.paper_width - 2.0 * options.margin) * DIPS_PER_POINT;
    if options.rewrap {
        (page, 1.0)
    } else if options.window_width_dips > page {
        (options.window_width_dips, page / options.window_width_dips)
    } else {
        (options.window_width_dips, 1.0)
    }
}

/// Writes the whole PDF to `file`. Runs on a worker. `progress` gets 0 to 1 and never goes
/// back. Cancel is read before each line and each segment of a long line.
fn write_document(
    source: &dyn TextSnapshot,
    options: &PdfOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
    file: &mut PdfFile,
) -> Result<PdfSummary, PdfError> {
    check_options(options)?;
    let (width, scale) = wrap_width_and_scale(options);
    let engine = DirectWriteLayout::with_font(
        &options.font_family,
        options.font_points,
        DWRITE_FONT_WEIGHT_NORMAL,
    )
    .map_err(|e| PdfError::Font(format!("The PDF's font could not be used: {e}")))?
    .with_wrap(true)
    .with_tab_width(options.tab_width)
    .with_full_rows_at(Dip(width));

    let mut builder = PageBuilder::new(options, scale);
    let mut fonts = FontSet::default();
    let lines = source.line_count().max(1);
    let total = source.len_bytes().0.max(1) as f64;
    let mut summary = PdfSummary::default();
    for line in 0..lines {
        if cancel.load(Ordering::Relaxed) {
            return Err(PdfError::Cancelled);
        }
        let index = LineIndex(line);
        let layout_error = |e: qe_layout::LayoutError| PdfError::Layout {
            line,
            message: e.to_string(),
        };
        let count = engine.segment_count(source, index).map_err(layout_error)?;
        for segment in 0..count {
            if segment > 0 && cancel.load(Ordering::Relaxed) {
                return Err(PdfError::Cancelled);
            }
            let layout = engine
                .layout_segment(source, index, segment, Dip(width))
                .map_err(layout_error)?;
            for row in &layout.rows {
                if builder.place_row(row, source, &mut fonts, file)? {
                    progress_for(row, total, progress);
                }
                if options.keep_rows {
                    summary.rows.push((builder.page_index(), row.range.start));
                }
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(PdfError::Cancelled);
    }
    summary.pages = builder.finish_pages(&mut fonts, file, HeaderText::of(options), cancel)?;
    summary.color_glyphs = builder.color_glyphs();
    if file.object_count() > MOST_OBJECTS {
        return Err(PdfError::TooLong(format!(
            "This document needs {} pages, which is more than a PDF can hold.",
            summary.pages
        )));
    }
    summary.fonts = fonts
        .fonts
        .iter()
        .map(|font| FontSummary {
            postscript_name: font.postscript_name.clone(),
            glyph_ids: font.cids.iter().skip(1).map(|c| c.glyph).collect(),
        })
        .collect();
    progress(1.0);
    Ok(summary)
}

/// Runs once for each finished page.
fn progress_for(row: &LayoutRow, total: f64, progress: &mut dyn FnMut(f64)) {
    // The last part of the work is the fonts and the file's end, so this stops under 1.
    progress((row.range.start.0 as f64 / total).min(1.0) * 0.98);
}

static TEMP_NUMBER: AtomicU64 = AtomicU64::new(0);

/// Exports to `path` on the calling thread, which must not be the UI thread for anything
/// but a short text. A temp file beside the target becomes the target only after a check
/// passes, so a failure or a cancel leaves no file, and any old file as it was.
pub fn export_to_file(
    source: &dyn TextSnapshot,
    options: &PdfOptions,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<PdfSummary, PdfError> {
    let name = path
        .file_name()
        .ok_or_else(|| PdfError::Write("The PDF's path has no file name.".into()))?
        .to_string_lossy()
        .into_owned();
    let parent = match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let number = TEMP_NUMBER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.qe-{}-{number}.tmp", std::process::id()));
    let result = write_and_replace(source, options, &temp, path, cancel, progress);
    if result.is_err()
        && temp.exists()
        && let Err(problem) = std::fs::remove_file(&temp)
    {
        // The export's own error matters more, and this one is named with it.
        return Err(PdfError::Write(format!(
            "The PDF failed ({}), and its temporary file {} could not be removed: {problem}",
            result.err().map_or_else(String::new, |e| e.to_string()),
            temp.display()
        )));
    }
    result
}

fn write_and_replace(
    source: &dyn TextSnapshot,
    options: &PdfOptions,
    temp: &Path,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<PdfSummary, PdfError> {
    let mut file = PdfFile::create(temp)?;
    let mut summary = write_document(source, options, cancel, progress, &mut file)?;
    summary.bytes = file.finish(options, summary.pages)?;
    writer::verify_on_disk(temp, summary.pages)?;
    // Once more right before the move, so that a late cancel never replaces the old file.
    if cancel.load(Ordering::Relaxed) {
        return Err(PdfError::Cancelled);
    }
    std::fs::rename(temp, path).map_err(|problem| {
        PdfError::Write(format!(
            "The PDF could not be saved as {}. If another program has that file open, close \
             it and try again. ({problem})",
            path.display()
        ))
    })?;
    Ok(summary)
}

/// A running export. Dropping it does not stop it, so call `cancel` first.
pub struct PdfJob {
    cancel: Arc<AtomicBool>,
    fraction: Arc<AtomicU32>,
    handle: JoinHandle<Result<PdfSummary, PdfError>>,
}
impl PdfJob {
    /// Stops at the next check, within one line of layout. It leaves no file behind.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// 0 to 1, for a progress bar. Reading it costs one atomic load.
    pub fn fraction(&self) -> f64 {
        f64::from(self.fraction.load(Ordering::Relaxed)) / 1_000_000.0
    }
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
    /// Waits for the end. Call it after `is_finished` says true, or off the UI thread.
    pub fn join(self) -> Result<PdfSummary, PdfError> {
        self.handle
            .join()
            .map_err(|_| PdfError::Write("The PDF export stopped unexpectedly.".into()))?
    }
}

/// Starts an export on its own thread, from `EditorView::worker_snapshot()`. `wake` runs
/// on the worker when it ends, and the host then calls `join`. The Mac shows its progress
/// sheet only when a write still runs after 1 second, and reads the fraction 10 times a
/// second while it shows.
pub fn export_worker(
    source: Arc<dyn TextSnapshot>,
    options: PdfOptions,
    path: PathBuf,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<PdfJob, PdfError> {
    let cancel = Arc::new(AtomicBool::new(false));
    let fraction = Arc::new(AtomicU32::new(0));
    let (stop, shown) = (cancel.clone(), fraction.clone());
    let handle = std::thread::Builder::new()
        .name("qe-pdf-export".into())
        .spawn(move || {
            let mut report = |value: f64| {
                let value = (value.clamp(0.0, 1.0) * 1_000_000.0) as u32;
                shown.fetch_max(value, Ordering::Relaxed);
            };
            let result = export_to_file(source.as_ref(), &options, &path, &stop, &mut report);
            if let Some(wake) = wake {
                wake();
            }
            result
        })
        .map_err(|problem| PdfError::Write(format!("The PDF export could not start: {problem}")))?;
    Ok(PdfJob {
        cancel,
        fraction,
        handle,
    })
}

/// Today's short date and time in the user's own format, for `PdfOptions::date`.
pub fn header_date_now() -> String {
    use windows::Win32::Globalization::{
        DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS,
    };
    use windows::core::PCWSTR;
    let mut date = [0u16; 80];
    let mut time = [0u16; 80];
    let date_length = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            None,
            PCWSTR::null(),
            Some(&mut date),
            PCWSTR::null(),
        )
    };
    let time_length = unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            None,
            PCWSTR::null(),
            Some(&mut time),
        )
    };
    let read = |buffer: &[u16], length: i32| {
        buffer
            .get(..(length.max(1) - 1) as usize)
            .map(String::from_utf16_lossy)
            .unwrap_or_default()
    };
    let date = read(&date, date_length);
    let time = read(&time, time_length);
    format!("{date}, {time}")
        .trim_matches([',', ' '])
        .to_string()
}
