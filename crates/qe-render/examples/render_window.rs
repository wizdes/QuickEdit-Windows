#![windows_subsystem = "windows"]
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
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::{
            Dwm::DwmFlush,
            Gdi::{BeginPaint, EndPaint, PAINTSTRUCT},
        },
        System::{LibraryLoader::GetModuleHandleW, Performance::QueryPerformanceCounter},
        UI::WindowsAndMessaging::*,
    },
    core::PCWSTR,
};

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
    width: u32,
    height: u32,
    typed: String,
    start: i64,
    first: bool,
    launch_log: Option<String>,
    type_log: Option<File>,
    exit_first: bool,
}
impl State {
    fn draw(&mut self, hwnd: HWND) -> Result<(), String> {
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: rect(0.0, 0.0, self.width as f32, self.height as f32),
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
                font: "Cascadia Mono".into(),
                color: Color::rgb(0, 0, 0),
            });
        }
        self.renderer.begin_frame().map_err(|e| e.to_string())?;
        self.renderer.draw(&list).map_err(|e| e.to_string())?;
        self.renderer.end_frame().map_err(|e| e.to_string())?;
        self.renderer.present(hwnd).map_err(|e| e.to_string())?;
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
    fn typed(&mut self, hwnd: HWND, wp: WPARAM) -> Result<(), String> {
        let Some(ch) = char::from_u32(wp.0 as u32) else {
            return Ok(());
        };
        if ch.is_control() {
            return Ok(());
        }
        let received = qpc()?;
        self.typed.push(ch);
        if self.typed.len() > 44 {
            self.typed.clear();
            self.typed.push(ch);
        }
        self.renderer
            .invalidate(rect(0.0, 0.0, self.width as f32, 18.0));
        self.draw(hwnd)?;
        let after = qpc()?;
        if let Some(log) = &mut self.type_log {
            writeln!(log, "{received},{after},{}", wp.0).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
static STATE: AtomicPtr<State> = AtomicPtr::new(ptr::null_mut());
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let p = STATE.load(Ordering::Relaxed);
    if p.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let state = unsafe { &mut *p };
    let result: Result<(), String> = match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps) };
            let r = state.draw(hwnd);
            if !unsafe { EndPaint(hwnd, &ps) }.as_bool() {
                Err("EndPaint failed".into())
            } else {
                r
            }
        }
        WM_CHAR => state.typed(hwnd, wp),
        WM_SIZE => {
            let w = (lp.0 as u32 & 0xffff).max(1);
            let h = ((lp.0 as u32 >> 16) & 0xffff).max(1);
            state.width = w;
            state.height = h;
            state.renderer.resize(w, h).map_err(|e| e.to_string())
        }
        WM_DPICHANGED => state
            .renderer
            .set_dpi((wp.0 as u32 & 0xffff).max(1))
            .map_err(|e| e.to_string()),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            Ok(())
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    };
    if let Err(e) = result {
        eprintln!("window renderer: {e}");
        unsafe { PostQuitMessage(1) };
    }
    LRESULT(0)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut launch_log = None;
    let mut type_log = None;
    let mut exit_first = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--launch-log" => launch_log = Some(args.next().ok_or("--launch-log needs a path")?),
            "--type-log" => {
                type_log = Some(File::create(args.next().ok_or("--type-log needs a path")?)?)
            }
            "--exit-after-first-frame" => exit_first = true,
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    let start = qpc()?;
    let renderer = BitmapRenderer::new(724, 540)?;
    let mut state = Box::new(State {
        renderer,
        width: 724,
        height: 540,
        typed: String::new(),
        start,
        first: false,
        launch_log,
        type_log,
        exit_first,
    });
    STATE.store(state.as_mut(), Ordering::Relaxed);
    let class = wide("QuickEditRendererWindow");
    let title = wide("QuickEdit renderer");
    let module = unsafe { GetModuleHandleW(None).map_err(|e| e.to_string())? };
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
        )
        .map_err(|e| e.to_string())?
    };
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    let mut msg = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 == 0 {
            break;
        }
        if result.0 < 0 {
            return Err("GetMessageW failed".into());
        }
        let _ = unsafe { TranslateMessage(&msg) };
        unsafe { DispatchMessageW(&msg) };
    }
    STATE.store(ptr::null_mut(), Ordering::Relaxed);
    Ok(())
}
