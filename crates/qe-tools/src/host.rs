//! `ToolsHost`: the find bar, the panels and their workers, for one window. `WIRING.md` lists
//! the calls a window makes. All of them run on the UI thread, and none reads a mapped file's
//! disk there. One does work that grows with the text: applying a finished Replace All or
//! transform, through `apply_edits` in `qe-editor`. `WIRING.md` says what would bound it.
use crate::complete::complete_candidates;
use crate::lines::{LINE_SUGGESTION, LineError, LineSpec, line_error_message, lines_for_spec};
use crate::replace::{ReplaceAllReady, Worker, prepare_replace_all};
use crate::search::{
    Direction, FindJob, FindOptions, JobState, MatchKind, Matcher, ScanEnd, Wake, find_step,
};
use crate::substitute::Substitutions;
use crate::text::{is_word, readable_len};
use crate::transform::{Transform, prepare_transform};
use crate::ui::{
    BAR_ROW_HEIGHT, BarAction, BarStop, FindBar, PanelStop, SelectLinePanel, contains, mouse_point,
};
use qe_api::{
    ByteOffset, CommandId, Dip, DisplayList, Handled, InputEvent, InputKind, LookupError, Rect,
    TextMeasurer, Theme, View, ViewContext, ViewRequest, Widget, WidgetId,
};
use qe_editor::{
    ChangesSince, DecorationKind, EditorView, HostCommand, PreparedEdits, TextEdit, prepare_edits,
};
use qe_ui::{
    AlertModel, DialogFrame, DialogResult, MenuItem, MenuModel, MenuView, SharedClipboard,
    SystemClipboard, place_popup,
};
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

/// A selection up to this size transforms at once on the UI thread. A longer one uses a worker.
pub const TRANSFORM_INLINE_BYTES: usize = 64 * 1024;
/// The most marks set at one time. Only text near the view has marks.
pub const MARK_LIMIT: usize = 8192;
/// Text on each side of the view that gets marks, so that a small scroll needs no new scan.
const MARK_MARGIN: usize = 16 * 1024;
/// Steps that wait while one runs, as when F3 repeats.
const STEP_QUEUE: usize = 64;
/// The longest selection that Use Selection for Find takes.
const SELECTION_FOR_FIND: usize = 64 * 1024;
/// The bytes before the caret that a typing check needs.
pub const TYPING_CONTEXT_BYTES: usize = 8;
const OPTION_IGNORE_CASE: &str = "ignore-case";
const OPTION_WRAP: &str = "wrap";
const OPTION_CONTAINS: &str = "contains";
const OPTION_STARTS: &str = "starts";
const OPTION_FULL: &str = "full";
const OPTION_ENDS: &str = "ends";

/// Where the keyboard is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolsFocus {
    /// The text. The tools take no keys.
    Editor,
    Bar(BarStop),
    SelectLine,
    /// A message on top of the panel.
    Alert,
    /// The list of the options button or of Complete.
    List,
}

struct StepResult {
    found: Option<Range<ByteOffset>>,
    revision: u64,
    direction: Direction,
    complete: bool,
    known: usize,
    from: Range<ByteOffset>,
}
struct CompleteResult {
    words: Vec<String>,
    replace: Range<ByteOffset>,
    revision: u64,
}
struct OpenList {
    menu: MenuView,
    complete: Option<(Range<ByteOffset>, u64)>,
}
/// What the user asked Replace All to do, kept with its job.
#[derive(Clone)]
struct ReplaceRequest {
    matcher: Matcher,
    replacement: String,
}
struct ReplaceRun {
    worker: Worker<Option<ReplaceAllReady>>,
    request: ReplaceRequest,
}
struct VisibleScan {
    wanted: Range<usize>,
    job: FindJob,
    covers: Range<usize>,
    revision: u64,
}
struct TransformRun {
    worker: Worker<Option<PreparedEdits>>,
    start: ByteOffset,
    keep_selected: bool,
}

/// The find bar, the Select Line panel, Complete's list and the workers behind them.
pub struct ToolsHost {
    bar: FindBar,
    panel: SelectLinePanel,
    alert: Option<DialogFrame>,
    list: Option<OpenList>,
    wake: Option<Wake>,
    query: String,
    options: FindOptions,
    substitutions: Substitutions,
    area: Rect,
    count_job: Option<FindJob>,
    count: u64,
    count_done: bool,
    status: Option<(String, bool)>,
    visible: Option<VisibleScan>,
    covered: Option<Range<usize>>,
    step: Option<Worker<StepResult>>,
    step_queue: VecDeque<Direction>,
    step_waiting: Option<(Direction, Range<ByteOffset>, usize)>,
    current: Option<Range<ByteOffset>>,
    anchor: ByteOffset,
    typed_search: bool,
    seen_revision: u64,
    replace_job: Option<ReplaceRun>,
    replace_ready: Option<(ReplaceAllReady, ReplaceRequest)>,
    scanned_for: Option<Range<usize>>,
    layout_changed: bool,
    view_has_keys: bool,
    transform_job: Option<TransformRun>,
    complete_job: Option<Worker<CompleteResult>>,
    complete_again: Option<u64>,
    pending_lines: Option<(String, bool)>,
    pending_center: bool,
    beep: bool,
    last_replaced: Option<u64>,
    workers_started: u64,
    marks_set: u64,
}

