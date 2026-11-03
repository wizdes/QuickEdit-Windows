//! What each incoming item does, and what each event from the store shows.
use crate::alerts::{AlertPurpose, AlertRequest};
use crate::app::{App, Effect, Incoming};
use crate::platform::Signal;
use qe_api::{CommandId, InputEvent, InputKind, ViewContext};
use qe_docs::handoff::HandoffEvent;
use qe_docs::{Answer, DocEvent, DocId, OpenTarget, WindowId};
use qe_settings::SettingChange;
use qe_shell::{ShellEvent, VIEW_TIMER};
use qe_views::system_reduce_motion;
use std::time::Instant;

/// A button's text for a question's choice.
pub fn answer_label(answer: Answer) -> &'static str {
    match answer {
        Answer::Save => "Save",
        Answer::DontSave => "Don't Save",
        Answer::Cancel => "Cancel",
        Answer::Duplicate => "Duplicate",
        Answer::Overwrite => "Overwrite",
        Answer::SaveAnyway => "Save Anyway",
        Answer::Revert => "Revert",
        Answer::Unlock => "Unlock",
        Answer::ChooseEncoding => "Choose Encoding",
    }
}

impl App {
    pub fn handle(&mut self, item: Incoming) {
        match item {
            Incoming::Shell(window, event) => self.shell_event(window, event),
            Incoming::Signal(Signal::Wake) => self.on_wake(),
            Incoming::Signal(Signal::Deadline) => {
                self.deadline_set = None;
                self.store.poll(Instant::now());
            }
            Incoming::Setting(change) => self.setting_changed(change),
            Incoming::Activated(window) => self.store.window_activated(window),
            Incoming::AppActivated => self.store.app_activated(),
            Incoming::FrameChanged(window, by_user) => self.frame_changed(window, by_user),
            Incoming::Dropped(window, paths) => {
                if let Err(e) = self
                    .store
                    .open_paths(&paths, OpenTarget::Window(window), None)
                {
                    self.error_alert("The files could not open", &e.message);
                }
            }
            Incoming::EndSession => {
                self.begin_quit();
                self.store.wait_for_quit(std::time::Duration::from_secs(4));
                self.effects.push(Effect::Exit);
            }
            Incoming::SystemSetting => {
                let reduce = system_reduce_motion();
                for ui in self.docs.values_mut() {
                    ui.frame.set_reduce_motion(reduce);
                }
            }
            Incoming::TabDrop { doc, at } => self.tab_dropped(doc, at),
            Incoming::Alert(done) => self.alert_done(done),
            Incoming::Panel(job, choice) => self.panel_done(job, choice),
            Incoming::Run(window, command) => self.run_command(window, command, None),
            Incoming::Quit => self.begin_quit(),
            Incoming::Note(text) => {
                if !text.is_empty() {
                    self.note(text);
                }
            }
            Incoming::Action(window, id, action) => self.widget_action(window, id, action),
        }
    }

