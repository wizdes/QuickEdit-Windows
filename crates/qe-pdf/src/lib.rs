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
use qe_api::testing::StringStore;
use qe_api::{ByteOffset, Dip, LayoutRow, LineIndex, TextSnapshot};
use qe_layout::DirectWriteLayout;
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use writer::PdfFile;

/// A point of paper is this many dips.
pub(crate) const DIPS_PER_POINT: f32 = 96.0 / 72.0;
/// PDF readers commonly stop at this many objects (PDF 1.7, Annex C).
const MOST_OBJECTS: usize = 8_388_607;
/// A PDF's table of objects has 10 digits for each place in the file.
const MOST_BYTES: u64 = 9_999_999_999;
/// How often a worker that waits for a whole text wakes the host, which then offers it.
const WAKE_EVERY: Duration = Duration::from_millis(100);
/// How often that worker reads cancel.
const WAIT_STEP: Duration = Duration::from_millis(50);
/// The share of the progress bar that the wait for the whole text takes.
const WAIT_SHARE: f64 = 0.1;

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
    /// What a PDF can hold. Only a test changes these.
    pub limits: PdfLimits,
    /// Runs on the finished temp file before its check. Only a test sets this.
    pub after_write: Option<fn(&Path)>,
}

/// What a PDF can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfLimits {
    pub most_objects: usize,
    pub most_bytes: u64,
}
impl Default for PdfLimits {
    fn default() -> Self {
        Self {
            most_objects: MOST_OBJECTS,
            most_bytes: MOST_BYTES,
        }
    }
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
            limits: PdfLimits::default(),
            after_write: None,
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
    /// The text could not be read, or its file failed.
    Read(String),
    /// The text given is only part of the document, so nothing was written.
    NotComplete(String),
}
impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "The PDF export was cancelled."),
            Self::BadOptions(message)
            | Self::Font(message)
            | Self::Write(message)
            | Self::TooLong(message)
            | Self::Read(message)
            | Self::NotComplete(message) => write!(f, "{message}"),
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

fn too_long(limits: &PdfLimits, needs: &str) -> PdfError {
    PdfError::TooLong(format!(
        "This document is too long for a PDF: it needs {needs}, and a PDF holds at most {} \
         objects and {} bytes. No PDF was written.",
        limits.most_objects, limits.most_bytes
    ))
}

fn not_complete() -> PdfError {
    PdfError::NotComplete(
        "QuickEdit has not finished reading this file, so no PDF was written. Export it \
         again once the file has been read."
            .into(),
    )
}

fn read_failed(message: &str) -> PdfError {
    PdfError::Read(format!(
        "The file could not be read to its end, so no PDF was written: {message}"
    ))
}

/// A whole document, or an error. A mapped file's snapshot holds what was known when it was
/// taken, so an export of one taken during the index would end early with no warning. Each
/// line of such a snapshot can look whole, so only the status can say.
fn check_whole(source: &dyn TextSnapshot) -> Result<(), PdfError> {
    let status = source.status();
    if let Some(failure) = &status.failure {
        return Err(read_failed(&failure.message));
    }
    if !status.complete {
        return Err(not_complete());
    }
    let last = LineIndex(source.line_count().saturating_sub(1));
    match source.line_range(last) {
        Some(range) if range.end == source.len_bytes() => Ok(()),
        _ => Err(not_complete()),
    }
}

fn make_engine(options: &PdfOptions, width: f32) -> Result<DirectWriteLayout, PdfError> {
    Ok(DirectWriteLayout::with_font(
        &options.font_family,
        options.font_points,
        DWRITE_FONT_WEIGHT_NORMAL,
    )
    .map_err(|e| PdfError::Font(format!("The PDF's font could not be used: {e}")))?
    .with_wrap(true)
    .with_tab_width(options.tab_width)
    .with_full_rows_at(Dip(width)))
}

/// The options' own font and em size, and its row height, from one laid-out letter.
fn plain_row(engine: &DirectWriteLayout, width: f32) -> Result<((u32, f32), f32), PdfError> {
    let store = StringStore::new("x");
    let layout = engine
        .try_layout_line(&store, LineIndex(0), Dip(width))
        .map_err(|e| PdfError::Font(format!("The PDF's font could not be used: {e}")))?;
    layout
        .rows
        .first()
        .and_then(|row| Some((row.glyph_runs.first()?, row.height.0)))
        .map(|(run, height)| ((run.font_id, run.font_em_size.0), height))
        .ok_or_else(|| PdfError::Font("The PDF's font laid out no letter.".into()))
}

