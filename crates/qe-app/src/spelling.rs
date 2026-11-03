//! Spelling. `ISpellChecker` loads only on the worker of a document with spelling on, so a run
//! with spelling off never loads it.
use crate::alerts::{AlertPurpose, AlertRequest};
use crate::app::{App, Effect};
use crate::commands::SPELL_ROW;
use qe_api::{ByteOffset, CommandId, Point, ViewContext};
use qe_docs::{DocId, WindowId};
use qe_spell::{Action, Checker, FindResult, FixItem, FixMenu, RawError, SpellService, SpellState};
use qe_ui::MenuItem;

/// The test run's checker: it knows one wrong word, and never loads the Windows one.
struct StubChecker;
impl Checker for StubChecker {
    fn check(&mut self, text: &[u16]) -> Result<Vec<RawError>, String> {
        let word: Vec<u16> = "speling".encode_utf16().collect();
        let mut found = Vec::new();
        let mut at = 0;
        while at + word.len() <= text.len() {
            if text[at..at + word.len()] == word[..] {
                found.push(RawError {
                    start: at,
                    length: word.len(),
                    action: Action::Suggest,
                });
                at += word.len();
            } else {
                at += 1;
            }
        }
        Ok(found)
    }
    fn suggest(&mut self, _word: &str, most: usize) -> Result<Vec<String>, String> {
        Ok(vec!["spelling".to_owned()].into_iter().take(most).collect())
    }
    fn add(&mut self, _word: &str) -> Result<(), String> {
        Ok(())
    }
}

