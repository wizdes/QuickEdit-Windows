//! What each incoming item does, and what each event from the store shows.
use crate::alerts::{AlertPurpose, AlertRequest};
use crate::app::{App, Effect, Incoming};
use crate::platform::Signal;
use qe_api::{CommandId, InputEvent, InputKind, ViewContext};
use qe_docs::handoff::HandoffEvent;
use qe_docs::{Answer, DocEvent, DocId, OpenTarget, WindowId};
use qe_settings::SettingChange;
use qe_shell::{ShellEvent, TabStrip, VIEW_TIMER};
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
            // The handler runs these itself, with `App` free.
            Incoming::Signal(Signal::BenchStep) => {}
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
            // TSF's calls need `App` free, so all of this runs as one effect.
            Incoming::EndSession => self.effects.push(Effect::EndSession),
            Incoming::SystemSetting => {
                let reduce = system_reduce_motion();
                for ui in self.docs.values_mut() {
                    ui.frame.set_reduce_motion(reduce);
                }
                // Settings copies a text window's theme, and it may hear of this first. It
                // looks again after the message loop's turn, when each window has heard.
                if let Some(shared) = self.me.upgrade() {
                    crate::app::post_later(&shared, Incoming::SettingsTheme);
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
            Incoming::AlertNext => self.effects.push(Effect::AlertNext),
            Incoming::SettingsTheme => self.effects.push(Effect::SettingsTheme),
            Incoming::TestPanel(job) => self.test_panels.push_back(job),
            Incoming::CloseDoc(doc) => self.store.request_close(doc),
            Incoming::CloseWindow(window) => self.store.request_close_window(window),
            Incoming::StayFor(requests) => self.stay_for(requests),
        }
    }

    /// Launches handed off during the exit, and were told their files were taken.
    fn stay_for(&mut self, requests: Vec<qe_docs::handoff::HandoffRequest>) {
        self.ready_to_quit = false;
        self.store.stop_quit_for_new_files();
        let mut opened = false;
        for request in requests {
            for error in &request.refused {
                self.error_alert("A file could not open", &error.message);
            }
            if !request.paths.is_empty() {
                opened = true;
                if let Err(e) =
                    self.store
                        .open_paths(&request.paths, OpenTarget::LastUsedWindow, None)
                {
                    self.error_alert("The files could not open", &e.message);
                }
            }
        }
        if !opened
            && self.store.windows().is_empty()
            && let Err(e) = self.store.new_window(true)
        {
            self.error_alert("A window could not open", &e.message);
        }
    }

    /// The tab's X, for the test channel.
    pub fn close_tab_from_shell(&mut self, window: WindowId, tab: qe_shell::TabId) {
        self.shell_event(window, ShellEvent::CloseTabRequested(tab));
    }

    /// Close, for one tab. A composition goes in first, so its text counts as unsaved.
    pub fn close_doc(&mut self, doc: DocId) {
        if self.ime.is_composing(doc) {
            self.effects.push(Effect::CommitThen(
                vec![doc],
                Box::new(Incoming::CloseDoc(doc)),
            ));
        } else {
            self.store.request_close(doc);
        }
    }

    /// Close, for a window: the same for every tab in it.
    fn close_window(&mut self, window: WindowId) {
        let tabs = self.store.tabs(window);
        if self.ime.any_composing_in(&tabs) {
            self.effects.push(Effect::CommitThen(
                tabs,
                Box::new(Incoming::CloseWindow(window)),
            ));
        } else {
            self.store.request_close_window(window);
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
                    self.close_doc(doc);
                    // A question or a save came first, so that a close, if any, comes later
                    // and by another hand than the one on the X.
                    if self.store.tabs(window).contains(&doc)
                        && let Some(win) = self.windows.get(&window)
                        && let Err(e) = win.handle.with_strip(TabStrip::forget_pointer_close)
                    {
                        eprintln!("Answering a close of a tab: {e}");
                    }
                }
            }
            ShellEvent::CloseWindowRequested => self.close_window(window),
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
                if let Some(win) = self.windows.get_mut(&window) {
                    win.has_keys = true;
                }
                self.effects.push(Effect::ImeFocus(window, doc));
            }
            InputKind::Blur => {
                if let Some(win) = self.windows.get_mut(&window) {
                    win.has_keys = false;
                }
            }
            _ => {}
        }
        // A question about this document is open: what it decides must not move underneath it.
        // A click can reach Replace All or start a drag, so only what changes nothing goes on.
        if self.asking.contains(&doc)
            && !matches!(
                input.kind,
                InputKind::Focus
                    | InputKind::Blur
                    | InputKind::KeyUp { .. }
                    | InputKind::MouseMove { .. }
                    | InputKind::Wheel { .. }
                    | InputKind::DpiChange { .. }
            )
        {
            return;
        }
        // A message on show takes the first Esc and any click on itself.
        let dismissing = match input.kind {
            InputKind::KeyDown {
                virtual_key: 0x1b, ..
            } => {
                self.windows
                    .get(&window)
                    .is_some_and(|win| win.tools.focus() == qe_tools::ToolsFocus::Editor)
                    && !self.ime.is_composing(doc)
            }
            InputKind::MouseDown {
                at,
                button: qe_api::MouseButton::Left,
                ..
            } => self.notice_hit(window, doc, at),
            _ => false,
        };
        if dismissing && self.dismiss_notice(doc) {
            return;
        }
        let is_dpi = matches!(input.kind, InputKind::DpiChange { .. });
        if let InputKind::DpiChange { dpi } = input.kind
            && let Some(win) = self.windows.get(&window)
        {
            win.dpi.set(dpi);
        }
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
                ui.synced_revision = view.revision();
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
        // A key's timer began when the message loop took its message. A key that shows nothing,
        // such as the first half of a typed letter, leaves it running for the second half.
        if is_key
            && repaint
            && (self.timing_on || self.bench.is_some())
            && let Some(started) = self.timing.arrived.take()
        {
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
        // TSF writes where its focus is. When the keyboard goes to a field beside the text, it
        // leaves the text, and the field's own input path takes over.
        let tools_have_keys = win.tools.focus() != qe_tools::ToolsFocus::Editor;
        if tools_have_keys != win.tools_had_keys {
            win.tools_had_keys = tools_have_keys;
            if tools_have_keys {
                self.effects.push(Effect::ImeLeave(doc));
            } else if win.has_keys {
                // TSF may start only now, if the first focus found the tools with the keyboard.
                self.effects.push(Effect::ImeFocus(window, doc));
            }
        }
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
        let queued = view.has_queued_edits();
        let bytes = self.store.byte_count(doc);
        let Some(ui) = self.docs.get_mut(&doc) else {
            return;
        };
        ui.bytes.set(bytes);
        let text_changed = ui.seen_revision != revision;
        let place_changed = ui.seen_place != place;
        // Queued keys that ran and changed nothing leave the revision as it was, but the text no
        // longer counts as unsaved.
        let queue_ended = ui.seen_queued_edits && !queued;
        ui.seen_revision = revision;
        ui.seen_place = place;
        ui.seen_queued_edits = queued;
        if text_changed || queue_ended {
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
        if text_changed {
            self.after_text_change(window, doc);
        }
        let warning = self.store.view(doc).and_then(|view| view.warning());
        if self
            .docs
            .get(&doc)
            .is_some_and(|ui| ui.seen_warning.as_deref() != warning)
        {
            let warning = warning.map(str::to_owned);
            if let Some(ui) = self.docs.get_mut(&doc) {
                ui.seen_warning = warning;
            }
            self.notices_changed(doc);
        }
    }

    /// The wiring lists' step after any edit, whichever path made it: an input method, a screen
    /// reader or a tool. The gutter and the map follow, and so does the tools' search.
    fn after_text_change(&mut self, window: WindowId, doc: DocId) {
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        let (Some(win), Some(ui), Some(view)) = (
            self.windows.get_mut(&window),
            self.docs.get_mut(&doc),
            self.store.view_mut(doc),
        ) else {
            return;
        };
        if win.shown != Some(doc) {
            return;
        }
        // A key through the frame has just done this.
        if ui.synced_revision != view.revision() {
            ui.frame.sync(view, measure.as_ref(), &mut ctx);
            ui.synced_revision = view.revision();
        }
        let tools_at_work = !win.tools.is_idle();
        win.tools.poll(view, measure.as_ref(), &mut ctx);
        self.apply(window, ctx.requests);
        if tools_at_work {
            self.after_tools(window, doc);
        }
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
        if self.ime.owed.take() {
            self.effects.push(Effect::ImeSyncAll);
        }
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
                        let window = self.store.last_used_window();
                        self.bring_forward(window);
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

    fn bring_forward(&mut self, window: Option<WindowId>) {
        self.forward_window = window;
        if self.hidden {
            return;
        }
        if let Some(window) = window
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
            SettingChange::Value(qe_api::SettingKey::ShowDocumentMap) => {
                let on = self
                    .settings
                    .get_bool(qe_api::SettingKey::ShowDocumentMap)
                    .unwrap_or(true);
                let measure = self.measure.clone();
                let docs: Vec<DocId> = self.docs.keys().copied().collect();
                for doc in docs {
                    let mut ctx = ViewContext::default();
                    if let (Some(ui), Some(view)) =
                        (self.docs.get_mut(&doc), self.store.view_mut(doc))
                    {
                        ui.frame
                            .set_document_map(on, view, measure.as_ref(), &mut ctx);
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
            SettingChange::Value(
                qe_api::SettingKey::OpenEncoding
                | qe_api::SettingKey::SaveEncoding
                | qe_api::SettingKey::AddTxtExtension
                | qe_api::SettingKey::ShowPageBreaks,
            ) => {
                let settings = self.settings.clone();
                self.store.follow_settings(&settings);
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

    /// Shows a note in the window in use, at once. It stays until the person dismisses it.
    pub fn note(&mut self, text: String) {
        eprintln!("{text}");
        self.notes.push(text.clone());
        let doc = self
            .store
            .last_used_window()
            .and_then(|window| self.store.active_tab(window));
        match doc {
            Some(doc) => self.add_notice(doc, text),
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
            doc: None,
        });
    }

    pub fn show_alert(&mut self, request: AlertRequest) {
        self.effects.push(Effect::Alert(request));
    }

    pub fn begin_quit(&mut self) {
        // A composition is not in the text yet, so it goes in before the text is kept.
        if self.ime.any_composing() {
            self.effects.push(Effect::CommitThen(
                self.store.all_documents(),
                Box::new(Incoming::Quit),
            ));
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
        self.refresh_autosave_failures();
        // Notes from when there was no window, or from a tab that has closed.
        if !self.waiting_notes.is_empty()
            && let Some(doc) = self
                .store
                .last_used_window()
                .filter(|window| self.windows.contains_key(window))
                .and_then(|window| self.store.active_tab(window))
        {
            for text in std::mem::take(&mut self.waiting_notes) {
                self.add_notice(doc, text);
            }
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
                    self.window_failed(window, &e);
                    return;
                }
                self.sync_tabs(window);
                self.relayout(window);
                // A window has no keyboard until Windows says so, and with none the caret
                // sets no timer. A hidden window never gets it, and so never wakes.
                self.send_input(window, InputKind::Blur);
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
                self.bring_forward(Some(window));
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
                self.autosave_failing.remove(&doc);
                self.commit_asked.remove(&doc);
                if let Some(mut ui) = self.docs.remove(&doc) {
                    ui.spell.take();
                    // A note outlives its tab, as it may be about something else.
                    self.waiting_notes.append(&mut ui.notices);
                }
                self.pdf_poll();
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
                    doc: Some(doc),
                });
            }
            DocEvent::AutosaveFailed { doc, error } => {
                // A copy that keeps failing stays on show, on its own tab, until one succeeds.
                if self.store.autosave_failure(doc).is_some() {
                    eprintln!("Unsaved text could not be kept: {}", error.message);
                    self.notes
                        .push(format!("Unsaved text could not be kept: {}", error.message));
                    self.autosave_failing.insert(doc);
                    self.doc_ui(doc);
                    self.update_tab(doc);
                    self.notices_changed(doc);
                } else {
                    self.note(format!("Unsaved text could not be kept: {}", error.message));
                }
            }
            DocEvent::Reloaded(doc) => self.doc_changed(doc),
            DocEvent::ReloadFailed { doc, error } => {
                self.doc_changed(doc);
                self.error_alert("The new version could not be shown", &error.message);
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
                    doc: None,
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

    /// A window could not be made. Its tabs need a place, or the text has no way to show. With
    /// no window at all, the process must not stay on, invisible and holding the pipe: the text
    /// goes to its copies, and the app ends with the reason.
    fn window_failed(&mut self, window: WindowId, problem: &str) {
        let target = self
            .store
            .windows()
            .into_iter()
            .find(|other| *other != window && self.windows.contains_key(other));
        let tabs = self.store.tabs(window);
        match target {
            Some(target) => {
                self.error_alert("A window could not open", problem);
                for doc in tabs {
                    self.store.move_tab(doc, target, usize::MAX);
                }
            }
            None => {
                crate::app::fatal_box(
                    self.hidden,
                    &format!(
                        "QuickEdit has to stop, because its window could not open: {problem}. Your text is kept, and it comes back at the next launch."
                    ),
                );
                self.exit_code = 1;
                self.begin_quit();
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
        let asking: std::collections::HashSet<DocId> =
            open.iter().map(|question| question.doc).collect();
        let lifted: Vec<DocId> = self.asking.difference(&asking).copied().collect();
        for doc in lifted {
            self.commit_asked.remove(&doc);
            if let Some(view) = self.store.view_mut(doc) {
                view.set_hold_edits(false);
            }
            if self.ime.started() {
                self.effects.push(Effect::ImeSync(doc));
            }
        }
        // Every path that edits this text is refused while its question is open. A composition
        // or a key that came first goes in first, because refusing them would lose them.
        for doc in &asking {
            if self.ime.is_composing(*doc) {
                if self.commit_asked.insert(*doc) {
                    self.effects.push(Effect::CommitThen(
                        vec![*doc],
                        Box::new(Incoming::Note(String::new())),
                    ));
                }
                continue;
            }
            if let Some(view) = self.store.view_mut(*doc)
                && !view.edits_are_held()
                && view.queued_input_count() == 0
            {
                view.set_hold_edits(true);
                if self.ime.started() {
                    self.effects.push(Effect::ImeSync(*doc));
                }
            }
        }
        self.asking = asking;
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
            doc: Some(question.doc),
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
        if let Some(doc) = self.store.active_tab(window) {
            tree.children.extend(self.notice_widgets(doc, win.content));
        }
        tree.children.extend(win.tools.widgets());
        Some(tree)
    }
}