/// Finds a document too long for a PDF before anything is written. Each line takes at least
/// one row, each page holds so many rows, and each page takes two or three objects. So
/// this never refuses a file that would have fit. The checks during the write catch the rest.
fn check_fits(source: &dyn TextSnapshot, options: &PdfOptions) -> Result<(), PdfError> {
    let (width, scale) = wrap_width_and_scale(options);
    let engine = make_engine(options, width)?;
    let (_, height) = plain_row(&engine, width)?;
    let page = f64::from(options.paper_height - 2.0 * options.margin) * f64::from(DIPS_PER_POINT)
        / f64::from(scale);
    let rows = (page / f64::from(height.max(0.001))).floor().max(1.0) as usize;
    let pages = source.line_count().div_ceil(rows);
    let each = if options.header_and_footer { 3 } else { 2 };
    if pages.saturating_mul(each) > options.limits.most_objects {
        return Err(too_long(&options.limits, &format!("{pages} pages")));
    }
    Ok(())
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
    let (width, scale) = wrap_width_and_scale(options);
    let engine = make_engine(options, width)?;
    let (plain_font, _) = plain_row(&engine, width)?;
    let mut builder = PageBuilder::new(options, scale, plain_font);
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
                    check_size(file, &options.limits)?;
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
    check_size(file, &options.limits)?;
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

/// Stops as soon as the file is past what a PDF can hold.
fn check_size(file: &PdfFile, limits: &PdfLimits) -> Result<(), PdfError> {
    if file.object_count() > limits.most_objects {
        return Err(too_long(
            limits,
            &format!("more than {} objects", limits.most_objects),
        ));
    }
    if file.bytes_written() > limits.most_bytes {
        return Err(too_long(
            limits,
            &format!("more than {} bytes", limits.most_bytes),
        ));
    }
    Ok(())
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
///
/// `source` must be the whole document. A mapped file's snapshot taken during its index is
/// refused with `NotComplete`. `export_worker` waits for a whole one.
pub fn export_to_file(
    source: &dyn TextSnapshot,
    options: &PdfOptions,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<PdfSummary, PdfError> {
    check_options(options)?;
    check_whole(source)?;
    check_fits(source, options)?;
    if path.file_name().is_none() {
        return Err(PdfError::Write("The PDF's path has no file name.".into()));
    }
    let parent = match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let number = TEMP_NUMBER.fetch_add(1, Ordering::Relaxed);
    // This name owes nothing to the target's, so a long target name still has room.
    let temp = parent.join(format!(".qe-pdf-{}-{number}.tmp", std::process::id()));
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
    // The new file replaces the old one, so it starts with the old one's rules.
    let mut rules = writer::read_access_rules(path)?;
    let mut file = PdfFile::create(temp, rules.as_mut())?;
    let mut summary = write_document(source, options, cancel, progress, &mut file)?;
    summary.bytes = file.finish(options, summary.pages)?;
    if let Some(after_write) = options.after_write {
        after_write(temp);
    }
    if summary.bytes > options.limits.most_bytes {
        return Err(too_long(
            &options.limits,
            &format!("{} bytes", summary.bytes),
        ));
    }
    // The move below needs the file closed.
    drop(file);
    writer::verify_on_disk(temp, summary.pages, cancel)?;
    // A store can fail after the export began, and reads then give what they can.
    if let Some(failure) = source.status().failure {
        return Err(read_failed(&failure.message));
    }
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

/// A running export. Dropping it does not stop a write, so call `cancel` first. A worker
/// that still waits for a whole text ends with `NotComplete` when the job is dropped.
pub struct PdfJob {
    cancel: Arc<AtomicBool>,
    fraction: Arc<AtomicU32>,
    wants: Arc<AtomicBool>,
    offers: Sender<Arc<dyn TextSnapshot>>,
    /// The result, set before `finished` and before the last wake.
    result: Arc<Mutex<Option<Result<PdfSummary, PdfError>>>>,
    finished: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}
impl PdfJob {
    /// Stops at the next check: within one line of layout, one glyph of a font, or the wait
    /// for a whole text. It leaves no file behind.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// 0 to 1, for a progress bar. Reading it costs one atomic load.
    pub fn fraction(&self) -> f64 {
        f64::from(self.fraction.load(Ordering::Relaxed)) / 1_000_000.0
    }
    /// True while the worker waits for a file's index, so a host can say "Reading the file".
    /// On each wake, a host then calls `offer_snapshot(editor.worker_snapshot())`.
    pub fn wants_snapshot(&self) -> bool {
        self.wants.load(Ordering::Acquire)
    }
    /// Gives the waiting worker a newer snapshot. A partial one only moves the progress bar.
    /// This reads the snapshot's status and nothing more, so the UI thread can call it.
    pub fn offer_snapshot(&self, snapshot: Arc<dyn TextSnapshot>) {
        let status = snapshot.status();
        if status.complete || status.failure.is_some() {
            // A worker that has gone needs nothing, and `join` gives its result.
            let _ = self.offers.send(snapshot);
        } else {
            let known = status.known_bytes.0 as f64 / snapshot.len_bytes().0.max(1) as f64;
            self.fraction
                .fetch_max(to_fraction(known * WAIT_SHARE), Ordering::Relaxed);
        }
    }
    /// True once the result is ready. It is true by the time of the last wake, so a host that
    /// reads it only on a wake always sees the end.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    /// Waits for the end. Call it after `is_finished` says true, or off the UI thread.
    pub fn join(self) -> Result<PdfSummary, PdfError> {
        // With no more offers coming, a worker that still waits ends now.
        drop(self.offers);
        let stopped = || PdfError::Write("The PDF export stopped unexpectedly.".into());
        self.handle.join().map_err(|_| stopped())?;
        self.result
            .lock()
            .map_err(|_| stopped())?
            .take()
            .ok_or_else(stopped)?
    }
}

fn to_fraction(value: f64) -> u32 {
    (value.clamp(0.0, 1.0) * 1_000_000.0) as u32
}

/// Waits until the source is whole, by asking the host for newer snapshots. The host holds
/// the editor, and it answers on the UI thread, which never waits for this worker.
fn wait_for_whole(
    mut source: Arc<dyn TextSnapshot>,
    offers: &mpsc::Receiver<Arc<dyn TextSnapshot>>,
    wants: &AtomicBool,
    cancel: &AtomicBool,
    wake: Option<&(dyn Fn() + Send + Sync)>,
) -> Result<Arc<dyn TextSnapshot>, PdfError> {
    let mut last_wake: Option<Instant> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(PdfError::Cancelled);
        }
        let status = source.status();
        if status.failure.is_some() || status.complete {
            return Ok(source);
        }
        wants.store(true, Ordering::Release);
        if last_wake.is_none_or(|at| at.elapsed() >= WAKE_EVERY) {
            last_wake = Some(Instant::now());
            if let Some(wake) = wake {
                wake();
            }
        }
        match offers.recv_timeout(WAIT_STEP) {
            Ok(newer) => source = newer,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(not_complete()),
        }
    }
}

/// Starts an export on its own thread, from `EditorView::worker_snapshot()`. `wake` runs on
/// the worker when it ends, and about 10 times a second while it waits for a mapped file's
/// index. On each wake the host calls `offer_snapshot` if `wants_snapshot` says so, and
/// `join` if `is_finished` says so. The Mac shows its progress sheet only when a write
/// still runs after 1 second, and reads the fraction 10 times a second while it shows.
pub fn export_worker(
    source: Arc<dyn TextSnapshot>,
    options: PdfOptions,
    path: PathBuf,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<PdfJob, PdfError> {
    let cancel = Arc::new(AtomicBool::new(false));
    let fraction = Arc::new(AtomicU32::new(0));
    let wants = Arc::new(AtomicBool::new(false));
    let (offers, inbox) = mpsc::channel();
    let (stop, shown, asking) = (cancel.clone(), fraction.clone(), wants.clone());
    let result = Arc::new(Mutex::new(None));
    let finished = Arc::new(AtomicBool::new(false));
    let (slot, done) = (result.clone(), finished.clone());
    let handle = std::thread::Builder::new()
        .name("qe-pdf-export".into())
        .spawn(move || {
            let waited = !source.status().complete;
            let whole = wait_for_whole(source, &inbox, &asking, &stop, wake.as_deref());
            asking.store(false, Ordering::Release);
            let start = if waited { WAIT_SHARE } else { 0.0 };
            let mut report = |value: f64| {
                let value = to_fraction(start + value * (1.0 - start));
                shown.fetch_max(value, Ordering::Relaxed);
            };
            let result = whole.and_then(|source| {
                export_to_file(source.as_ref(), &options, &path, &stop, &mut report)
            });
            // In this order: a host that reads `is_finished` on the last wake must see true.
            match slot.lock() {
                Ok(mut slot) => *slot = Some(result),
                // Only a panic while holding this lock poisons it, and the app aborts on panic.
                Err(mut poisoned) => **poisoned.get_mut() = Some(result),
            }
            done.store(true, Ordering::Release);
            if let Some(wake) = wake {
                wake();
            }
        })
        .map_err(|problem| PdfError::Write(format!("The PDF export could not start: {problem}")))?;
    Ok(PdfJob {
        cancel,
        fraction,
        wants,
        offers,
        result,
        finished,
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
