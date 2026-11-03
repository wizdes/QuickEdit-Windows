//! The test channel behind `--test-channel`. One command per line on stdin, one answer per line
//! on stdout. It drives the same paths as a person: real keys go through `content_input`, real
//! menu rows through `run_command`. A file dialog waits here for the test's answer. When stdin
//! ends, the app ends, so a test that dies leaves no process behind.
use crate::app::{App, Effect, Incoming, Shared};
use crate::panels::PanelChoice;
use qe_api::{CommandId, InputKind, KeyBinding, Modifiers, SettingKey, ViewContext};
use qe_docs::{DocId, WindowId};
use qe_shell::ContrastColors;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SendMessageW, WM_KEYDOWN, WM_KEYUP, WM_PAINT,
};

type Inbox = Arc<Mutex<Vec<String>>>;
static INBOX: std::sync::OnceLock<Inbox> = std::sync::OnceLock::new();

/// `%XX` for anything that is not plain, so that each command stays on one line.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(&text[at + 1..at + 3], 16)
        {
            out.push(value);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn reply(text: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = writeln!(out, "{text}").and_then(|()| out.flush()) {
        eprintln!("The test channel could not answer: {e}");
    }
}

/// A thread reads the lines, and a wake hands them to the UI thread.
pub fn start(shared: &Rc<Shared>) {
    let inbox: Inbox = Arc::default();
    if INBOX.set(inbox.clone()).is_err() {
        return;
    }
    let wake = shared.state.borrow().wake.clone();
    let spawned = std::thread::Builder::new()
        .name("qe-test-channel".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) => {
                        if let Ok(mut lines) = inbox.lock() {
                            lines.push(line);
                        }
                        wake();
                    }
                    Err(_) => break,
                }
            }
            if let Ok(mut lines) = inbox.lock() {
                lines.push("stdin-ended".to_owned());
            }
            wake();
        });
    if let Err(e) = spawned {
        eprintln!("The test channel could not start: {e}");
    }
}

pub fn take_lines() -> Vec<String> {
    INBOX
        .get()
        .and_then(|inbox| {
            inbox
                .lock()
                .ok()
                .map(|mut lines| std::mem::take(&mut *lines))
        })
        .unwrap_or_default()
}

