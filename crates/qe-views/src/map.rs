//! The document map's geometry and its worker, as `DocumentMapView.swift` has them.
//!
//! Each line is one row 2 dips tall, drawn as blocks for its runs of non-space
//! characters, read straight from the text and never through layout. The worker reads only
//! the rows the map can show, plus a small margin, so its work does not grow with the
//! document. It is never asked on a key: a key at most starts the 0.1 s wait.
use qe_api::{ByteOffset, LineIndex, LookupError, TextSnapshot};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender},
};

pub const MAP_WIDTH: f32 = 100.0;
pub const MAP_ROW_HEIGHT: f32 = 2.0;
pub const MAP_COLUMN_WIDTH: f32 = 1.0;
/// The height of a row's blocks. The rest of the row is the gap below.
pub const MAP_BLOCK_HEIGHT: f32 = 1.4;
/// While typing, the rows redraw at most this often.
pub const MAP_REDRAW_SECONDS: f64 = 0.1;
/// How far left of the map the pointer shows the box.
pub const MAP_HOVER_MARGIN: f32 = 8.0;
/// Rows read past each end of what shows, so that a small scroll needs no new read.
pub(crate) const MAP_SLACK_ROWS: usize = 64;
/// How far in from the map's left edge the first column starts.
pub(crate) const MAP_LEFT: f32 = 2.0;
/// The most rows one job reads. A window this tall is 16,384 dips, so a real window never
/// reaches it, and a taller one leaves its lower part blank rather than doing more work.
pub(crate) const MAP_MOST_ROWS: usize = 8192;

/// The map's top line for `line_count` lines, of which `on_screen` show and `first` is the
/// first: 0 when everything fits, or else the same fraction of the overflow as the text
/// is scrolled.
pub fn map_top_line(first: f64, on_screen: usize, line_count: usize, rows: usize) -> f64 {
    let Some(overflow) = line_count.checked_sub(rows).filter(|n| *n > 0) else {
        return 0.0;
    };
    let scrollable = line_count.saturating_sub(on_screen).max(1);
    (first / scrollable as f64).clamp(0.0, 1.0) * overflow as f64
}

/// The first line (it can be fractional) that puts the box's top at `box_top`, the inverse
/// of the box's position with `map_top_line`. It is the Mac's `firstLine(forBoxTop:)`
/// without the final rounding, because this view scrolls in dips and not in whole lines.
pub fn first_line_for_box_top(
    box_top: f64,
    line_count: usize,
    on_screen: usize,
    rows: usize,
    height: f64,
) -> f64 {
    let scrollable = line_count.saturating_sub(on_screen);
    if scrollable == 0 {
        return 0.0;
    }
    let box_rows = box_top / f64::from(MAP_ROW_HEIGHT);
    let first = if line_count <= rows {
        box_rows
    } else if rows > on_screen {
        box_rows * scrollable as f64 / (rows - on_screen) as f64
    } else {
        // The box fills the map: follow the pointer's place in it.
        box_top / height.max(1.0) * scrollable as f64
    };
    first.clamp(0.0, scrollable as f64)
}

/// What the map shows now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapGeometry {
    /// The first line on screen. It is fractional, because it follows the scroll position.
    pub first_line: f64,
    pub on_screen: usize,
    pub line_count: usize,
    pub top_line: f64,
    pub rows: usize,
}

impl MapGeometry {
    pub fn new(first_line: f64, on_screen: usize, line_count: usize, rows: usize) -> Self {
        Self {
            first_line,
            on_screen,
            line_count,
            rows,
            top_line: map_top_line(first_line, on_screen, line_count, rows),
        }
    }
    /// The box's top, in dips from the map's top.
    pub fn box_top(&self) -> f64 {
        (self.first_line - self.top_line) * f64::from(MAP_ROW_HEIGHT)
    }
    pub fn box_height(&self) -> f64 {
        self.on_screen as f64 * f64::from(MAP_ROW_HEIGHT)
    }
    /// The lines the map's rows reach, including the one a fractional top leaves partly
    /// on screen.
    pub(crate) fn lines_shown(&self) -> std::ops::Range<usize> {
        let first = self.top_line.floor() as usize;
        first..first.saturating_add(self.rows).saturating_add(1)
    }
}

/// Finds the runs of non-space characters in the start of a line, as (column, length). It
/// reads at most `columns` UTF-16 units and stops. A tab moves to the next stop.
pub fn row_runs(text: &str, columns: usize, tab_width: usize) -> Vec<(u16, u16)> {
    let mut scan = RowScan::new(columns, tab_width);
    scan.feed(text);
    scan.finish()
}

struct RowScan {
    columns: usize,
    tab_width: usize,
    column: usize,
    units: usize,
    run_start: Option<usize>,
    runs: Vec<(u16, u16)>,
}
impl RowScan {
    fn new(columns: usize, tab_width: usize) -> Self {
        Self {
            columns,
            tab_width: tab_width.max(1),
            column: 0,
            units: 0,
            run_start: None,
            runs: Vec::new(),
        }
    }
    fn end_run(&mut self) {
        if let Some(start) = self.run_start.take() {
            let end = self.column.min(self.columns);
            if end > start {
                self.runs.push((start as u16, (end - start) as u16));
            }
        }
    }
    /// Returns false when no more text can show.
    fn feed(&mut self, text: &str) -> bool {
        for unit in text.encode_utf16() {
            if self.units >= self.columns || self.column >= self.columns {
                return false;
            }
            self.units += 1;
            match unit {
                0x0A | 0x0D | 0x85 | 0x2028 | 0x2029 => self.end_run(),
                0x09 => {
                    self.end_run();
                    self.column = (self.column / self.tab_width + 1) * self.tab_width;
                }
                0x20 | 0xA0 => {
                    self.end_run();
                    self.column += 1;
                }
                _ => {
                    if self.run_start.is_none() {
                        self.run_start = Some(self.column);
                    }
                    self.column += 1;
                }
            }
        }
        true
    }
    fn finish(mut self) -> Vec<(u16, u16)> {
        self.end_run();
        self.runs
    }
}

