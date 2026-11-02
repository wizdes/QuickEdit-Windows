//! Spell check through Windows' `ISpellChecker`, on a worker thread.
//!
//! # What runs where
//! - The UI thread owns a [`SpellService`], only while spelling is on. It never waits for
//!   the checker. It sends text as a snapshot, and reads answers in [`SpellService::poll`].
//! - The worker owns the checker, and is the only thread that loads the checker's DLL.
//!
//! # The host's calls
//! 1. [`SpellService::start`] when spelling turns on. With spelling off, nothing of this
//!    crate runs and no spelling DLL loads.
//! 2. [`SpellService::refresh`] after an edit, a scroll, or a `poll_layout`.
//! 3. [`SpellService::poll`] when the wake it was given fires.
//! 4. [`SpellService::menu_at`] on `ViewRequest::ContextMenu`.
//!
//! # What a check covers
//! The text on screen and [`AHEAD_BYTES`] below it, less what was already checked. After an
//! edit, only the words that it touched go back to the checker.
mod checker;
mod ranges;
mod worker;

pub use checker::{
    Action, Checker, RawError, WindowsChecker, choose_language, default_language,
    without_sort_order,
};
pub use ranges::CheckedRanges;

use qe_api::{ByteOffset, Point, ViewContext};
use qe_editor::{ChangesSince, DecorationKind, EditorView, TextEdit, prepare_edits};
use std::{
    collections::HashSet,
    fmt,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};
use worker::{Answer, Command, Job};

/// Over this many bytes, spelling turns off, as on the Mac.
pub const MAX_BYTES: usize = 50 * 1024 * 1024;
/// The fix menu shows at most this many suggestions. The Mac leaves the number to AppKit,
/// and its source does not state one, so this number is a choice for the user to check.
pub const MAX_SUGGESTIONS: usize = 5;
/// How far below the screen a check reaches, so that a small scroll finds it done.
pub const AHEAD_BYTES: usize = 4096;
/// Marks past this count are dropped when far from the screen, because setting the marks
/// takes time in proportion to the list.
const MAX_MARKS: usize = 20_000;
const KEEP_NEAR_BYTES: usize = 1024 * 1024;
/// Each edit walks the list of checked ranges, so a long scroll must not make it long.
const MAX_CHECKED_RANGES: usize = 4096;

/// Why spelling does not run for a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpellOff {
    /// The setting is off.
    NotOn,
    /// Over 50 MB, or a mapped file. The host shows the large-file title.
    TooLarge,
    /// A read-only view shows no underlines, and offers no fixes.
    ReadOnly,
}
impl fmt::Display for SpellOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotOn => "Spelling is off.",
            Self::TooLarge => "Spelling is off for files over 50 MB.",
            Self::ReadOnly => "Spelling is off while this document cannot be edited.",
        })
    }
}

/// Why this view gets no spelling now, if any reason applies. A mapped file is always over
/// the limit, even while its index knows less than 50 MB of it.
pub fn why_off(view: &EditorView, enabled: bool) -> Option<SpellOff> {
    if !enabled {
        return Some(SpellOff::NotOn);
    }
    let bytes = view.store_status().known_bytes.0;
    if bytes > MAX_BYTES || view.worker_snapshot().is_read_only() {
        return Some(SpellOff::TooLarge);
    }
    if !view.can_edit() {
        return Some(SpellOff::ReadOnly);
    }
    None
}

/// One misspelled word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misspelling {
    pub range: Range<ByteOffset>,
    pub word: String,
    pub suggestions: Vec<String>,
}

/// The fix menu's data, for a right click on a misspelled word. The host builds and paints
/// the menu, because `qe-ui` is not a dependency here. See the items in [`FixMenu::items`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixMenu {
    pub range: Range<ByteOffset>,
    pub word: String,
    pub suggestions: Vec<String>,
    revision: u64,
    document: u64,
}
/// One line of the fix menu. The Mac's menu comes from AppKit, not from its own source. This
/// order is AppKit's from memory: the suggestions, Ignore Spelling, then Learn Spelling.
/// AppKit also lists more, such as autocorrect, which belongs to other milestones of this row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FixItem {
    /// Replaces the word with this text.
    Suggestion(String),
    /// Shown as a gray line when there are no suggestions.
    NoSuggestions,
    AddToDictionary,
    Ignore,
}
impl FixMenu {
    pub fn items(&self) -> Vec<FixItem> {
        let mut items: Vec<FixItem> = self
            .suggestions
            .iter()
            .cloned()
            .map(FixItem::Suggestion)
            .collect();
        if items.is_empty() {
            items.push(FixItem::NoSuggestions);
        }
        items.push(FixItem::Ignore);
        items.push(FixItem::AddToDictionary);
        items
    }
}

