//! Document tools shared by the Windows host. Long scans and edit preparation run on workers.
use qe_api::{ByteOffset, LineIndex, LookupError, TextSnapshot, ViewContext};
use qe_editor::{EditorView, HostCommand, PreparedEdits, TextEdit, prepare_edits};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

pub const SELECT_LINE_SIZE: (f32, f32) = (253.0, 62.0);
pub const SELECT_LINE_FIELD: (f32, f32) = (138.0, 22.0);
pub const SELECT_LINE_BUTTON: (f32, f32) = (77.0, 32.0);
pub const LINE_ERROR: &str = "Invalid line specification";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSpec {
    pub from: usize,
    pub to: usize,
    pub relative: i8,
}
impl LineSpec {
    pub fn parse(input: &str) -> Option<Self> {
        let mut s = input.trim_start();
        let relative = if let Some(rest) = s.strip_prefix('+') {
            s = rest;
            1
        } else if let Some(rest) = s.strip_prefix('-') {
            s = rest;
            -1
        } else {
            0
        };
        let (from, rest) = scan_number(s)?;
        s = rest.trim_start();
        let to = if let Some(rest) = s.strip_prefix('-') {
            let (n, tail) = scan_number(rest)?;
            if !tail.trim().is_empty() || n < from {
                return None;
            }
            n
        } else if s.is_empty() {
            from
        } else {
            return None;
        };
        Some(Self { from, to, relative })
    }
    pub fn resolve(self, current: usize, total: usize) -> Option<Range<usize>> {
        let count = self.to.checked_sub(self.from)?.checked_add(1)?;
        let start = match self.relative {
            0 => self.from.checked_sub(1)?,
            1 => current.checked_add(self.from)?,
            -1 => current.checked_sub(self.to)?,
            _ => return None,
        };
        let end = start.checked_add(count)?;
        (start < total && end <= total).then_some(start..end)
    }
}
fn scan_number(s: &str) -> Option<(usize, &str)> {
    let s = s.trim_start();
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    Some((s.get(..n)?.parse().ok()?, s.get(n..)?))
}
pub fn select_lines(
    view: &mut EditorView,
    input: &str,
    ctx: &mut ViewContext,
) -> Result<(), String> {
    let spec = LineSpec::parse(input).ok_or_else(|| line_error(input))?;
    let status = view.store_status();
    if !status.complete {
        return Err("The line index is still loading.".into());
    }
    let selection = view.selection();
    let at = if spec.relative > 0 && selection.start < selection.end {
        selection.end.0.saturating_sub(1)
    } else {
        selection.start.0
    };
    let current = view
        .line_of_byte(ByteOffset(at))
        .map_err(|e| line_lookup_message(e, input))?
        .0;
    let lines = spec
        .resolve(current, status.known_lines)
        .ok_or_else(|| line_error(input))?;
    let first = view
        .line_range(LineIndex(lines.start))
        .map_err(|e| line_lookup_message(e, input))?;
    let last = view
        .line_range(LineIndex(lines.end - 1))
        .map_err(|e| line_lookup_message(e, input))?;
    view.run_host(
        HostCommand::Select {
            range: first.start..last.end,
            reveal: true,
        },
        ctx,
    );
    Ok(())
}
fn lookup_message(e: LookupError) -> String {
    match e {
        LookupError::NotYetKnown { .. } => "The line index is still loading.".into(),
        _ => LINE_ERROR.into(),
    }
}
fn line_error(input: &str) -> String {
    let short: String = input.chars().take(90).collect();
    let suffix = if input.chars().count() >= 100 {
        "\u{2026}"
    } else {
        ""
    };
    format!("{LINE_ERROR} \u{201c}{short}{suffix}\u{201d}.")
}
fn line_lookup_message(e: LookupError, input: &str) -> String {
    match e {
        LookupError::NotYetKnown { .. } => "The line index is still loading.".into(),
        _ => line_error(input),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
}
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
fn is_extend(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f | 0x1f3fb..=0x1f3ff)
        || c == '\u{200d}'
}
fn boundary(text: &str, at: usize) -> bool {
    if at == 0 || at == text.len() {
        return true;
    }
    let before = text.get(..at).and_then(|s| s.chars().next_back());
    let after = text.get(at..).and_then(|s| s.chars().next());
    match (before, after) {
        (Some(b), Some(a)) => !is_extend(a) && b != '\u{200d}' && !(b == '\r' && a == '\n'),
        _ => false,
    }
}
pub fn find_text(text: &str, query: &str, options: FindOptions) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    // Search at character boundaries. Case mapping is Unicode scalar lowercase, like
    // NSString's ordinary case-insensitive search for these scripts.
    let needle = if options.case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let mut at = 0;
    while at < text.len() {
        let Some(part) = text.get(at..) else { break };
        let mut found = None;
        for (delta, _) in part.char_indices() {
            let start = at + delta;
            let Some(end) = text.get(start..).and_then(|s| {
                s.char_indices()
                    .nth(query.chars().count())
                    .map(|(n, _)| start + n)
                    .or_else(|| (s.chars().count() == query.chars().count()).then_some(text.len()))
            }) else {
                continue;
            };
            let Some(candidate) = text.get(start..end) else {
                continue;
            };
            let equal = if options.case_sensitive {
                candidate == query
            } else {
                candidate.to_lowercase() == needle
            };
            if !equal || !boundary(text, start) || !boundary(text, end) {
                continue;
            }
            if options.whole_word {
                let left = text
                    .get(..start)
                    .and_then(|s| s.chars().next_back())
                    .is_some_and(is_word);
                let right = text
                    .get(end..)
                    .and_then(|s| s.chars().next())
                    .is_some_and(is_word);
                if left || right {
                    continue;
                }
            }
            found = Some(start..end);
            break;
        }
        match found {
            Some(r) => {
                at = r.end;
                matches.push(r);
            }
            None => break,
        }
    }
    matches
}