/// Rows the worker read. Rows are in line order with no gaps, from `first_line`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MapRows {
    pub first_line: usize,
    /// One past the last run of each row, in `runs`.
    pub row_ends: Vec<u32>,
    pub runs: Vec<(u16, u16)>,
    /// One past the last byte these rows read. An edit after it cannot change them.
    pub end_byte: usize,
    /// True when the job ran past the document's last line.
    pub reached_end: bool,
}
impl MapRows {
    pub fn row(&self, line: usize) -> Option<&[(u16, u16)]> {
        let index = line.checked_sub(self.first_line)?;
        let end = *self.row_ends.get(index)? as usize;
        let start = match index {
            0 => 0,
            _ => *self.row_ends.get(index - 1)? as usize,
        };
        self.runs.get(start..end)
    }
    pub fn line_end(&self) -> usize {
        self.first_line + self.row_ends.len()
    }
    pub fn covers(&self, lines: &std::ops::Range<usize>) -> bool {
        !self.row_ends.is_empty()
            && lines.start >= self.first_line
            && (lines.end <= self.line_end() || self.reached_end)
    }
}

pub(crate) struct Job {
    pub snapshot: Arc<dyn TextSnapshot>,
    pub first_line: usize,
    pub count: usize,
    pub tab_width: usize,
    pub columns: usize,
    pub number: u64,
}

pub(crate) struct Finished {
    pub rows: MapRows,
    pub number: u64,
}

pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// One thread per window, started at the first job. With no job it sleeps in `recv`, so
/// it runs nothing while idle. It ends when the frame drops the sender.
pub(crate) struct MapWorker {
    jobs: Sender<Job>,
    results: Receiver<Finished>,
    newest: Arc<AtomicU64>,
}

impl MapWorker {
    pub fn start(wake: Option<Wake>) -> Result<Self, String> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, results) = mpsc::channel();
        let newest = Arc::new(AtomicU64::new(0));
        let seen = newest.clone();
        std::thread::Builder::new()
            .name("qe-document-map".into())
            .spawn(move || {
                while let Ok(mut job) = inbox.recv() {
                    // Only the newest job matters.
                    while let Ok(next) = inbox.try_recv() {
                        job = next;
                    }
                    let Some(rows) = read_rows(&job, &seen) else {
                        continue;
                    };
                    let finished = Finished {
                        rows,
                        number: job.number,
                    };
                    if outbox.send(finished).is_err() {
                        return;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            })
            .map_err(|problem| format!("The document map could not start: {problem}"))?;
        Ok(Self {
            jobs,
            results,
            newest,
        })
    }
    pub fn send(&mut self, job: Job) -> Result<(), String> {
        self.newest.store(job.number, Ordering::Release);
        self.jobs
            .send(job)
            .map_err(|_| "The document map's worker has stopped.".to_string())
    }
    /// The newest finished rows, if any, with no waiting.
    pub fn take(&mut self) -> Option<Finished> {
        let mut newest = None;
        while let Ok(finished) = self.results.try_recv() {
            newest = Some(finished);
        }
        newest
    }
}

/// `None` when a newer job has replaced this one.
fn read_rows(job: &Job, newest: &AtomicU64) -> Option<MapRows> {
    let mut rows = MapRows {
        first_line: job.first_line,
        ..MapRows::default()
    };
    for line in job.first_line..job.first_line.saturating_add(job.count) {
        if newest.load(Ordering::Acquire) != job.number {
            return None;
        }
        let known = match job.snapshot.try_known_line(LineIndex(line)) {
            Ok(known) => known,
            Err(LookupError::OutOfRange) => {
                rows.reached_end = true;
                break;
            }
            // The index has not reached this line. A later job reads it, once
            // `store_status` shows more. A store failure shows in the editor's warning.
            Err(_) => break,
        };
        rows.end_byte = rows.end_byte.max(known.range.end.0);
        let mut scan = RowScan::new(job.columns, job.tab_width);
        read_start_of_line(job, known.range, &mut scan);
        rows.runs.extend(scan.finish());
        rows.row_ends.push(rows.runs.len() as u32);
    }
    Some(rows)
}

/// The read's far end may fall inside a character, so it steps back until the range is
/// good. A line that still cannot be read shows empty, and the next job reads it again.
fn read_start_of_line(job: &Job, range: std::ops::Range<ByteOffset>, scan: &mut RowScan) {
    // A UTF-16 unit takes at most 3 bytes of UTF-8. Four bytes make two units.
    let limit = range
        .start
        .0
        .saturating_add(job.columns.saturating_mul(3))
        .min(range.end.0);
    for back in 0..4 {
        let end = limit.saturating_sub(back).max(range.start.0);
        let mut going = true;
        let good =
            job.snapshot
                .visit_chunks_while(range.start..ByteOffset(end), &mut |part: &str| {
                    going = going && scan.feed(part);
                    going
                });
        if good || end == range.start.0 {
            return;
        }
    }
}
