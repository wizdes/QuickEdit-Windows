//! A mapped view during another app's lock, and layout on the real `MappedStore`. The
//! host here does only what a real one does: it polls when the store wakes it, and it
//! runs a timer only when the view asks for one. No key or mouse move helps it.
use qe_api::{
    ByteOffset, Dip, DisplayList, DrawCommand, InputEvent, InputKind, LineIndex, LookupError,
    Modifiers, Point, Rect, StoreStatus, TextPosition, TextSnapshot, TextStore, Theme, ThemeKind,
    Utf16Offset, View, ViewContext, ViewRequest,
};
use qe_buffer::MappedStore;
use qe_editor::EditorView;
use qe_layout::{DirectWriteLayout, LayoutError};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Remove(PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("qe-maplock-{}-{name}", std::process::id()))
}

fn measure() -> DirectWriteLayout {
    DirectWriteLayout::new().unwrap()
}

fn rows(e: &EditorView) -> usize {
    let mut list = DisplayList::default();
    e.draw(&mut list, &Theme::new(ThemeKind::Light), &measure());
    list.commands
        .iter()
        .filter(|c| matches!(c, DrawCommand::GlyphRun { .. }))
        .count()
}

/// Polls when the store wakes it, fires the timer the view last asked for, and paints
/// only when the view asks for that. A paint asks the store for pages, so a test that
/// painted on its own would bring the view back by itself.
struct Host {
    wakes: Arc<AtomicUsize>,
    seen: usize,
    timer_at: Option<Instant>,
    polls: usize,
    timers: usize,
    painted: usize,
}

