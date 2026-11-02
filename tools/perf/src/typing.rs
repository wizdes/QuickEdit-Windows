//! `qe-perf typing`: real keys to a real window, timed by screen capture.
//!
//! It sends real keys, so it refuses to send one unless the target is the
//! foreground window and nothing covers it. The gate runs before every key and
//! once more before any number is printed.
use crate::dup::Capture;
use crate::win::{self, H, KeyInput, Rect, WinPoint};
use crate::{Child, is_spike, spawn, visible_window, wait_window};
use qe_perf::{
    GateFacts, Rng, ScreenRect, Target, TypingReport, WindowAbove, first_cover, gate,
    parse_type_log, quote_windows_arg, ticks_to_ms, typing_report,
};
use std::{
    env, fs, mem, thread,
    time::{Duration, Instant},
};

const FOREGROUND_WAIT: Duration = Duration::from_secs(2);
/// A pixel change this long after a key does not count for that key.
const LOST_AFTER: Duration = Duration::from_secs(1);
/// The window must show no change for this long before the first key. Arm d's
/// switch to the GPU path changes pixels, so the keys wait for it.
const SETTLE: Duration = Duration::from_secs(1);
const SETTLE_LIMIT: Duration = Duration::from_secs(20);

fn client_region(hwnd: H) -> Result<ScreenRect, String> {
    let mut rect = Rect::default();
    let mut origin = WinPoint::default();
    if unsafe { win::GetClientRect(hwnd, &mut rect) } == 0
        || unsafe { win::ClientToScreen(hwnd, &mut origin) } == 0
    {
        return Err("reading the target window's position failed".into());
    }
    Ok(ScreenRect {
        left: origin.x,
        top: origin.y,
        right: origin.x + rect.right,
        bottom: origin.y + rect.bottom,
    })
}

/// A window's class name. Titles stay out of reports, because they can hold
/// the name of whatever the user is working on.
fn class_of(hwnd: H) -> String {
    let mut buffer = [0u16; 128];
    let len = unsafe { win::GetClassNameW(hwnd, buffer.as_mut_ptr(), 128) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..len])
}

/// Reads one DWM attribute of a window, returning false if the call fails.
fn dwm_attribute<T>(get: DwmGetAttribute, hwnd: H, attribute: u32, value: &mut T) -> bool {
    let size = mem::size_of::<T>() as u32;
    unsafe { get(hwnd, attribute, value as *mut T as H, size) >= 0 }
}

type DwmGetAttribute = unsafe extern "system" fn(H, u32, H, u32) -> i32;