/// What a [`SpellService::refresh`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpellState {
    /// All that is on screen has been checked.
    Done,
    /// A check is on the worker. The wake fires when it ends.
    Working,
    /// The layout has not settled. Call `refresh` after the next `poll_layout`.
    WaitingForLayout,
    /// Spelling turned off for this reason, and the marks are gone.
    Off(SpellOff),
    /// The checker failed, and `poll` has reported why. Spelling stays off for this view.
    Failed,
}

/// What a [`SpellService::poll`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpellPoll {
    /// The marks changed.
    pub marks_changed: bool,
    /// The answer to [`SpellService::find_next`], when it has come.
    pub found: Option<FindResult>,
    /// One thing failed, and spelling stays on. Show this to the user.
    pub problem: Option<String>,
}

/// How a [`SpellService::find_next`] ended. Each call gets exactly one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindResult {
    Found(Misspelling),
    /// The whole document has no misspelling.
    NoneLeft,
    /// The text changed, or spelling turned off, so the search stopped. Ask again if needed.
    TextChanged,
}

/// Counts for tests and for measuring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpellCounts {
    pub jobs_sent: u64,
    pub bytes_asked: u64,
    pub answers_applied: u64,
    pub answers_dropped: u64,
    pub marks_set: u64,
}

type Wake = Arc<dyn Fn() + Send + Sync>;
/// A newer number tells the worker to stop what is older. A search has its own number, so
/// that a check does not stop it, and it does not stop a check.
#[derive(Default)]
pub(crate) struct Latest {
    pub(crate) check: AtomicU64,
    pub(crate) find: AtomicU64,
}
/// Makes the checker on the worker, the only thread that touches it.
pub type MakeChecker = Box<dyn FnOnce() -> Result<Box<dyn Checker>, String> + Send>;

/// Spelling for one view.
pub struct SpellService {
    jobs: Sender<Command>,
    answers: Receiver<Answer>,
    latest: Arc<Latest>,
    marks: Vec<Misspelling>,
    checked: CheckedRanges,
    revision: u64,
    document: u64,
    in_flight: bool,
    finding: bool,
    /// A search stopped, and the next `poll` says so.
    find_stopped: bool,
    ignored: HashSet<String>,
    failed: bool,
    /// Why the worker never began, for the first `poll` to say.
    start_failure: Option<String>,
    off: Option<SpellOff>,
    counts: SpellCounts,
    max_marks: usize,
    keep_near_bytes: usize,
    max_checked_ranges: usize,
}
impl Drop for SpellService {
    /// The worker must not go on with a long check for a tab that has closed. A search stops
    /// by itself, because it sees the commands' channel close between pieces.
    fn drop(&mut self) {
        self.latest.check.fetch_add(1, Ordering::Relaxed);
    }
}

