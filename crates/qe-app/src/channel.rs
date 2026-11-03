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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock,
};
use windows::Win32::System::ProcessStatus::{
    EnumProcessModules, GetModuleBaseNameW, GetModuleFileNameExW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::Shell::DROPFILES;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GW_OWNER, GetClassNameW, GetWindow, GetWindowRect, PostMessageW,
    SendMessageW, WM_CLOSE, WM_DPICHANGED, WM_DROPFILES, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_PAINT, WM_SETTINGCHANGE,
};
use windows::core::BOOL;

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
pub fn start(shared: &Rc<Shared>, watchdog_ms: u64) {
    let inbox: Inbox = Arc::default();
    if INBOX.set(inbox.clone()).is_err() {
        return;
    }
    let wake = shared.state.borrow().wake.clone();
    if watchdog_ms > 0 {
        start_watchdog(inbox.clone(), wake.clone(), watchdog_ms);
    }
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

static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);

/// A test's watchdog. It asks the UI thread for a beat through the message loop, and if none
/// comes within the limit, the process ends with code 99. So a hang, a pump that never ends
/// included, fails a test at once with a clear reason.
fn start_watchdog(inbox: Inbox, wake: Arc<dyn Fn() + Send + Sync>, limit_ms: u64) {
    let began = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("qe-test-watchdog".into())
        .spawn(move || {
            let mut asked = 0;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(mut lines) = inbox.lock() {
                    lines.push("beat".to_owned());
                }
                wake();
                let now = began.elapsed().as_millis() as u64;
                asked = asked.max(LAST_BEAT_MS.load(Ordering::Acquire));
                if now.saturating_sub(asked) > limit_ms {
                    eprintln!("WATCHDOG: the UI thread gave no answer for {limit_ms} ms");
                    std::process::exit(99);
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("The watchdog could not start: {e}");
    }
    BEAT_START.get_or_init(|| began);
}
static BEAT_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

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
    pub fn doc_in_use(&self) -> Option<DocId> {
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
            "beat" => {
                if let Some(began) = BEAT_START.get() {
                    LAST_BEAT_MS.store(began.elapsed().as_millis() as u64, Ordering::Release);
                }
                return;
            }
            "compose" | "tsf-focus" | "commit-now" | "compose-cancel" => {
                self.effects.push(Effect::ChannelIme(
                    verb.to_owned(),
                    rest.first().map(|word| (*word).to_owned()),
                ));
                return;
            }
            "composing" => self
                .doc_in_use()
                .is_some_and(|doc| self.ime.is_composing(doc))
                .to_string(),
            "pause-workers" => {
                let paused = rest.first() == Some(&"true");
                if let Some(view) = self.doc_in_use().and_then(|doc| self.store.view_mut(doc)) {
                    view.test_pause_workers(paused);
                }
                "ok".to_owned()
            }
            "window-enabled" => match window.and_then(|window| self.windows.get(&window)) {
                // SAFETY: a plain query.
                Some(win) => unsafe { IsWindowEnabled(win.hwnd) }.as_bool().to_string(),
                None => "error no window".to_owned(),
            },
            "frame-text" => {
                let Some(window) = window else {
                    self.say("error no window".to_owned());
                    return;
                };
                self.effects.push(Effect::ChannelFrameText(window));
                return;
            }
            "named" => {
                // A control found by its name, through a screen reader's action or a real click.
                let name = decode(rest.get(1).copied().unwrap_or(""));
                let found = window.and_then(|window| {
                    let tree = self.widget_tree(window)?;
                    let widget = find_named(&tree, &name)?;
                    Some((window, widget.id, widget.bounds))
                });
                match (found, rest.first().copied()) {
                    (Some((window, id, _)), Some("invoke")) => {
                        self.widget_action(window, id, qe_a11y::WidgetAction::Invoke);
                        "ok".to_owned()
                    }
                    (Some((window, _, bounds)), Some("click")) => {
                        let at = qe_api::Point {
                            x: qe_api::Dip(bounds.x.0 + bounds.width.0 / 2.0),
                            y: qe_api::Dip(bounds.y.0 + bounds.height.0 / 2.0),
                        };
                        for down in [true, false] {
                            let kind = if down {
                                InputKind::MouseDown {
                                    at,
                                    button: qe_api::MouseButton::Left,
                                    modifiers: Modifiers::default(),
                                }
                            } else {
                                InputKind::MouseUp {
                                    at,
                                    button: qe_api::MouseButton::Left,
                                    modifiers: Modifiers::default(),
                                }
                            };
                            self.send_input(window, kind);
                        }
                        "ok".to_owned()
                    }
                    _ => format!("error no control named {name}"),
                }
            }
            "spell-menu" => {
                let at = rest.first().and_then(|n| n.parse().ok()).unwrap_or(0);
                let found = self.doc_in_use().is_some_and(|doc| {
                    let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view(doc))
                    else {
                        return false;
                    };
                    let menu = ui
                        .spell
                        .as_mut()
                        .and_then(|service| service.menu_at(view, qe_api::ByteOffset(at)));
                    ui.found = menu;
                    ui.found.is_some()
                });
                found.to_string()
            }
            "spell-row" => {
                let row = decode(rest.first().copied().unwrap_or(""));
                match window {
                    Some(window) => {
                        self.run_command(
                            window,
                            CommandId::CheckDocumentSpelling,
                            Some(format!("{}{row}", crate::commands::SPELL_ROW)),
                        );
                        "ok".to_owned()
                    }
                    None => "error no window".to_owned(),
                }
            }
            "drop-file" => {
                // A real `WM_DROPFILES` for a name given in UTF-16 units, as hex.
                let wide: Vec<u16> = rest
                    .first()
                    .copied()
                    .unwrap_or("")
                    .as_bytes()
                    .chunks(4)
                    .filter_map(|unit| {
                        u16::from_str_radix(std::str::from_utf8(unit).ok()?, 16).ok()
                    })
                    .collect();
                match window.and_then(|window| self.windows.get(&window)) {
                    Some(win) => post_drop(win.hwnd, &wide),
                    None => "error no window".to_owned(),
                }
            }
            "modules" => loaded_modules().join(" "),
            "module-paths" => encode(&loaded_module_paths().join("\n")),
            "com-check" => com_check(),
            "say" => {
                self.note(decode(rest.first().copied().unwrap_or("")));
                "ok".to_owned()
            }
            "close-window-at" => {
                // The window's own X, on a window that need not be the one in use.
                let at = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                match self.windows.values().nth(at) {
                    Some(win) => {
                        // SAFETY: a window of this thread.
                        match unsafe {
                            PostMessageW(Some(win.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0))
                        } {
                            Ok(()) => "ok".to_owned(),
                            Err(e) => format!("error {e}"),
                        }
                    }
                    None => "error no such window".to_owned(),
                }
            }
            "hwnd-at" => {
                let at = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                self.windows.values().nth(at).map_or_else(
                    || "none".to_owned(),
                    |win| format!("{}", win.hwnd.0 as isize),
                )
            }
            "enabled-at" => {
                let at = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                match self.windows.values().nth(at) {
                    // SAFETY: a plain query.
                    Some(win) => unsafe { IsWindowEnabled(win.hwnd) }.as_bool().to_string(),
                    None => "error no such window".to_owned(),
                }
            }
            "tab-x" => {
                // The tab's own X, as the shell reports it.
                let tab = window.zip(self.doc_in_use()).and_then(|(window, doc)| {
                    let win = self.windows.get(&window)?;
                    let (_, tab) = win.tabs.iter().find(|(known, _)| *known == doc)?;
                    Some((window, *tab))
                });
                match tab {
                    Some((window, tab)) => {
                        self.close_tab_from_shell(window, tab);
                        "ok".to_owned()
                    }
                    None => "error no tab".to_owned(),
                }
            }
            "queued-keys" => self
                .doc_in_use()
                .and_then(|doc| self.store.view(doc))
                .map_or(0, qe_editor::EditorView::queued_input_count)
                .to_string(),
            "gutter-reads" => self
                .doc_in_use()
                .and_then(|doc| self.docs.get(&doc))
                .map_or(0, |ui| ui.frame.counts().gutter_reads)
                .to_string(),
            "collapse" => match window {
                Some(window) => {
                    self.widget_action(
                        window,
                        qe_api::WidgetId(0),
                        qe_a11y::WidgetAction::Collapse,
                    );
                    "ok".to_owned()
                }
                None => "error no window".to_owned(),
            },
            "invoke-id" => match (window, rest.first().and_then(|n| n.parse().ok())) {
                (Some(window), Some(id)) => {
                    self.widget_action(window, qe_api::WidgetId(id), qe_a11y::WidgetAction::Invoke);
                    "ok".to_owned()
                }
                _ => "error bad invoke-id".to_owned(),
            },
            "break-loop" => {
                if let Some(shared) = self.me.upgrade() {
                    shared.loop_broken.set(true);
                    (shared.wake)();
                }
                "ok".to_owned()
            }
            "exit-pauses" => {
                let number = |at: usize| {
                    rest.get(at)
                        .and_then(|n| n.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                if let Some(shared) = self.me.upgrade() {
                    shared.exit_pauses.set((number(0), number(1)));
                }
                "ok".to_owned()
            }
            "fail-windows" => {
                self.fail_windows = rest.first().and_then(|n| n.parse().ok()).unwrap_or(0);
                "ok".to_owned()
            }
            "forward-window" => {
                let at = self
                    .forward_window
                    .and_then(|window| self.windows.keys().position(|known| *known == window));
                at.map_or_else(|| "none".to_owned(), |at| at.to_string())
            }
            "last-used-window" => {
                let at = self
                    .store
                    .last_used_window()
                    .and_then(|window| self.windows.keys().position(|known| *known == window));
                at.map_or_else(|| "none".to_owned(), |at| at.to_string())
            }
            "post" => {
                // Any message to the window's own procedure, as Windows would send it.
                let number = |at: usize| {
                    rest.get(at)
                        .and_then(|n| n.parse::<usize>().ok())
                        .unwrap_or(0)
                };
                match window.and_then(|window| self.windows.get(&window)) {
                    Some(win) => {
                        // SAFETY: a window of this thread.
                        match unsafe {
                            PostMessageW(
                                Some(win.hwnd),
                                number(0) as u32,
                                WPARAM(number(1)),
                                LPARAM(number(2) as isize),
                            )
                        } {
                            Ok(()) => "ok".to_owned(),
                            Err(e) => format!("error {e}"),
                        }
                    }
                    None => "error no window".to_owned(),
                }
            }
            // `WM_ENDSESSION` during a borrow of `App`, with the end Windows may bring when the
            // message's answer comes.
            "session-end-then-die" => match window.and_then(|window| self.windows.get(&window)) {
                Some(win) => {
                    // SAFETY: a window of this thread, and the end of this process.
                    unsafe {
                        SendMessageW(win.hwnd, 0x16, Some(WPARAM(1)), Some(LPARAM(0)));
                        let _ = windows::Win32::System::Threading::TerminateProcess(
                            windows::Win32::System::Threading::GetCurrentProcess(),
                            0,
                        );
                    }
                    "ok".to_owned()
                }
                None => "error no window".to_owned(),
            },
            "popup-hwnd" => thread_window_of_class("QuickEditPopup")
                .map_or_else(|| "none".to_owned(), |hwnd| format!("{}", hwnd.0 as isize)),
            "settings-hwnd" => {
                let hwnd = self.me.upgrade().and_then(|shared| {
                    let slot = shared.settings_window.try_borrow().ok()?;
                    slot.as_ref()?.hwnd()
                });
                hwnd.map_or_else(|| "none".to_owned(), |hwnd| format!("{}", hwnd.0 as isize))
            }
            "get-bool" => {
                let key = rest.first().and_then(|name| parse_setting(name));
                match key {
                    Some(key) => format!("{:?}", self.settings.get_bool(key)),
                    None => format!("error bad get-bool {rest:?}"),
                }
            }
            "stdin-ended" => {
                self.begin_quit();
                return;
            }
            "ping" => "ok".to_owned(),
            "view-info" => self.view_info(),
            "panel-info" => self.panel_info(),
            "menu-dump" => {
                let index = rest.first().and_then(|n| n.parse().ok()).unwrap_or(0);
                match window {
                    Some(window) => {
                        let model = self.menu_model(window, index);
                        let mut lines = Vec::new();
                        dump_rows(&model, 0, &mut lines);
                        encode(&lines.join("\n"))
                    }
                    None => "error no window".to_owned(),
                }
            }
            "set-int" => {
                let key = rest.first().and_then(|name| parse_setting(name));
                let value = rest.get(1).and_then(|n| n.parse::<i64>().ok());
                match (key, value) {
                    (Some(key), Some(value)) => match self.settings.set_integer(key, value) {
                        Ok(()) => "ok".to_owned(),
                        Err(e) => format!("error {e}"),
                    },
                    _ => format!("error bad set-int {rest:?}"),
                }
            }
            "set-text" => {
                let key = rest.first().and_then(|name| parse_setting(name));
                let value = decode(rest.get(1).copied().unwrap_or(""));
                match key {
                    Some(key) => match self.settings.set_text(key, value) {
                        Ok(()) => "ok".to_owned(),
                        Err(e) => format!("error {e}"),
                    },
                    None => format!("error bad set-text {rest:?}"),
                }
            }
            "encodings" => {
                let names: Vec<&str> = self
                    .settings
                    .enabled_encodings()
                    .into_iter()
                    .map(qe_api::TextEncoding::name)
                    .collect();
                encode(&names.join("|"))
            }
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
                    pdf: job.options.pdf,
                    note: None,
                };
                self.handle(Incoming::Panel(job, Some(choice)));
                "ok".to_owned()
            }
            "panel-pdf" => {
                // The PDF dialog's answer: the path, then the header box and the rewrap box.
                let Some(job) = self.test_panels.pop_front() else {
                    self.say("error no panel".to_owned());
                    return;
                };
                let choice = PanelChoice {
                    path: decode(rest.first().copied().unwrap_or("")),
                    row: None,
                    add_txt: None,
                    pdf: Some(qe_ui::PdfChecks {
                        header: rest.get(1) == Some(&"true"),
                        rewrap: Some(rest.get(2) == Some(&"true")),
                    }),
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
            "alert"
            | "press"
            | "alert-field"
            | "alert-post"
            | "alert-hwnd"
            | "alert-x"
            | "alert-owner"
            | "alert-dpi"
            | "alert-scale"
            | "alert-theme"
            | "alert-mouse"
            | "alert-setting-change" => {
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

    /// What a document's view holds now, as `name=value` words.
    fn view_info(&mut self) -> String {
        let Some(doc) = self.doc_in_use() else {
            return "error no document".to_owned();
        };
        let measure = self.measure.clone();
        let (Some(ui), Some(view)) = (self.docs.get(&doc), self.store.view_mut(doc)) else {
            return "error no view".to_owned();
        };
        let frame = &ui.frame;
        let token = format!("{} {}", frame.font_family(), frame.base_points());
        let cell = qe_api::TextMeasurer::measure(measure.as_ref(), "0000000000", &token)
            .width
            .0
            / 10.0;
        let row = qe_api::TextMeasurer::measure(measure.as_ref(), "0\n0", &token)
            .height
            .0
            - qe_api::TextMeasurer::measure(measure.as_ref(), "0", &token)
                .height
                .0;
        let paper = frame.paper();
        format!(
            "family={} points={} tab={} zoom={} page={} lines={} map={} paper={}x{} textw={} \
             texth={} row={row} cell={} gutterw={} mapw={} top={}",
            encode(frame.font_family()),
            frame.base_points(),
            frame.tab_width(),
            frame.zoom(),
            frame.page_wrap(),
            frame.line_numbers(),
            frame.document_map(),
            paper.width,
            paper.height,
            frame.window_width_at_100_percent(view),
            view.text_area().height.0,
            cell,
            frame.gutter_area().width.0,
            frame.map_area().width.0,
            view.test_byte_at_top(),
        )
    }

    /// The first waiting file dialog and its controls.
    fn panel_info(&self) -> String {
        let Some(job) = self.test_panels.front() else {
            return "none".to_owned();
        };
        let rows: Vec<String> = job
            .options
            .encodings
            .iter()
            .map(|row| format!("{}:{}", row.name, row.enabled))
            .collect();
        format!(
            "file={} selected={} txt={:?} header={:?} rewrap={:?} rows={}",
            encode(job.options.file_name.as_deref().unwrap_or("")),
            job.options.selected,
            job.options.add_txt,
            job.options.pdf.map(|checks| checks.header),
            job.options.pdf.and_then(|checks| checks.rewrap),
            encode(&rows.join("|")),
        )
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

/// Every row of a menu, one per line, with its depth: `depth|label|key|on|checked`.
fn dump_rows(model: &qe_ui::MenuModel, depth: usize, lines: &mut Vec<String>) {
    for item in &model.items {
        lines.push(format!(
            "{depth}|{}|{}|{}|{}",
            item.label, item.key_text, item.enabled, item.checked
        ));
        if let Some(sub) = &item.submenu {
            dump_rows(sub, depth + 1, lines);
        }
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

fn find_named<'a>(widget: &'a qe_api::Widget, name: &str) -> Option<&'a qe_api::Widget> {
    if widget.name == name {
        return Some(widget);
    }
    widget
        .children
        .iter()
        .find_map(|child| find_named(child, name))
}

/// The base names of every loaded module, in lower case.
fn loaded_modules() -> Vec<String> {
    // SAFETY: the buffer lives through the call, and the handle is the pseudo handle.
    each_module(|module, name| unsafe {
        GetModuleBaseNameW(GetCurrentProcess(), Some(module), name)
    })
    .into_iter()
    .map(|name| name.to_lowercase())
    .collect()
}

fn loaded_module_paths() -> Vec<String> {
    // SAFETY: the buffer lives through the call, and the handle is the pseudo handle.
    each_module(|module, name| unsafe {
        GetModuleFileNameExW(Some(GetCurrentProcess()), Some(module), name)
    })
}

/// Makes each COM object that a run may need, with no window, on a thread of its own, under the
/// rule for where DLLs load from. "ok", or the first that failed.
fn com_check() -> String {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::core::{GUID, IUnknown};
    let check = || {
        // SAFETY: plain COM set-up on a thread that this function alone uses.
        if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
            return format!("error COM set-up: {e}");
        }
        for (name, class) in [
            ("file open dialog", 0xDC1C5A9C_E88A_4DDE_A5A1_60F82A20AEF7),
            ("file save dialog", 0xC0B4E2F3_BA21_4773_8DBA_335EC946EB8B),
            ("spelling factory", 0x7AB36653_1796_484B_BDFA_E74F1DB7C1DC),
            ("text services", 0x529A9E6B_6587_4F23_AB9E_9C7D683E3C50),
        ] {
            // SAFETY: the class id lives through the call. The object is released at its drop.
            let made: windows::core::Result<IUnknown> =
                unsafe { CoCreateInstance(&GUID::from_u128(class), None, CLSCTX_INPROC_SERVER) };
            if let Err(e) = made {
                return format!("error {name}: {e}");
            }
        }
        "ok".to_owned()
    };
    match std::thread::spawn(check).join() {
        Ok(answer) => answer,
        Err(_) => "error the check's thread stopped".to_owned(),
    }
}

fn each_module(read: impl Fn(HMODULE, &mut [u16]) -> u32) -> Vec<String> {
    let mut modules = vec![HMODULE::default(); 1024];
    let mut needed = 0;
    // SAFETY: the buffer and the count live through each call.
    unsafe {
        let process = GetCurrentProcess();
        if let Err(e) = EnumProcessModules(
            process,
            modules.as_mut_ptr(),
            (modules.len() * size_of::<HMODULE>()) as u32,
            &mut needed,
        ) {
            return vec![format!("error-{e}")];
        }
        modules.truncate(needed as usize / size_of::<HMODULE>());
        modules
            .iter()
            .map(|module| {
                let mut name = [0u16; 1024];
                let length = read(*module, &mut name) as usize;
                String::from_utf16_lossy(&name[..length])
            })
            .collect()
    }
}

/// This thread's top-level window of a class, for a popup that a test asks about first.
fn thread_window_of_class(class: &str) -> Option<HWND> {
    struct Search<'a> {
        class: &'a str,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Search` below, which lives through `EnumThreadWindows`.
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut name = [0u16; 128];
        // SAFETY: the buffer lives through the call.
        let length = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
        if String::from_utf16_lossy(&name[..length]) == search.class {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { class, found: None };
    // SAFETY: the pointer lives through the call, which runs on this thread. Stopping early
    // comes back as an error, and the search says what it found.
    let _ = unsafe {
        EnumThreadWindows(
            GetCurrentThreadId(),
            Some(each),
            LPARAM(&raw mut search as isize),
        )
    };
    search.found
}

/// Builds the block a drop hands over, and posts it as Windows does.
fn post_drop(hwnd: HWND, wide: &[u16]) -> String {
    let header = size_of::<DROPFILES>();
    let bytes = header + (wide.len() + 2) * 2;
    // SAFETY: the block is big enough for the header and the name with its two zeros. The
    // handler ends it with `DragFinish`, as for a real drop.
    unsafe {
        let block = match GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes) {
            Ok(block) => block,
            Err(e) => return format!("error {e}"),
        };
        let at = GlobalLock(block).cast::<u8>();
        let drop_files = at.cast::<DROPFILES>();
        (*drop_files).pFiles = header as u32;
        (*drop_files).fWide = BOOL(1);
        std::ptr::copy_nonoverlapping(wide.as_ptr(), at.add(header).cast::<u16>(), wide.len());
        let _ = GlobalUnlock(block);
        match PostMessageW(
            Some(hwnd),
            WM_DROPFILES,
            WPARAM(block.0 as usize),
            LPARAM(0),
        ) {
            Ok(()) => "ok".to_owned(),
            Err(e) => format!("error {e}"),
        }
    }
}

/// Paints one frame and says what text it held.
pub fn frame_text(shared: &Rc<Shared>, window: WindowId) {
    let found = shared.state.try_borrow().ok().and_then(|app| {
        let win = app.windows.get(&window)?;
        Some((win.handle.clone(), win.hwnd))
    });
    let Some((handle, hwnd)) = found else {
        reply("error no window");
        return;
    };
    if let Err(e) = handle.invalidate_content() {
        reply(&format!("error {e}"));
        return;
    }
    // SAFETY: a window of this thread. A hidden window gets no WM_PAINT from Windows.
    unsafe {
        SendMessageW(hwnd, WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    let text = shared.state.try_borrow().ok().and_then(|app| {
        let win = app.windows.get(&window)?;
        Some(win.painted_text.join("\n"))
    });
    reply(&encode(&text.unwrap_or_default()));
}

/// The channel's verbs that reach TSF, which needs `App` free.
pub fn ime_verb(shared: &Rc<Shared>, verb: &str, word: Option<&str>) {
    let doc = shared
        .state
        .try_borrow()
        .ok()
        .and_then(|app| app.doc_in_use());
    let Some(doc) = doc else {
        reply("error no document");
        return;
    };
    match verb {
        "compose" => {
            let text = decode(word.unwrap_or(""));
            match shared.ime.compose_for_test(doc, &text) {
                Ok(()) => reply("ok"),
                Err(e) => reply(&format!("error {e}")),
            }
        }
        "compose-cancel" => {
            shared.ime.cancel(doc);
            reply("ok");
        }
        "tsf-focus" => reply(if shared.ime.has_focus(doc) {
            "doc"
        } else {
            "other"
        }),
        _ => {
            shared.ime.commit_docs(&[doc]);
            reply("ok");
        }
    }
}

/// The channel's verbs that need no borrow of `App`.
pub fn alert_verb(shared: &Rc<Shared>, verb: &str, word: Option<&str>) {
    match verb {
        "alert-x" => match shared.alerts.window() {
            Some(hwnd) => {
                // SAFETY: a window of this thread. The window's own X sends this message.
                let posted = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                match posted {
                    Ok(()) => reply("ok"),
                    Err(e) => reply(&format!("error {e}")),
                }
            }
            None => reply("error no alert"),
        },
        "alert-owner" => match shared.alerts.window() {
            Some(hwnd) => {
                // SAFETY: a plain query. No owner comes back as an error.
                let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
                reply(&format!("{}", owner.map_or(0, |owner| owner.0 as isize)));
            }
            None => reply("none"),
        },
        "alert-dpi" => match shared.alerts.window() {
            Some(hwnd) => {
                let dpi = word
                    .and_then(|word| word.parse::<usize>().ok())
                    .unwrap_or(96);
                let mut rect = RECT::default();
                let old = shared.alerts.look().map_or(1.0, |(scale, _)| scale);
                // SAFETY: the rectangle lives through both calls, on this thread's window.
                unsafe {
                    let _ = GetWindowRect(hwnd, &raw mut rect);
                    // As Windows suggests: the same top left, and the size at the new scale.
                    let grow = dpi as f32 / 96.0 / old;
                    rect.right = rect.left + ((rect.right - rect.left) as f32 * grow) as i32;
                    rect.bottom = rect.top + ((rect.bottom - rect.top) as f32 * grow) as i32;
                    SendMessageW(
                        hwnd,
                        WM_DPICHANGED,
                        Some(WPARAM(dpi | (dpi << 16))),
                        Some(LPARAM(&raw const rect as isize)),
                    );
                }
                reply("ok");
            }
            None => reply("error no alert"),
        },
        "alert-setting-change" => match shared.alerts.window() {
            Some(hwnd) => {
                // SAFETY: a window of this thread.
                unsafe {
                    SendMessageW(hwnd, WM_SETTINGCHANGE, Some(WPARAM(0)), Some(LPARAM(0)));
                }
                reply("ok");
            }
            None => reply("error no alert"),
        },
        "alert-scale" => match shared.alerts.look() {
            Some((scale, _)) => reply(&format!("{scale}")),
            None => reply("none"),
        },
        "alert-theme" => match shared.alerts.look() {
            Some((_, theme)) => reply(&format!("{:?}", theme.kind)),
            None => reply("none"),
        },
        "alert-mouse" => {
            // A real click on a button of the open alert, at its place on screen by this scale.
            let index = word
                .and_then(|word| word.parse::<usize>().ok())
                .unwrap_or(0);
            match (shared.alerts.window(), shared.alerts.button_center(index)) {
                (Some(hwnd), Some((x, y))) => {
                    let l = LPARAM((x as isize & 0xffff) | ((y as isize & 0xffff) << 16));
                    // SAFETY: a window of this thread.
                    let posted = unsafe {
                        PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), l)
                            .and_then(|()| PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), l))
                    };
                    match posted {
                        Ok(()) => reply("ok"),
                        Err(e) => reply(&format!("error {e}")),
                    }
                }
                _ => reply("error no such button"),
            }
        }
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