impl Host {
    fn new(wakes: Arc<AtomicUsize>) -> Self {
        Self {
            seen: 0,
            wakes,
            timer_at: None,
            polls: 0,
            timers: 0,
            painted: 0,
        }
    }
    fn note(&mut self, e: &EditorView, ctx: ViewContext) {
        let mut paint = false;
        for request in ctx.requests {
            match request {
                ViewRequest::RequestTimer(ms) => {
                    self.timer_at = Some(Instant::now() + Duration::from_millis(u64::from(ms)));
                }
                ViewRequest::Invalidate(_) => paint = true,
                _ => {}
            }
        }
        if paint {
            self.painted = rows(e);
        }
    }
    fn input(&mut self, e: &mut EditorView, kind: InputKind) {
        let mut ctx = ViewContext::default();
        e.handle(&InputEvent { time_us: 0, kind }, &mut ctx);
        self.note(e, ctx);
    }
    fn poll(&mut self, e: &mut EditorView) {
        let mut ctx = ViewContext::default();
        e.poll_layout(&mut ctx);
        self.note(e, ctx);
    }
    /// One pass of the message loop.
    fn step(&mut self, e: &mut EditorView) {
        let now = self.wakes.load(Ordering::Relaxed);
        if now != self.seen {
            self.seen = now;
            self.polls += 1;
            self.poll(e);
        }
        if self.timer_at.is_some_and(|at| at <= Instant::now()) {
            self.timer_at = None;
            self.timers += 1;
            let mut ctx = ViewContext::default();
            e.on_timer(&mut ctx);
            self.note(e, ctx);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    fn run_for(&mut self, e: &mut EditorView, time: Duration) {
        let start = Instant::now();
        while start.elapsed() < time {
            self.step(e);
        }
    }
    fn run_until(
        &mut self,
        e: &mut EditorView,
        limit: Duration,
        done: impl Fn(&Self, &EditorView) -> bool,
    ) {
        let start = Instant::now();
        while !done(self, e) {
            assert!(start.elapsed() < limit, "it never came back");
            self.step(e);
        }
    }
}

/// The view moves to a place whose page is not in memory, while the whole file is locked.
fn check_view_comes_back(name: &str, set_up: impl Fn(&mut Host, &mut EditorView)) {
    let path = temp(name);
    let line = "0123456789 abcdefghijklmnopqrstuvwxyz\n";
    std::fs::write(&path, line.repeat(300_000)).unwrap();
    let _remove = Remove(path.clone());
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let store = MappedStore::open_with_notify(
        &path,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .unwrap();
    let mut e = EditorView::from_mapped(store).unwrap();
    let area = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(800.0),
        height: Dip(600.0),
    };
    e.layout(area, &measure());
    let mut host = Host::new(wakes.clone());
    host.poll(&mut e);
    set_up(&mut host, &mut e);
    host.run_until(&mut e, Duration::from_secs(60), |host, e| {
        e.store_status().complete && host.painted > 3 && !e.pending_layout()
    });

    let locker = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    locker.lock().unwrap();
    let before = e.scroll_offset();
    let polls_before = host.polls;
    host.input(
        &mut e,
        InputKind::Wheel {
            at: Point {
                x: Dip(1.0),
                y: Dip(1.0),
            },
            delta_x: 0.0,
            delta_y: -6_000_000.0,
            modifiers: Modifiers::default(),
        },
    );
    assert!(
        e.scroll_offset() > before + 100_000.0,
        "the view did not go far"
    );
    host.run_for(&mut e, Duration::from_millis(350));
    assert_eq!(host.painted, 0, "the lock proves nothing: its text arrived");
    assert_eq!(e.store_status().failure, None, "a lock is not a failure");
    assert!(
        host.polls > polls_before,
        "the store never woke the view during the lock"
    );
    locker.unlock().unwrap();
    host.run_until(&mut e, Duration::from_secs(5), |host, _| host.painted > 30);
    host.run_until(&mut e, Duration::from_secs(5), |_, e| !e.pending_layout());
    assert_eq!(e.store_status().failure, None);

    // With the lock over, nothing goes on running: no wake, no poll and no timer.
    host.run_for(&mut e, Duration::from_millis(250));
    let (polls, timers) = (host.polls, host.timers);
    assert!(host.timer_at.is_none(), "a timer stayed on");
    host.run_for(&mut e, Duration::from_millis(400));
    assert_eq!(host.polls, polls, "the store went on waking the view");
    assert_eq!(host.timers, timers, "a timer went on running");
}

#[test]
fn a_view_with_no_focus_comes_back_after_a_lock_with_no_input() {
    check_view_comes_back("no-focus.txt", |_, _| {});
}

#[test]
fn a_view_with_a_selection_comes_back_after_a_lock_with_no_input() {
    check_view_comes_back("selection.txt", |host, e| {
        host.input(e, InputKind::Focus);
        let mut ctx = ViewContext::default();
        e.select(ByteOffset(0)..ByteOffset(5), &mut ctx);
        host.note(e, ctx);
    });
}

/// The file opens during the lock, so even the first page is not known. It shows no
/// made-up text, and it comes back by itself.
#[test]
fn a_view_of_a_file_opened_during_a_lock_comes_back_with_no_input() {
    let path = temp("opened.txt");
    let line = "0123456789 abcdefghijklmnopqrstuvwxyz\n";
    std::fs::write(&path, line.repeat(3000)).unwrap();
    let _remove = Remove(path.clone());
    let locker = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    locker.lock().unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let store = MappedStore::open_with_notify(
        &path,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .unwrap();
    let mut e = EditorView::from_mapped(store).unwrap();
    let area = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(800.0),
        height: Dip(600.0),
    };
    e.layout(area, &measure());
    let mut host = Host::new(wakes);
    host.poll(&mut e);
    host.run_for(&mut e, Duration::from_millis(200));
    assert_eq!(rows(&e), 0, "text with no source");
    assert_eq!(e.store_status().failure, None, "a lock is not a failure");
    locker.unlock().unwrap();
    host.run_until(&mut e, Duration::from_secs(5), |host, _| host.painted > 30);
    assert_eq!(e.store_status().failure, None);
}

/// The window's thread sees a mapped snapshot through this, as the editor's own wrapper
/// does: a page that is not in memory gives no answer.
struct NonBlocking(Arc<dyn TextSnapshot>);
impl TextSnapshot for NonBlocking {
    fn revision(&self) -> u64 {
        1
    }
    fn len_bytes(&self) -> ByteOffset {
        self.0.status().known_bytes
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.0.status().known_utf16
    }
    fn line_count(&self) -> usize {
        self.0.status().known_lines
    }
    fn byte_to_utf16(&self, b: ByteOffset) -> Option<Utf16Offset> {
        self.0.try_byte_to_utf16(b).ok()
    }
    fn utf16_to_byte(&self, u: Utf16Offset) -> Option<ByteOffset> {
        self.0.try_utf16_to_byte(u).ok()
    }
    fn byte_to_position(&self, b: ByteOffset) -> Option<TextPosition> {
        self.0.try_byte_to_position(b).ok()
    }
    fn position_to_byte(&self, p: TextPosition) -> Option<ByteOffset> {
        self.0.try_position_to_byte(p).ok()
    }
    fn line_range(&self, l: LineIndex) -> Option<Range<ByteOffset>> {
        self.0.try_line_range(l).ok()
    }
    fn visit_chunks(&self, r: Range<ByteOffset>, v: &mut dyn FnMut(&str)) -> bool {
        self.0.try_visit_chunks(r, v).is_ok()
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        self.0.status()
    }
    fn try_byte_to_utf16(&self, b: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.0.try_byte_to_utf16(b)
    }
}

const LONG_LINES: usize = 30;
const LONG_LINE: usize = 70_000;

fn long_lines_file(name: &str) -> (Remove, MappedStore) {
    let path = temp(name);
    let line = "word ".repeat(LONG_LINE / 5);
    let mut text = String::with_capacity((LONG_LINE + 1) * LONG_LINES);
    for _ in 0..LONG_LINES {
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(&path, &text).unwrap();
    let store = MappedStore::open(&path).unwrap();
    let start = Instant::now();
    while !store.is_index_complete() {
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(1));
    }
    store.set_test_read_delay(Duration::from_millis(5));
    (Remove(path), store)
}

/// The run that used to panic in debug and give wrong edges in release, on the real
/// store: 70,000-character lines, a 5 ms read delay, and asks that race the page.
#[test]
fn segment_edges_on_a_page_still_on_its_way_are_an_error_or_the_truth() {
    let (_remove, store) = long_lines_file("edges.txt");
    let engine = measure();
    let snapshot = store.snapshot();
    let window_view = NonBlocking(snapshot.clone());
    let mut asked = 0;
    let mut not_yet = 0;
    for line in (1..LONG_LINES).rev() {
        for index in 0..8 {
            let truth = engine
                .segment_range(snapshot.as_ref(), LineIndex(line), index)
                .unwrap();
            // Push this page out with reads of other lines, then ask with no wait.
            for far in [0, LONG_LINES / 2, LONG_LINES - 1] {
                let _ = engine.segment_range(snapshot.as_ref(), LineIndex(far), 20);
            }
            for _ in 0..2 {
                asked += 1;
                match engine.segment_range(&window_view, LineIndex(line), index) {
                    Ok(range) => assert_eq!(range, truth, "line {line}, segment {index}"),
                    Err(LayoutError::BadRange(_)) => not_yet += 1,
                    Err(e) => panic!("{e}"),
                }
            }
        }
    }
    println!("{asked} asks, {not_yet} not yet");
    assert_eq!(store.error_state(), None);
}

/// A worker's snapshot waits for its page, so a page out of memory is never an error.
/// A line that failed that way would stay failed until its list is cleared.
#[test]
fn a_worker_snapshot_always_gets_segment_edges_for_a_page_out_of_memory() {
    let (_remove, store) = long_lines_file("worker.txt");
    let engine = measure();
    let snapshot = store.snapshot();
    let reads_before = store.lookup_disk_reads();
    for round in 0..3 {
        for line in 0..LONG_LINES {
            for index in [0, 5, 20] {
                engine
                    .segment_range(snapshot.as_ref(), LineIndex(line), index)
                    .unwrap_or_else(|e| panic!("round {round}, line {line}: {e}"));
            }
        }
    }
    assert!(
        store.lookup_disk_reads() > reads_before + 16,
        "pages stayed in memory, which proves nothing"
    );
}
