//! Optional spelling, owned by an editor view only while enabled.
use qe_api::{ByteOffset, TextSnapshot, ViewContext};
use qe_editor::{DecorationKind, EditorView};
use std::{
    ops::Range,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};
use windows::{
    Win32::{
        Globalization::{ISpellChecker, ISpellCheckerFactory, SpellCheckerFactory},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
    },
    core::{PCWSTR, PWSTR},
};

pub const MAX_BYTES: usize = 50 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misspelling {
    pub range: Range<ByteOffset>,
    pub word: String,
    pub suggestions: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixMenu {
    pub range: Range<ByteOffset>,
    pub word: String,
    pub suggestions: Vec<String>,
    pub add_to_dictionary: bool,
    pub ignore: bool,
}
pub enum SpellAction {
    Add(String),
    Ignore(String),
}
struct Job {
    snapshot: Arc<dyn TextSnapshot>,
    range: Range<ByteOffset>,
    revision: u64,
}
enum Command {
    Check(Job),
    Action(SpellAction),
}
struct Answer {
    revision: u64,
    range: Range<ByteOffset>,
    errors: Vec<Misspelling>,
    error: Option<String>,
}

/// All COM calls live on a single-threaded apartment worker.
pub struct SpellService {
    jobs: Sender<Command>,
    answers: Receiver<Answer>,
    errors: Vec<Misspelling>,
    revision: u64,
    document: u64,
}
impl SpellService {
    pub fn eligible(bytes: usize, enabled: bool) -> bool {
        enabled && bytes <= MAX_BYTES
    }
    pub fn start(
        view: &EditorView,
        enabled: bool,
        language: &str,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Option<Self>, String> {
        if !Self::eligible(view.store_status().known_bytes.0, enabled) {
            return Ok(None);
        }
        let (job_tx, job_rx) = mpsc::channel();
        let (answer_tx, answer_rx) = mpsc::channel();
        let language = language.to_owned();
        thread::Builder::new()
            .name("qe-spell".into())
            .spawn(move || worker(language, job_rx, answer_tx, wake))
            .map_err(|e| format!("Cannot start spelling worker: {e}"))?;
        Ok(Some(Self {
            jobs: job_tx,
            answers: answer_rx,
            errors: Vec::new(),
            revision: view.revision(),
            document: view.document_number(),
        }))
    }
    pub fn schedule_visible(&mut self, view: &EditorView) -> Result<(), String> {
        if !Self::eligible(view.store_status().known_bytes.0, true) {
            return Err("Spelling is off for files over 50 MiB.".into());
        }
        if view.document_number() != self.document {
            self.document = view.document_number();
            self.errors.clear();
        }
        let visible = view
            .visible_range()
            .map_err(|e| format!("Spelling waits for layout: {e:?}"))?;
        let snapshot = view.worker_snapshot();
        let mut end = (visible.end.0 + 4096).min(view.store_status().known_bytes.0);
        while end > visible.end.0 && snapshot.try_byte_to_utf16(ByteOffset(end)).is_err() {
            end -= 1;
        }
        let range = visible.start..ByteOffset(end);
        self.jobs
            .send(Command::Check(Job {
                snapshot,
                range,
                revision: view.revision(),
            }))
            .map_err(|_| "Spelling worker stopped.".to_owned())
    }
    pub fn poll(&mut self, view: &mut EditorView, ctx: &mut ViewContext) -> Result<bool, String> {
        let mut changed = false;
        while let Ok(answer) = self.answers.try_recv() {
            if let Some(error) = answer.error {
                return Err(error);
            }
            if answer.revision != view.revision() || self.document != view.document_number() {
                continue;
            }
            self.errors
                .retain(|e| e.range.end <= answer.range.start || e.range.start >= answer.range.end);
            self.errors.extend(answer.errors);
            self.errors.sort_by_key(|e| e.range.start.0);
            self.revision = answer.revision;
            changed = true;
        }
        if changed {
            view.set_decorations(
                DecorationKind::SpellingUnderline,
                self.errors.iter().map(|e| e.range.clone()).collect(),
                ctx,
            );
        }
        Ok(changed)
    }
    pub fn menu_at(&self, view: &EditorView, byte: ByteOffset) -> Option<FixMenu> {
        if self.revision != view.revision() || self.document != view.document_number() {
            return None;
        }
        let e = self
            .errors
            .iter()
            .find(|e| e.range.start <= byte && byte < e.range.end)?;
        Some(FixMenu {
            range: e.range.clone(),
            word: e.word.clone(),
            suggestions: e.suggestions.clone(),
            add_to_dictionary: true,
            ignore: true,
        })
    }
    pub fn act(&self, action: SpellAction) -> Result<(), String> {
        self.jobs
            .send(Command::Action(action))
            .map_err(|_| "Spelling worker stopped.".into())
    }
}
pub fn apply_fix(
    view: &mut EditorView,
    range: Range<ByteOffset>,
    replacement: &str,
    ctx: &mut ViewContext,
) -> Result<(), String> {
    if !view.can_edit() {
        return Err("This document is read only.".into());
    }
    view.replace_range(range, replacement, ctx);
    Ok(())
}
pub fn word_is_candidate(token: &str) -> bool {
    !token.contains("://")
        && !token.contains("www.")
        && !token.contains('@')
        && !token.contains('.')
        && token.chars().any(char::is_alphabetic)
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
fn worker(
    language: String,
    jobs: Receiver<Command>,
    answers: Sender<Answer>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let checker = if initialized.is_ok() {
        (|| -> windows::core::Result<ISpellChecker> {
            let factory: ISpellCheckerFactory =
                unsafe { CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER)? };
            let tag = wide(&language);
            unsafe { factory.CreateSpellChecker(PCWSTR(tag.as_ptr())) }
        })()
    } else {
        Err(initialized.into())
    };
    for command in jobs {
        match command {
            Command::Action(action) => {
                if let Ok(checker) = &checker {
                    let (word, add) = match action {
                        SpellAction::Add(w) => (w, true),
                        SpellAction::Ignore(w) => (w, false),
                    };
                    let w = wide(&word);
                    let result = unsafe {
                        if add {
                            checker.Add(PCWSTR(w.as_ptr()))
                        } else {
                            checker.Ignore(PCWSTR(w.as_ptr()))
                        }
                    };
                    if let Err(e) = result {
                        let _ = answers.send(Answer {
                            revision: 0,
                            range: ByteOffset(0)..ByteOffset(0),
                            errors: Vec::new(),
                            error: Some(format!("Cannot update spelling dictionary: {e}")),
                        });
                        wake();
                    }
                }
            }
            Command::Check(job) => {
                let result = match &checker {
                    Ok(checker) => check_job(checker, &job),
                    Err(e) => Err(format!("Windows spelling is unavailable: {e}")),
                };
                let (errors, error) = match result {
                    Ok(v) => (v, None),
                    Err(e) => (Vec::new(), Some(e)),
                };
                if answers
                    .send(Answer {
                        revision: job.revision,
                        range: job.range,
                        errors,
                        error,
                    })
                    .is_err()
                {
                    break;
                }
                wake();
            }
        }
    }
    drop(checker);
    if initialized.is_ok() {
        unsafe { CoUninitialize() };
    }
}
fn check_job(checker: &ISpellChecker, job: &Job) -> Result<Vec<Misspelling>, String> {
    let mut text = String::new();
    let mut end = job
        .range
        .end
        .0
        .min(job.range.start.0.saturating_add(64 * 1024));
    while end > job.range.start.0 && job.snapshot.try_byte_to_utf16(ByteOffset(end)).is_err() {
        end -= 1;
    }
    let end = ByteOffset(end);
    job.snapshot
        .try_visit_chunks(job.range.start..end, &mut |s| text.push_str(s))
        .map_err(|e| format!("Spelling text is not ready: {e:?}"))?;
    let w = wide(&text);
    let enumerator = unsafe { checker.Check(PCWSTR(w.as_ptr())) }
        .map_err(|e| format!("Cannot check spelling: {e}"))?;
    let mut out = Vec::new();
    loop {
        let mut error = None;
        let status = unsafe { enumerator.Next(&mut error) };
        if status.0 == 1 {
            break;
        }
        status
            .ok()
            .map_err(|e| format!("Cannot read spelling results: {e}"))?;
        let Some(error) = error else { break };
        let start = unsafe { error.StartIndex() }.map_err(|e| e.to_string())? as usize;
        let end = start + unsafe { error.Length() }.map_err(|e| e.to_string())? as usize;
        let Some(local_start) = utf16_to_byte(&text, start) else {
            continue;
        };
        let Some(local_end) = utf16_to_byte(&text, end) else {
            continue;
        };
        let Some(word) = text.get(local_start..local_end) else {
            continue;
        };
        if !word_is_candidate(word) {
            continue;
        }
        let before = text.get(..local_start).unwrap_or("");
        let after = text.get(local_end..).unwrap_or("");
        let token_start = before
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        let token_end = local_end + after.find(char::is_whitespace).unwrap_or(after.len());
        let token = text.get(token_start..token_end).unwrap_or("");
        if token.contains("://")
            || token.contains("www.")
            || token.contains('@')
            || token.contains('.')
                && token
                    .trim_end_matches(|c: char| !c.is_alphanumeric())
                    .contains('.')
        {
            continue;
        }
        let suggestions = suggest(checker, word)?;
        out.push(Misspelling {
            range: ByteOffset(job.range.start.0 + local_start)
                ..ByteOffset(job.range.start.0 + local_end),
            word: word.to_owned(),
            suggestions,
        });
    }
    Ok(out)
}
fn suggest(checker: &ISpellChecker, word: &str) -> Result<Vec<String>, String> {
    let w = wide(word);
    let values = unsafe { checker.Suggest(PCWSTR(w.as_ptr())) }
        .map_err(|e| format!("Cannot get spelling suggestions: {e}"))?;
    let mut out = Vec::new();
    for _ in 0..5 {
        let mut item = [PWSTR::null()];
        let status = unsafe { values.Next(&mut item, None) };
        if status.0 == 1 {
            break;
        }
        status.ok().map_err(|e| e.to_string())?;
        if item[0].is_null() {
            break;
        }
        let word = unsafe { item[0].to_string() }.map_err(|e| e.to_string());
        unsafe { CoTaskMemFree(Some(item[0].0.cast())) };
        out.push(word?);
    }
    Ok(out)
}
fn utf16_to_byte(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += ch.len_utf16();
    }
    (units == offset).then_some(text.len())
}
