//! `--bench-keys N`: real keys through the app's own input path, in a hidden window, with the
//! same timer as `qe-editor`'s `editor` example. It runs on the UI thread with no borrow held.
use crate::app::{App, Effect, Shared, pump};
use qe_api::{ByteOffset, InputKind, Modifiers, ViewContext};
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_PAINT};

pub struct Bench {
    pub keys: usize,
    pub middle: bool,
    pub started: bool,
}
impl Bench {
    pub fn new(keys: usize, middle: bool) -> Self {
        Self {
            keys,
            middle,
            started: false,
        }
    }
}

impl App {
    /// Starts once a first frame has shown and no file is loading.
    pub fn bench_tick(&mut self) {
        let Some(bench) = &self.bench else {
            return;
        };
        if bench.started || self.timing.first_frame.get().is_none() {
            return;
        }
        let loading = self
            .store
            .all_documents()
            .iter()
            .any(|doc| self.store.is_loading(*doc));
        if loading || !self.restored {
            return;
        }
        if let Some(bench) = &mut self.bench {
            bench.started = true;
        }
        self.effects.push(Effect::RunBench);
    }
}

pub fn run(shared: &Rc<Shared>) {
    let found = shared.state.try_borrow().ok().and_then(|app| {
        let window = app.store.last_used_window()?;
        let bench = app.bench.as_ref()?;
        Some((
            window,
            app.windows.get(&window)?.hwnd,
            bench.keys,
            bench.middle,
        ))
    });
    let Some((window, hwnd, keys, middle)) = found else {
        return;
    };
    // SAFETY: a window of this thread.
    let paint = || unsafe {
        SendMessageW(hwnd, WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
    };
    let can_edit = shared
        .state
        .try_borrow()
        .ok()
        .and_then(|app| {
            let doc = app.store.active_tab(window)?;
            Some(app.store.view(doc)?.can_edit())
        })
        .unwrap_or(false);
    if middle && let Ok(mut app) = shared.state.try_borrow_mut() {
        let limit = Instant::now() + Duration::from_secs(60);
        while Instant::now() < limit {
            let done = app
                .store
                .active_tab(window)
                .and_then(|doc| app.store.view(doc))
                .is_none_or(|view| view.store_status().complete);
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(doc) = app.store.active_tab(window) {
            let mut ctx = ViewContext::default();
            if let Some(view) = app.store.view_mut(doc) {
                let at = ByteOffset(view.store_status().known_bytes.0 / 2);
                view.select(at..at, &mut ctx);
            }
            app.apply(window, ctx.requests);
        }
    }
    pump(shared);
    paint();
    let down = |code: u16| InputKind::KeyDown {
        virtual_key: code,
        modifiers: Modifiers::default(),
        repeat: false,
    };
    for index in 0..keys {
        let kind = match (can_edit, index % 2 == 0) {
            (true, true) => InputKind::Char('q'),
            (true, false) => down(0x08),
            (false, true) => down(0x28),
            (false, false) => down(0x26),
        };
        if let Ok(mut app) = shared.state.try_borrow_mut() {
            app.send_input(window, kind);
        }
        pump(shared);
        paint();
        // A key on a shaped line ends when its worker's result is on screen.
        let limit = Instant::now() + Duration::from_secs(1);
        while !shared.timing.keys_waiting.borrow().is_empty() && Instant::now() < limit {
            std::thread::sleep(Duration::from_micros(100));
            if let Ok(mut app) = shared.state.try_borrow_mut() {
                app.tick_window(window);
            }
            pump(shared);
            paint();
        }
        // Workers run while a person's fingers move. Their results are not key work.
        std::thread::sleep(Duration::from_millis(3));
        if let Ok(mut app) = shared.state.try_borrow_mut() {
            app.tick_window(window);
        }
        pump(shared);
    }
    let label = if can_edit {
        "typing and Backspace, editable file"
    } else {
        "Down and Up, read-only file"
    };
    shared.timing.report(label, shared.process_start);
    if let Ok(mut app) = shared.state.try_borrow_mut() {
        // The bench types text that no one asked to keep.
        app.bench = None;
        app.effects.push(Effect::Exit);
    }
    pump(shared);
}