/// Paints one frame and writes its pixels as `w h\n` and BGRA bytes, for the test to judge.
pub fn picture(shared: &Rc<Shared>, window: WindowId, path: &Path, theme: &str) {
    let found = shared.state.try_borrow().ok().and_then(|app| {
        let win = app.windows.get(&window)?;
        Some((win.handle.clone(), win.hwnd, win.capture.clone()))
    });
    let Some((handle, hwnd, capture)) = found else {
        reply("error no window");
        return;
    };
    let contrast = ContrastColors {
        window: qe_api::Color::rgb(0, 0, 0),
        window_text: qe_api::Color::rgb(255, 255, 255),
        highlight: qe_api::Color::rgb(26, 235, 255),
        highlight_text: qe_api::Color::rgb(0, 0, 0),
        gray_text: qe_api::Color::rgb(63, 242, 63),
    };
    let applied = match theme {
        "light" => handle.apply_theme(false, None),
        "dark" => handle.apply_theme(true, None),
        _ => handle.apply_theme(true, Some(contrast)),
    };
    if let Err(e) = applied {
        reply(&format!("error {e}"));
        return;
    }
    capture.borrow_mut().wanted = true;
    if let Err(e) = handle.invalidate_content() {
        reply(&format!("error {e}"));
        return;
    }
    // SAFETY: a window of this thread. A hidden window gets no WM_PAINT from Windows.
    unsafe {
        SendMessageW(hwnd, WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    let frame = capture.borrow_mut().frame.take();
    match frame {
        Some(Ok(bitmap)) => {
            let mut bytes = format!("{} {}\n", bitmap.width, bitmap.height).into_bytes();
            bytes.extend_from_slice(&bitmap.bgra);
            match std::fs::write(path, bytes) {
                Ok(()) => reply("ok"),
                Err(e) => reply(&format!("error {e}")),
            }
        }
        Some(Err(e)) => reply(&format!("error {e}")),
        None => reply("error no frame was painted"),
    }
}

fn parse_command(name: &str) -> Option<CommandId> {
    CommandId::ALL
        .iter()
        .copied()
        .find(|command| format!("{command:?}") == name)
}
fn parse_setting(name: &str) -> Option<SettingKey> {
    SettingKey::ALL
        .iter()
        .copied()
        .find(|key| key.name() == name)
}

impl App {
    fn window_in_use(&self) -> Option<WindowId> {
        self.store
            .last_used_window()
            .filter(|window| self.windows.contains_key(window))
            .or_else(|| self.windows.keys().next().copied())
    }
    fn doc_in_use(&self) -> Option<DocId> {
        self.store.active_tab(self.window_in_use()?)
    }
    fn say(&mut self, text: String) {
        self.effects.push(Effect::Reply(text));
    }

    pub fn channel_inbox(&mut self) {
        if !self.channel_on {
            return;
        }
        for line in take_lines() {
            self.channel_line(&line);
        }
    }

    pub fn channel_line(&mut self, line: &str) {
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or("");
        let rest: Vec<&str> = words.collect();
        let window = self.window_in_use();
        let answer = match verb {
            "stdin-ended" => {
                self.begin_quit();
                return;
            }
            "ping" => "ok".to_owned(),
            "restored" => self.restored.to_string(),
            "windows" => self.describe_windows(),
            "text" => {
                let text = self.doc_in_use().and_then(|doc| self.text_of(doc));
                encode(&text.unwrap_or_default())
            }
            "doc" => self.describe_doc(),
            "run" => {
                let command = rest.first().and_then(|name| parse_command(name));
                match (window, command) {
                    (Some(window), Some(command)) => {
                        self.run_command(window, command, None);
                        "ok".to_owned()
                    }
                    _ => format!("error bad run {rest:?}"),
                }
            }
            "state" => {
                let command = rest.first().and_then(|name| parse_command(name));
                match (window, command) {
                    (Some(window), Some(command)) => {
                        let (on, checked) = self.command_state(window, command);
                        format!("{on} {checked}")
                    }
                    _ => format!("error bad state {rest:?}"),
                }
            }
            "type" => {
                let text = decode(rest.first().copied().unwrap_or(""));
                if let Some(window) = window {
                    for typed in text.chars() {
                        self.send_input(window, InputKind::Char(typed));
                    }
                }
                "ok".to_owned()
            }
            "key" => {
                let code = rest
                    .first()
                    .and_then(|n| n.parse::<u16>().ok())
                    .unwrap_or(0);
                let has = |flag: &str| rest.contains(&flag);
                let modifiers = Modifiers {
                    ctrl: has("ctrl"),
                    alt: has("alt"),
                    shift: has("shift"),
                    win: false,
                };
                if let Some(window) = window {
                    self.send_input(
                        window,
                        InputKind::KeyDown {
                            virtual_key: code,
                            modifiers,
                            repeat: false,
                        },
                    );
                }
                "ok".to_owned()
            }
            // A real message through the window's own procedure.
            "post-key" => {
                let code = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                match window.and_then(|window| self.windows.get(&window)) {
                    Some(win) => {
                        // SAFETY: a window of this thread.
                        match unsafe {
                            PostMessageW(Some(win.hwnd), WM_KEYDOWN, WPARAM(code), LPARAM(0))
                        } {
                            Ok(()) => "ok".to_owned(),
                            Err(e) => format!("error {e}"),
                        }
                    }
                    None => "error no window".to_owned(),
                }
            }
            "close-window" => match window.and_then(|window| self.windows.get(&window)) {
                Some(win) => {
                    // SAFETY: a window of this thread. Alt+F4 ends here too.
                    match unsafe {
                        PostMessageW(
                            Some(win.hwnd),
                            windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                            WPARAM(0),
                            LPARAM(0),
                        )
                    } {
                        Ok(()) => "ok".to_owned(),
                        Err(e) => format!("error {e}"),
                    }
                }
                None => "error no window".to_owned(),
            },
            "post-syskey" => {
                let code = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                match window.and_then(|window| self.windows.get(&window)) {
                    Some(win) => {
                        // SAFETY: a window of this thread. Bit 29 says Alt is down.
                        match unsafe {
                            PostMessageW(
                                Some(win.hwnd),
                                windows::Win32::UI::WindowsAndMessaging::WM_SYSKEYDOWN,
                                WPARAM(code),
                                LPARAM(1 << 29),
                            )
                        } {
                            Ok(()) => "ok".to_owned(),
                            Err(e) => format!("error {e}"),
                        }
                    }
                    None => "error no window".to_owned(),
                }
            }
            "menu-open" => window
                .and_then(|window| self.windows.get(&window))
                .and_then(|win| win.handle.menu_is_open().ok())
                .unwrap_or(false)
                .to_string(),
            "invoke-tab" => {
                let index = rest.first().and_then(|n| n.parse().ok()).unwrap_or(0);
                match window {
                    Some(window) => {
                        let id = qe_shell::TabStrip::widget_id(qe_shell::CaptionHit::Tab(index));
                        self.widget_action(window, id, qe_a11y::WidgetAction::Invoke);
                        "ok".to_owned()
                    }
                    None => "error no window".to_owned(),
                }
            }
            "invoke-row" => {
                let index = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                let tree = window.and_then(|window| self.widget_tree(window));
                let row = tree.as_ref().and_then(|tree| first_open_row(tree, index));
                match (window, row) {
                    (Some(window), Some(id)) => {
                        self.widget_action(window, id, qe_a11y::WidgetAction::Invoke);
                        "ok".to_owned()
                    }
                    _ => "error no such row".to_owned(),
                }
            }
            "loaded" => {
                let name = rest.first().copied().unwrap_or("");
                // SAFETY: the string lives through the call, and a null answer is the answer.
                let found = unsafe {
                    windows::Win32::System::LibraryLoader::GetModuleHandleW(
                        &windows::core::HSTRING::from(name),
                    )
                };
                found.is_ok().to_string()
            }
            "tsf-started" => self.ime.started().to_string(),
            "focus" | "blur" => {
                let kind = if verb == "focus" {
                    InputKind::Focus
                } else {
                    InputKind::Blur
                };
                if let Some(window) = window {
                    self.send_input(window, kind);
                }
                "ok".to_owned()
            }
            "rows-on-screen" => {
                let rows = self
                    .doc_in_use()
                    .and_then(|doc| self.store.view(doc))
                    .map(|view| {
                        let rows = view.visible_rows();
                        format!("{} {}", rows.rows.len(), rows.complete)
                    });
                rows.unwrap_or_else(|| "error no view".to_owned())
            }
            "hwnd" => match window.and_then(|window| self.windows.get(&window)) {
                Some(win) => format!("{}", win.hwnd.0 as isize),
                None => "error no window".to_owned(),
            },
            "open" => {
                let paths: Vec<PathBuf> = rest
                    .iter()
                    .map(|path| PathBuf::from(decode(path)))
                    .collect();
                match window {
                    Some(window) => {
                        match self.store.open_paths(
                            &paths,
                            qe_docs::OpenTarget::Window(window),
                            None,
                        ) {
                            Ok(_) => "ok".to_owned(),
                            Err(e) => format!("error {}", e.message),
                        }
                    }
                    None => "error no window".to_owned(),
                }
            }
            "open-recent" => {
                if let Some(window) = window {
                    self.run_recent_row(window, &decode(rest.first().copied().unwrap_or("")));
                }
                "ok".to_owned()
            }
            "panels" => self.test_panels.len().to_string(),
            "panel-title" => self
                .test_panels
                .front()
                .and_then(|job| job.options.title.clone())
                .map_or_else(|| "none".to_owned(), |title| encode(&title)),
            "panel-path" => {
                let Some(job) = self.test_panels.pop_front() else {
                    self.say("error no panel".to_owned());
                    return;
                };
                let row = rest.get(1).and_then(|n| n.parse().ok());
                let add_txt = rest.get(2).map(|flag| *flag == "txt");
                let choice = PanelChoice {
                    path: decode(rest.first().copied().unwrap_or("")),
                    row: row
                        .or(Some(job.options.selected))
                        .filter(|_| !job.options.encodings.is_empty()),
                    add_txt: add_txt.or(job.options.add_txt),
                    note: None,
                };
                self.handle(Incoming::Panel(job, Some(choice)));
                "ok".to_owned()
            }
            "panel-cancel" => {
                let Some(job) = self.test_panels.pop_front() else {
                    self.say("error no panel".to_owned());
                    return;
                };
                self.handle(Incoming::Panel(job, None));
                "ok".to_owned()
            }
            "alert" | "press" | "alert-field" | "alert-post" | "alert-hwnd" => {
                self.effects.push(Effect::ChannelAlert(
                    verb.to_owned(),
                    rest.first().map(|word| (*word).to_owned()),
                ));
                return;
            }
            "notes" => encode(&self.notes.join("\n")),
            "marks" => {
                let count = self
                    .doc_in_use()
                    .and_then(|doc| self.docs.get(&doc))
                    .and_then(|ui| ui.spell.as_ref())
                    .map_or(0, |service| service.marks().len());
                count.to_string()
            }
            "underlines" => {
                let count = self
                    .doc_in_use()
                    .and_then(|doc| self.store.view(doc))
                    .map_or(0, |view| {
                        view.decorations(qe_editor::DecorationKind::SpellingUnderline)
                            .len()
                    });
                count.to_string()
            }
            "find-count" => match window.and_then(|window| self.windows.get(&window)) {
                Some(win) => {
                    let (count, done) = win.tools.match_count();
                    format!("{count} {done}")
                }
                None => "error no window".to_owned(),
            },
            "tools-idle" => match window.and_then(|window| self.windows.get(&window)) {
                Some(win) => win.tools.is_idle().to_string(),
                None => "error no window".to_owned(),
            },
            "selection" => {
                let range = self
                    .doc_in_use()
                    .and_then(|doc| self.store.view(doc))
                    .map(|view| view.selection());
                match range {
                    Some(range) => format!("{} {}", range.start.0, range.end.0),
                    None => "error no view".to_owned(),
                }
            }
            "select" => {
                let parse = |at: usize| {
                    rest.get(at)
                        .and_then(|n| n.parse::<usize>().ok())
                        .unwrap_or(0)
                };
                let range = qe_api::ByteOffset(parse(0))..qe_api::ByteOffset(parse(1));
                if let (Some(window), Some(doc)) = (window, self.doc_in_use()) {
                    let mut ctx = ViewContext::default();
                    if let Some(view) = self.store.view_mut(doc) {
                        view.select(range, &mut ctx);
                    }
                    self.apply(window, ctx.requests);
                    self.after_view_call(window, doc);
                }
                "ok".to_owned()
            }
            "replace-all" => {
                let find = decode(rest.first().copied().unwrap_or(""));
                let with = decode(rest.get(1).copied().unwrap_or(""));
                self.test_replace_all(&find, &with);
                "ok".to_owned()
            }
            "tools-text" => self.tools_status(),
            "set-key" => {
                let command = rest.first().and_then(|name| parse_command(name));
                let code = rest.get(1).and_then(|n| n.parse::<u16>().ok());
                match (command, code) {
                    (Some(command), Some(code)) => {
                        let key = KeyBinding::new(code, false, false, false);
                        match self.settings.set_key(command, key) {
                            Ok(()) => "ok".to_owned(),
                            Err(e) => format!("error {e:?}"),
                        }
                    }
                    _ => format!("error bad set-key {rest:?}"),
                }
            }
            "key-of" => {
                let key = rest
                    .first()
                    .and_then(|name| parse_command(name))
                    .and_then(|c| self.settings.key_of(c));
                key.map_or_else(|| "none".to_owned(), |key| encode(&qe_ui::key_text(key)))
            }
            "menu-row" => {
                let index = rest.first().and_then(|n| n.parse().ok()).unwrap_or(0);
                let wanted = rest.get(1).and_then(|name| parse_command(name));
                match (window, wanted) {
                    (Some(window), Some(command)) => {
                        let model = self.menu_model(window, index);
                        row_text(&model, command)
                            .map_or_else(|| "none".to_owned(), |text| encode(&text))
                    }
                    _ => "error".to_owned(),
                }
            }
            "set-bool" => {
                let key = rest.first().and_then(|name| parse_setting(name));
                match key {
                    Some(key) => match self.settings.set_bool(key, rest.get(1) == Some(&"true")) {
                        Ok(()) => "ok".to_owned(),
                        Err(e) => format!("error {e}"),
                    },
                    None => format!("error bad set-bool {rest:?}"),
                }
            }
            "settings-open" => {
                self.effects.push(Effect::ChannelSettingsOpen);
                return;
            }
            "widget-ids" => match window.and_then(|window| self.widget_tree(window)) {
                Some(tree) => {
                    let mut ids = Vec::new();
                    collect_ids(&tree, &mut ids);
                    let total = ids.len();
                    ids.sort_unstable();
                    ids.dedup();
                    format!("{total} {}", ids.len())
                }
                None => "error no tree".to_owned(),
            },
            "pumps" => {
                self.effects.push(Effect::ChannelPumps);
                return;
            }
            "picture" => {
                let (Some(window), Some(path)) = (window, rest.first()) else {
                    self.say("error bad picture".to_owned());
                    return;
                };
                self.effects.push(Effect::Picture {
                    window,
                    path: PathBuf::from(decode(path)),
                    theme: rest.get(1).copied().unwrap_or("light").to_owned(),
                });
                return;
            }
            "paint" => {
                if let Some(window) = window {
                    self.effects.push(Effect::PaintHidden(window));
                }
                "ok".to_owned()
            }
            "quit" => {
                self.begin_quit();
                "ok".to_owned()
            }
            "tab-drop-new" => {
                // A tab let go over nothing of ours: far outside any window.
                if let Some(doc) = self.doc_in_use() {
                    let at = windows::Win32::Foundation::POINT {
                        x: -32000,
                        y: -32000,
                    };
                    self.tab_dropped(doc, at);
                }
                "ok".to_owned()
            }
            "tab-into" => {
                // A tab let go over another of our windows.
                let index = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                let target = self.windows.keys().copied().find(|id| Some(*id) != window);
                match (self.doc_in_use(), target) {
                    (Some(doc), Some(target)) => {
                        self.store.move_tab(doc, target, index);
                        "ok".to_owned()
                    }
                    _ => "error no other window".to_owned(),
                }
            }
            "use-window" => {
                let index = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                match self.windows.keys().nth(index).copied() {
                    Some(id) => {
                        self.store.window_activated(id);
                        "ok".to_owned()
                    }
                    None => "error no such window".to_owned(),
                }
            }
            "settle" => {
                let busy = self
                    .store
                    .all_documents()
                    .iter()
                    .any(|doc| self.store.is_loading(*doc) || self.store.is_saving(*doc));
                (!busy).to_string()
            }
            other => format!("error unknown command {other}"),
        };
        self.say(answer);
    }

    fn describe_windows(&self) -> String {
        let mut parts = Vec::new();
        for window in self.store.windows() {
            let active = self.store.active_tab(window);
            let tabs: Vec<String> = self
                .store
                .tabs(window)
                .iter()
                .map(|doc| {
                    format!(
                        "{}{}",
                        encode(&self.store.display_name(*doc)),
                        if Some(*doc) == active { "*" } else { "" }
                    )
                })
                .collect();
            parts.push(tabs.join(","));
        }
        parts.join(" ")
    }

    fn describe_doc(&self) -> String {
        let Some(doc) = self.doc_in_use() else {
            return "none".to_owned();
        };
        format!(
            "name={} dirty={} loading={} saving={} bytes={} path={} note={}",
            encode(&self.store.display_name(doc)),
            self.store.is_dirty(doc),
            self.store.is_loading(doc),
            self.store.is_saving(doc),
            self.store.byte_count(doc),
            encode(
                &self
                    .store
                    .path(doc)
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
            encode(&self.store.title_note(doc).unwrap_or_default()),
        )
    }

    fn text_of(&self, doc: DocId) -> Option<String> {
        let view = self.store.view(doc)?;
        let snapshot = view.worker_snapshot();
        let mut out = String::new();
        let end = snapshot.len_bytes();
        snapshot.visit_chunks(qe_api::ByteOffset(0)..end, &mut |chunk| out.push_str(chunk));
        Some(out)
    }

    fn tools_status(&self) -> String {
        let Some(win) = self
            .window_in_use()
            .and_then(|window| self.windows.get(&window))
        else {
            return "error no window".to_owned();
        };
        format!(
            "bar={} line={} status={}",
            win.tools.is_bar_open(),
            win.tools.is_select_line_open(),
            encode(win.tools.status().unwrap_or(""))
        )
    }

    /// The bar's own path for Replace All, through the tools' test hooks.
    fn test_replace_all(&mut self, find: &str, with: &str) {
        let (Some(window), Some(doc)) = (self.window_in_use(), self.doc_in_use()) else {
            return;
        };
        let measure = self.measure.clone();
        let mut ctx = ViewContext::default();
        if let (Some(win), Some(view)) = (self.windows.get_mut(&window), self.store.view_mut(doc)) {
            win.tools
                .run_command(CommandId::FindReplace, view, measure.as_ref(), &mut ctx);
            win.tools
                .set_query_for_test(find, view, measure.as_ref(), &mut ctx);
            win.tools.set_replace_for_test(with);
            win.tools.press_replace_all_for_test(view);
        }
        self.apply(window, ctx.requests);
        self.after_tools(window, doc);
        self.after_view_call(window, doc);
    }
}

fn row_text(model: &qe_ui::MenuModel, command: CommandId) -> Option<String> {
    for item in &model.items {
        if item.command == Some(command) {
            return Some(format!(
                "{}|{}|{}|{}",
                item.label, item.key_text, item.enabled, item.checked
            ));
        }
        if let Some(sub) = &item.submenu
            && let Some(found) = row_text(sub, command)
        {
            return Some(found);
        }
    }
    None
}

/// The nth row of the menu that is open, whichever label holds it.
fn first_open_row(widget: &qe_api::Widget, index: usize) -> Option<qe_api::WidgetId> {
    if widget.role == qe_api::WidgetRole::Menu
        && let Some(row) = widget.children.get(index)
    {
        return Some(row.id);
    }
    widget
        .children
        .iter()
        .find_map(|child| first_open_row(child, index))
}

fn collect_ids(widget: &qe_api::Widget, ids: &mut Vec<u64>) {
    ids.push(widget.id.0);
    for child in &widget.children {
        collect_ids(child, ids);
    }
}

/// The channel's verbs that need no borrow of `App`.
pub fn alert_verb(shared: &Rc<Shared>, verb: &str, word: Option<&str>) {
    match verb {
        "alert" => match shared.alerts.describe() {
            Some((title, message, buttons)) => reply(&format!(
                "{} {} {}",
                encode(&title),
                encode(&message),
                encode(&buttons.join("|"))
            )),
            None => reply("none"),
        },
        "press" => {
            let index = word.and_then(|word| word.parse().ok());
            if shared.alerts.describe().is_none() {
                reply("error no alert");
            } else {
                reply("ok");
                shared.alerts.finish(shared, index);
            }
        }
        "alert-hwnd" => match shared.alerts.window() {
            Some(hwnd) => reply(&format!("{}", hwnd.0 as isize)),
            None => reply("none"),
        },
        "alert-post" => {
            let code = word
                .and_then(|word| word.parse::<usize>().ok())
                .unwrap_or(0);
            match shared.alerts.window() {
                Some(hwnd) => {
                    // SAFETY: a window of this thread.
                    let posted = unsafe {
                        PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(code), LPARAM(0)).and_then(
                            |()| PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(code), LPARAM(0)),
                        )
                    };
                    match posted {
                        Ok(()) => reply("ok"),
                        Err(e) => reply(&format!("error {e}")),
                    }
                }
                None => reply("error no alert"),
            }
        }
        _ => {
            let text = decode(word.unwrap_or(""));
            reply(if shared.alerts.set_field(&text) {
                "ok"
            } else {
                "error no field"
            });
        }
    }
}