/// Every window above the target in the z-order, nearest first.
fn windows_above(hwnd: H, get: DwmGetAttribute) -> Vec<WindowAbove> {
    const GW_HWNDPREV: u32 = 3;
    const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
    const DWMWA_CLOAKED: u32 = 14;
    let mut windows = Vec::new();
    let mut current = unsafe { win::GetWindow(hwnd, GW_HWNDPREV) };
    while !current.is_null() {
        // The extended frame bounds leave out the invisible resize border, which
        // overlaps neighbors without covering them. If DWM cannot say, the full
        // window rectangle is used: larger, so this errs toward refusing.
        let mut rect = Rect::default();
        if !dwm_attribute(get, current, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rect) {
            unsafe { win::GetWindowRect(current, &mut rect) };
        }
        let mut cloaked = 0u32;
        if !dwm_attribute(get, current, DWMWA_CLOAKED, &mut cloaked) {
            cloaked = 0;
        }
        windows.push(WindowAbove {
            name: format!("window class {}", class_of(current)),
            visible: unsafe { win::IsWindowVisible(current) } != 0,
            cloaked: cloaked != 0,
            minimized: unsafe { win::IsIconic(current) } != 0,
            rect: ScreenRect {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
        });
        current = unsafe { win::GetWindow(current, GW_HWNDPREV) };
    }
    windows
}

fn modifier_held() -> bool {
    // Shift, Ctrl, Alt, left Windows and right Windows. The high bit means "down".
    [0x10, 0x11, 0x12, 0x5b, 0x5c]
        .iter()
        .any(|vk| unsafe { win::GetAsyncKeyState(*vk) } < 0)
}

struct Watch {
    hwnd: H,
    region: ScreenRect,
    dwm_get: DwmGetAttribute,
}

impl Watch {
    fn check(&self) -> Result<(), String> {
        let alive = unsafe { win::IsWindow(self.hwnd) } != 0;
        let facts = GateFacts {
            window_alive: alive,
            foreground: unsafe { win::GetForegroundWindow() } == self.hwnd,
            minimized: alive && unsafe { win::IsIconic(self.hwnd) } != 0,
            covered_by: if alive {
                first_cover(&self.region, &windows_above(self.hwnd, self.dwm_get))
                    .map(|w| w.name.clone())
            } else {
                None
            },
            region_moved: alive && client_region(self.hwnd)? != self.region,
            modifier_held: modifier_held(),
        };
        gate(&facts)
    }
}

fn send_key(vk: u16) -> Result<(), String> {
    const INPUT_KEYBOARD: u32 = 1;
    const KEYEVENTF_KEYUP: u32 = 2;
    let event = |flags| KeyInput {
        kind: INPUT_KEYBOARD,
        padding: 0,
        vk,
        scan: 0,
        flags,
        time: 0,
        extra: 0,
        union_tail: [0; 8],
    };
    let inputs = [event(0), event(KEYEVENTF_KEYUP)];
    let sent = unsafe { win::SendInput(2, inputs.as_ptr(), mem::size_of::<KeyInput>() as i32) };
    if sent != 2 {
        return Err(format!(
            "SendInput sent {sent} of 2 events: error {}",
            unsafe { win::GetLastError() }
        ));
    }
    Ok(())
}

fn settle(capture: &Capture) -> Result<(), String> {
    let began = Instant::now();
    let mut seen = capture.last_change()?;
    let mut quiet_since = Instant::now();
    loop {
        if !capture.is_running() {
            return Err("the screen capture stopped while waiting for the window".into());
        }
        let last = capture.last_change()?;
        if last != seen {
            seen = last;
            quiet_since = Instant::now();
        }
        if quiet_since.elapsed() >= SETTLE && capture.frames() > 0 {
            return Ok(());
        }
        if began.elapsed() >= SETTLE_LIMIT {
            return Err("the target window never stopped changing".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn print_report(report: &TypingReport) {
    let ms = |value: Option<f64>| value.map_or("n/a".to_owned(), |v| format!("{v:.3}"));
    println!(
        "key_to_screen_ms p50={:.3} p95={:.3} (before SendInput to the frame's LastPresentTime)",
        report.screen_p50_ms, report.screen_p95_ms
    );
    println!(
        "key_to_capture_ms p50={:.3} p95={:.3} (before SendInput to this tool getting the frame)",
        report.captured_p50_ms, report.captured_p95_ms
    );
    println!(
        "app_ms p50={} p95={} (WM_CHAR received to present returned, from the target's log)",
        ms(report.app_p50_ms),
        ms(report.app_p95_ms)
    );
    println!("lost_keys {}/{}", report.lost, report.keys);
    println!("unattributed_changes {}", report.unattributed);
    println!(
        "budget p50<25ms p95<33ms no lost keys: {}",
        if report.meets_budget() {
            "met"
        } else {
            "MISSED"
        }
    );
}

pub fn run_typing(target: Target, keys: usize) -> Result<(), String> {
    let hz = win::frequency();
    if hz <= 0 {
        return Err("QueryPerformanceFrequency failed".into());
    }
    // The capture and the window position must use the same physical pixels.
    if unsafe { win::SetProcessDpiAwarenessContext(-4) } == 0 {
        return Err("SetProcessDpiAwarenessContext failed".into());
    }
    let dwm_get: DwmGetAttribute =
        unsafe { mem::transmute(win::system_proc("dwmapi.dll", b"DwmGetWindowAttribute\0")?) };

    let spike = is_spike(&target.exe);
    let log = (spike && !target.args.contains("--type-log"))
        .then(|| env::temp_dir().join(format!("qe-perf-typing-{}.csv", std::process::id())));
    let extra = log.as_ref().map_or(String::new(), |path| {
        format!("--type-log {}", quote_windows_arg(&path.to_string_lossy()))
    });
    let (child, _) = spawn(&target, &extra)?;
    let result = drive(&child, keys, hz, dwm_get, log.as_deref());
    drop(child);
    let mut detail = String::new();
    if let Some(path) = &log {
        // The target writes its own failure next to its log.
        let err_path = format!("{}.err", path.to_string_lossy());
        if let Ok(text) = fs::read_to_string(&err_path) {
            detail = format!(" (the target said: {text})");
            fs::remove_file(&err_path).map_err(|e| format!("removing {err_path}: {e}"))?;
        }
        if path.exists() {
            fs::remove_file(path).map_err(|e| format!("removing the type log: {e}"))?;
        }
    }
    result.map_err(|e| format!("{e}{detail}"))
}

fn drive(
    child: &Child,
    keys: usize,
    hz: i64,
    dwm_get: DwmGetAttribute,
    log: Option<&std::path::Path>,
) -> Result<(), String> {
    wait_window(child, Duration::from_secs(20))?;
    let hwnd = visible_window(child.info.pid).ok_or("the target window disappeared")?;
    unsafe { win::SetForegroundWindow(hwnd) };
    let began = Instant::now();
    while unsafe { win::GetForegroundWindow() } != hwnd && began.elapsed() < FOREGROUND_WAIT {
        thread::sleep(Duration::from_millis(10));
    }
    let watch = Watch {
        hwnd,
        region: client_region(hwnd)?,
        dwm_get,
    };
    if watch.region.is_empty() {
        return Err("the target window has an empty client area".into());
    }
    refuse_if_blocked(&watch)?;
    let capture = Capture::start(watch.region)?;
    settle(&capture)?;
    refuse_if_blocked(&watch)?;
    let settled_at = capture.last_change()?;

    let mut rng = Rng::new(win::qpc() as u64);
    let mut sent = Vec::with_capacity(keys);
    for _ in 0..keys {
        if !capture.is_running() {
            break; // `finish` below returns the capture's error.
        }
        refuse_if_blocked(&watch)?;
        let vk = rng.letter_key();
        sent.push(win::qpc());
        send_key(vk)?;
        thread::sleep(Duration::from_millis(rng.gap_ms()));
    }
    thread::sleep(LOST_AFTER);
    refuse_if_blocked(&watch)?;
    let (changes, _) = capture.finish()?;
    if sent.len() != keys {
        return Err(format!("only {} of {keys} keys were sent", sent.len()));
    }
    // Changes up to the settled point belong to the window's start.
    let changes: Vec<_> = changes
        .into_iter()
        .filter(|c| settled_at.is_none_or(|s| c.present > s))
        .collect();

    let app_log = match log {
        Some(path) => {
            let data =
                fs::read_to_string(path).map_err(|e| format!("reading the type log: {e}"))?;
            Some(parse_type_log(&data)?)
        }
        None => None,
    };
    let lost_after = (LOST_AFTER.as_secs_f64() * hz as f64) as i64;
    let report = typing_report(&sent, &changes, app_log.as_deref(), lost_after, hz)
        .map_err(|e| format!("REFUSED, no result: {e}"))?;

    println!("key,sent_qpc,key_to_screen_ms,key_to_capture_ms");
    let matched = qe_perf::match_keys(&sent, &changes, lost_after);
    for (index, (key, change)) in sent.iter().zip(&matched.keys).enumerate() {
        match change {
            Some(c) => println!(
                "{},{key},{:.3},{:.3}",
                index + 1,
                ticks_to_ms((c.present - key) as u64, hz),
                ticks_to_ms((c.acquired - key).max(0) as u64, hz)
            ),
            None => println!("{},{key},lost,lost", index + 1),
        }
    }
    print_report(&report);
    Ok(())
}

fn refuse_if_blocked(watch: &Watch) -> Result<(), String> {
    watch
        .check()
        .map_err(|e| format!("REFUSED, no result: {e}"))
}

/// Proves the capture and gate code on this machine with no visible run. It
/// sends no keys and opens no window. It reads frames from each 64 x 64 corner
/// of the main display for one second, keeps only the newest in memory, and asks
/// the gate about the user's current foreground window. A still corner next to a
/// busy one shows that the compare can tell frames apart.
pub fn run_dup_check() -> Result<(), String> {
    if unsafe { win::SetProcessDpiAwarenessContext(-4) } == 0 {
        return Err("SetProcessDpiAwarenessContext failed".into());
    }
    // SM_CXSCREEN and SM_CYSCREEN: the main display, in physical pixels here.
    let (width, height) = unsafe { (win::GetSystemMetrics(0), win::GetSystemMetrics(1)) };
    let corners = [
        ("top left", 0, 0),
        ("top right", width - 64, 0),
        ("bottom left", 0, height - 64),
        ("bottom right", width - 64, height - 64),
    ];
    for (name, left, top) in corners {
        let capture = Capture::start(ScreenRect {
            left,
            top,
            right: left + 64,
            bottom: top + 64,
        })?;
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(1) && capture.is_running() {
            thread::sleep(Duration::from_millis(20));
        }
        let (changes, frames) = capture.finish()?;
        println!(
            "dup-check capture {name}: {frames} frames read in about 1 s, {} of them changed the corner",
            changes.len()
        );
    }

    let dwm_get: DwmGetAttribute =
        unsafe { mem::transmute(win::system_proc("dwmapi.dll", b"DwmGetWindowAttribute\0")?) };
    let hwnd = unsafe { win::GetForegroundWindow() };
    if hwnd.is_null() {
        println!("dup-check gate: no foreground window right now");
        return Ok(());
    }
    let watch = Watch {
        hwnd,
        region: client_region(hwnd)?,
        dwm_get,
    };
    let above = windows_above(hwnd, dwm_get);
    println!(
        "dup-check gate: foreground window class {}, {} windows above it, {} shown",
        class_of(hwnd),
        above.len(),
        above
            .iter()
            .filter(|w| w.visible && !w.cloaked && !w.minimized)
            .count()
    );
    match watch.check() {
        Ok(()) => println!("dup-check gate: it would allow keys to that window"),
        Err(e) => println!("dup-check gate: it would refuse: {e}"),
    }
    Ok(())
}