    fn shell_event(&mut self, window: WindowId, event: ShellEvent) {
        match event {
            ShellEvent::NewTab => {
                if let Err(e) = self.store.new_tab(window) {
                    self.error_alert("A tab could not open", &e.message);
                }
            }
            ShellEvent::CloseTabRequested(tab) => {
                if let Some(doc) = self.doc_of_tab(window, tab) {
                    self.store.request_close(doc);
                }
            }
            ShellEvent::CloseWindowRequested => self.store.request_close_window(window),
            ShellEvent::SelectTab(tab) => {
                if let Some(doc) = self.doc_of_tab(window, tab)
                    && self.store.active_tab(window) != Some(doc)
                {
                    self.store.set_active_tab(window, doc);
                }
            }
            ShellEvent::ReorderTab { tab, to } => {
                if let Some(doc) = self.doc_of_tab(window, tab) {
                    self.store.move_tab(doc, window, to);
                }
            }
            ShellEvent::OpenMenuGroup(index, anchor) => {
                let model = self.menu_model(window, index);
                if let Some(win) = self.windows.get(&window)
                    && let Err(e) = win.handle.open_menu(model, anchor)
                {
                    self.note(format!("A menu could not open: {e}"));
                }
            }
            ShellEvent::TearOffTab { tab, at: _ } => self.begin_tab_drag(window, tab),
            ShellEvent::CaptionCommand(_) => {}
            ShellEvent::FocusContent => {}
            ShellEvent::ContentInput(input) => self.content_input(window, &input),
            ShellEvent::ContentCaptureLost => {
                self.with_active(window, |app, doc, ctx| {
                    if let (Some(ui), Some(view)) =
                        (app.docs.get_mut(&doc), app.store.view_mut(doc))
                    {
                        ui.frame.capture_lost(view, ctx);
                    }
                });
            }
            ShellEvent::ContentPointerLeft => {
                self.with_active(window, |app, doc, ctx| {
                    if let Some(ui) = app.docs.get_mut(&doc) {
                        ui.frame.pointer_left(ctx);
                    }
                });
            }
            ShellEvent::Timer(VIEW_TIMER) => self.view_tick(window, true),
            ShellEvent::Timer(_) => {}
            ShellEvent::Wake => self.view_tick(window, false),
            ShellEvent::CancelComposition => {
                if let Some(doc) = self.store.active_tab(window) {
                    self.effects.push(Effect::ImeCancel(doc));
                }
            }
            ShellEvent::ContextMenu(at) => self.context_menu(window, at),
            ShellEvent::RunCommand(command) => {
                let data = self
                    .windows
                    .get(&window)
                    .and_then(|win| win.handle.take_menu_chosen_data().ok().flatten());
                match data {
                    Some(path) if command == CommandId::OpenRecent => {
                        self.open_recent(window, &path);
                    }
                    other => self.run_command(window, command, other),
                }
            }
            ShellEvent::WidgetsChanged => self.report_widgets(window),
            ShellEvent::WindowDestroyed => {}
            ShellEvent::Problem(problem) => self.note(problem.to_string()),
        }
    }

    fn doc_of_tab(&self, window: WindowId, tab: qe_shell::TabId) -> Option<DocId> {
        let win = self.windows.get(&window)?;
        win.tabs
            .iter()
            .find(|(_, id)| *id == tab)
            .map(|(doc, _)| *doc)
    }

