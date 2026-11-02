//! Shared by the test files. Each test binary uses only some of this.
#![allow(dead_code)]
use qe_api::{Dip, Rect, View, ViewContext};
use qe_editor::{DecorationKind, EditorView};
use qe_layout::DirectWriteLayout;
use qe_spell::{Action, Checker, RawError, SpellService};
use std::{
    collections::HashSet,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub fn area(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}
pub fn settle(view: &mut EditorView) {
    let mut ctx = ViewContext::default();
    view.poll_layout(&mut ctx);
    for _ in 0..5000 {
        if !view.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
        view.poll_layout(&mut ctx);
    }
    panic!("the view did not settle");
}
pub fn laid_out(text: &str) -> EditorView {
    let mut view = EditorView::from_text(text).unwrap();
    view.layout(area(800.0, 400.0), &DirectWriteLayout::new().unwrap());
    settle(&mut view);
    view
}
pub fn marks(view: &EditorView) -> Vec<(usize, usize)> {
    view.decorations(DecorationKind::SpellingUnderline)
        .iter()
        .map(|r| (r.start.0, r.end.0))
        .collect()
}
pub fn no_wake() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}
/// Polls until the worker has nothing left. Fails after 30 seconds.
pub fn finish(service: &mut SpellService, view: &mut EditorView, ctx: &mut ViewContext) {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        service.poll(view, ctx).unwrap();
        if !service.is_working() {
            return;
        }
        assert!(Instant::now() < until, "the worker did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// What a fake checker did. The test keeps a copy.
#[derive(Default)]
pub struct FakeLog {
    pub checked: Mutex<Vec<String>>,
    pub suggested: AtomicUsize,
    pub added: Mutex<Vec<String>>,
    /// Set when the worker has let go of the checker, which it does as it ends.
    pub dropped: std::sync::atomic::AtomicBool,
    /// How many checks have begun, counted before each waits at the gate.
    pub entered: AtomicUsize,
    /// While true, `check` waits. It shows that the UI thread does not wait with it.
    pub gate: Mutex<bool>,
    pub gate_changed: Condvar,
    pub fail_next: Mutex<bool>,
}
impl FakeLog {
    pub fn hold(&self) {
        *self.gate.lock().unwrap() = true;
    }
    pub fn release(&self) {
        *self.gate.lock().unwrap() = false;
        self.gate_changed.notify_all();
    }
    pub fn checked_text(&self) -> Vec<String> {
        self.checked.lock().unwrap().clone()
    }
    /// Waits until the worker is inside a check, held at the gate.
    pub fn wait_until_held(&self) {
        let until = Instant::now() + Duration::from_secs(30);
        while self.entered.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < until, "the worker never began");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn clear(&self) {
        self.checked.lock().unwrap().clear();
    }
}

/// Marks the words in its list, and asks for the action the word implies: a word that
/// starts with "dup" is a doubled word, and one that starts with "fix" has one sure fix.
pub struct FakeChecker {
    pub bad: HashSet<String>,
    pub log: Arc<FakeLog>,
}
impl Checker for FakeChecker {
    fn check(&mut self, text: &[u16]) -> Result<Vec<RawError>, String> {
        self.log.entered.fetch_add(1, Ordering::Relaxed);
        let mut gate = self.log.gate.lock().unwrap();
        while *gate {
            gate = self.log.gate_changed.wait(gate).unwrap();
        }
        drop(gate);
        if std::mem::take(&mut *self.log.fail_next.lock().unwrap()) {
            return Err("The fake checker failed.".into());
        }
        assert_eq!(text.last(), Some(&0), "the checker needs a 0 at the end");
        let text = &text[..text.len() - 1];
        self.log
            .checked
            .lock()
            .unwrap()
            .push(String::from_utf16_lossy(text));
        let mut out = Vec::new();
        let (mut word, mut start, mut at) = (String::new(), 0, 0);
        let decoded: Vec<char> = char::decode_utf16(text.iter().copied())
            .map(|c| c.unwrap_or('\u{fffd}'))
            .collect();
        for c in decoded.iter().chain(std::iter::once(&' ')) {
            if c.is_alphabetic() || ('\u{300}'..='\u{36f}').contains(c) {
                if word.is_empty() {
                    start = at;
                }
                word.push(*c);
            } else if !word.is_empty() {
                if self.bad.contains(&word) || (self.bad.contains("qqq") && word.starts_with("qqq"))
                {
                    let action = if word.starts_with("dup") {
                        Action::Delete
                    } else if word.starts_with("fix") {
                        Action::Replace("fixed".into())
                    } else {
                        Action::Suggest
                    };
                    out.push(RawError {
                        start,
                        length: word.encode_utf16().count(),
                        action,
                    });
                }
                word.clear();
            }
            at += c.len_utf16();
        }
        Ok(out)
    }
    fn suggest(&mut self, word: &str, most: usize) -> Result<Vec<String>, String> {
        self.log.suggested.fetch_add(1, Ordering::Relaxed);
        Ok((0..8).map(|i| format!("{word}-{i}")).take(most).collect())
    }
    fn add(&mut self, word: &str) -> Result<(), String> {
        self.log.added.lock().unwrap().push(word.to_owned());
        Ok(())
    }
}
impl Drop for FakeChecker {
    fn drop(&mut self) {
        self.log.dropped.store(true, Ordering::Relaxed);
    }
}

pub fn fake_service(view: &EditorView, bad: &[&str]) -> (SpellService, Arc<FakeLog>) {
    let log = Arc::new(FakeLog::default());
    let checker = FakeChecker {
        bad: bad.iter().map(|w| (*w).to_owned()).collect(),
        log: log.clone(),
    };
    let service = SpellService::start_with(
        view,
        Box::new(move || Ok(Box::new(checker) as Box<dyn Checker>)),
        no_wake(),
    );
    (service, log)
}

pub fn draw(view: &EditorView, kind: qe_api::ThemeKind) -> qe_api::DisplayList {
    let mut list = qe_api::DisplayList::default();
    view.draw(
        &mut list,
        &qe_api::Theme::new(kind),
        &DirectWriteLayout::new().unwrap(),
    );
    list
}