pub struct FindWorker {
    cancel: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<Result<Vec<Range<ByteOffset>>, String>>>,
}
impl FindWorker {
    pub fn start(snapshot: Arc<dyn TextSnapshot>, query: String, options: FindOptions) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let join = thread::spawn(move || {
            let mut text = String::new();
            let len = snapshot.len_bytes().0;
            let valid = snapshot.visit_chunks_while(ByteOffset(0)..ByteOffset(len), &mut |part| {
                if flag.load(Ordering::Acquire) {
                    return false;
                }
                text.push_str(part);
                true
            });
            if flag.load(Ordering::Acquire) {
                return Err("Search cancelled.".into());
            }
            if !valid {
                return Err("The file could not be searched.".into());
            }
            Ok(find_text(&text, &query, options)
                .into_iter()
                .map(|r| ByteOffset(r.start)..ByteOffset(r.end))
                .collect())
        });
        Self {
            cancel,
            join: Some(join),
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn take_if_ready(&mut self) -> Option<Result<Vec<Range<ByteOffset>>, String>> {
        if !self.join.as_ref()?.is_finished() {
            return None;
        }
        Some(
            self.join
                .take()?
                .join()
                .unwrap_or_else(|_| Err("The search worker stopped.".into())),
        )
    }
}
impl Drop for FindWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn prepare_replace_all(
    snapshot: &dyn TextSnapshot,
    matches: &[Range<ByteOffset>],
    replacement: &str,
    cancel: &AtomicBool,
) -> Result<PreparedEdits, String> {
    let edits: Vec<_> = matches
        .iter()
        .map(|r| TextEdit {
            range: r.clone(),
            text: replacement.into(),
        })
        .collect();
    prepare_edits(snapshot, &edits, cancel)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    Upper,
    Lower,
    Capitalize,
}
pub fn transform_text(text: &str, transform: Transform) -> String {
    match transform {
        Transform::Upper => text.to_uppercase(),
        Transform::Lower => text.to_lowercase(),
        Transform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut starts_word = true;
            for c in text.chars() {
                if c.is_alphanumeric() {
                    if starts_word {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    starts_word = false;
                } else {
                    out.push(c);
                    starts_word = true;
                }
            }
            out
        }
    }
}
pub fn prepare_transform(
    snapshot: &dyn TextSnapshot,
    range: Range<ByteOffset>,
    transform: Transform,
    cancel: &AtomicBool,
) -> Result<PreparedEdits, String> {
    if range.start >= range.end {
        return Err("Select text to transform.".into());
    }
    let mut text = String::new();
    if !snapshot.visit_chunks(range.clone(), &mut |part| text.push_str(part)) {
        return Err("The selected text could not be read.".into());
    }
    if cancel.load(Ordering::Acquire) {
        return Err("The transformation was cancelled.".into());
    }
    prepare_edits(
        snapshot,
        &[TextEdit {
            range,
            text: transform_text(&text, transform),
        }],
        cancel,
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Substitutions {
    pub smart_copy: bool,
    pub quotes: bool,
    pub dashes: bool,
}
impl Default for Substitutions {
    fn default() -> Self {
        Self {
            smart_copy: true,
            quotes: false,
            dashes: false,
        }
    }
}
pub fn substitute_typed(typed: &str, before: &str, opts: Substitutions) -> String {
    if opts.dashes && typed == "-" && before.ends_with('-') {
        return "\u{2014}".into();
    }
    if opts.quotes && typed == "\"" {
        let opening = before
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || "([{\u{2014}".contains(c));
        return if opening { "\u{201c}" } else { "\u{201d}" }.into();
    }
    if opts.quotes && typed == "'" {
        let opening = before
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || "([{\u{2014}".contains(c));
        return if opening { "\u{2018}" } else { "\u{2019}" }.into();
    }
    typed.into()
}
pub fn smart_paste(text: &str, before: &str, after: &str, opts: Substitutions) -> String {
    if !opts.smart_copy || text.is_empty() {
        return text.into();
    }
    let mut out = String::new();
    if before
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
        && text.chars().next().is_some_and(char::is_alphanumeric)
    {
        out.push(' ');
    }
    out.push_str(text);
    if after.chars().next().is_some_and(char::is_alphanumeric)
        && text.chars().next_back().is_some_and(char::is_alphanumeric)
    {
        out.push(' ');
    }
    out
}

pub struct SelectLinePanel {
    pub field: qe_ui::TextField,
    pub select: qe_ui::Button,
    pub open: bool,
}
impl SelectLinePanel {
    pub fn new() -> Self {
        let mut field = qe_ui::TextField::new(qe_api::WidgetId(12001), "line number");
        field.placeholder = "Line number".into();
        Self {
            field,
            select: qe_ui::Button::new(qe_api::WidgetId(12002), "Select"),
            open: false,
        }
    }
    pub fn submit(
        &mut self,
        view: &mut EditorView,
        return_key: bool,
        ctx: &mut ViewContext,
    ) -> Result<(), String> {
        select_lines(view, &self.field.text, ctx)?;
        if return_key {
            self.open = false;
        }
        Ok(())
    }
}
impl Default for SelectLinePanel {
    fn default() -> Self {
        Self::new()
    }
}
impl qe_api::View for SelectLinePanel {
    fn handle(&mut self, event: &qe_api::InputEvent, ctx: &mut ViewContext) -> qe_api::Handled {
        if !self.open {
            return qe_api::Handled::No;
        }
        if self.field.handle(event, ctx) == qe_api::Handled::Yes {
            return qe_api::Handled::Yes;
        }
        self.select.handle(event, ctx)
    }
    fn draw(
        &self,
        list: &mut qe_api::DisplayList,
        theme: &qe_api::Theme,
        measure: &dyn qe_api::TextMeasurer,
    ) {
        if self.open {
            self.field.draw(list, theme, measure);
            self.select.draw(list, theme, measure);
        }
    }
    fn layout(&mut self, bounds: qe_api::Rect, measure: &dyn qe_api::TextMeasurer) {
        use qe_api::{Dip, Rect};
        self.field.layout(
            Rect {
                x: Dip(bounds.x.0 + 20.0),
                y: Dip(bounds.y.0 + 20.0),
                width: Dip(138.0),
                height: Dip(22.0),
            },
            measure,
        );
        self.select.layout(
            Rect {
                x: Dip(bounds.x.0 + 162.0),
                y: Dip(bounds.y.0 + 13.0),
                width: Dip(77.0),
                height: Dip(32.0),
            },
            measure,
        );
    }
    fn widgets(&self) -> Option<qe_api::Widget> {
        None
    }
}

pub fn complete_candidates(
    snapshot: &dyn TextSnapshot,
    prefix: &str,
    at: ByteOffset,
) -> Result<Vec<String>, String> {
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let len = snapshot.len_bytes().0;
    let start = at.0.saturating_sub(32 * 1024);
    let end = at.0.saturating_add(32 * 1024).min(len);
    let mut text = String::new();
    if !snapshot.visit_chunks(ByteOffset(start)..ByteOffset(end), &mut |s| {
        text.push_str(s)
    }) {
        return Err("The words near the caret could not be read.".into());
    }
    let mut words = std::collections::BTreeSet::new();
    let mut word = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if is_word(c) {
            word.push(c);
        } else {
            if word.starts_with(prefix) && word != prefix {
                words.insert(std::mem::take(&mut word));
            } else {
                word.clear();
            }
        }
    }
    Ok(words.into_iter().take(100).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindAction {
    Open,
    OpenReplace,
    Next,
    Previous,
    Replace,
    ReplaceAll,
    Close,
    None,
}
pub fn find_key(virtual_key: u16, ctrl: bool, shift: bool) -> FindAction {
    match (virtual_key, ctrl, shift) {
        (0x46, true, _) => FindAction::Open,
        (0x48, true, _) => FindAction::OpenReplace,
        (0x72, false, true) => FindAction::Previous,
        (0x72, false, false) | (0x0d, false, false) => FindAction::Next,
        (0x0d, false, true) => FindAction::Previous,
        (0x1b, false, _) => FindAction::Close,
        _ => FindAction::None,
    }
}
pub struct FindBar {
    pub find: qe_ui::TextField,
    pub replace: qe_ui::TextField,
    pub next: qe_ui::Button,
    pub previous: qe_ui::Button,
    pub replace_one: qe_ui::Button,
    pub replace_all: qe_ui::Button,
    pub open: bool,
    pub replacing: bool,
    pub count: usize,
    pub complete: bool,
    pub current: Option<Range<ByteOffset>>,
}
impl Default for FindBar {
    fn default() -> Self {
        Self::new()
    }
}
impl FindBar {
    pub fn new() -> Self {
        let mut find = qe_ui::TextField::new(qe_api::WidgetId(12101), "Find");
        find.placeholder = "Find".into();
        let mut replace = qe_ui::TextField::new(qe_api::WidgetId(12102), "Replace");
        replace.placeholder = "Replace".into();
        Self {
            find,
            replace,
            next: qe_ui::Button::new(qe_api::WidgetId(12103), "Next"),
            previous: qe_ui::Button::new(qe_api::WidgetId(12104), "Previous"),
            replace_one: qe_ui::Button::new(qe_api::WidgetId(12105), "Replace"),
            replace_all: qe_ui::Button::new(qe_api::WidgetId(12106), "Replace All"),
            open: false,
            replacing: false,
            count: 0,
            complete: false,
            current: None,
        }
    }
    pub fn close(&mut self, view: &mut EditorView, ctx: &mut ViewContext) {
        self.open = false;
        self.current = None;
        view.set_decorations(qe_editor::DecorationKind::FindMatch, Vec::new(), ctx);
        view.set_decorations(qe_editor::DecorationKind::CurrentFindMatch, Vec::new(), ctx);
    }
    pub fn show_matches(
        &mut self,
        view: &mut EditorView,
        matches: &[Range<ByteOffset>],
        ctx: &mut ViewContext,
    ) {
        if !self.open {
            return;
        }
        self.count = matches.len();
        self.complete = true;
        let near = match view.visible_range() {
            Ok(visible) => matches
                .iter()
                .filter(|r| r.end >= visible.start && r.start <= visible.end)
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        };
        view.set_decorations(qe_editor::DecorationKind::FindMatch, near, ctx);
        view.set_decorations(
            qe_editor::DecorationKind::CurrentFindMatch,
            self.current.clone().into_iter().collect(),
            ctx,
        );
    }
    pub fn navigate(
        &mut self,
        view: &mut EditorView,
        matches: &[Range<ByteOffset>],
        forward: bool,
        ctx: &mut ViewContext,
    ) {
        if matches.is_empty() {
            return;
        }
        let selection = view.selection();
        let next = if forward {
            matches
                .iter()
                .find(|r| r.start >= selection.end)
                .or_else(|| matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|r| r.end <= selection.start)
                .or_else(|| matches.last())
        };
        if let Some(r) = next {
            self.current = Some(r.clone());
            view.run_host(
                HostCommand::Select {
                    range: r.clone(),
                    reveal: true,
                },
                ctx,
            );
            self.show_matches(view, matches, ctx);
        }
    }
}
impl qe_api::View for FindBar {
    fn handle(&mut self, event: &qe_api::InputEvent, ctx: &mut ViewContext) -> qe_api::Handled {
        if !self.open {
            return qe_api::Handled::No;
        }
        if self.find.handle(event, ctx) == qe_api::Handled::Yes {
            return qe_api::Handled::Yes;
        }
        if self.replacing && self.replace.handle(event, ctx) == qe_api::Handled::Yes {
            return qe_api::Handled::Yes;
        }
        for button in [
            &mut self.previous,
            &mut self.next,
            &mut self.replace_one,
            &mut self.replace_all,
        ] {
            if button.handle(event, ctx) == qe_api::Handled::Yes {
                return qe_api::Handled::Yes;
            }
        }
        qe_api::Handled::No
    }
    fn draw(
        &self,
        list: &mut qe_api::DisplayList,
        theme: &qe_api::Theme,
        measure: &dyn qe_api::TextMeasurer,
    ) {
        if !self.open {
            return;
        }
        self.find.draw(list, theme, measure);
        self.previous.draw(list, theme, measure);
        self.next.draw(list, theme, measure);
        if self.replacing {
            self.replace.draw(list, theme, measure);
            self.replace_one.draw(list, theme, measure);
            self.replace_all.draw(list, theme, measure);
        }
    }
    fn layout(&mut self, bounds: qe_api::Rect, measure: &dyn qe_api::TextMeasurer) {
        use qe_api::{Dip, Rect};
        let box_at = |x: f32, y: f32, w: f32, h: f32| Rect {
            x: Dip(bounds.x.0 + x),
            y: Dip(bounds.y.0 + y),
            width: Dip(w),
            height: Dip(h),
        };
        self.find.layout(box_at(8.0, 5.0, 200.0, 24.0), measure);
        self.previous
            .layout(box_at(215.0, 3.0, 75.0, 28.0), measure);
        self.next.layout(box_at(295.0, 3.0, 65.0, 28.0), measure);
        if self.replacing {
            self.replace.layout(box_at(8.0, 37.0, 200.0, 24.0), measure);
            self.replace_one
                .layout(box_at(215.0, 35.0, 75.0, 28.0), measure);
            self.replace_all
                .layout(box_at(295.0, 35.0, 95.0, 28.0), measure);
        }
    }
    fn widgets(&self) -> Option<qe_api::Widget> {
        None
    }
}

pub struct CompletionRequest {
    pub snapshot: Arc<dyn TextSnapshot>,
    pub prefix: String,
    pub at: ByteOffset,
    pub replace: Range<ByteOffset>,
    pub revision: u64,
}
pub fn completion_request(view: &mut EditorView) -> Result<Option<CompletionRequest>, String> {
    let at = view.caret();
    let word = view.word_range_at(at).map_err(lookup_message)?;
    if word.start >= at {
        return Ok(None);
    }
    let snapshot = view.worker_snapshot();
    let mut prefix = String::new();
    snapshot
        .try_visit_chunks(word.start..at, &mut |s| prefix.push_str(s))
        .map_err(lookup_message)?;
    if prefix.is_empty() {
        return Ok(None);
    }
    Ok(Some(CompletionRequest {
        snapshot,
        prefix,
        at,
        replace: word.start..at,
        revision: view.revision(),
    }))
}

pub fn smart_cut_range(text: &str, selected: Range<usize>, opts: Substitutions) -> Range<usize> {
    if !opts.smart_copy || selected.start >= selected.end || text.get(selected.clone()).is_none() {
        return selected;
    }
    let before = text
        .get(..selected.start)
        .and_then(|s| s.chars().next_back());
    let after = text.get(selected.end..).and_then(|s| s.chars().next());
    if before == Some(' ')
        && text
            .get(selected.clone())
            .and_then(|s| s.chars().next())
            .is_some_and(char::is_alphanumeric)
    {
        return selected.start - 1..selected.end;
    }
    if after == Some(' ') && before.is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation()) {
        return selected.start..selected.end + 1;
    }
    selected
}
