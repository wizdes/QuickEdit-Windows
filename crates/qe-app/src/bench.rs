//! `--bench-keys N`: real key messages, posted to the hidden window, through the real message
//! loop, `TranslateMessage` and the shell, with TSF running. Each key's timer starts when the
//! message loop takes the message, and ends at the first present of a frame that shows it.
//!
//! What a hidden run cannot measure: a hidden window has an empty paint rectangle, so the last
//! step, the copy of the pixels to the screen, does not happen. `--timings` in a visible run
//! measures that too, with real keys.
use crate::app::{App, Effect, Shared, pump};
use qe_api::{ByteOffset, InputKind, ViewContext};
use qe_docs::WindowId;
use qe_shell::{Tab, TabId};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP};

/// How long one key may take before the bench goes on without it.
const KEY_LIMIT: Duration = Duration::from_secs(1);
/// The gap between keys. Workers run while a person's fingers move.
const KEY_GAP: Duration = Duration::from_millis(3);
/// The gap between keys in a paced run: a very fast typist, and time for timers to fire.
const PACED_GAP: Duration = Duration::from_millis(8);

pub struct Bench {
    pub keys: usize,
    pub middle: bool,
    pub started: bool,
    paced: bool,
    tab_motion: bool,
    sent: usize,
    run: Option<Run>,
}
/// The thread that posts the keys of a paced run.
struct Pacer {
    sent: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
    /// The tab that comes and goes, to keep motion running.
    extra: Option<TabId>,
}
struct Run {
    window: WindowId,
    hwnd: HWND,
    can_edit: bool,
    /// The middle of the text is not known until the whole index is.
    waiting_for_index: Option<Instant>,
    /// The key on its way, and when the bench stops waiting for it.
    key_limit: Option<Instant>,
    pacer: Option<Pacer>,
}
impl Bench {
    pub fn new(keys: usize, middle: bool, paced: bool, tab_motion: bool) -> Self {
        Self {
            keys,
            middle,
            started: false,
            paced,
            tab_motion,
            sent: 0,
            run: None,
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

/// Starts the bench. The message loop runs it from here on.
pub fn run(shared: &Rc<Shared>) {
    let Ok(mut app) = shared.state.try_borrow_mut() else {
        return;
    };
    let Some(window) = app.store.last_used_window() else {
        return;
    };
    let Some(hwnd) = app.windows.get(&window).map(|win| win.hwnd) else {
        return;
    };
    let can_edit = app
        .store
        .active_tab(window)
        .and_then(|doc| app.store.view(doc))
        .is_some_and(qe_editor::EditorView::can_edit);
    let middle = app.bench.as_ref().is_some_and(|bench| bench.middle);
    // The keyboard comes to this window, so TSF and the caret run as they do for a person.
    app.send_input(window, InputKind::Focus);
    if let Some(bench) = &mut app.bench {
        bench.run = Some(Run {
            window,
            hwnd,
            can_edit,
            waiting_for_index: middle.then(|| Instant::now() + Duration::from_secs(60)),
            key_limit: None,
            pacer: None,
        });
    }
    if app.bench.as_ref().is_some_and(|bench| bench.tab_motion)
        && let Some(win) = app.windows.get(&window)
    {
        // A hidden window has no motion of its own, and a real list waits a second between
        // changes. Here something moves all the time, which is far more than a person sees.
        let started = win
            .handle
            .set_animation_setting_off(false)
            .and_then(|()| win.handle.force_motion(Some(true)))
            .and_then(|()| {
                win.handle
                    .with_strip(|strip| strip.set_motion_quiet_ms(0.0))
            });
        if let Err(e) = started {
            eprintln!("The bench could not start motion: {e}");
        }
    }
    drop(app);
    pump(shared);
    shared.wake_window.post_bench_step();
}

/// One turn. It always gives the message loop its own turn before the next.
pub fn step(shared: &Rc<Shared>) {
    let Ok(mut app) = shared.state.try_borrow_mut() else {
        shared.wake_window.post_bench_step();
        return;
    };
    let App { bench, store, .. } = &mut *app;
    let Some(bench) = bench else {
        return;
    };
    let keys = bench.keys;
    let sent = bench.sent;
    let Some(run) = &mut bench.run else {
        return;
    };
    let window = run.window;
    if let Some(limit) = run.waiting_for_index {
        let doc = store.active_tab(window);
        let done = doc
            .and_then(|doc| store.view(doc))
            .is_none_or(|view| view.store_status().complete);
        if !done && Instant::now() < limit {
            drop(app);
            std::thread::sleep(Duration::from_millis(5));
            shared.wake_window.post_bench_step();
            return;
        }
        run.waiting_for_index = None;
        if let Some(doc) = doc {
            let mut ctx = ViewContext::default();
            if let Some(view) = store.view_mut(doc) {
                let at = ByteOffset(view.store_status().known_bytes.0 / 2);
                view.select(at..at, &mut ctx);
            }
            app.apply(window, ctx.requests);
        }
        drop(app);
        pump(shared);
        shared.wake_window.post_bench_step();
        return;
    }
    // The key on its way is done when a frame has shown it, or when its time is up.
    if let Some(limit) = run.key_limit
        && shared.timing.keys_done() < sent
        && Instant::now() < limit
    {
        drop(app);
        std::thread::sleep(Duration::from_micros(100));
        shared.wake_window.post_bench_step();
        return;
    }
    run.key_limit = None;
    if bench.paced {
        return paced_step(shared, app);
    }
    if sent >= keys {
        let can_edit = run.can_edit;
        let label = if can_edit {
            "typing and Backspace, editable file"
        } else {
            "Down and Up, read-only file"
        };
        shared.timing.report(label, shared.process_start);
        // The bench types text that no one asked to keep.
        app.bench = None;
        app.effects.push(Effect::Exit);
        drop(app);
        pump(shared);
        return;
    }
    let code: usize = match (run.can_edit, sent % 2 == 0) {
        (true, true) => 0x51, // Q
        (true, false) => 0x08,
        (false, true) => 0x28,
        (false, false) => 0x26,
    };
    run.key_limit = Some(Instant::now() + KEY_LIMIT);
    let hwnd = run.hwnd;
    bench.sent += 1;
    drop(app);
    std::thread::sleep(KEY_GAP);
    // SAFETY: a window of this thread. The scan code in the high bits lets TranslateMessage
    // make its character, as for a real key.
    let posted = unsafe {
        let scan = isize::try_from(windows::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(
            code as u32,
            windows::Win32::UI::Input::KeyboardAndMouse::MAPVK_VK_TO_VSC,
        ))
        .unwrap_or(0);
        PostMessageW(
            Some(hwnd),
            WM_KEYDOWN,
            WPARAM(code),
            LPARAM(1 | (scan << 16)),
        )
        .and_then(|()| {
            PostMessageW(
                Some(hwnd),
                WM_KEYUP,
                WPARAM(code),
                LPARAM(1 | (scan << 16) | (1 << 30) | (1 << 31)),
            )
        })
    };
    if let Err(e) = posted {
        eprintln!("The bench could not post a key: {e}");
    }
    shared.wake_window.post_bench_step();
}

fn post_key(hwnd: isize, code: usize) {
    let hwnd = HWND(hwnd as *mut std::ffi::c_void);
    // SAFETY: Windows checks the handle. The scan code in the high bits lets TranslateMessage
    // make its character, as for a real key.
    let posted = unsafe {
        let scan = isize::try_from(windows::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(
            code as u32,
            windows::Win32::UI::Input::KeyboardAndMouse::MAPVK_VK_TO_VSC,
        ))
        .unwrap_or(0);
        PostMessageW(
            Some(hwnd),
            WM_KEYDOWN,
            WPARAM(code),
            LPARAM(1 | (scan << 16)),
        )
        .and_then(|()| {
            PostMessageW(
                Some(hwnd),
                WM_KEYUP,
                WPARAM(code),
                LPARAM(1 | (scan << 16) | (1 << 30) | (1 << 31)),
            )
        })
    };
    if let Err(e) = posted {
        eprintln!("The bench could not post a key: {e}");
    }
}

/// A paced run posts from another thread, so that this loop rests as it does for a person, and
/// the timers fire. Each post of a step lets the bench look at where it stands.
fn paced_step(shared: &Rc<Shared>, mut app: std::cell::RefMut<'_, App>) {
    let step_target = shared.wake_window.bench_step_target();
    let App { bench, .. } = &mut *app;
    let Some(bench) = bench else {
        return;
    };
    let (keys, tab_motion) = (bench.keys, bench.tab_motion);
    let Some(run) = &mut bench.run else {
        return;
    };
    let (window, hwnd, can_edit) = (run.window, run.hwnd.0 as isize, run.can_edit);
    let pacer = run.pacer.get_or_insert_with(|| {
        let sent = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_sent, thread_stop) = (sent.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                std::thread::sleep(PACED_GAP);
                let done = thread_sent.load(Ordering::Acquire);
                if done < keys {
                    post_key(
                        hwnd,
                        match (can_edit, done % 2 == 0) {
                            (true, true) => 0x51,
                            (true, false) => 0x08,
                            (false, true) => 0x28,
                            (false, false) => 0x26,
                        },
                    );
                    thread_sent.store(done + 1, Ordering::Release);
                }
                crate::platform::post_bench_step_to(step_target);
            }
        });
        Pacer {
            sent,
            stop,
            thread,
            extra: None,
        }
    });
    let all_sent = pacer.sent.load(Ordering::Acquire) >= keys;
    let mut extra = pacer.extra;
    if all_sent && shared.timing.keys_done() >= keys {
        pacer.stop.store(true, Ordering::Release);
        if let Some(pacer) = run.pacer.take()
            && pacer.thread.join().is_err()
        {
            eprintln!("The bench's key thread stopped on a panic.");
        }
        let label = if can_edit {
            "typing and Backspace, editable file"
        } else {
            "Down and Up, read-only file"
        };
        let what = if tab_motion {
            format!("{label}, paced, with tab motion")
        } else {
            format!("{label}, paced")
        };
        shared.timing.report(&what, shared.process_start);
        if tab_motion && let Some(win) = app.windows.get(&window) {
            match win.handle.motion_stats() {
                Ok(stats) => report_motion(&stats),
                Err(e) => eprintln!("The bench could not read its motion: {e}"),
            }
        }
        app.bench = None;
        app.effects.push(Effect::Exit);
        drop(app);
        pump(shared);
        return;
    }
    // Keep something moving. Nothing here waits, so the next step comes from the thread.
    if tab_motion && let Some(win) = app.windows.get(&window) {
        let handle = win.handle.clone();
        let changed = handle.with_strip(|strip| {
            if !strip.is_moving() {
                match extra.take() {
                    Some(id) => strip.close(id),
                    None => {
                        let active = strip.tab_id(strip.active());
                        extra = Some(strip.add_tab(Tab::new("Bench")));
                        if let Some(active) = active {
                            strip.select(active);
                        }
                    }
                }
                drop(strip.take_events());
            }
        });
        if let Err(e) = changed {
            eprintln!("The bench could not change a tab: {e}");
        }
        if let Some(bench) = &mut app.bench
            && let Some(run) = &mut bench.run
            && let Some(pacer) = &mut run.pacer
        {
            pacer.extra = extra;
        }
    }
    drop(app);
    pump(shared);
}

fn report_motion(stats: &qe_shell::MotionStats) {
    let mut times = stats.frame_us.clone();
    times.sort_unstable();
    let at = |part: f64| {
        times
            .get(((times.len() as f64 * part).ceil() as usize).saturating_sub(1))
            .copied()
            .unwrap_or(0)
    };
    eprintln!(
        "motion: {} ticks, {} frames, tick and paint p50 {} us, p99 {} us, max {} us",
        stats.ticks,
        stats.frames,
        at(0.5),
        at(0.99),
        at(1.0)
    );
}