impl Default for ToolsHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolsHost {
    pub fn new() -> Self {
        Self::with_clipboard(Arc::new(SystemClipboard))
    }
    /// The clipboard that the bar's fields use. A test passes a `MemoryClipboard`.
    pub fn with_clipboard(clipboard: SharedClipboard) -> Self {
        Self {
            bar: FindBar::new(clipboard.clone()),
            panel: SelectLinePanel::new(clipboard),
            alert: None,
            list: None,
            wake: None,
            query: String::new(),
            options: FindOptions::default(),
            substitutions: Substitutions::default(),
            area: Rect::default(),
            count_job: None,
            count: 0,
            count_done: false,
            status: None,
            visible: None,
            covered: None,
            step: None,
            step_queue: VecDeque::new(),
            step_waiting: None,
            current: None,
            anchor: ByteOffset(0),
            typed_search: false,
            seen_revision: 0,
            replace_job: None,
            replace_ready: None,
            scanned_for: None,
            layout_changed: false,
            view_has_keys: true,
            transform_job: None,
            complete_job: None,
            complete_again: None,
            pending_lines: None,
            pending_center: false,
            beep: false,
            last_replaced: None,
            workers_started: 0,
            marks_set: 0,
        }
    }
    /// Runs on a worker's thread when its results wait. It must only post a message, and the
    /// message's handler calls `poll`.
    pub fn set_wake(&mut self, wake: Wake) {
        self.wake = Some(wake);
    }
    pub fn set_substitutions(&mut self, substitutions: Substitutions) {
        self.substitutions = substitutions;
    }
    /// The toggles as the Substitutions menu has them.
    pub fn substitutions(&self) -> Substitutions {
        self.substitutions
    }
    pub fn find_options(&self) -> FindOptions {
        self.options
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn bar(&self) -> &FindBar {
        &self.bar
    }
    pub fn panel(&self) -> &SelectLinePanel {
        &self.panel
    }
    pub fn is_bar_open(&self) -> bool {
        self.bar.open
    }
    pub fn is_select_line_open(&self) -> bool {
        self.panel.open
    }
    /// The words of Complete's list while it is open.
    pub fn complete_words(&self) -> Option<Vec<String>> {
        let list = self.list.as_ref()?;
        list.complete.as_ref()?;
        Some(
            list.menu
                .model
                .items
                .iter()
                .map(|i| i.label.clone())
                .collect(),
        )
    }
    /// The message on top of the Select Line panel.
    pub fn alert_text(&self) -> Option<&str> {
        self.alert.as_ref().map(|a| a.title.as_str())
    }
    /// The match the bar stands on.
    pub fn current_match(&self) -> Option<Range<ByteOffset>> {
        self.current.clone()
    }
    /// All matches counted so far, and whether the count is final.
    pub fn match_count(&self) -> (u64, bool) {
        (self.count, self.count_done)
    }
    /// How many a finished Replace All replaced.
    pub fn last_replaced(&self) -> Option<u64> {
        self.last_replaced
    }
    /// The bar's status or a problem, in words.
    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.0.as_str())
    }
    /// True once when the host should beep, as the Mac does at a bad entry or no match.
    pub fn take_beep(&mut self) -> bool {
        std::mem::take(&mut self.beep)
    }
    /// How many worker threads have started. A closed bar adds none.
    pub fn workers_started(&self) -> u64 {
        self.workers_started
    }
    /// How many times marks were set. A closed bar adds none.
    pub fn marks_set(&self) -> u64 {
        self.marks_set
    }
    /// True when `poll` has nothing to look at, so it returns at once.
    pub fn is_idle(&self) -> bool {
        !self.bar.open
            && self.step.is_none()
            && self.step_queue.is_empty()
            && self.step_waiting.is_none()
            && self.replace_job.is_none()
            && self.replace_ready.is_none()
            && self.transform_job.is_none()
            && self.complete_job.is_none()
            && self.complete_again.is_none()
            && self.pending_lines.is_none()
            && !self.pending_center
    }
    /// True when no worker runs and no result waits, with the bar open or not.
    pub fn is_quiet(&self) -> bool {
        self.count_job.is_none()
            && self.visible.is_none()
            && self.step.is_none()
            && self.step_queue.is_empty()
            && self.step_waiting.is_none()
            && self.replace_job.is_none()
            && self.replace_ready.is_none()
            && self.transform_job.is_none()
            && self.complete_job.is_none()
            && self.complete_again.is_none()
            && self.pending_lines.is_none()
            && !self.pending_center
    }
    #[doc(hidden)]
    pub fn set_step_waiting_for_test(&mut self, direction: Direction, from: Range<ByteOffset>) {
        self.step_waiting = Some((direction, from, usize::MAX));
    }
    #[doc(hidden)]
    pub fn is_step_waiting_for_test(&self) -> bool {
        self.step_waiting.is_some()
    }
    #[doc(hidden)]
    pub fn press_replace_all_for_test(&mut self, view: &mut EditorView) {
        self.press_replace_all(view);
    }
    /// True while a Replace All runs or waits behind keys.
    #[doc(hidden)]
    pub fn replace_is_ready_for_test(&self) -> bool {
        self.replace_job.is_some() || self.replace_ready.is_some()
    }
    /// True when a finished Replace All waits for keys, for tests.
    #[doc(hidden)]
    pub fn replace_waits_for_test(&self) -> bool {
        self.replace_ready.is_some()
    }
    /// Sets the options with no list open, for tests.
    #[doc(hidden)]
    pub fn set_find_options_for_test(&mut self, options: FindOptions) {
        self.options = options;
    }
    /// Sets the query with no typing, for tests.
    #[doc(hidden)]
    pub fn set_query_for_test(
        &mut self,
        query: &str,
        view: &mut EditorView,
        _measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        self.set_query(query.to_string());
        self.query_changed(view, ctx);
        self.anchor = view.selection().start;
        self.start_search(view, true, ctx);
    }
    /// Where the keyboard is. Keys go to the editor only for `ToolsFocus::Editor`.
    pub fn focus(&self) -> ToolsFocus {
        if self.list.is_some() {
            ToolsFocus::List
        } else if self.alert.is_some() {
            ToolsFocus::Alert
        } else if self.panel.keyboard.is_some() {
            ToolsFocus::SelectLine
        } else if let Some(stop) = self.bar.focus {
            ToolsFocus::Bar(stop)
        } else {
            ToolsFocus::Editor
        }
    }
    /// True for the commands `run_command` takes.
    pub fn takes_command(command: CommandId) -> bool {
        use CommandId as C;
        matches!(
            command,
            C::Find
                | C::FindReplace
                | C::FindNext
                | C::FindPrevious
                | C::UseSelectionForFind
                | C::JumpToSelection
                | C::SelectLine
                | C::Complete
                | C::UpperCase
                | C::LowerCase
                | C::Capitalize
                | C::SmartCopyPaste
                | C::SmartQuotes
                | C::SmartDashes
        )
    }
    /// `(enabled, checked)` for a menu row, or `None` for a command that is not this crate's.
    pub fn command_state(&self, command: CommandId, view: &EditorView) -> Option<(bool, bool)> {
        use CommandId as C;
        let selected = {
            let selection = view.selection();
            selection.start < selection.end
        };
        let large = view.store_status().known_bytes.0 as u64 > crate::SUBSTITUTION_LIMIT_BYTES;
        Some(match command {
            C::Find | C::FindNext | C::FindPrevious | C::SelectLine | C::JumpToSelection => {
                (true, false)
            }
            C::FindReplace => (view.can_edit(), false),
            C::UseSelectionForFind => (selected, false),
            C::Complete | C::UpperCase | C::LowerCase | C::Capitalize => (view.can_edit(), false),
            C::SmartCopyPaste => (true, self.substitutions.smart_copy),
            C::SmartQuotes => (!large, self.substitutions.quotes),
            C::SmartDashes => (!large, self.substitutions.dashes),
            _ => return None,
        })
    }

    // ---- Commands ----

    /// Runs a menu or key command. `Handled::No` means that it is not this crate's.
    pub fn run_command(
        &mut self,
        command: CommandId,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Handled {
        use CommandId as C;
        match command {
            C::Find => self.open_bar(false, view, measure, ctx),
            C::FindReplace => self.open_bar(true, view, measure, ctx),
            C::FindNext => self.request_step(Direction::Forward, view),
            C::FindPrevious => self.request_step(Direction::Backward, view),
            C::UseSelectionForFind => self.use_selection(view, ctx),
            C::JumpToSelection => self.jump_to_selection(view, ctx),
            C::SelectLine => {
                self.panel.show(ctx);
                self.bar.set_focus(None, ctx);
                self.panel.place(self.area, measure);
                ctx.invalidate(self.panel.bounds);
            }
            // Complete is for the text, so it does nothing while a field has the keyboard.
            C::Complete if self.focus() != ToolsFocus::Editor => {}
            C::Complete => self.start_complete(view),
            C::UpperCase => self.transform(Transform::Upper, view, ctx),
            C::LowerCase => self.transform(Transform::Lower, view, ctx),
            C::Capitalize => self.transform(Transform::Capitalize, view, ctx),
            C::SmartCopyPaste => self.substitutions.smart_copy ^= true,
            C::SmartQuotes => self.substitutions.quotes ^= true,
            C::SmartDashes => self.substitutions.dashes ^= true,
            _ => return Handled::No,
        }
        self.sync_view_focus(view, ctx);
        Handled::Yes
    }

    fn snapshot_wake(&self) -> Option<Wake> {
        self.wake.clone()
    }
    fn fail(&mut self, message: impl Into<String>, view: &mut EditorView) {
        let message = message.into();
        self.beep = true;
        if self.bar.open {
            self.bar.set_count(message.clone(), true);
            self.status = Some((message, true));
        } else {
            view.show_warning(message.clone());
            self.status = Some((message, true));
        }
    }
    fn open_bar(
        &mut self,
        replacing: bool,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        let opening = !self.bar.open;
        let editable = view.can_edit();
        self.layout_changed |= opening || (replacing && !self.bar.replacing);
        self.panel.set_keyboard(None, ctx);
        self.bar.open = true;
        self.bar.replacing = replacing || self.bar.replacing;
        self.bar.replace.enabled = editable;
        self.bar.replace_one.enabled = editable;
        self.bar.replace_all.enabled = editable;
        if opening || self.bar.focus.is_none() {
            self.anchor = view.selection().start;
        }
        let stop = if replacing && editable {
            BarStop::Replace
        } else {
            BarStop::Find
        };
        self.bar.set_focus(Some(stop), ctx);
        let field = if stop == BarStop::Find {
            &mut self.bar.find
        } else {
            &mut self.bar.replace
        };
        field.anchor = 0;
        field.caret = field.text.len();
        self.place_all(measure);
        ctx.invalidate(self.area);
        if opening {
            self.seen_revision = view.revision();
            if !self.query.is_empty() {
                self.start_search(view, false, ctx);
            }
        }
    }
    fn close_bar(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        self.bar.set_focus(None, ctx);
        self.bar.open = false;
        self.cancel_search();
        self.replace_job = None;
        self.replace_ready = None;
        self.status = None;
        self.clear_marks(view, ctx);
        // A step has already selected its match, so the selection stays as it is. Selecting here
        // would pull it back from wherever the user has gone since.
        self.current = None;
        self.layout_changed = true;
        ctx.invalidate(self.area);
    }
    fn cancel_search(&mut self) {
        if let Some(job) = self.count_job.take() {
            job.cancel();
        }
        if let Some(scan) = self.visible.take() {
            scan.job.cancel();
        }
        if let Some(step) = self.step.take() {
            step.cancel();
        }
        self.step_queue.clear();
        self.step_waiting = None;
        self.covered = None;
        self.scanned_for = None;
    }
    fn clear_marks(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        if !view.decorations(DecorationKind::FindMatch).is_empty() {
            view.set_decorations(DecorationKind::FindMatch, Vec::new(), ctx);
        }
        if !view
            .decorations(DecorationKind::CurrentFindMatch)
            .is_empty()
        {
            view.set_decorations(DecorationKind::CurrentFindMatch, Vec::new(), ctx);
        }
    }
    fn matcher(&self) -> Option<Matcher> {
        Matcher::new(&self.query, self.options)
    }
    /// Starts over for the query as it stands. `incremental` also moves to the first match
    /// from where the search began, as the Mac's bar does while typing.
    fn start_search(&mut self, view: &mut EditorView, incremental: bool, ctx: &mut ViewContext) {
        self.cancel_search();
        self.count = 0;
        self.count_done = false;
        self.status = None;
        self.last_replaced = None;
        self.seen_revision = view.revision();
        let Some(matcher) = self.matcher() else {
            self.current = None;
            self.clear_marks(view, ctx);
            self.bar.set_count("", false);
            self.count_done = true;
            return;
        };
        let snapshot = view.worker_snapshot();
        let len = snapshot.len_bytes();
        self.workers_started += 1;
        self.count_job = Some(FindJob::start(
            snapshot,
            matcher,
            ByteOffset(0)..len,
            0,
            self.snapshot_wake(),
        ));
        self.update_count_label();
        self.refresh_marks(view);
        if incremental {
            self.typed_search = true;
            let from = self.anchor..self.anchor;
            self.start_step(Direction::Forward, from, view);
        }
    }
    fn update_count_label(&mut self) {
        if self.status.is_some() {
            return;
        }
        let text = if self.query.is_empty() {
            String::new()
        } else if !self.count_done {
            format!("{} found\u{2026}", grouped(self.count))
        } else if self.count == 0 {
            "Not found".to_string()
        } else {
            format!("{} found", grouped(self.count))
        };
        self.bar.set_count(text, false);
    }
    fn request_step(&mut self, direction: Direction, view: &mut EditorView) {
        if self.query.is_empty() {
            self.beep = true;
            return;
        }
        if self.step.is_some() {
            if self.step_queue.len() < STEP_QUEUE {
                self.step_queue.push_back(direction);
            }
            return;
        }
        self.typed_search = false;
        let from = view.selection();
        self.start_step(direction, from, view);
    }
    fn start_step(&mut self, direction: Direction, from: Range<ByteOffset>, view: &mut EditorView) {
        // This step is newer than any that waits for the index.
        self.step_waiting = None;
        let Some(matcher) = self.matcher() else {
            return;
        };
        let snapshot = view.worker_snapshot();
        let revision = view.revision();
        let wrap = self.options.wrap;
        self.workers_started += 1;
        self.step = Some(Worker::start(
            "qe-tools-step",
            self.snapshot_wake(),
            move |cancel| {
                let asked = from.clone();
                let found = find_step(snapshot.as_ref(), &matcher, from, direction, wrap, cancel);
                let status = snapshot.status();
                match found {
                    Ok(result) => Ok(StepResult {
                        found: result.map(|(found, _)| found),
                        complete: status.complete,
                        known: readable_len(snapshot.as_ref()),
                        from: asked,
                        revision,
                        direction,
                    }),
                    Err(ScanEnd::Failed(message)) => Err(message),
                    Err(_) => Err("The search was cancelled.".to_string()),
                }
            },
        ));
    }
    fn on_step(
        &mut self,
        result: Result<StepResult, String>,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) {
        let result = match result {
            Ok(result) => result,
            Err(message) => {
                self.fail(message, view);
                return;
            }
        };
        // Offsets from older text mean nothing now, so look again on the text as it stands.
        if result.revision != view.revision() {
            let from = if self.typed_search {
                self.anchor..self.anchor
            } else {
                view.selection()
            };
            self.start_step(result.direction, from, view);
            return;
        }
        // A mapped file still indexing: no match yet is not "not found". Look again as it grows.
        if result.found.is_none() && !result.complete {
            self.step_waiting = Some((result.direction, result.from, result.known));
            return;
        }
        let Some(found) = result.found else {
            self.current = None;
            if self.bar.open {
                self.set_current_mark(view, ctx);
            }
            if !self.typed_search {
                self.beep = true;
            }
            if !self.bar.open {
                view.show_warning("Not found");
            }
            return;
        };
        self.current = self.bar.open.then(|| found.clone());
        view.run_host(
            HostCommand::Select {
                range: found.clone(),
                reveal: true,
            },
            ctx,
        );
        if !self.typed_search {
            self.anchor = found.start;
        }
        if self.bar.open {
            self.set_current_mark(view, ctx);
            self.covered = None;
            self.scanned_for = None;
            self.refresh_marks(view);
        }
    }
    fn set_current_mark(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        self.marks_set += 1;
        view.set_decorations(
            DecorationKind::CurrentFindMatch,
            self.current.clone().into_iter().collect(),
            ctx,
        );
    }
    /// Scans from the view for marks, if the view has left what was scanned. A scan that
    /// stopped at its limit, or that reached all the text it can read, is not started again
    /// for the same view.
    fn refresh_marks(&mut self, view: &mut EditorView) {
        let Ok(visible) = view.visible_range() else {
            return;
        };
        let wanted = visible.start.0..visible.end.0;
        let covers = |range: &Range<usize>| range.start <= wanted.start && wanted.end <= range.end;
        if self.covered.as_ref().is_some_and(covers) || self.scanned_for.as_ref() == Some(&wanted) {
            return;
        }
        let revision = view.revision();
        if self
            .visible
            .as_ref()
            .is_some_and(|scan| scan.revision == revision && scan.wanted == wanted)
        {
            return;
        }
        let Some(matcher) = self.matcher() else {
            return;
        };
        let margin = MARK_MARGIN.max(matcher.max_match_bytes() * 2);
        let snapshot = view.worker_snapshot();
        // It starts at the view, less room for a match that begins just above it, so that a
        // dense query spends its marks on what shows.
        let scan = wanted.start.saturating_sub(matcher.max_match_bytes())
            ..wanted
                .end
                .saturating_add(margin)
                .min(readable_len(snapshot.as_ref()));
        self.workers_started += 1;
        let job = FindJob::start(
            snapshot,
            matcher,
            ByteOffset(scan.start)..ByteOffset(scan.end),
            MARK_LIMIT,
            self.snapshot_wake(),
        );
        self.visible = Some(VisibleScan {
            job,
            covers: scan,
            wanted,
            revision,
        });
    }
    fn on_visible_done(&mut self, scan: VisibleScan, view: &mut EditorView, ctx: &mut ViewContext) {
        if scan.revision != view.revision() {
            return;
        }
        let matches = scan.job.drain();
        let mut covers = scan.covers;
        // With more matches than marks, only the part with marks counts as scanned.
        if scan.job.dropped() > 0
            && let Some(last) = matches.last()
        {
            covers.end = last.end.0;
        }
        let current = self.current.clone();
        let marks: Vec<_> = matches
            .into_iter()
            .filter(|m| current.as_ref() != Some(m))
            .collect();
        self.marks_set += 1;
        view.set_decorations(DecorationKind::FindMatch, marks, ctx);
        self.covered = Some(covers);
        self.scanned_for = Some(scan.wanted);
    }

    // ---- Replace ----

    fn replace_one(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        if !view.can_edit() {
            self.beep = true;
            return;
        }
        let selection = view.selection();
        let eligible = selection.start < selection.end
            && self.current.as_ref() == Some(&selection)
            && self.current_still_matches(selection.clone(), view);
        if !eligible {
            self.typed_search = false;
            self.request_step(Direction::Forward, view);
            return;
        }
        let text = self.bar.replace.text.clone();
        let caret = ByteOffset(selection.start.0 + text.len());
        let snapshot = view.worker_snapshot();
        let never = std::sync::atomic::AtomicBool::new(false);
        match prepare_edits(
            snapshot.as_ref(),
            &[TextEdit {
                range: selection,
                text,
            }],
            &never,
        ) {
            Ok(prepared) => {
                view.run_host(
                    HostCommand::ApplyEdits {
                        edits: Arc::new(prepared),
                        selection: caret..caret,
                    },
                    ctx,
                );
                self.current = None;
                self.typed_search = false;
                self.request_step(Direction::Forward, view);
            }
            Err(message) => self.fail(message, view),
        }
    }
    /// The All button: the request is what the bar says now.
    fn press_replace_all(&mut self, view: &mut EditorView) {
        let Some(matcher) = self.matcher() else {
            self.beep = true;
            return;
        };
        self.start_replace_all(
            ReplaceRequest {
                matcher,
                replacement: self.bar.replace.text.clone(),
            },
            view,
        );
    }
    /// Starts or restarts a request. It keeps its own query and replacement, so a run again
    /// after an edit never reads the fields, which the user may have changed.
    fn start_replace_all(&mut self, request: ReplaceRequest, view: &mut EditorView) {
        if !view.can_edit() {
            self.beep = true;
            return;
        }
        self.replace_ready = None;
        let snapshot = view.worker_snapshot();
        self.workers_started += 1;
        let (matcher, replacement) = (request.matcher.clone(), request.replacement.clone());
        self.replace_job = Some(ReplaceRun {
            worker: Worker::start("qe-tools-replace", self.snapshot_wake(), move |cancel| {
                prepare_replace_all(snapshot.as_ref(), &matcher, &replacement, cancel)
            }),
            request,
        });
    }
    /// A new query or option ends what was asked for the old one.
    fn query_changed(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        if let Some(run) = self.replace_job.take() {
            run.worker.cancel();
        }
        self.replace_ready = None;
        if self.current.take().is_some() {
            self.set_current_mark(view, ctx);
        }
    }
    /// Applies a finished Replace All. Text that moved since refuses it, and it runs again.
    fn finish_replace_all(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        let Some((ready, request)) = self.replace_ready.take() else {
            return;
        };
        // Keys still waiting would run first and move the text, so wait for them.
        if view.queued_input_count() > 0 {
            self.replace_ready = Some((ready, request));
            return;
        }
        if ready.prepared.base_revision() != view.revision() {
            self.start_replace_all(request, view);
            return;
        }
        let caret = ready.caret_after;
        match view.apply_edits(&ready.prepared, caret..caret, ctx) {
            Ok(()) => {
                self.current = None;
                let message = format!("{} replaced", grouped(ready.count));
                self.seen_revision = view.revision();
                self.start_search(view, false, ctx);
                self.last_replaced = Some(ready.count);
                self.bar.set_count(message.clone(), false);
                self.status = Some((message, false));
            }
            Err(message) => self.fail(message, view),
        }
    }

    // ---- Selection for find, Jump ----

    fn use_selection(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        let selection = view.selection();
        if selection.start >= selection.end {
            self.beep = true;
            return;
        }
        if selection.end.0 - selection.start.0 > SELECTION_FOR_FIND {
            self.fail("The selection is too long to find.", view);
            return;
        }
        let snapshot = view.worker_snapshot();
        let mut text = String::new();
        match snapshot.try_visit_chunks(selection.clone(), &mut |part| text.push_str(part)) {
            Ok(()) => {
                self.set_query(text);
                self.query_changed(view, ctx);
                self.anchor = selection.start;
                self.typed_search = false;
                if self.bar.open {
                    self.current = Some(selection);
                    self.start_search(view, false, ctx);
                    self.set_current_mark(view, ctx);
                }
            }
            Err(error) => {
                let message = lookup_message(&error);
                self.fail(message, view);
            }
        }
    }
    /// Sets the search text and shows it, on one line, in the field.
    fn set_query(&mut self, text: String) {
        self.bar.find.text = text
            .replace("\r\n", " ")
            .replace(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}', '\t'], " ");
        self.bar.find.caret = self.bar.find.text.len();
        self.bar.find.anchor = 0;
        self.query = text;
    }
    fn jump_to_selection(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        let selection = view.selection();
        view.run_host(
            HostCommand::Select {
                range: selection,
                reveal: true,
            },
            ctx,
        );
        self.pending_center = true;
        self.center_selection(view, ctx);
    }
    /// Puts the selection in the middle of the view, as `centerSelectionInVisibleArea:` does.
    fn center_selection(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        if view.queued_input_count() > 0 {
            return;
        }
        let start = view.selection().start;
        let Ok(rects) = view.range_rects(start..start) else {
            return;
        };
        let Some(first) = rects.first() else {
            self.pending_center = false;
            return;
        };
        let area = view.text_area();
        let middle = f64::from(area.y.0 + area.height.0 / 2.0);
        let y = f64::from(first.rect.y.0 + first.rect.height.0 / 2.0);
        let target = (view.scroll_offset() + y - middle).clamp(0.0, view.max_scroll_position());
        view.run_host(HostCommand::ScrollTo(target), ctx);
        self.pending_center = false;
    }

    // ---- Select Line ----

    /// Selects the panel's entry. On success, `close` closes the panel, as Return does.
    fn submit_lines(
        &mut self,
        input: String,
        close: bool,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) {
        if input.is_empty() {
            return;
        }
        let Some(spec) = LineSpec::parse(&input) else {
            self.pending_lines = None;
            self.show_line_error(&input, ctx);
            return;
        };
        match lines_for_spec(view, spec) {
            Ok(range) => {
                self.pending_lines = None;
                view.run_host(
                    HostCommand::Select {
                        range,
                        reveal: true,
                    },
                    ctx,
                );
                if close {
                    self.panel.hide(ctx);
                    ctx.invalidate(self.area);
                }
            }
            Err(LineError::Invalid) => {
                self.pending_lines = None;
                self.show_line_error(&input, ctx);
            }
            Err(LineError::NotYetKnown { indexed_up_to }) => {
                // The honest answer: the panel stays, and the wake that says the index grew
                // asks again.
                let known = view.store_status().known_lines;
                view.show_warning(format!(
                    "The file is still being read, so line numbers past {} are not known yet (read up to byte {}). This line is selected once it is known.",
                    grouped(known as u64),
                    grouped(indexed_up_to.0 as u64)
                ));
                self.pending_lines = Some((input, close));
            }
            Err(LineError::Failed(message)) => {
                self.pending_lines = None;
                self.fail(format!("The file could not be read: {message}"), view);
            }
        }
    }
    fn show_line_error(&mut self, input: &str, ctx: &mut ViewContext) {
        self.beep = true;
        let title = line_error_message(input);
        let model = AlertModel::new(
            title.clone(),
            format!("{title}\n{LINE_SUGGESTION}"),
            vec!["OK".to_string()],
        );
        let width = 340.0_f32.min(self.area.width.0.max(200.0));
        let bounds = Rect {
            x: Dip(self.area.x.0 + ((self.area.width.0 - width) / 2.0).max(0.0)),
            y: Dip(self.area.y.0 + 24.0),
            width: Dip(width),
            height: Dip(116.0),
        };
        self.alert = Some(DialogFrame::from_alert(WidgetId(12010), &model, bounds));
        ctx.invalidate(bounds);
    }

    // ---- Transformations ----

    fn transform(&mut self, transform: Transform, view: &mut EditorView, ctx: &mut ViewContext) {
        if !view.can_edit() {
            self.beep = true;
            return;
        }
        let selection = view.selection();
        let (range, keep_selected) = if selection.start < selection.end {
            (selection, true)
        } else {
            // AppKit acts on the word at the caret and moves past it.
            match view.word_range_at(selection.start) {
                Ok(word) if word.start < word.end => (word, false),
                Ok(_) => {
                    self.beep = true;
                    return;
                }
                Err(error) => {
                    self.fail(lookup_message(&error), view);
                    return;
                }
            }
        };
        let snapshot = view.worker_snapshot();
        let start = range.start;
        if range.end.0 - range.start.0 <= TRANSFORM_INLINE_BYTES {
            let never = std::sync::atomic::AtomicBool::new(false);
            match prepare_transform(snapshot.as_ref(), range, transform, &never) {
                Ok(Some(prepared)) => {
                    self.apply_transform(prepared, start, keep_selected, view, ctx);
                }
                Ok(None) => {}
                Err(message) => self.fail(message, view),
            }
            return;
        }
        self.workers_started += 1;
        if let Some(old) = self.transform_job.take() {
            old.worker.cancel();
        }
        let worker = Worker::start("qe-tools-transform", self.snapshot_wake(), move |cancel| {
            prepare_transform(snapshot.as_ref(), range, transform, cancel)
        });
        self.transform_job = Some(TransformRun {
            worker,
            start,
            keep_selected,
        });
    }
    fn apply_transform(
        &mut self,
        prepared: PreparedEdits,
        start: ByteOffset,
        keep_selected: bool,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) {
        let end = ByteOffset(start.0 + prepared.new_len());
        let selection = if keep_selected { start..end } else { end..end };
        // In order with keys that wait. If one of them moves the text, the editor refuses
        // this and shows why.
        view.run_host(
            HostCommand::ApplyEdits {
                edits: Arc::new(prepared),
                selection,
            },
            ctx,
        );
    }

    // ---- Complete ----

    fn start_complete(&mut self, view: &mut EditorView) {
        if !view.can_edit() {
            self.beep = true;
            return;
        }
        let selection = view.selection();
        if selection.start < selection.end {
            self.beep = true;
            return;
        }
        let at = selection.start;
        // Past the last word, the view's word query can give the run after the caret, so
        // the word that counts is the one that ends at the caret.
        let word = match view.word_range_at(at) {
            Ok(word) => word,
            Err(error) => {
                self.fail(lookup_message(&error), view);
                return;
            }
        };
        let snapshot = view.worker_snapshot();
        let mut start = word.start;
        if start >= at && at.0 > 0 {
            // The caret stands at the start of this run. Ask about the character before it.
            let mut back = at.0 - 1;
            let mut found = None;
            for _ in 0..4 {
                match view.word_range_at(ByteOffset(back)) {
                    Ok(range) => {
                        found = Some(range);
                        break;
                    }
                    Err(LookupError::OutOfRange) if back > 0 => back -= 1,
                    Err(error) => {
                        self.fail(lookup_message(&error), view);
                        return;
                    }
                }
            }
            match found {
                Some(range) if range.end >= at => start = range.start,
                _ => {
                    self.beep = true;
                    return;
                }
            }
        }
        if start >= at {
            self.beep = true;
            return;
        }
        let mut prefix = String::new();
        if let Err(error) = snapshot.try_visit_chunks(start..at, &mut |part| prefix.push_str(part))
        {
            self.fail(lookup_message(&error), view);
            return;
        }
        // Only the trailing word characters count.
        let keep = prefix
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map_or(prefix.len(), |(i, _)| i);
        let prefix = prefix.get(keep..).unwrap_or("").to_string();
        if prefix.is_empty() {
            self.beep = true;
            return;
        }
        let replace = ByteOffset(at.0 - prefix.len())..at;
        let revision = view.revision();
        self.workers_started += 1;
        self.complete_job = Some(Worker::start(
            "qe-tools-complete",
            self.snapshot_wake(),
            move |cancel| {
                let words = complete_candidates(snapshot.as_ref(), &prefix, at, cancel)?;
                Ok(CompleteResult {
                    words,
                    replace,
                    revision,
                })
            },
        ));
    }
    fn open_complete_list(
        &mut self,
        result: CompleteResult,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        if result.revision != view.revision() {
            return;
        }
        if result.words.is_empty() {
            self.beep = true;
            return;
        }
        let items = result
            .words
            .iter()
            .map(|word| {
                let mut item = MenuItem::command(word.clone(), CommandId::Complete);
                item.data = Some(word.clone());
                item
            })
            .collect();
        let model = MenuModel { items };
        let mut menu = MenuView::new(WidgetId(12020), "Completions", model);
        menu.open_keyboard();
        let anchor = view.caret_rect().unwrap_or(self.area);
        let size = menu.model.preferred_size(measure);
        let height = size.1.min(240.0);
        let bounds = place_popup(anchor, (size.0, height), self.area, false);
        menu.work_area = Some(self.area);
        menu.layout(bounds, measure);
        ctx.invalidate(bounds);
        self.list = Some(OpenList {
            menu,
            complete: Some((result.replace, result.revision)),
        });
    }
    fn open_options_list(&mut self, measure: &dyn TextMeasurer, ctx: &mut ViewContext) {
        let row = |label: &str, data: &str, checked: bool| {
            let mut item = MenuItem::command(label, CommandId::Find);
            item.data = Some(data.to_string());
            item.checked = checked;
            item
        };
        let o = self.options;
        let model = MenuModel {
            items: vec![
                row("Ignore Case", OPTION_IGNORE_CASE, o.ignore_case),
                row("Wrap Around", OPTION_WRAP, o.wrap),
                MenuItem::separator(),
                row("Contains", OPTION_CONTAINS, o.kind == MatchKind::Contains),
                row(
                    "Starts With",
                    OPTION_STARTS,
                    o.kind == MatchKind::StartsWith,
                ),
                row("Full Word", OPTION_FULL, o.kind == MatchKind::FullWord),
                row("Ends With", OPTION_ENDS, o.kind == MatchKind::EndsWith),
            ],
        };
        let mut menu = MenuView::new(WidgetId(12030), "Find options", model);
        menu.open_keyboard();
        let size = menu.model.preferred_size(measure);
        let bounds = place_popup(self.bar.options_bounds(), size, self.area, false);
        menu.work_area = Some(self.area);
        menu.layout(bounds, measure);
        ctx.invalidate(bounds);
        self.list = Some(OpenList {
            menu,
            complete: None,
        });
    }
    /// Sends an event to the open list. A choice comes back as its row's data.
    fn handle_list(
        &mut self,
        event: &InputEvent,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) -> Handled {
        let Some(list) = self.list.as_mut() else {
            return Handled::No;
        };
        // Tab takes the row, as on the Mac.
        let tab = matches!(
            event.kind,
            InputKind::KeyDown {
                virtual_key: 0x09,
                ..
            }
        );
        let typed_word = match &event.kind {
            InputKind::Char(c) if list.complete.is_some() => Some(is_word(*c)),
            _ => None,
        };
        if let Some(word) = typed_word {
            // A word character narrows the list after the editor takes it. Anything else
            // closes the list and goes to the text.
            let revision = view.revision();
            self.list = None;
            ctx.invalidate(self.area);
            if word {
                self.complete_again = Some(revision);
            }
            return Handled::No;
        }
        let mut inner = ViewContext::default();
        let enter = InputEvent {
            time_us: event.time_us,
            kind: InputKind::KeyDown {
                virtual_key: 0x0d,
                modifiers: Default::default(),
                repeat: false,
            },
        };
        let handled = list
            .menu
            .handle(if tab { &enter } else { event }, &mut inner);
        let closed = list.menu.closed;
        // Rows carry a command only to end the list, so it does not go to the host.
        for request in inner.requests {
            if !matches!(request, ViewRequest::Run(_)) {
                ctx.requests.push(request);
            }
        }
        if closed {
            let chosen = list.menu.take_chosen_data();
            let complete = list.complete.take();
            self.list = None;
            ctx.invalidate(self.area);
            if let Some(data) = chosen {
                match complete {
                    Some((replace, revision)) => {
                        self.accept_completion(&data, replace, revision, view, ctx)
                    }
                    None => self.choose_option(&data, view, ctx),
                }
            }
            return Handled::Yes;
        }
        // While the list is open, plain keys stop here.
        match event.kind {
            InputKind::KeyDown { .. } | InputKind::KeyUp { .. } => Handled::Yes,
            _ => handled,
        }
    }
    fn accept_completion(
        &mut self,
        word: &str,
        replace: Range<ByteOffset>,
        revision: u64,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) {
        if revision != view.revision() {
            self.fail("The text changed, so the word was not put in.", view);
            return;
        }
        let snapshot = view.worker_snapshot();
        let never = std::sync::atomic::AtomicBool::new(false);
        let caret = ByteOffset(replace.start.0 + word.len());
        match prepare_edits(
            snapshot.as_ref(),
            &[TextEdit {
                range: replace,
                text: word.to_string(),
            }],
            &never,
        ) {
            Ok(prepared) => view.run_host(
                HostCommand::ApplyEdits {
                    edits: Arc::new(prepared),
                    selection: caret..caret,
                },
                ctx,
            ),
            Err(message) => self.fail(message, view),
        }
    }
    fn choose_option(&mut self, data: &str, view: &mut EditorView, ctx: &mut ViewContext) {
        match data {
            OPTION_IGNORE_CASE => self.options.ignore_case ^= true,
            OPTION_WRAP => self.options.wrap ^= true,
            OPTION_CONTAINS => self.options.kind = MatchKind::Contains,
            OPTION_STARTS => self.options.kind = MatchKind::StartsWith,
            OPTION_FULL => self.options.kind = MatchKind::FullWord,
            OPTION_ENDS => self.options.kind = MatchKind::EndsWith,
            _ => return,
        }
        // Wrap Around changes no match, but a new search is the plain way to show the rest.
        self.query_changed(view, ctx);
        self.anchor = view.selection().start;
        self.start_search(view, true, ctx);
    }

    // ---- Input ----

    /// Sends an event to the tools first. `Handled::Yes` ends it. Otherwise the host looks up
    /// its keys, and then gives the event to the editor if `event_for_editor` says so.
    pub fn handle_input(
        &mut self,
        event: &InputEvent,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Handled {
        let handled = self.route_input(event, view, measure, ctx);
        self.sync_view_focus(view, ctx);
        handled
    }
    /// True when an event that `handle_input` left goes on to the editor. The mouse, the wheel
    /// and window events always do. Keys do only while the text has the keyboard.
    pub fn event_for_editor(&self, event: &InputEvent) -> bool {
        match event.kind {
            InputKind::KeyDown { .. }
            | InputKind::KeyUp { .. }
            | InputKind::Char(_)
            | InputKind::ImeStart
            | InputKind::ImeUpdate { .. }
            | InputKind::ImeEnd { .. }
            | InputKind::Focus
            | InputKind::Blur => self.focus() == ToolsFocus::Editor,
            _ => true,
        }
    }
    /// True once after the bar's height changed, so the editor's area must be laid out again.
    pub fn take_layout_changed(&mut self) -> bool {
        std::mem::take(&mut self.layout_changed)
    }
    /// Tells the view when the keyboard moves between the text and the tools, so that only one
    /// caret blinks.
    fn sync_view_focus(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        let has_keys = self.focus() == ToolsFocus::Editor;
        if has_keys != self.view_has_keys {
            self.view_has_keys = has_keys;
            let kind = if has_keys {
                InputKind::Focus
            } else {
                InputKind::Blur
            };
            view.handle(&InputEvent { time_us: 0, kind }, ctx);
        }
    }
    fn route_input(
        &mut self,
        event: &InputEvent,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) -> Handled {
        // The window gained or lost the keyboard: whichever control has it hears this.
        if matches!(event.kind, InputKind::Focus | InputKind::Blur) {
            if let Some(stop) = self.panel.keyboard {
                self.panel.control(stop).handle(event, ctx);
                return Handled::Yes;
            }
            if let Some(stop) = self.bar.focus {
                self.bar.control(stop).handle(event, ctx);
                return Handled::Yes;
            }
            return Handled::No;
        }
        if self.list.is_some() {
            return self.handle_list(event, view, ctx);
        }
        if let Some(alert) = self.alert.as_mut() {
            alert.handle(event, ctx);
            if matches!(
                alert.result(),
                Some(DialogResult::Button(_) | DialogResult::Dismissed)
            ) {
                let bounds = alert.bounds;
                self.alert = None;
                ctx.invalidate(bounds);
            }
            return Handled::Yes;
        }
        if self.panel.open
            && let Some(handled) = self.handle_panel(event, view, ctx)
        {
            return handled;
        }
        if !self.bar.open {
            return Handled::No;
        }
        let (handled, action) = self.bar.handle_bar(event, ctx);
        match action {
            BarAction::None => {}
            BarAction::QueryChanged => {
                self.query = self.bar.find.text.clone();
                self.query_changed(view, ctx);
                self.start_search(view, true, ctx);
                ctx.invalidate(self.bar.bounds);
            }
            BarAction::Next => {
                self.typed_search = false;
                self.request_step(Direction::Forward, view);
            }
            BarAction::Previous => {
                self.typed_search = false;
                self.request_step(Direction::Backward, view);
            }
            BarAction::Replace => self.replace_one(view, ctx),
            BarAction::ReplaceAll => self.press_replace_all(view),
            BarAction::Close => self.close_bar(view, ctx),
            BarAction::Options => self.open_options_list(measure, ctx),
        }
        if self.bar.focus.is_some() {
            if let InputKind::Char(_) | InputKind::ImeUpdate { .. } | InputKind::ImeEnd { .. } =
                event.kind
            {
                return Handled::Yes;
            }
            // A plain key that nothing used stops here. Ctrl, Alt and function keys go on to
            // the host's key table.
            if let InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } = &event.kind
                && !modifiers.ctrl
                && !modifiers.alt
                && !(0x70..=0x87).contains(virtual_key)
            {
                return Handled::Yes;
            }
        }
        handled
    }
    /// `None` passes the event on: a click outside the panel, or a key while the panel does
    /// not have the keyboard.
    fn handle_panel(
        &mut self,
        event: &InputEvent,
        view: &mut EditorView,
        ctx: &mut ViewContext,
    ) -> Option<Handled> {
        if let Some(at) = mouse_point(&event.kind) {
            let inside = contains(self.panel.frame, at);
            if matches!(event.kind, InputKind::MouseDown { .. }) {
                if inside {
                    self.bar.set_focus(None, ctx);
                    // Buttons do not take the keyboard from the field on a click, as on the
                    // Mac, so a click anywhere in the panel leaves it in the field.
                    self.panel.set_keyboard(Some(PanelStop::Field), ctx);
                } else {
                    self.panel.set_keyboard(None, ctx);
                }
            }
            self.panel.field.handle(event, ctx);
            self.panel.select.handle(event, ctx);
            self.panel.close.handle(event, ctx);
            self.after_panel_input(view, ctx);
            return inside.then_some(Handled::Yes);
        }
        let stop = self.panel.keyboard?;
        if let InputKind::KeyDown {
            virtual_key,
            modifiers,
            ..
        } = &event.kind
        {
            let composing = self.panel.field.composition.is_some();
            if !composing && !modifiers.alt && !modifiers.ctrl {
                match *virtual_key {
                    0x1b => {
                        self.close_panel(ctx);
                        return Some(Handled::Yes);
                    }
                    0x0d if stop == PanelStop::Field => {
                        let input = self.panel.field.text.clone();
                        self.submit_lines(input, true, view, ctx);
                        return Some(Handled::Yes);
                    }
                    0x09 => {
                        let order = [PanelStop::Field, PanelStop::Select, PanelStop::Close];
                        let at = order.iter().position(|s| *s == stop).unwrap_or(0);
                        let next = if modifiers.shift { at + 2 } else { at + 1 } % order.len();
                        self.panel.set_keyboard(Some(order[next]), ctx);
                        return Some(Handled::Yes);
                    }
                    _ => {}
                }
            }
        }
        let handled = self.panel.control(stop).handle(event, ctx);
        self.after_panel_input(view, ctx);
        Some(match (&event.kind, handled) {
            (
                InputKind::KeyDown {
                    modifiers,
                    virtual_key,
                    ..
                },
                Handled::No,
            ) if modifiers.ctrl || modifiers.alt || (0x70..=0x87).contains(virtual_key) => {
                Handled::No
            }
            _ => Handled::Yes,
        })
    }
    fn close_panel(&mut self, ctx: &mut ViewContext) {
        self.pending_lines = None;
        self.panel.hide(ctx);
        ctx.invalidate(self.panel.frame);
    }
    fn after_panel_input(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        use qe_ui::Control;
        self.panel.field.take_activated();
        if self.panel.close.take_activated() {
            self.close_panel(ctx);
            return;
        }
        if self.panel.select.take_activated() {
            let input = self.panel.field.text.clone();
            self.submit_lines(input, false, view, ctx);
        }
    }

    // ---- Poll ----

    /// Takes results that wait and keeps the marks current. Call it from the wake, and after
    /// the view's `poll_layout` and each edit. It returns at once when `is_idle`.
    pub fn poll(
        &mut self,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        if self.is_idle() {
            return;
        }
        self.poll_jobs(view, measure, ctx);
        self.sync_view_focus(view, ctx);
    }
    fn poll_jobs(
        &mut self,
        view: &mut EditorView,
        measure: &dyn TextMeasurer,
        ctx: &mut ViewContext,
    ) {
        // First the edits since the last look, so that a result for the text as it stands is
        // not moved through them a second time.
        if self.bar.open {
            self.follow_changes(view, ctx);
        }
        if let Some(result) = self.step.as_ref().and_then(Worker::take) {
            self.step = None;
            self.on_step(result, view, ctx);
            if self.step.is_none()
                && let Some(next) = self.step_queue.pop_front()
            {
                self.typed_search = false;
                let from = view.selection();
                self.start_step(next, from, view);
            }
        }
        if let Some((direction, from, past)) = self.step_waiting.clone() {
            let status = view.store_status();
            if status.complete || status.known_bytes.0 > past {
                self.start_step(direction, from, view);
            }
        }
        if let Some(result) = self.replace_job.as_ref().and_then(|run| run.worker.take())
            && let Some(run) = self.replace_job.take()
        {
            match result {
                Ok(Some(ready)) => self.replace_ready = Some((ready, run.request)),
                Ok(None) => {
                    self.beep = true;
                    self.bar.set_count("Not found", false);
                }
                Err(message) => self.fail(message, view),
            }
        }
        self.finish_replace_all(view, ctx);
        if let Some(result) = self
            .transform_job
            .as_ref()
            .and_then(|run| run.worker.take())
            && let Some(run) = self.transform_job.take()
        {
            match result {
                // No change: no edit and no undo step, and the selection stays.
                Ok(None) => {}
                // A transformation does not run again, because the selection it was for has
                // moved.
                Ok(Some(prepared)) if prepared.base_revision() == view.revision() => {
                    self.apply_transform(prepared, run.start, run.keep_selected, view, ctx);
                }
                Ok(Some(_)) => self.fail(
                    "The text changed while the selection was being transformed, so nothing was changed.",
                    view,
                ),
                Err(message) => self.fail(message, view),
            }
        }
        if let Some(result) = self.complete_job.as_ref().and_then(Worker::take) {
            self.complete_job = None;
            match result {
                Ok(result) => self.open_complete_list(result, view, measure, ctx),
                Err(message) => self.fail(message, view),
            }
        }
        // One look, once a letter that went to the editor has gone in. A letter still waiting
        // behind a key keeps it for later, and nothing else keeps it open.
        if let Some(revision) = self.complete_again {
            if revision != view.revision() {
                self.complete_again = None;
                if self.focus() == ToolsFocus::Editor {
                    self.start_complete(view);
                }
            } else if view.queued_input_count() == 0 {
                self.complete_again = None;
            }
        }
        if let Some((input, close)) = self.pending_lines.take() {
            self.submit_lines(input, close, view, ctx);
        }
        if self.pending_center {
            self.center_selection(view, ctx);
        }
        if !self.bar.open {
            return;
        }
        self.poll_search(view, ctx);
    }
    /// Moves the bar's state through the edits since the last look.
    fn follow_changes(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        let revision = view.revision();
        if revision == self.seen_revision {
            return;
        }
        match view.changes_since(self.seen_revision) {
            ChangesSince::Current => {}
            ChangesSince::Changes(list) => {
                // The editor moved the mark with its text, and dropped it if an edit cut it.
                self.current = view
                    .decorations(DecorationKind::CurrentFindMatch)
                    .into_iter()
                    .next();
                for tracked in &list {
                    self.anchor = move_offset(self.anchor, tracked);
                }
                // An edit beside a match can end it, as a letter after a Full Word match does.
                if !self.query.is_empty()
                    && let Some(current) = self.current.clone()
                    && !self.current_still_matches(current, view)
                {
                    self.current = None;
                    self.set_current_mark(view, ctx);
                }
                self.on_text_changed(view, false, ctx);
            }
            ChangesSince::Rescan => self.on_text_changed(view, true, ctx),
        }
        self.seen_revision = revision;
    }
    fn current_still_matches(&self, current: Range<ByteOffset>, view: &EditorView) -> bool {
        // Only an editable view has edits, and its text is in memory.
        let never = std::sync::atomic::AtomicBool::new(false);
        self.matcher().is_some_and(|matcher| {
            matcher.still_matches(view.worker_snapshot().as_ref(), current, &never)
        })
    }
    fn poll_search(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        // Prevent Editing can change while the bar stands open.
        let editable = view.can_edit();
        self.bar.replace.enabled = editable;
        self.bar.replace_one.enabled = editable;
        self.bar.replace_all.enabled = editable;
        if !editable
            && matches!(
                self.bar.focus,
                Some(BarStop::Replace | BarStop::ReplaceOne | BarStop::ReplaceAll)
            )
        {
            self.bar.set_focus(Some(BarStop::Find), ctx);
        }
        if self.query.is_empty() {
            return;
        }
        if let Some(job) = &self.count_job {
            self.count = job.count();
            job.feed(view);
            // Reading lets the worker wake the host again, so the count keeps coming.
            job.drain();
            match job.state() {
                JobState::Running => {}
                JobState::Finished => {
                    self.count_done = true;
                    self.count_job = None;
                }
                JobState::Cancelled => self.count_job = None,
                JobState::Failed(message) => {
                    self.count_job = None;
                    self.fail(message, view);
                }
            }
            self.update_count_label();
            ctx.invalidate(self.bar.count.bounds);
        }
        if let Some(scan) = self.visible.take() {
            match scan.job.state() {
                JobState::Running => self.visible = Some(scan),
                JobState::Finished => self.on_visible_done(scan, view, ctx),
                JobState::Cancelled => {}
                JobState::Failed(message) => self.fail(message, view),
            }
        }
        self.refresh_marks(view);
    }
    fn on_text_changed(&mut self, view: &mut EditorView, rescan: bool, ctx: &mut ViewContext) {
        if rescan {
            self.current = None;
        }
        self.status = None;
        if rescan {
            self.clear_marks(view, ctx);
        }
        if let Some(job) = self.count_job.take() {
            job.cancel();
        }
        if let Some(scan) = self.visible.take() {
            scan.job.cancel();
        }
        self.covered = None;
        self.scanned_for = None;
        self.count = 0;
        self.count_done = false;
        if let Some(matcher) = self.matcher() {
            let snapshot = view.worker_snapshot();
            let len = snapshot.len_bytes();
            self.workers_started += 1;
            self.count_job = Some(FindJob::start(
                snapshot,
                matcher,
                ByteOffset(0)..len,
                0,
                self.snapshot_wake(),
            ));
        }
    }
    /// Call this when a window shows another document. `old` loses its marks, and the bar
    /// searches `new`.
    pub fn view_switched(
        &mut self,
        old: Option<&mut EditorView>,
        new: &mut EditorView,
        ctx: &mut ViewContext,
    ) {
        self.cancel_search();
        self.replace_job = None;
        self.replace_ready = None;
        self.transform_job = None;
        self.complete_job = None;
        self.complete_again = None;
        self.pending_lines = None;
        self.pending_center = false;
        self.list = None;
        self.current = None;
        let mut old_view = old;
        if let Some(old) = old_view.as_deref_mut() {
            self.clear_marks(old, ctx);
        }
        if let (Some(old), true) = (old_view, self.view_has_keys) {
            let blur = InputEvent {
                time_us: 0,
                kind: InputKind::Blur,
            };
            old.handle(&blur, ctx);
        }
        self.view_has_keys = false;
        self.sync_view_focus(new, ctx);
        self.anchor = new.selection().start;
        self.seen_revision = new.revision();
        if self.bar.open {
            self.start_search(new, false, ctx);
        }
    }

    // ---- Layout, paint ----

    /// Takes the window's text area, places the bar at its top, and gives back what is left
    /// for the editor. With the bar closed it gives back the same area.
    pub fn layout(&mut self, area: Rect, measure: &dyn TextMeasurer) -> Rect {
        self.area = area;
        self.place_all(measure);
        let used = self.bar.height();
        Rect {
            x: area.x,
            y: Dip(area.y.0 + used),
            width: area.width,
            height: Dip((area.height.0 - used).max(0.0)),
        }
    }
    fn place_all(&mut self, measure: &dyn TextMeasurer) {
        if self.bar.open {
            let bounds = Rect {
                x: self.area.x,
                y: self.area.y,
                width: self.area.width,
                height: Dip(self.bar.height().min(BAR_ROW_HEIGHT * 2.0)),
            };
            self.bar.place(bounds, measure);
        }
        if self.panel.open {
            self.panel.place(self.area, measure);
        }
    }
    /// Paints the bar, the panel, its message and any open list. Nothing while all are closed.
    /// Paint it after the editor, so that these stand on top.
    pub fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        self.bar.draw_bar(list, theme, measure);
        self.panel.draw_panel(list, theme, measure);
        if let Some(alert) = &self.alert {
            alert.draw(list, theme, measure);
            let t = qe_ui::UiTokens::from_theme(theme);
            list.push(qe_api::DrawCommand::StrokeRect {
                rect: alert.bounds,
                radius: Dip(0.0),
                width: Dip(1.0),
                color: t.popup_border,
            });
        }
        if let Some(open) = &self.list {
            open.menu.draw(list, theme, measure);
        }
    }
    /// What a screen reader sees of the tools.
    pub fn widgets(&self) -> Vec<Widget> {
        let mut out: Vec<Widget> = [self.bar.widget(), self.panel.widget()]
            .into_iter()
            .flatten()
            .collect();
        if let Some(alert) = &self.alert
            && let Some(widget) = alert.widgets()
        {
            out.push(widget);
        }
        if let Some(open) = &self.list
            && let Some(widget) = open.menu.widgets()
        {
            out.push(widget);
        }
        out
    }
}

fn lookup_message(error: &LookupError) -> String {
    match error {
        LookupError::NotYetKnown { .. } => {
            "The file is still being read, so this text is not ready yet. Try again in a moment."
                .to_string()
        }
        LookupError::OutOfRange => "That text is not in the document.".to_string(),
        LookupError::TooLong { .. } => "That text is too long for this.".to_string(),
        LookupError::Failed(failure) => format!("The file could not be read: {}", failure.message),
    }
}

/// `1234567` as `1,234,567`.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Where an offset stands after one edit. Inside the edit, it goes to the edit's start.
fn move_offset(at: ByteOffset, tracked: &qe_api::TrackedChange) -> ByteOffset {
    let (start, end) = (
        tracked.change.replaced.start.0,
        tracked.change.replaced.end.0,
    );
    if at.0 >= end {
        ByteOffset(at.0 - (end - start) + tracked.change.new_len)
    } else if at.0 > start {
        ByteOffset(start)
    } else {
        at
    }
}