    /// Runs `body` for the document on show, then carries out what it asked.
    pub fn with_active(
        &mut self,
        window: WindowId,
        body: impl FnOnce(&mut Self, DocId, &mut ViewContext),
    ) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        let mut ctx = ViewContext::default();
        body(self, doc, &mut ctx);
        self.apply(window, ctx.requests);
    }

    /// One input event, in the order the wiring lists give.
    pub fn content_input(&mut self, window: WindowId, input: &InputEvent) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        match input.kind {
            InputKind::Resize { .. } => {
                self.relayout(window);
                self.frame_changed(window, false);
                return;
            }
            InputKind::Focus => {
                self.effects.push(Effect::ImeFocus(window, doc));
            }
            _ => {}
        }
        // A question about this document is open: what it decides must not move underneath it.
        if self.asking.contains(&doc)
            && matches!(
                input.kind,
                InputKind::Char(_)
                    | InputKind::KeyDown { .. }
                    | InputKind::ImeStart
                    | InputKind::ImeUpdate { .. }
                    | InputKind::ImeEnd { .. }
            )
        {
            return;
        }
        let is_dpi = matches!(input.kind, InputKind::DpiChange { .. });
        if let InputKind::DpiChange { dpi } = input.kind
            && let Some(win) = self.windows.get(&window)
        {
            win.dpi.set(dpi);
        }
        let started = Instant::now();
        let is_key = matches!(input.kind, InputKind::KeyDown { .. } | InputKind::Char(_));
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        let mut command: Option<CommandId> = None;
        let (Some(win), Some(view)) = (self.windows.get_mut(&window), self.store.view_mut(doc))
        else {
            return;
        };
        let taken = win
            .tools
            .handle_input(input, view, measure.as_ref(), &mut ctx);
        if taken == qe_api::Handled::No {
            if let InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } = input.kind
                && let Some(found) = self.settings.command_for_press(virtual_key, modifiers)
            {
                command = Some(found);
            } else if win.tools.event_for_editor(input)
                && let Some(ui) = self.docs.get_mut(&doc)
            {
                ui.frame.handle(view, input, measure.as_ref(), &mut ctx);
            }
        }
        if input.kind == InputKind::Blur
            && let Err(e) = win.handle.kill_timer(VIEW_TIMER)
        {
            eprintln!("Stopping the caret's timer: {e}");
        }
        let repaint = ctx
            .requests
            .iter()
            .any(|r| matches!(r, qe_api::ViewRequest::Invalidate(_)));
        self.apply(window, ctx.requests);
        if let Some(command) = command {
            self.run_command(window, command, None);
        }
        self.after_tools(window, doc);
        if is_dpi {
            self.relayout(window);
        }
        if let InputKind::Char(typed) = input.kind {
            self.note_typed(doc, typed);
        }
        self.after_view_call(window, doc);
        if is_key && (repaint || command.is_none()) && self.bench.is_some() {
            self.timing.note_key(started);
        }
    }

    /// What ToolsHost asks after every call into it.
    pub fn after_tools(&mut self, window: WindowId, doc: DocId) {
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        let layout_changed = win.tools.take_layout_changed();
        let beep = win.tools.take_beep();
        if let Some(ui) = self.docs.get(&doc) {
            ui.subs.set(win.tools.substitutions());
        }
        // What a sighted user reads as it changes, a screen reader hears.
        let said = {
            let tools = &win.tools;
            let (count, done) = tools.match_count();
            let found = (tools.is_bar_open() && done).then(|| format!("{count} found"));
            let message = tools.alert_text().map(str::to_owned);
            (found, message)
        };
        if said != win.said {
            let (found, message) = &said;
            let changed = if message != &win.said.1 {
                message.clone()
            } else {
                found.clone()
            };
            win.said = said;
            if let Some(text) = changed
                && win.slot.current().is_some()
            {
                self.effects.push(Effect::Announce(window, text));
            }
        }
        if layout_changed {
            self.relayout(window);
        }
        if beep {
            self.effects.push(Effect::Beep);
        }
        self.report_widgets(window);
    }

    /// A screen reader's link asks this app questions as it reports, so a report waits until
    /// `App` is free. With no client ever, nothing is queued.
    pub fn report_widgets(&mut self, window: WindowId) {
        if self
            .windows
            .get(&window)
            .is_some_and(|win| win.slot.current().is_some())
            && !self
                .effects
                .iter()
                .any(|e| matches!(e, Effect::ReportWidgets(w) if *w == window))
        {
            self.effects.push(Effect::ReportWidgets(window));
        }
    }
    pub fn report_input(&mut self, window: WindowId) {
        if self
            .windows
            .get(&window)
            .is_some_and(|win| win.slot.current().is_some())
            && !self
                .effects
                .iter()
                .any(|e| matches!(e, Effect::ReportInput(w) if *w == window))
        {
            self.effects.push(Effect::ReportInput(window));
        }
    }

    /// Tells everyone who follows the text that a view call may have changed it.
    pub fn after_view_call(&mut self, window: WindowId, doc: DocId) {
        let Some(view) = self.store.view(doc) else {
            return;
        };
        let (revision, place) = (view.revision(), (view.selection(), view.scroll_offset()));
        let bytes = self.store.byte_count(doc);
        let Some(ui) = self.docs.get_mut(&doc) else {
            return;
        };
        ui.bytes.set(bytes);
        let text_changed = ui.seen_revision != revision;
        let place_changed = ui.seen_place != place;
        ui.seen_revision = revision;
        ui.seen_place = place;
        if text_changed {
            self.store.note_view_changed(doc);
        }
        if place_changed {
            self.store.note_place_changed(doc);
        }
        if text_changed || place_changed {
            self.report_input(window);
            self.spell_refresh(doc);
            if self.ime.started() {
                self.effects.push(Effect::ImeSync(doc));
            }
        }
    }

    /// One look at this window's view, as a wake gives.
    pub fn tick_window(&mut self, window: WindowId) {
        self.view_tick(window, false);
    }

    /// A view timer, or a wake for this window.
    fn view_tick(&mut self, window: WindowId, timer: bool) {
        // The document on show, whose state the window's tools hold. The store's active tab
        // can be ahead of it until `TabsChanged` is read.
        let Some(doc) = self.windows.get(&window).and_then(|win| win.shown) else {
            return;
        };
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        let (Some(win), Some(view), Some(ui)) = (
            self.windows.get_mut(&window),
            self.store.view_mut(doc),
            self.docs.get_mut(&doc),
        ) else {
            return;
        };
        if timer {
            view.on_timer(&mut ctx);
        } else {
            view.poll_layout(&mut ctx);
        }
        ui.frame.sync(view, measure.as_ref(), &mut ctx);
        win.tools.poll(view, measure.as_ref(), &mut ctx);
        self.apply(window, ctx.requests);
        self.after_tools(window, doc);
        self.after_view_call(window, doc);
        if self.docs.get(&doc).is_some_and(|ui| ui.spell_waiting) {
            self.spell_refresh(doc);
        }
    }

    /// Work has finished somewhere. A background tab has no work of its own to look at.
    fn on_wake(&mut self) {
        self.channel_inbox();
        // A tab change from there must reach the window before the window's work below.
        self.after_event();
        self.settings.poll_writes();
        let server = self.server.take();
        if let Some(server) = &server {
            for request in server.take_requests() {
                for error in &request.refused {
                    self.error_alert("A file could not open", &error.message);
                }
                if request.paths.is_empty() {
                    if request.refused.is_empty() && self.restored {
                        self.bring_forward();
                    }
                } else if let Err(e) =
                    self.store
                        .open_paths(&request.paths, OpenTarget::LastUsedWindow, None)
                {
                    self.error_alert("The files could not open", &e.message);
                }
            }
            for event in server.take_events() {
                match event {
                    HandoffEvent::ServerRestarted { reason } => {
                        self.note(format!(
                            "Files from other launches stopped for a moment: {reason}"
                        ));
                    }
                    HandoffEvent::ServerStopped { reason } => {
                        self.error_alert(
                            "Files from other launches will open in their own windows",
                            &reason,
                        );
                    }
                }
            }
        }
        self.server = server;
        self.store.poll(Instant::now());
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for window in windows {
            self.view_tick(window, false);
        }
        self.spell_poll();
        self.pdf_poll();
        self.bench_tick();
    }

    fn bring_forward(&mut self) {
        if self.hidden {
            return;
        }
        if let Some(window) = self.store.last_used_window()
            && let Some(win) = self.windows.get(&window)
        {
            // SAFETY: a window of this thread. Windows may refuse, and the taskbar then flashes.
            let _ =
                unsafe { windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(win.hwnd) };
        }
    }

    fn frame_changed(&mut self, window: WindowId, by_user: bool) {
        let Some(win) = self.windows.get(&window) else {
            return;
        };
        let full = matches!(win.handle.frame_mode(), Ok(qe_shell::FrameMode::FullScreen));
        if let Some((frame, show)) = crate::ui::read_placement(win.hwnd) {
            let show = if full {
                qe_docs::ShowState::FullScreen
            } else {
                show
            };
            self.store.set_window_frame(window, frame, show, by_user);
        }
    }

    fn setting_changed(&mut self, change: SettingChange) {
        match change {
            SettingChange::Value(qe_api::SettingKey::ShowLineNumbers) => {
                let on = self
                    .settings
                    .get_bool(qe_api::SettingKey::ShowLineNumbers)
                    .unwrap_or(false);
                let measure = self.measure.clone();
                let docs: Vec<DocId> = self.docs.keys().copied().collect();
                for doc in docs {
                    let mut ctx = ViewContext::default();
                    if let (Some(ui), Some(view)) =
                        (self.docs.get_mut(&doc), self.store.view_mut(doc))
                    {
                        ui.frame
                            .set_line_numbers(on, view, measure.as_ref(), &mut ctx);
                    }
                    if let Some(window) = self.store.window_of(doc) {
                        self.apply(window, ctx.requests);
                    }
                }
            }
            SettingChange::Value(qe_api::SettingKey::TabWidth) => {
                let width = self
                    .settings
                    .get_integer(qe_api::SettingKey::TabWidth)
                    .unwrap_or(8)
                    .max(1) as usize;
                self.for_each_prepared(|frame, view, measure, ctx| {
                    frame.set_tab_width(width, view, measure, ctx);
                });
            }
            SettingChange::Value(qe_api::SettingKey::PaperSize) => {
                let paper = crate::docs::paper_from_settings(&self.settings);
                self.for_each_prepared(|frame, view, measure, ctx| {
                    frame.set_paper(paper, view, measure, ctx);
                });
            }
            SettingChange::WriteFailed(message) => self.note(message),
            SettingChange::FileProblem(Some(message)) => self.note(message),
            // Menus read the keys each time they open, and each press looks its key up.
            SettingChange::Keys
            | SettingChange::Value(_)
            | SettingChange::FileProblem(None)
            | SettingChange::WriteRecovered => {}
        }
    }

    /// Runs `body` on every document that has shown, and carries out what it asks.
    fn for_each_prepared(
        &mut self,
        mut body: impl FnMut(
            &mut qe_views::ViewFrame,
            &mut qe_editor::EditorView,
            &dyn qe_api::TextMeasurer,
            &mut ViewContext,
        ),
    ) {
        let measure = self.measure.clone();
        let docs: Vec<DocId> = self
            .docs
            .iter()
            .filter(|(_, ui)| ui.prepared)
            .map(|(doc, _)| *doc)
            .collect();
        for doc in docs {
            let mut ctx = ViewContext::default();
            if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc)) {
                body(&mut ui.frame, view, measure.as_ref(), &mut ctx);
            }
            if let Some(window) = self.store.window_of(doc) {
                self.apply(window, ctx.requests);
            }
        }
    }

    /// Shows a note in the window in use, at once. It stays until the next edit there.
    pub fn note(&mut self, text: String) {
        eprintln!("{text}");
        self.notes.push(text.clone());
        let doc = self
            .store
            .last_used_window()
            .and_then(|window| self.store.active_tab(window));
        match doc.and_then(|doc| self.store.view_mut(doc)) {
            Some(view) => view.show_warning(text),
            None => self.waiting_notes.push(text),
        }
    }

    pub fn error_alert(&mut self, title: &str, message: &str) {
        self.show_alert(AlertRequest {
            title: title.to_owned(),
            message: message.to_owned(),
            buttons: vec!["OK".to_owned()],
            purpose: AlertPurpose::Note,
            field: None,
            owner: None,
        });
    }

    pub fn show_alert(&mut self, mut request: AlertRequest) {
        if request.owner.is_none() {
            request.owner = self
                .store
                .last_used_window()
                .and_then(|window| self.windows.get(&window))
                .map(|win| win.hwnd);
        }
        self.effects.push(Effect::Alert(request));
    }

    pub fn begin_quit(&mut self) {
        // A composition is not in the text yet, so it goes in before the text is kept.
        if self.ime.any_composing() {
            self.effects.push(Effect::ImeCommitThenQuit);
            return;
        }
        // Cancel in a question ends a quit and the app stays open, so a later quit starts anew.
        if !self.ready_to_quit {
            self.store.begin_quit();
        }
    }

    /// After each item: the store's events, its view requests and its one timer.
    pub fn after_event(&mut self) {
        loop {
            let events = self.store.take_events();
            let requests = self.store.take_view_requests();
            if events.is_empty() && requests.is_empty() {
                break;
            }
            for (doc, request) in requests {
                if let Some(window) = self.store.window_of(doc)
                    && self
                        .windows
                        .get(&window)
                        .is_some_and(|win| win.shown == Some(doc))
                {
                    self.apply(window, vec![request]);
                }
            }
            for event in events {
                self.store_event(event);
            }
        }
        self.sync_questions();
        if self.ime.owed.take() {
            self.effects.push(Effect::ImeSyncAll);
        }
        let deadline = self.store.next_deadline();
        if deadline != self.deadline_set {
            self.deadline_set = deadline;
            let wait = deadline.map(|at| {
                let left = at.saturating_duration_since(Instant::now());
                u32::try_from(left.as_millis().saturating_add(1)).unwrap_or(u32::MAX)
            });
            if let Err(e) = self.wake_window.set_deadline(wait) {
                self.note(format!(
                    "A timer could not start, so autosave may wait: {e}"
                ));
            }
        }
        if self.smoke && !self.smoke_done && self.timing.first_frame.get().is_some() {
            let loading = self
                .store
                .all_documents()
                .iter()
                .any(|doc| self.store.is_loading(*doc));
            if !loading && !self.launch_has_waiting_text() {
                self.smoke_done = true;
                self.effects.push(Effect::FinishSmoke);
                self.begin_quit();
            }
        }
        self.bench_tick();
    }

    /// A smoke run with a file waits until a frame holds that file's text.
    fn launch_has_waiting_text(&self) -> bool {
        let any_text = self
            .store
            .all_documents()
            .iter()
            .any(|doc| self.store.byte_count(*doc) > 0);
        any_text && self.timing.first_text.get().is_none()
    }

    fn store_event(&mut self, event: DocEvent) {
        match event {
            DocEvent::WindowAdded(window) => {
                if let Err(e) = self.create_window(window) {
                    self.error_alert("A window could not open", &e);
                }
                self.sync_tabs(window);
                self.relayout(window);
                // A window has no keyboard until Windows says so, and with none the caret
                // sets no timer. A hidden window never gets it, and so never wakes.
                self.send_input(window, InputKind::Blur);
                // Notes from before there was a window to show them in.
                let waiting = std::mem::take(&mut self.waiting_notes);
                let doc = self.store.active_tab(window);
                for text in waiting {
                    if let Some(view) = doc.and_then(|doc| self.store.view_mut(doc)) {
                        view.show_warning(text);
                    }
                }
            }
            DocEvent::WindowClosed(window) => {
                self.effects.push(Effect::DestroyWindow(window));
            }
            DocEvent::LastWindowGone { opened_nothing } => {
                if opened_nothing {
                    // A file failed to load. The app never runs with nothing on screen.
                    if let Err(e) = self.store.new_window(true) {
                        self.error_alert("A window could not open", &e.message);
                        self.begin_quit();
                    }
                } else {
                    // The user closed the last window, and that quits.
                    self.begin_quit();
                }
            }
            DocEvent::LeftoverDeleted { .. } => {}
            DocEvent::LeftoverNotDeleted { leftover, error } => {
                let title = format!("{} could not be deleted", leftover.display());
                self.error_alert(&title, &error.message);
            }
            DocEvent::TabsChanged(window) => {
                self.sync_tabs(window);
                self.relayout(window);
            }
            DocEvent::Focus { window, doc } => {
                self.store.set_active_tab(window, doc);
                self.bring_forward();
            }
            DocEvent::DocChanged(doc) => self.doc_changed(doc),
            DocEvent::TextReplaced(doc) => {
                let bytes = self.store.byte_count(doc);
                let measure = self.measure.clone();
                let mut ctx = ViewContext::default();
                self.doc_ui(doc);
                if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
                {
                    ui.bytes.set(bytes);
                    ui.frame.set_file_bytes(Some(bytes));
                    ui.frame.sync(view, measure.as_ref(), &mut ctx);
                    ui.seen_revision = view.revision();
                    ui.seen_place = (view.selection(), view.scroll_offset());
                }
                if let Some(window) = self.store.window_of(doc) {
                    self.apply(window, ctx.requests);
                    if let Some(win) = self.windows.get(&window)
                        && win.shown == Some(doc)
                        && let Err(e) = win.handle.invalidate_content()
                    {
                        eprintln!("Repainting new text: {e}");
                    }
                    if self.hidden {
                        self.effects.push(Effect::PaintHidden(window));
                    }
                }
                self.doc_changed(doc);
                self.spell_refresh(doc);
                if self.ime.started() {
                    self.effects.push(Effect::ImeSync(doc));
                }
            }
            DocEvent::DocClosed(doc) => {
                self.effects.push(Effect::ImeClose(doc));
                if let Some(mut ui) = self.docs.remove(&doc) {
                    ui.spell.take();
                }
                for win in self.windows.values_mut() {
                    if win.shown == Some(doc) {
                        win.shown = None;
                    }
                }
            }
            DocEvent::OpenFailed { path, error } => {
                let title = format!("{} could not open", path.display());
                self.error_alert(&title, &error.message);
            }
            DocEvent::AlreadyOpen { .. } => {}
            // `sync_questions` reads the store's own list, because this one may be stale.
            DocEvent::Question(_) => {}
            DocEvent::NeedsSavePanel(request) => self.needs_save_panel(request),
            DocEvent::SaveFinished {
                doc,
                other_version,
                backup_kept,
                ..
            } => {
                self.doc_changed(doc);
                if let Some(path) = other_version {
                    self.note(format!(
                        "Another program wrote to this file at the last moment. Its version is in {}.",
                        path.display()
                    ));
                }
                if let Some(path) = backup_kept {
                    self.note(format!(
                        "The old copy could not be removed. It is safe to delete {}.",
                        path.display()
                    ));
                }
            }
            DocEvent::SaveFailed {
                doc,
                error,
                offer_save_as,
            } => {
                let mut message = error.message.clone();
                for path in &error.left_on_disk {
                    message.push_str(&format!("\nA file of ours stayed at {}.", path.display()));
                }
                let buttons = if offer_save_as {
                    vec!["Save As…".to_owned(), "OK".to_owned()]
                } else {
                    vec!["OK".to_owned()]
                };
                self.show_alert(AlertRequest {
                    title: format!("{} could not be saved", self.store.display_name(doc)),
                    message,
                    buttons,
                    purpose: AlertPurpose::SaveFailed { doc, offer_save_as },
                    field: None,
                    owner: None,
                });
            }
            DocEvent::AutosaveFailed { error, .. } => {
                self.note(format!("Unsaved text could not be kept: {}", error.message));
            }
            DocEvent::Reloaded(doc) => self.doc_changed(doc),
            DocEvent::ReloadFailed { doc, error } => {
                self.doc_changed(doc);
                self.error_alert("The new version could not be read", &error.message);
            }
            DocEvent::FileMoved { doc, .. } => self.doc_changed(doc),
            DocEvent::MoveFailed { doc, error } => {
                self.doc_changed(doc);
                self.error_alert("The file could not move", &error.message);
            }
            DocEvent::FileDeleted { doc, .. } => self.doc_changed(doc),
            DocEvent::UnlockFailed { doc, error } => {
                self.doc_changed(doc);
                self.error_alert("The file could not be unlocked", &error.message);
            }
            DocEvent::EncodingFit { doc, unfit } => self.encoding_fit(doc, unfit),
            DocEvent::RestoredOverNewerFile { path, .. } => {
                self.note(format!(
                    "{} changed on disk after this text was kept. Saving will ask first.",
                    path.display()
                ));
            }
            DocEvent::RestoreFinished { windows } => {
                self.restored = true;
                if windows == 0
                    && let Err(e) = self.store.new_window(true)
                {
                    self.error_alert("A window could not open", &e.message);
                }
            }
            DocEvent::RestoreProblem(error) => self.note(error.message),
            DocEvent::LeftoverFound { path, leftover } => {
                self.show_alert(AlertRequest {
                    title: "Text left from a crash".to_owned(),
                    message: format!(
                        "A crash during a save left a file beside {}. It may hold your text.",
                        path.display()
                    ),
                    buttons: vec!["Recover".to_owned(), "Delete".to_owned(), "Keep".to_owned()],
                    purpose: AlertPurpose::Leftover { path, leftover },
                    field: None,
                    owner: None,
                });
            }
            DocEvent::LeftoverRecovered { path } => {
                if let Err(e) = self
                    .store
                    .open_paths(&[path], OpenTarget::LastUsedWindow, None)
                {
                    self.error_alert("The file could not open", &e.message);
                }
            }
            DocEvent::LeftoverNotRecovered { path, error } => {
                let title = format!("{} could not be recovered", path.display());
                self.error_alert(&title, &error.message);
            }
            DocEvent::RecentChanged => {
                // Menus read the list each time they open.
            }
            DocEvent::SessionWriteFailed(error) => {
                self.note(format!(
                    "The list of open windows could not be saved: {}",
                    error.message
                ));
            }
            DocEvent::WatchUnavailable { folder, reason } => {
                self.note(format!(
                    "{} cannot be watched, so outside changes show only when you come back to QuickEdit: {reason}",
                    folder.display()
                ));
            }
            DocEvent::ReadyToQuit => {
                if !self.ready_to_quit {
                    self.ready_to_quit = true;
                    self.effects.push(Effect::Exit);
                }
            }
        }
    }

    /// The title, the dirty mark and what the file's size turns off.
    fn doc_changed(&mut self, doc: DocId) {
        // This can arrive after the document closed.
        if self.store.view(doc).is_none() {
            return;
        }
        self.update_tab(doc);
        let bytes = self.store.byte_count(doc);
        let large = self.store.is_large(doc);
        if let Some(ui) = self.docs.get_mut(&doc) {
            ui.bytes.set(bytes);
            ui.frame.set_file_bytes(Some(bytes));
            if large && ui.spell.is_some() {
                self.spell_refresh(doc);
            }
        }
    }

    /// Shows each open question once, from the store's list. A question that has left the list
    /// closes. Documents with an open question take no edits until it is answered.
    fn sync_questions(&mut self) {
        let open = self.store.open_questions();
        self.asking = open.iter().map(|question| question.doc).collect();
        let gone: Vec<qe_docs::QuestionId> = self
            .shown_questions
            .iter()
            .filter(|id| !open.iter().any(|question| question.id == **id))
            .copied()
            .collect();
        for id in gone {
            self.shown_questions.remove(&id);
            self.effects.push(Effect::DropQuestion(id));
        }
        for question in open {
            if self.shown_questions.insert(question.id) {
                self.show_question(question);
            }
        }
    }

    fn show_question(&mut self, question: qe_docs::Question) {
        let buttons = question
            .choices
            .iter()
            .map(|answer| answer_label(*answer).to_owned())
            .collect();
        self.show_alert(AlertRequest {
            title: question.title.clone(),
            message: question.detail.clone(),
            buttons,
            purpose: AlertPurpose::Question {
                id: question.id,
                choices: question.choices.clone(),
            },
            field: None,
            owner: None,
        });
    }

    /// Sends one key or character the way the shell does, for tests and the bench.
    pub fn send_input(&mut self, window: WindowId, kind: InputKind) {
        self.content_input(window, &InputEvent { time_us: 0, kind });
    }

    /// The tree a test or a screen reader reads, on this window.
    pub fn widget_tree(&self, window: WindowId) -> Option<qe_api::Widget> {
        let win = self.windows.get(&window)?;
        let mut tree = win.handle.widgets().ok()?;
        tree.children.extend(win.tools.widgets());
        Some(tree)
    }
}