impl SpellService {
    /// Starts the worker when this view can have spelling. `language` is a tag such as
    /// `en-US`, or `None` for the user's language, which is what the Mac follows. Returns
    /// the reason when it stays off, and then loads nothing.
    pub fn start(
        view: &EditorView,
        enabled: bool,
        language: Option<String>,
        wake: Wake,
    ) -> Result<Self, SpellOff> {
        if let Some(off) = why_off(view, enabled) {
            return Err(off);
        }
        Ok(Self::start_with(
            view,
            Box::new(move || {
                WindowsChecker::new(language.as_deref())
                    .map(|checker| Box::new(checker) as Box<dyn Checker>)
            }),
            wake,
        ))
    }
    /// [`SpellService::start`] with another checker. Tests use this.
    pub fn start_with(view: &EditorView, make: MakeChecker, wake: Wake) -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (answer_tx, answer_rx) = mpsc::channel();
        let latest = Arc::new(Latest::default());
        let for_worker = latest.clone();
        let spawned = thread::Builder::new()
            .name("qe-spell".into())
            .spawn(move || worker::run(make, job_rx, answer_tx, wake, for_worker));
        let mut service = Self {
            jobs: job_tx,
            answers: answer_rx,
            latest,
            marks: Vec::new(),
            checked: CheckedRanges::default(),
            revision: view.revision(),
            document: view.document_number(),
            in_flight: false,
            finding: false,
            find_stopped: false,
            ignored: HashSet::new(),
            failed: false,
            start_failure: None,
            off: None,
            counts: SpellCounts::default(),
            max_marks: MAX_MARKS,
            keep_near_bytes: KEEP_NEAR_BYTES,
            max_checked_ranges: MAX_CHECKED_RANGES,
        };
        if let Err(e) = spawned {
            service.failed = true;
            service.start_failure = Some(format!("The spell checker could not start: {e}"));
        }
        service
    }
    pub fn counts(&self) -> SpellCounts {
        self.counts
    }
    /// Smaller limits for the marks' list, so that a test can reach them quickly.
    #[doc(hidden)]
    pub fn set_limits_for_test(&mut self, max_marks: usize, keep_near_bytes: usize) {
        self.max_marks = max_marks;
        self.keep_near_bytes = keep_near_bytes;
    }
    /// A smaller limit for the checked ranges, so that a test can reach it quickly.
    #[doc(hidden)]
    pub fn set_range_limit_for_test(&mut self, max_checked_ranges: usize) {
        self.max_checked_ranges = max_checked_ranges;
    }
    /// How many separate checked ranges the service holds.
    #[doc(hidden)]
    pub fn checked_range_count(&self) -> usize {
        self.checked.ranges().len()
    }
    /// Fills the lists to their limits, as a very long session would, to measure an edit.
    #[doc(hidden)]
    pub fn fill_lists_for_test(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        self.catch_up(view);
        for at in 0..MAX_MARKS {
            self.marks.push(Misspelling {
                range: ByteOffset(1_000 + at * 16)..ByteOffset(1_000 + at * 16 + 4),
                word: "fill".to_owned(),
                suggestions: Vec::new(),
            });
        }
        for at in 0..MAX_CHECKED_RANGES {
            self.checked.add(1_000 + at * 64..1_000 + at * 64 + 32);
        }
        self.show_marks(view, ctx);
    }
    pub fn marks(&self) -> &[Misspelling] {
        &self.marks
    }
    /// True while a check or a search is on the worker.
    pub fn is_working(&self) -> bool {
        self.in_flight || self.finding
    }

    /// Sends what still needs a check. It takes one look at the change log, and it never
    /// waits for the checker. One job runs at a time, and `poll` sends the next.
    pub fn refresh(&mut self, view: &mut EditorView, ctx: &mut ViewContext) -> SpellState {
        if self.failed {
            return SpellState::Failed;
        }
        if let Some(off) = why_off(view, true) {
            self.turn_off(view, off, ctx);
            return SpellState::Off(off);
        }
        self.off = None;
        self.catch_up(view);
        if self.in_flight {
            return SpellState::Working;
        }
        let Ok(visible) = view.visible_range() else {
            return SpellState::WaitingForLayout;
        };
        let known = view.store_status().known_bytes.0;
        let window = visible.start.0..visible.end.0.saturating_add(AHEAD_BYTES).min(known);
        let gaps = self.checked.gaps(&window);
        if gaps.is_empty() {
            return SpellState::Done;
        }
        self.prune(&window, view, ctx);
        let generation = self.latest.check.fetch_add(1, Ordering::Relaxed) + 1;
        self.counts.jobs_sent += 1;
        self.counts.bytes_asked += gaps.iter().map(|g| g.len() as u64).sum::<u64>();
        let job = Job {
            snapshot: view.worker_snapshot(),
            ranges: gaps,
            revision: view.revision(),
            document: view.document_number(),
            generation,
        };
        if self.jobs.send(Command::Check(job)).is_err() {
            self.failed = true;
            return SpellState::Failed;
        }
        self.in_flight = true;
        SpellState::Working
    }
    /// Reads the worker's answers, sets the marks, and sends the next check. A failure comes
    /// back once, in plain words, and spelling then stays off for this view.
    pub fn poll(
        &mut self,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) -> Result<SpellPoll, String> {
        let mut out = SpellPoll::default();
        self.catch_up(view);
        let off_now = why_off(view, true);
        let mut changed = false;
        let mut failure = self.start_failure.take();
        loop {
            let answer = match self.answers.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => break,
                // The worker has gone with work unanswered, so no answer can come.
                Err(TryRecvError::Disconnected) => {
                    if self.in_flight || self.finding {
                        self.in_flight = false;
                        self.finding = false;
                        self.failed = true;
                        failure = Some(stopped());
                    }
                    break;
                }
            };
            match answer {
                Answer::Checked {
                    revision,
                    document,
                    checked,
                    errors,
                } => {
                    self.in_flight = false;
                    changed |= self.apply_answer(view, revision, document, checked, errors);
                }
                Answer::Skipped => {
                    self.in_flight = false;
                    self.counts.answers_dropped += 1;
                }
                Answer::Found { generation, found } => {
                    // A search that stopped or was replaced has its answer elsewhere.
                    if self.finding && generation == self.latest.find.load(Ordering::Relaxed) {
                        self.finding = false;
                        out.found = Some(found.map_or(FindResult::NoneLeft, FindResult::Found));
                    } else {
                        self.counts.answers_dropped += 1;
                    }
                }
                Answer::Problem(message) => out.problem = Some(message),
                Answer::AddFailed { word, message } => {
                    // The word is not in the dictionary after all, so a new check marks it.
                    self.ignored.remove(&word);
                    self.checked.clear();
                    out.problem = Some(message);
                }
                Answer::Failed(message) => {
                    self.in_flight = false;
                    self.finding = false;
                    self.failed = true;
                    failure = Some(message);
                }
            }
        }
        if let Some(message) = failure {
            self.clear_marks(view, ctx);
            return Err(message);
        }
        if let Some(off) = off_now {
            out.marks_changed = !view
                .decorations(DecorationKind::SpellingUnderline)
                .is_empty();
            self.turn_off(view, off, ctx);
        } else if changed {
            self.show_marks(view, ctx);
            out.marks_changed = true;
        }
        if std::mem::take(&mut self.find_stopped) {
            out.found = Some(FindResult::TextChanged);
        }
        if !self.failed && !self.in_flight && off_now.is_none() {
            let _ = self.refresh(view, ctx);
        }
        Ok(out)
    }
    /// The fix menu for a byte, when it is in a misspelled word.
    pub fn menu_at(&mut self, view: &EditorView, byte: ByteOffset) -> Option<FixMenu> {
        if why_off(view, true).is_some() {
            return None;
        }
        self.catch_up(view);
        let at = self.marks.partition_point(|m| m.range.end <= byte);
        let mark = self.marks.get(at).filter(|m| m.range.start <= byte)?;
        Some(FixMenu {
            range: mark.range.clone(),
            word: mark.word.clone(),
            suggestions: mark.suggestions.clone(),
            revision: view.revision(),
            document: view.document_number(),
        })
    }
    /// [`SpellService::menu_at`] for `ViewRequest::ContextMenu`. A point with no layout under
    /// it has no menu. The Mac selects the word when it opens this menu, so the host calls
    /// `select(menu.range)` too.
    pub fn menu_at_point(&mut self, view: &EditorView, at: Point) -> Option<FixMenu> {
        let hit = view.byte_at_point(at).ok()?;
        self.catch_up(view);
        // The hit is the nearest caret place, so the right half of a word's last letter gives
        // the word's end. The word's own rectangles say whether the point is on it.
        let first = self.marks.partition_point(|m| m.range.end < hit.byte);
        let mark = self
            .marks
            .get(first)
            .filter(|m| m.range.start <= hit.byte)?;
        let start = mark.range.start;
        let on_word = view
            .range_rects(mark.range.clone())
            .ok()?
            .iter()
            .any(|part| {
                let r = part.rect;
                at.x.0 >= r.x.0
                    && at.x.0 < r.x.0 + r.width.0
                    && at.y.0 >= r.y.0
                    && at.y.0 < r.y.0 + r.height.0
            });
        if on_word {
            self.menu_at(view, start)
        } else {
            None
        }
    }
    /// Replaces the word with a suggestion, as one edit and one undo step.
    pub fn apply_fix(
        &mut self,
        view: &mut EditorView,
        menu: &FixMenu,
        replacement: &str,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        if !view.can_edit() {
            return Err("This document cannot be edited, so the word stays as it is.".into());
        }
        if menu.revision != view.revision() || menu.document != view.document_number() {
            return Err("The text changed after this menu opened. Open the menu again.".into());
        }
        let edit = TextEdit {
            range: menu.range.clone(),
            text: replacement.to_owned(),
        };
        let snapshot = view.worker_snapshot();
        let prepared = prepare_edits(snapshot.as_ref(), &[edit], &AtomicBool::new(false))?;
        let caret = ByteOffset(menu.range.start.0 + replacement.len());
        view.apply_edits(&prepared, caret..caret, ctx)?;
        let _ = self.refresh(view, ctx);
        Ok(())
    }
    /// Adds the word to the user's dictionary, for every document. Marks of it go at once.
    pub fn add_to_dictionary(
        &mut self,
        view: &mut EditorView,
        menu: &FixMenu,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.send_word(Command::Add(menu.word.clone()))?;
        self.forget_word(view, &menu.word, ctx);
        Ok(())
    }
    /// Ignores the word in this document until it closes. Marks of it go at once.
    pub fn ignore(
        &mut self,
        view: &mut EditorView,
        menu: &FixMenu,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        self.send_word(Command::Ignore(menu.word.clone()))?;
        self.forget_word(view, &menu.word, ctx);
        Ok(())
    }
    /// Looks for the next misspelling after the selection, through the whole document and
    /// around to its start. This is the Mac's Check Document Now. The answer comes in `poll`,
    /// and the host selects it. An edit stops the search, and so does a new `find_next`.
    pub fn find_next(&mut self, view: &EditorView) -> Result<(), String> {
        if let Some(off) = why_off(view, true) {
            return Err(off.to_string());
        }
        self.catch_up(view);
        // The text is the same, so a search still running gets no answer of its own.
        self.find_stopped = false;
        let generation = self.latest.find.fetch_add(1, Ordering::Relaxed) + 1;
        self.finding = true;
        self.jobs
            .send(Command::FindNext {
                snapshot: view.worker_snapshot(),
                from: view.selection().end.0,
                generation,
            })
            .map_err(|_| {
                self.finding = false;
                stopped()
            })
    }
    /// Takes the marks away. The host drops the service after this.
    pub fn stop(mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        self.clear_marks(view, ctx);
    }

    fn send_word(&self, command: Command) -> Result<(), String> {
        if self.failed {
            return Err(stopped());
        }
        self.jobs.send(command).map_err(|_| stopped())
    }
    /// Stops a search whose text has changed. The next `poll` tells the host.
    fn stop_find(&mut self) {
        if self.finding {
            self.finding = false;
            self.find_stopped = true;
            self.latest.find.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn forget_word(&mut self, view: &mut EditorView, word: &str, ctx: &mut ViewContext) {
        self.ignored.insert(word.to_owned());
        self.catch_up(view);
        self.marks.retain(|m| m.word != word);
        self.show_marks(view, ctx);
    }
    /// Brings the marks and the checked ranges up to the view's text.
    fn catch_up(&mut self, view: &EditorView) {
        if view.document_number() != self.document {
            self.forget_all(view);
            return;
        }
        match view.changes_since(self.revision) {
            ChangesSince::Current => {}
            ChangesSince::Rescan => self.forget_all(view),
            ChangesSince::Changes(list) => {
                if !list.is_empty() {
                    self.stop_find();
                }
                for change in &list {
                    let c = &change.change;
                    edit_marks(
                        &mut self.marks,
                        c.replaced.start.0,
                        c.replaced.end.0,
                        c.new_len,
                    );
                    self.checked
                        .edit(c.replaced.start.0, c.replaced.end.0, c.new_len);
                }
                self.revision = view.revision();
            }
        }
    }
    fn forget_all(&mut self, view: &EditorView) {
        self.stop_find();
        self.marks.clear();
        self.checked.clear();
        // Ignore lasts as long as the document does, so a new look at its text keeps it.
        if view.document_number() != self.document && !self.ignored.is_empty() {
            self.ignored.clear();
            if self.jobs.send(Command::ForgetIgnored).is_err() {
                self.failed = true;
            }
        }
        self.revision = view.revision();
        self.document = view.document_number();
        // A job for the old text is of no use. The worker skips what it has not begun.
        self.latest.check.fetch_add(1, Ordering::Relaxed);
    }
    fn apply_answer(
        &mut self,
        view: &EditorView,
        revision: u64,
        document: u64,
        mut checked: Vec<Range<usize>>,
        mut errors: Vec<Misspelling>,
    ) -> bool {
        if document != view.document_number() {
            self.counts.answers_dropped += 1;
            return false;
        }
        // Typing went on during the check. The answer moves along with the text.
        let mut answer_ranges = CheckedRanges::from_ranges(std::mem::take(&mut checked));
        match view.changes_since(revision) {
            ChangesSince::Current => {}
            ChangesSince::Rescan => {
                self.counts.answers_dropped += 1;
                return false;
            }
            ChangesSince::Changes(list) => {
                for change in &list {
                    let c = &change.change;
                    edit_marks(&mut errors, c.replaced.start.0, c.replaced.end.0, c.new_len);
                    answer_ranges.edit(c.replaced.start.0, c.replaced.end.0, c.new_len);
                }
            }
        }
        self.counts.answers_applied += 1;
        errors.retain(|e| !self.ignored.contains(&e.word));
        let in_answer = |m: &Misspelling| {
            answer_ranges
                .ranges()
                .iter()
                .any(|r| m.range.start.0 < r.end && r.start < m.range.end.0)
        };
        self.marks.retain(|m| !in_answer(m));
        self.marks.extend(errors);
        self.marks.sort_by_key(|m| m.range.start);
        self.marks.dedup_by(|a, b| a.range.start < b.range.end);
        for range in answer_ranges.ranges() {
            self.checked.add(range.clone());
        }
        true
    }
    fn show_marks(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        let ranges: Vec<Range<ByteOffset>> = self.marks.iter().map(|m| m.range.clone()).collect();
        if ranges != view.decorations(DecorationKind::SpellingUnderline) {
            view.set_decorations(DecorationKind::SpellingUnderline, ranges, ctx);
        }
        self.counts.marks_set = self.marks.len() as u64;
    }
    fn clear_marks(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        self.marks.clear();
        self.checked.clear();
        if !view
            .decorations(DecorationKind::SpellingUnderline)
            .is_empty()
        {
            view.set_decorations(DecorationKind::SpellingUnderline, Vec::new(), ctx);
        }
    }
    fn turn_off(&mut self, view: &mut EditorView, off: SpellOff, ctx: &mut ViewContext) {
        // A new document that came while spelling was off starts clean all the same.
        if view.document_number() != self.document {
            self.forget_all(view);
        }
        if self.off != Some(off) {
            self.off = Some(off);
            self.latest.check.fetch_add(1, Ordering::Relaxed);
            self.stop_find();
        }
        // Every time, because a mark can arrive with no change of reason.
        self.clear_marks(view, ctx);
        self.revision = view.revision();
    }
    /// Keeps the marks' list short on a very misspelled file that has been scrolled through.
    fn prune(&mut self, window: &Range<usize>, view: &mut EditorView, ctx: &mut ViewContext) {
        if self.marks.len() <= self.max_marks
            && self.checked.ranges().len() <= self.max_checked_ranges
        {
            return;
        }
        let keep = window.start.saturating_sub(self.keep_near_bytes)
            ..window.end.saturating_add(self.keep_near_bytes);
        self.marks
            .retain(|m| m.range.end.0 > keep.start && m.range.start.0 < keep.end);
        self.checked.keep_only(&keep);
        self.show_marks(view, ctx);
    }
}

fn stopped() -> String {
    "The spell checker has stopped. Turn spelling off and on to start it again.".into()
}

/// Stricter than the editor's rule for its own marks in `HOOKS.md`: a fix must never come from
/// a word that was not checked as it stands now.
fn edit_marks(marks: &mut Vec<Misspelling>, start: usize, old_end: usize, new_len: usize) {
    let shift = |x: usize| (x + new_len).saturating_sub(old_end - start);
    marks.retain_mut(|m| {
        let (a, b) = (m.range.start.0, m.range.end.0);
        // A mark that touches the edit, even at its edge, may be part of a changed word. Only a
        // new check can say, and until then it offers no fix.
        if b < start {
            return true;
        }
        if a > old_end {
            m.range = ByteOffset(shift(a))..ByteOffset(shift(b));
            return true;
        }
        false
    });
}

/// Whether a word is part of a token that the Mac leaves alone: a web address, an email
/// address, or a file or domain name with a dot inside it. Windows alone would mark some.
pub fn token_is_skipped(token: &str) -> bool {
    let token = token.trim_matches(|c: char| !c.is_alphanumeric());
    token.contains("://") || token.starts_with("www.") || token.contains('@') || token.contains('.')
}