impl App {
    /// The document's service, started if the view can have spelling.
    fn spell_service(&mut self, doc: DocId) -> Result<(), String> {
        if self.docs.get(&doc).is_some_and(|ui| ui.spell.is_some()) {
            return Ok(());
        }
        let wake = self.wake.clone();
        let stub = self.stub_spell;
        let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view(doc)) else {
            return Ok(());
        };
        let service = if stub {
            if let Some(off) = qe_spell::why_off(view, true) {
                return Err(off.to_string());
            }
            SpellService::start_with(
                view,
                Box::new(|| Ok(Box::new(StubChecker) as Box<dyn Checker>)),
                wake,
            )
        } else {
            SpellService::start(view, true, None, wake).map_err(|off| off.to_string())?
        };
        ui.spell = Some(service);
        Ok(())
    }

    /// After an edit, a scroll or a new layout.
    pub fn spell_refresh(&mut self, doc: DocId) {
        let wanted = self.docs.get(&doc).is_some_and(|ui| ui.spell_on);
        let shown = self
            .store
            .window_of(doc)
            .and_then(|window| self.windows.get(&window))
            .is_some_and(|win| win.shown == Some(doc));
        if !wanted || !shown {
            return;
        }
        if let Err(off) = self.spell_service(doc) {
            // A large or read-only document says why in its title, so this is no error.
            eprintln!("{off}");
            return;
        }
        let mut ctx = ViewContext::default();
        // A failure comes back once from `poll`, in plain words, and is shown there.
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
            && let Some(service) = &mut ui.spell
        {
            ui.spell_waiting = service.refresh(view, &mut ctx) == SpellState::WaitingForLayout;
        }
        if let Some(window) = self.store.window_of(doc) {
            self.apply(window, ctx.requests);
        }
    }

    /// The workers' answers, for documents that have a service.
    pub fn spell_poll(&mut self) {
        let docs: Vec<DocId> = self
            .docs
            .iter()
            .filter(|(_, ui)| ui.spell.is_some())
            .map(|(doc, _)| *doc)
            .collect();
        for doc in docs {
            let mut ctx = ViewContext::default();
            let answer = match (self.docs.get_mut(&doc), self.store.view_mut(doc)) {
                (Some(ui), Some(view)) => match &mut ui.spell {
                    Some(service) => service.poll(view, &mut ctx),
                    None => continue,
                },
                _ => continue,
            };
            if let Some(window) = self.store.window_of(doc) {
                self.apply(window, ctx.requests);
            }
            match answer {
                Ok(poll) => {
                    if let Some(problem) = poll.problem {
                        self.note(problem);
                    }
                    if let Some(found) = poll.found {
                        self.spell_found(doc, found);
                    }
                    if poll.marks_changed {
                        self.try_correct(doc);
                        if self.docs.get(&doc).is_some_and(|ui| ui.spell_on) {
                            self.spell_refresh(doc);
                        }
                    }
                }
                Err(problem) => self.note(problem),
            }
        }
    }

    pub fn toggle_spelling(&mut self, doc: DocId) {
        let Some(ui) = self.docs.get_mut(&doc) else {
            return;
        };
        ui.spell_on = !ui.spell_on;
        let on = ui.spell_on;
        if let Err(e) = self
            .settings
            .set_bool(qe_api::SettingKey::CheckSpellingWhileTyping, on)
        {
            self.note(format!("The setting could not change: {e}"));
        }
        if on {
            self.spell_refresh(doc);
        } else {
            self.spell_stop(doc);
        }
    }

    fn spell_stop(&mut self, doc: DocId) {
        let mut ctx = ViewContext::default();
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
            && let Some(service) = ui.spell.take()
        {
            service.stop(view, &mut ctx);
        }
        if let Some(window) = self.store.window_of(doc) {
            self.apply(window, ctx.requests);
        }
    }

    pub fn toggle_correct(&mut self, doc: DocId) {
        let Some(ui) = self.docs.get_mut(&doc) else {
            return;
        };
        ui.correct_on = !ui.correct_on;
        let on = ui.correct_on;
        if on && !ui.spell_on {
            ui.spell_on = true;
        }
        if let Err(e) = self
            .settings
            .set_bool(qe_api::SettingKey::CorrectSpellingAutomatically, on)
        {
            self.note(format!("The setting could not change: {e}"));
        }
        self.spell_refresh(doc);
    }

    /// Check Document Now, and Show Spelling, which asks about each word it finds.
    pub fn spell_find(&mut self, _window: WindowId, doc: DocId, walk: bool) {
        if let Err(problem) = self.spell_service(doc) {
            self.note(problem);
            return;
        }
        let result = match (self.docs.get_mut(&doc), self.store.view(doc)) {
            (Some(ui), Some(view)) => {
                ui.spelling_walk = walk;
                match &mut ui.spell {
                    Some(service) => service.find_next(view),
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        };
        if let Err(problem) = result {
            self.note(problem);
        }
    }

    fn spell_found(&mut self, doc: DocId, found: FindResult) {
        let walk = self.docs.get(&doc).is_some_and(|ui| ui.spelling_walk);
        match found {
            FindResult::Found(word) => {
                let mut ctx = ViewContext::default();
                if let Some(view) = self.store.view_mut(doc) {
                    // In order with keys that wait, and brought into view.
                    let select = qe_editor::HostCommand::Select {
                        range: word.range.clone(),
                        reveal: true,
                    };
                    view.run_host(select, &mut ctx);
                }
                if let Some(window) = self.store.window_of(doc) {
                    self.apply(window, ctx.requests);
                    self.after_view_call(window, doc);
                }
                if walk {
                    let suggestion = word.suggestions.first().cloned();
                    let mut buttons = Vec::new();
                    if let Some(better) = &suggestion {
                        buttons.push(format!("Change to {better}"));
                    }
                    buttons.push("Ignore".to_owned());
                    buttons.push("Stop".to_owned());
                    self.show_alert(AlertRequest {
                        title: "Show Spelling".to_owned(),
                        message: format!("\"{}\" is not in the dictionary.", word.word),
                        buttons,
                        purpose: AlertPurpose::SpellWord { doc, suggestion },
                        field: None,
                        owner: None,
                    });
                }
            }
            FindResult::NoneLeft => {
                if walk {
                    self.error_alert("Show Spelling", "No misspelled words were found.");
                } else {
                    self.effects.push(Effect::Beep);
                }
            }
            FindResult::TextChanged => {
                if walk {
                    self.note("The text changed, so the spelling check stopped.".to_owned());
                }
            }
        }
    }

    /// The answer to Show Spelling. Change and Ignore go on to the next word.
    pub fn spell_answer(&mut self, doc: DocId, button: Option<usize>, suggestion: Option<String>) {
        let Some(view) = self.store.view(doc) else {
            return;
        };
        let at = view.selection().start;
        let (change, ignore) = match (button, &suggestion) {
            (Some(0), Some(_)) => (true, false),
            (Some(1), Some(_)) | (Some(0), None) => (false, true),
            _ => (false, false),
        };
        if !change && !ignore {
            if let Some(ui) = self.docs.get_mut(&doc) {
                ui.spelling_walk = false;
            }
            return;
        }
        let mut ctx = ViewContext::default();
        let mut problem = None;
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
            && let Some(service) = &mut ui.spell
            && let Some(menu) = service.menu_at(view, at)
        {
            let done = match (&suggestion, change) {
                (Some(better), true) => service.apply_fix(view, &menu, better, &mut ctx),
                _ => service.ignore(view, &menu, &mut ctx),
            };
            problem = done.err();
        }
        if let Some(window) = self.store.window_of(doc) {
            self.apply(window, ctx.requests);
            self.after_view_call(window, doc);
            if let Some(problem) = problem {
                self.note(problem);
            }
            self.spell_find(window, doc, true);
        }
    }

    /// Correct Spelling Automatically: a typed break after a word marks a place to look at.
    pub fn note_typed(&mut self, doc: DocId, typed: char) {
        if typed.is_alphanumeric() {
            return;
        }
        let caret = self.store.view(doc).map(|view| view.caret());
        if let (Some(ui), Some(caret)) = (self.docs.get_mut(&doc), caret)
            && ui.correct_on
        {
            ui.correct_at = Some(caret);
        }
    }

    /// The marks arrived. If one ends just before the typed break, its fix goes in.
    fn try_correct(&mut self, doc: DocId) {
        let Some(view) = self.store.view(doc) else {
            return;
        };
        let caret = view.caret();
        let Some(ui) = self.docs.get(&doc) else {
            return;
        };
        let Some(at) = ui.correct_at else {
            return;
        };
        if at != caret {
            return;
        }
        let word_end = ByteOffset(caret.0.saturating_sub(1));
        let Some(mark) = ui.spell.as_ref().and_then(|service| {
            service
                .marks()
                .iter()
                .find(|mark| mark.range.end == word_end)
        }) else {
            return;
        };
        let (start, grown) = match mark.suggestions.first() {
            Some(better) => (
                mark.range.start,
                better.len() as i64 - (mark.range.end.0 - mark.range.start.0) as i64,
            ),
            None => return,
        };
        let better = mark.suggestions[0].clone();
        let mut ctx = ViewContext::default();
        let mut problem = None;
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc)) {
            ui.correct_at = None;
            if let Some(service) = &mut ui.spell
                && let Some(menu) = service.menu_at(view, start)
            {
                match service.apply_fix(view, &menu, &better, &mut ctx) {
                    Ok(()) => {
                        let new_caret = ByteOffset((caret.0 as i64 + grown).max(0) as usize);
                        view.select(new_caret..new_caret, &mut ctx);
                    }
                    Err(e) => problem = Some(e),
                }
            }
        }
        if let Some(window) = self.store.window_of(doc) {
            self.apply(window, ctx.requests);
            self.after_view_call(window, doc);
        }
        if let Some(problem) = problem {
            self.note(problem);
        }
    }

    /// The rows for a right click on a misspelled word.
    pub fn spell_menu_rows(&mut self, doc: DocId, at: Point) -> Vec<MenuItem> {
        let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view(doc)) else {
            return Vec::new();
        };
        let Some(service) = &mut ui.spell else {
            return Vec::new();
        };
        let Some(menu) = service.menu_at_point(view, at) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for item in menu.items() {
            // A row needs a command, and this one's data says what it really is.
            let row = |label: &str, data: String| {
                let mut row = MenuItem::command(label, CommandId::CheckDocumentSpelling);
                row.data = Some(data);
                row
            };
            rows.push(match item {
                FixItem::Suggestion(text) => row(&text, format!("{SPELL_ROW}fix:{text}")),
                FixItem::NoSuggestions => {
                    let mut gray = row("No Guesses Found", format!("{SPELL_ROW}none"));
                    gray.enabled = false;
                    gray
                }
                FixItem::Ignore => row("Ignore Spelling", format!("{SPELL_ROW}ignore")),
                FixItem::AddToDictionary => row("Learn Spelling", format!("{SPELL_ROW}learn")),
            });
        }
        rows.push(MenuItem::separator());
        ui.found = Some(menu);
        rows
    }

    /// A row of that menu was chosen.
    pub fn spell_menu_row(&mut self, window: WindowId, doc: DocId, row: &str) {
        let mut ctx = ViewContext::default();
        let mut problem = None;
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
            && let (Some(service), Some(menu)) = (&mut ui.spell, ui.found.take())
        {
            let menu: FixMenu = menu;
            let done = if let Some(text) = row.strip_prefix("fix:") {
                service.apply_fix(view, &menu, text, &mut ctx)
            } else if row == "ignore" {
                service.ignore(view, &menu, &mut ctx)
            } else if row == "learn" {
                service.add_to_dictionary(view, &menu, &mut ctx)
            } else {
                Ok(())
            };
            problem = done.err();
        }
        self.apply(window, ctx.requests);
        self.after_view_call(window, doc);
        if let Some(problem) = problem {
            self.note(problem);
        }
    }
}
