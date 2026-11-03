//! Every menu row and key: which rows are on, and what each one does. The tools go first.
use crate::alerts::{AlertPurpose, AlertRequest};
use crate::app::{App, Effect, Incoming};
use crate::panels::PanelJob;
use qe_a11y::WidgetAction;
use qe_api::{CommandId, Dip, Handled, Rect, SettingKey, ViewContext, WidgetId};
use qe_docs::{DocId, OpenTarget, WindowId, recent_menu_rows};
use qe_editor::EditorCommand;
use qe_tools::ToolsHost;
use qe_ui::{MenuItem, MenuModel, menu_bar_groups_with_keys, set_command_state, set_recent_files};
use qe_views::wrap_menu_title;
use std::path::PathBuf;
use windows::Win32::UI::WindowsAndMessaging::{
    IsZoomed, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

/// A row of the right-click menu: the data on it says which part of the spelling menu it is.
pub const SPELL_ROW: &str = "spell:";

/// The label of a row with this command, at any depth.
fn set_row_label(model: &mut MenuModel, command: CommandId, label: &str) {
    for item in &mut model.items {
        if item.command == Some(command) {
            item.label = label.to_owned();
        }
        if let Some(sub) = &mut item.submenu {
            set_row_label(sub, command, label);
        }
    }
}

impl App {
    /// One menu group, built fresh: a new key or a new recent file shows at the next open.
    pub fn menu_model(&mut self, window: WindowId, index: usize) -> MenuModel {
        let settings = self.settings.clone();
        let groups = menu_bar_groups_with_keys(&|command| settings.key_of(command));
        let mut model = groups
            .into_iter()
            .nth(index)
            .and_then(|group| group.submenu)
            .unwrap_or_default();
        set_recent_files(&mut model, &recent_menu_rows(self.store.recent()));
        for command in CommandId::ALL {
            let (enabled, checked) = self.command_state(window, *command);
            set_command_state(&mut model, *command, enabled, checked);
        }
        let full = self.windows.get(&window).is_some_and(|win| {
            matches!(win.handle.frame_mode(), Ok(qe_shell::FrameMode::FullScreen))
        });
        if full {
            set_row_label(&mut model, CommandId::FullScreen, "Exit Full Screen");
        }
        if self.store.tabs(window).len() >= 2 {
            set_row_label(&mut model, CommandId::Close, "Close Tab");
        }
        if let Some(doc) = self.store.active_tab(window)
            && let Some(ui) = self.docs.get(&doc)
        {
            set_row_label(
                &mut model,
                CommandId::WrapToPage,
                wrap_menu_title(ui.frame.page_wrap()),
            );
        }
        model
    }

    /// `(enabled, checked)` for a menu row.
    pub fn command_state(&self, window: WindowId, command: CommandId) -> (bool, bool) {
        use CommandId::*;
        let Some(win) = self.windows.get(&window) else {
            return (false, false);
        };
        let Some(doc) = self.store.active_tab(window) else {
            return (false, false);
        };
        let Some(view) = self.store.view(doc) else {
            return (false, false);
        };
        if let Some(state) = win.tools.command_state(command, view) {
            return state;
        }
        let commands = self.store.commands(doc);
        let ui = self.docs.get(&doc);
        let tabs = self.store.tabs(window);
        let has_tab = |index: usize| (tabs.len() > index, false);
        let selected = !view.selection().is_empty();
        let spell_ok = qe_spell::why_off(view, true).is_none();
        match command {
            New | NewTab | Open | Close | OpenRecent | Settings => (true, false),
            ClearRecent => (!self.store.recent().is_empty(), false),
            Save => (commands.save, false),
            SaveAs => (commands.save_as, false),
            Duplicate => (commands.duplicate, false),
            Rename => (commands.rename, false),
            MoveTo => (commands.move_to, false),
            SaveAll => (
                self.store
                    .all_documents()
                    .iter()
                    .any(|doc| self.store.is_dirty(*doc)),
                false,
            ),
            RevertToSaved => (commands.revert, false),
            ExportPdf | Print => (!self.store.is_loading(doc), false),
            Undo | Redo | Paste => (view.can_edit(), false),
            Cut | Delete => (view.can_edit() && selected, false),
            Copy => (selected, false),
            SelectAll => (true, false),
            ShowSpelling | CheckDocumentSpelling => (spell_ok, false),
            // A row that is off shows no check.
            CheckSpellingWhileTyping => (spell_ok, spell_ok && ui.is_some_and(|ui| ui.spell_on)),
            CorrectSpellingAutomatically => {
                (spell_ok, spell_ok && ui.is_some_and(|ui| ui.correct_on))
            }
            // The three Smart rows beside it do this job. See the report's list of rows.
            ShowSubstitutions => (false, false),
            PreventEditing => (commands.prevent_editing, self.store.prevent_editing(doc)),
            WrapToPage => (true, self.store.wrap_to_page(doc)),
            ActualSize => (ui.is_some_and(|ui| ui.frame.zoom() != 1.0), false),
            ZoomIn => (ui.is_some_and(|ui| ui.frame.can_zoom_in()), false),
            ZoomOut => (ui.is_some_and(|ui| ui.frame.can_zoom_out()), false),
            // Off over 50 MiB, as on the Mac, where no line index is built. A row that is off
            // shows no check. Zoom, Wrap to Page and PDF have no size limit on the Mac.
            ShowLineNumbers => {
                let on = !self.store.is_large(doc);
                (on, on && ui.is_some_and(|ui| ui.frame.line_numbers()))
            }
            ShowDocumentMap => {
                let on = !self.store.is_large(doc);
                (on, on && ui.is_some_and(|ui| ui.frame.document_map()))
            }
            FullScreen | Minimize | ZoomWindow | BringAllToFront => (true, false),
            PreviousTab | NextTab => (tabs.len() > 1, false),
            Tab1 => has_tab(0),
            Tab2 => has_tab(1),
            Tab3 => has_tab(2),
            Tab4 => has_tab(3),
            Tab5 => has_tab(4),
            Tab6 => has_tab(5),
            Tab7 => has_tab(6),
            Tab8 => has_tab(7),
            Tab9 => has_tab(8),
            LastTab => (!tabs.is_empty(), false),
            MoveTabToNewWindow => (tabs.len() > 1, false),
            MergeAllWindows => (self.store.windows().len() > 1, false),
            // These are the tools' own. They answered above.
            Complete | Find | FindReplace | FindNext | FindPrevious | UseSelectionForFind
            | JumpToSelection | SelectLine | SmartCopyPaste | SmartQuotes | SmartDashes
            | UpperCase | LowerCase | Capitalize => (true, false),
        }
    }

    /// Runs a command, from a key or a menu row. `data` is what a chosen row carried.
    pub fn run_command(&mut self, window: WindowId, command: CommandId, data: Option<String>) {
        use CommandId::*;
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        if let Some(data) = data
            && let Some(row) = data.strip_prefix(SPELL_ROW)
        {
            if self.asking.contains(&doc) {
                self.effects.push(Effect::Beep);
                return;
            }
            self.spell_menu_row(window, doc, row);
            return;
        }
        // A composition is not in the text yet, so it goes in before the text leaves.
        let takes_text = matches!(
            command,
            Save | SaveAs | Duplicate | ExportPdf | Print | RevertToSaved | Close
        );
        if takes_text && self.ime.is_composing(doc) {
            self.effects.push(Effect::CommitThen(
                vec![doc],
                Box::new(Incoming::Run(window, command)),
            ));
            return;
        }
        if command == SaveAll && self.ime.any_composing() {
            self.effects.push(Effect::CommitThen(
                self.store.all_documents(),
                Box::new(Incoming::Run(window, command)),
            ));
            return;
        }
        // A question about this document is open: what it decides must not move underneath it.
        if self.asking.contains(&doc)
            && matches!(
                command,
                Undo | Redo
                    | Cut
                    | Paste
                    | Delete
                    | Complete
                    | UpperCase
                    | LowerCase
                    | Capitalize
                    | Save
                    | SaveAs
                    | RevertToSaved
                    | Rename
                    | MoveTo
                    | PreventEditing
            )
        {
            self.effects.push(Effect::Beep);
            return;
        }
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        if let (Some(win), Some(view)) = (self.windows.get_mut(&window), self.store.view_mut(doc))
            && win
                .tools
                .run_command(command, view, measure.as_ref(), &mut ctx)
                == Handled::Yes
        {
            self.apply(window, ctx.requests);
            self.after_tools(window, doc);
            self.after_view_call(window, doc);
            return;
        }
        self.apply(window, std::mem::take(&mut ctx.requests));
        if !ToolsHost::takes_command(command) && !self.command_state(window, command).0 {
            // A key for a row that is off does what a click on it does: nothing.
            self.effects.push(Effect::Beep);
            return;
        }
        match command {
            New => {
                if let Err(e) = self.store.new_window(false) {
                    self.error_alert("A window could not open", &e.message);
                }
            }
            NewTab => {
                if let Err(e) = self.store.new_tab(window) {
                    self.error_alert("A tab could not open", &e.message);
                }
            }
            Open => self.open_panel(window),
            OpenRecent => {
                // The row's path comes through the data above. A row with none has no file.
            }
            ClearRecent => self.store.clear_recent(),
            Close => self.store.request_close(doc),
            Save => {
                if let Err(e) = self.store.save(doc) {
                    self.error_alert("The file could not be saved", &e.message);
                }
            }
            Duplicate => {
                if let Err(e) = self.store.duplicate(doc) {
                    self.error_alert("The document could not be copied", &e.message);
                }
            }
            SaveAs => self.store.request_save_as(doc),
            Rename => {
                let name = self.store.display_name(doc);
                self.show_alert(AlertRequest {
                    title: "Rename".to_owned(),
                    message: "New name".to_owned(),
                    buttons: vec!["Rename".to_owned(), "Cancel".to_owned()],
                    purpose: AlertPurpose::Rename(doc),
                    field: Some(name),
                    doc: Some(doc),
                });
            }
            MoveTo => {
                let job = PanelJob::move_to(&self.store, doc);
                self.effects.push(Effect::Panel(job));
            }
            SaveAll => {
                for (_, error) in self.store.save_all() {
                    self.error_alert("A file could not be saved", &error.message);
                }
            }
            RevertToSaved => {
                if let Err(e) = self.store.request_revert(doc) {
                    self.error_alert("The file could not be read", &e.message);
                }
            }
            ExportPdf | Print => {
                let checks = self.pdf_checks(doc);
                let job = PanelJob::export(&self.store, doc, command == Print, checks);
                self.effects.push(Effect::Panel(job));
            }
            Settings => self.effects.push(Effect::OpenSettings),
            Undo => self.edit(window, doc, EditorCommand::Undo),
            Redo => self.edit(window, doc, EditorCommand::Redo),
            Cut => self.edit(window, doc, EditorCommand::Cut),
            Copy => self.edit(window, doc, EditorCommand::Copy),
            Paste => self.edit(window, doc, EditorCommand::Paste),
            Delete => self.edit(window, doc, EditorCommand::DeleteForward),
            SelectAll => self.edit(window, doc, EditorCommand::SelectAll),
            ShowSpelling => self.spell_find(window, doc, true),
            CheckDocumentSpelling => self.spell_find(window, doc, false),
            CheckSpellingWhileTyping => self.toggle_spelling(doc),
            CorrectSpellingAutomatically => self.toggle_correct(doc),
            PreventEditing => {
                let on = !self.store.prevent_editing(doc);
                self.store.set_prevent_editing(doc, on);
                self.spell_refresh(doc);
            }
            WrapToPage => {
                let on = !self.store.wrap_to_page(doc);
                self.store.set_wrap_to_page(doc, on);
                if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
                {
                    let done = ui.frame.set_page_wrap(on, view, measure.as_ref(), &mut ctx);
                    if let Err(e) = done {
                        self.note(format!("Wrap to Page could not change: {e}"));
                    }
                }
            }
            ActualSize | ZoomIn | ZoomOut => {
                if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc))
                {
                    let frame = &mut ui.frame;
                    let done = match command {
                        ActualSize => frame.actual_size(view, measure.as_ref(), &mut ctx),
                        ZoomIn => frame.zoom_in(view, measure.as_ref(), &mut ctx),
                        _ => frame.zoom_out(view, measure.as_ref(), &mut ctx),
                    };
                    if let Err(e) = done {
                        self.note(format!("The zoom could not change: {e}"));
                    }
                }
            }
            ShowLineNumbers => {
                let on = self
                    .docs
                    .get(&doc)
                    .is_some_and(|ui| !ui.frame.line_numbers());
                // The listener puts it on every window, as the Mac's setting does.
                if let Err(e) = self.settings.set_bool(SettingKey::ShowLineNumbers, on) {
                    self.note(format!("The setting could not change: {e}"));
                }
            }
            ShowDocumentMap => {
                let on = self
                    .docs
                    .get(&doc)
                    .is_some_and(|ui| !ui.frame.document_map());
                // The listener puts it on every window, as the Mac's setting does.
                if let Err(e) = self.settings.set_bool(SettingKey::ShowDocumentMap, on) {
                    self.note(format!("The setting could not change: {e}"));
                }
            }
            FullScreen => {
                if !self.hidden
                    && let Some(win) = self.windows.get(&window)
                    && let Err(e) = win.handle.toggle_full_screen()
                {
                    self.note(format!("Full screen could not change: {e}"));
                }
            }
            Minimize => self.show_command(window, false),
            ZoomWindow => self.show_command(window, true),
            PreviousTab | NextTab => {
                let tabs = self.store.tabs(window);
                if let Some(at) = tabs.iter().position(|known| *known == doc) {
                    let count = tabs.len();
                    let next = if command == NextTab {
                        (at + 1) % count
                    } else {
                        (at + count - 1) % count
                    };
                    self.store.set_active_tab(window, tabs[next]);
                }
            }
            Tab1 | Tab2 | Tab3 | Tab4 | Tab5 | Tab6 | Tab7 | Tab8 | Tab9 | LastTab => {
                let tabs = self.store.tabs(window);
                let index = match command {
                    Tab1 => 0,
                    Tab2 => 1,
                    Tab3 => 2,
                    Tab4 => 3,
                    Tab5 => 4,
                    Tab6 => 5,
                    Tab7 => 6,
                    Tab8 => 7,
                    Tab9 => 8,
                    _ => tabs.len().saturating_sub(1),
                };
                if let Some(target) = tabs.get(index) {
                    self.store.set_active_tab(window, *target);
                }
            }
            BringAllToFront => {
                if !self.hidden {
                    for win in self.windows.values() {
                        // SAFETY: a window of this thread. Windows may refuse.
                        let _ = unsafe { SetForegroundWindow(win.hwnd) };
                    }
                    if let Some(win) = self.windows.get(&window) {
                        // SAFETY: as above. The window in use ends on top.
                        let _ = unsafe { SetForegroundWindow(win.hwnd) };
                    }
                }
            }
            MoveTabToNewWindow => {
                self.store.move_tab_to_new_window(doc);
            }
            MergeAllWindows => self.store.merge_all_windows(),
            // The tools took each of these above, and a row that is off stopped earlier.
            ShowSubstitutions | Complete | Find | FindReplace | FindNext | FindPrevious
            | UseSelectionForFind | JumpToSelection | SelectLine | SmartCopyPaste | SmartQuotes
            | SmartDashes | UpperCase | LowerCase | Capitalize => {}
        }
        self.apply(window, ctx.requests);
        self.after_tools(window, doc);
        self.after_view_call(window, doc);
    }

    /// What choosing an Open Recent row does, for the test channel.
    pub fn run_recent_row(&mut self, window: WindowId, path: &str) {
        self.open_recent(window, path);
    }

    /// Open Recent's row: the data is the path.
    pub fn open_recent(&mut self, window: WindowId, path: &str) {
        let paths = [PathBuf::from(path)];
        if let Err(e) = self
            .store
            .open_paths(&paths, OpenTarget::Window(window), None)
        {
            self.error_alert("The file could not open", &e.message);
        }
    }

    fn edit(&mut self, window: WindowId, doc: DocId, command: EditorCommand) {
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc)) {
            view.run(command, &mut ctx);
            ui.frame.sync(view, measure.as_ref(), &mut ctx);
        }
        self.apply(window, ctx.requests);
    }

    /// Minimize, or Zoom. Hidden runs leave the window off the screen.
    fn show_command(&mut self, window: WindowId, zoom: bool) {
        if self.hidden {
            return;
        }
        if let Some(win) = self.windows.get(&window) {
            // SAFETY: plain calls on a window of this thread.
            unsafe {
                let how = if !zoom {
                    SW_MINIMIZE
                } else if IsZoomed(win.hwnd).as_bool() {
                    SW_RESTORE
                } else {
                    SW_MAXIMIZE
                };
                let _ = ShowWindow(win.hwnd, how);
            }
        }
    }

    /// The right-click menu: spelling fixes first when the word has any, then the edit rows.
    pub fn context_menu(&mut self, window: WindowId, at: qe_api::Point) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        let mut items: Vec<MenuItem> = self.spell_menu_rows(doc, at);
        let settings = self.settings.clone();
        for (label, command) in [
            ("Cut", CommandId::Cut),
            ("Copy", CommandId::Copy),
            ("Paste", CommandId::Paste),
            ("Select All", CommandId::SelectAll),
        ] {
            let mut row = MenuItem::command(label, command);
            if let Some(key) = settings.key_of(command) {
                row.key_text = qe_ui::key_text(key);
            }
            row.enabled = self.command_state(window, command).0;
            items.push(row);
        }
        let anchor = Rect {
            x: at.x,
            y: at.y,
            width: Dip(0.0),
            height: Dip(0.0),
        };
        if let Some(win) = self.windows.get(&window)
            && let Err(e) = win.handle.open_menu(MenuModel { items }, anchor)
        {
            self.note(format!("The menu could not open: {e}"));
        }
    }

    /// An action from a screen reader, on a painted control.
    pub fn widget_action(&mut self, window: WindowId, id: WidgetId, action: WidgetAction) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        // A tool's button can change the text, and a question about this text is open.
        if self.asking.contains(&doc)
            && !matches!(action, WidgetAction::Focus)
            && self.windows.get(&window).is_some_and(|win| {
                win.tools
                    .widgets()
                    .iter()
                    .any(|tree| crate::a11y::find(tree, id).is_some())
            })
        {
            self.effects.push(Effect::Beep);
            return;
        }
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        let mut result: Result<(), String> = Ok(());
        if let (Some(win), Some(view)) = (self.windows.get_mut(&window), self.store.view_mut(doc)) {
            let routed = win.tools.widget_action(
                id,
                &tool_action(&action),
                view,
                measure.as_ref(),
                &mut ctx,
            );
            match routed {
                Handled::Yes => {}
                Handled::No => match action {
                    WidgetAction::Invoke
                    | WidgetAction::Select
                    | WidgetAction::Toggle
                    | WidgetAction::Expand => {
                        result = match win.handle.invoke_widget(id) {
                            Ok(true) => Ok(()),
                            Ok(false) => Err(format!("no control {} can act now", id.0)),
                            Err(e) => Err(e.to_string()),
                        };
                    }
                    // Collapse takes back what Expand opened: the menu.
                    WidgetAction::Collapse => {
                        result = win.handle.close_menu().map_err(|e| e.to_string());
                    }
                    _ => {}
                },
            }
        }
        self.apply(window, ctx.requests);
        self.after_tools(window, doc);
        self.after_view_call(window, doc);
        if let Err(e) = result {
            self.note(format!("A control could not act: {e}"));
        }
    }
}

fn tool_action(action: &WidgetAction) -> qe_tools::ToolAction {
    match action {
        WidgetAction::Invoke | WidgetAction::Toggle | WidgetAction::Select => {
            qe_tools::ToolAction::Invoke
        }
        WidgetAction::SetValue(text) => qe_tools::ToolAction::SetValue(text.clone()),
        WidgetAction::Focus => qe_tools::ToolAction::Focus,
        _ => qe_tools::ToolAction::Other,
    }
}
