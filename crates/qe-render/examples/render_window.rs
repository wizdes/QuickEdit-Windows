//! A plain window that draws through qe-render, for timing and by-hand checks.
//!
//! This is a console program on purpose: a failure prints to stderr and the process
//! exits with code 1. `--hidden` never shows the window, so tests can run it. The
//! `--simulate-` options send the same messages that Windows sends.
use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::BitmapRenderer;
use std::{
    env,
    fs::{self, File},
    io::Write,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Dwm::DwmFlush,
            Gdi::{BeginPaint, EndPaint, PAINTSTRUCT},
        },
        System::{LibraryLoader::GetModuleHandleW, Performance::QueryPerformanceCounter},
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetProcessDpiAwarenessContext,
            },
            WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};

/// Any failure, as its message. The `windows` crate's error here does not implement
/// `std::error::Error`.
#[derive(Debug)]
struct Failure(String);
impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn qpc() -> Result<i64, String> {
    let mut n = 0;
    unsafe {
        QueryPerformanceCounter(&mut n).map_err(|e| e.to_string())?;
    }
    Ok(n)
}
fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}

struct State {
    renderer: BitmapRenderer,
    font: String,
    width: u32,
    height: u32,
    dpi: u32,
    typed: String,
    /// The first half of a UTF-16 surrogate pair. Windows sends each half in its own
    /// WM_CHAR.
    high_surrogate: Option<u16>,
    start: i64,
    first: bool,
    launch_log: Option<String>,
    type_log: Option<File>,
    exit_first: bool,
}
impl State {
    fn list(&self) -> DisplayList {
        let s = self.dpi as f32 / 96.0;
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: rect(0.0, 0.0, self.width as f32 / s, self.height as f32 / s),
            color: Color::rgb(255, 255, 255),
        });
        let heading = format!(
            "QuickEdit renderer race - Cascadia Mono 10 pt {}",
            self.typed
        );
        for row in 0..30 {
            let text = if row == 0 {
                heading.clone()
            } else {
                format!(
                    "QuickEdit text row {:02}: The quick brown fox jumps over the lazy dog.",
                    row + 1
                )
            };
            list.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(2.0),
                    y: Dip((row * 18 + 15) as f32),
                },
                text,
                font: self.font.clone(),
                font_size: Dip(13.33),
                color: Color::rgb(0, 0, 0),
            });
        }
        list
    }
    /// Draws what is pending and copies it to the window. A problem with one command
    /// does not stop the present, and it still comes back as an error.
    fn draw(&mut self, hwnd: HWND) -> Result<(), String> {
        let list = self.list();
        self.renderer.begin_frame().map_err(|e| e.to_string())?;
        let drawn = self.renderer.draw(&list).map_err(|e| e.to_string());
        self.renderer.end_frame().map_err(|e| e.to_string())?;
        self.renderer.present(hwnd).map_err(|e| e.to_string())?;
        drawn?;
        if !self.first {
            self.first = true;
            let first = qpc()?;
            if let Some(path) = &self.launch_log {
                unsafe {
                    DwmFlush().map_err(|e| e.to_string())?;
                }
                let flushed = qpc()?;
                fs::write(path, format!("{},{first},{flushed},\n", self.start))
                    .map_err(|e| e.to_string())?;
            }
            if self.exit_first {
                unsafe { PostQuitMessage(0) };
            }
        }
        Ok(())
    }
    /// Windows repaints what was hidden. New drawing happens only if something is
    /// pending. The rest comes straight from the bitmap.
    fn paint(&mut self, hwnd: HWND) -> Result<(), String> {
        let mut ps = PAINTSTRUCT::default();
        unsafe { BeginPaint(hwnd, &mut ps) };
        let mut result = self.draw(hwnd);
        if result.is_ok() {
            result = self
                .renderer
                .present_rect_to_dc(ps.hdc, ps.rcPaint)
                .map_err(|e| e.to_string());
        }
        if !unsafe { EndPaint(hwnd, &ps) }.as_bool() {
            return Err("EndPaint failed".into());
        }
        result
    }
    fn typed(&mut self, hwnd: HWND, wp: WPARAM) -> Result<(), String> {
        let unit = wp.0 as u32;
        let character = match (self.high_surrogate.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high_surrogate = Some(unit as u16);
                return Ok(());
            }
            (Some(high), 0xDC00..=0xDFFF) => char::decode_utf16([high, unit as u16])
                .next()
                .and_then(Result::ok),
            (_, 0xD800..=0xDFFF) => None,
            (_, other) => char::from_u32(other),
        };
        let Some(ch) = character.filter(|c| !c.is_control()) else {
            return Ok(());
        };
        let received = qpc()?;
        self.typed.push(ch);
        if self.typed.chars().count() > 44 {
            self.typed.clear();
            self.typed.push(ch);
        }
        let s = self.dpi as f32 / 96.0;
        self.renderer
            .invalidate(rect(0.0, 0.0, self.width as f32 / s, 18.0));
        self.draw(hwnd)?;
        let after = qpc()?;
        if let Some(log) = &mut self.type_log {
            writeln!(log, "{received},{after},{}", wp.0).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn dpi_changed(&mut self, hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Result<(), String> {
        let dpi = (wp.0 as u32 & 0xffff).max(1);
        self.dpi = dpi;
        self.renderer.set_dpi(dpi).map_err(|e| e.to_string())?;
        // Windows gives the rectangle to use at the new DPI. It stays valid during
        // this message.
        let suggested = unsafe { &*(lp.0 as *const RECT) };
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .map_err(|e| e.to_string())
        }
    }
}
static STATE: AtomicPtr<State> = AtomicPtr::new(ptr::null_mut());
fn fail(message: &str) {
    eprintln!("window renderer: {message}");
    unsafe { PostQuitMessage(1) };
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let p = STATE.load(Ordering::Relaxed);
    if p.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let state = unsafe { &mut *p };
    let result: Result<(), String> = match msg {
        WM_PAINT => state.paint(hwnd),
        WM_CHAR => state.typed(hwnd, wp),
        WM_SIZE => {
            let w = (lp.0 as u32 & 0xffff).max(1);
            let h = ((lp.0 as u32 >> 16) & 0xffff).max(1);
            state.width = w;
            state.height = h;
            state.renderer.resize(w, h).map_err(|e| e.to_string())
        }
        WM_DPICHANGED => state.dpi_changed(hwnd, wp, lp),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            Ok(())
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    };
    if let Err(e) = result {
        fail(&e);
    }
    LRESULT(0)
}
fn run() -> Result<i32, Failure> {
    let mut launch_log = None;
    let mut type_log = None;
    let mut exit_first = false;
    let mut hidden = false;
    let mut font = "Cascadia Mono".to_owned();
    let mut simulate_dpi = None;
    let mut simulate_char = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--launch-log" => launch_log = Some(args.next().ok_or("--launch-log needs a path")?),
            "--type-log" => {
                type_log = Some(File::create(args.next().ok_or("--type-log needs a path")?)?)
            }
            "--exit-after-first-frame" => exit_first = true,
            "--hidden" => hidden = true,
            "--font" => font = args.next().ok_or("--font needs a family")?,
            "--simulate-dpi-change" => {
                simulate_dpi = Some(
                    args.next()
                        .ok_or("--simulate-dpi-change needs a DPI")?
                        .parse::<u32>()?,
                )
            }
            "--simulate-char" => {
                let hex = args.next().ok_or("--simulate-char needs a hex value")?;
                simulate_char = Some(
                    char::from_u32(u32::from_str_radix(&hex, 16)?)
                        .ok_or("--simulate-char needs a character")?,
                )
            }
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    let start = qpc()?;
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    let renderer = BitmapRenderer::new(724, 540)?;
    let mut state = Box::new(State {
        renderer,
        font,
        width: 724,
        height: 540,
        dpi: 96,
        typed: String::new(),
        high_surrogate: None,
        start,
        first: false,
        launch_log,
        type_log,
        exit_first,
    });
    let state_ptr: *mut State = state.as_mut();
    let class = wide("QuickEditRendererWindow");
    let title = wide("QuickEdit renderer");
    let module = unsafe { GetModuleHandleW(None)? };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: HINSTANCE(module.0),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return Err("RegisterClassW failed".into());
    }
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            724,
            540,
            None,
            None,
            Some(HINSTANCE(module.0)),
            None,
        )?
    };
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        return Err("GetDpiForWindow failed".into());
    }
    state.dpi = dpi;
    state.renderer.set_dpi(dpi)?;
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client)? };
    state.width = (client.right - client.left).max(1) as u32;
    state.height = (client.bottom - client.top).max(1) as u32;
    state.renderer.resize(state.width, state.height)?;
    STATE.store(state_ptr, Ordering::Relaxed);
    if hidden {
        println!("start dpi {dpi}");
    } else {
        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    }
    if let Some(dpi) = simulate_dpi {
        let suggested = RECT {
            left: 10,
            top: 10,
            right: 910,
            bottom: 610,
        };
        unsafe {
            SendMessageW(
                hwnd,
                WM_DPICHANGED,
                Some(WPARAM((dpi | (dpi << 16)) as usize)),
                Some(LPARAM(&suggested as *const RECT as isize)),
            );
        }
        let mut now = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut now)? };
        let state = unsafe { &*state_ptr };
        println!(
            "dpi {} window {}x{}",
            state.dpi,
            now.right - now.left,
            now.bottom - now.top
        );
    }
    if let Some(ch) = simulate_char {
        let mut units = [0u16; 2];
        for unit in ch.encode_utf16(&mut units) {
            unsafe { SendMessageW(hwnd, WM_CHAR, Some(WPARAM(*unit as usize)), Some(LPARAM(0))) };
        }
        println!("typed {}", unsafe { &(*state_ptr).typed });
    }
    if hidden {
        // A hidden window gets no WM_PAINT.
        let drawn = unsafe { (*state_ptr).draw(hwnd) };
        if let Err(e) = drawn {
            fail(&e);
        }
    }
    let mut msg = MSG::default();
    let code = loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 == 0 {
            break msg.wParam.0 as i32;
        }
        if result.0 < 0 {
            return Err("GetMessageW failed".into());
        }
        let _ = unsafe { TranslateMessage(&msg) };
        unsafe { DispatchMessageW(&msg) };
    };
    STATE.store(ptr::null_mut(), Ordering::Relaxed);
    Ok(code)
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("window renderer: {}", e.0);
            std::process::exit(1);
        }
    }
}
