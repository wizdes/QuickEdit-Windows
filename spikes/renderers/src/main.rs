//! The renderer race: `spike-renderers --arm a|b|c|d`.
//!
//! The windows subsystem matters for timing: a console program would start a
//! console host, which the real app does not do. Errors go to stderr when it is
//! redirected, and to `<log file>.err` when a log file was asked for.
#![cfg_attr(not(test), windows_subsystem = "windows")]
// `vcall!` exists to make an unsafe call with its arguments. It is private to
// this crate, so the warning about hiding unsafe inside a macro does not apply.
#![allow(clippy::macro_metavars_in_unsafe)]

mod arm_a;
mod arm_b;
mod arm_c;
mod arm_d;
mod atlas;
mod dump;
mod ffi;
mod gpu;
mod text;

use arm_a::ArmA;
use arm_b::ArmB;
use arm_c::{ArmC, Output};
use arm_d::ArmD;
use ffi::{H, Msg, Rect, WndClass, qpc};
use std::{
    env,
    fs::{self, File},
    io::Write,
    mem, ptr,
    sync::{
        Arc,
        atomic::{AtomicPtr, Ordering},
    },
};
use text::{Font, Layout};

const WM_DESTROY: u32 = 0x0002;
const WM_PAINT: u32 = 0x000f;
const WM_CHAR: u32 = 0x0102;
const WM_DPICHANGED: u32 = 0x02e0;
const WINDOW_STYLE: u32 = 0x00cf0000;

#[derive(Default)]
struct Options {
    arm: char,
    launch_log: Option<String>,
    type_log: Option<String>,
    exit_first: bool,
    dump_frame: Option<String>,
    /// Proof tool: feeds these characters to the WM_CHAR code after the first
    /// frame. It uses no system input, so it can run while the user works.
    self_type: Option<String>,
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--arm" => options.arm = value()?.chars().next().unwrap_or('?'),
            "--launch-log" => options.launch_log = Some(value()?),
            "--type-log" => options.type_log = Some(value()?),
            "--dump-frame" => options.dump_frame = Some(value()?),
            "--self-type" => options.self_type = Some(value()?),
            "--exit-after-first-frame" => options.exit_first = true,
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    if !"abcd".contains(options.arm) {
        return Err("--arm a|b|c|d is required".into());
    }
    Ok(options)
}

enum Path {
    A(ArmA),
    B(ArmB),
    C(ArmC),
    D(ArmD),
}

/// The launch log's one line: main, first frame, DWM flush, arm d's takeover.
/// Every value is a QueryPerformanceCounter reading. The last is empty except
/// for arm d.
fn launch_line(start: i64, first: i64, flushed: i64, takeover: Option<i64>) -> String {
    let takeover = takeover.map(|t| t.to_string()).unwrap_or_default();
    format!("{start},{first},{flushed},{takeover}\n")
}

struct State {
    options: Options,
    hwnd: H,
    start: i64,
    first_frame: Option<i64>,
    flushed: i64,
    font: Arc<Font>,
    layout: Layout,
    path: Path,
    typed: Vec<char>,
    type_file: Option<File>,
    error: Option<String>,
}

impl State {
    fn fail(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(message);
        }
        unsafe { ffi::PostQuitMessage(1) };
    }

    fn draw(&mut self, dump_path: Option<&str>) -> Result<(), String> {
        match &mut self.path {
            Path::A(arm) => {
                arm.draw(&self.typed)?;
                if let Some(path) = dump_path {
                    arm.dump(path)?;
                }
                Ok(())
            }
            Path::B(arm) => arm.draw(&self.typed, dump_path),
            Path::C(arm) => arm.draw(&self.typed, dump_path),
            Path::D(arm) => arm.draw(&self.typed, dump_path),
        }
    }

    fn on_paint(&mut self) -> Result<(), String> {
        // After the takeover the visual covers the window, so GDI has nothing to do.
        if matches!(&self.path, Path::D(arm) if arm.has_taken_over()) {
            return Ok(());
        }
        if self.first_frame.is_some() {
            return self.draw(None);
        }
        let dump_path = self.options.dump_frame.clone();
        self.draw(dump_path.as_deref())?;
        let after = qpc();
        self.first_frame = Some(after);
        if let Some(path) = &dump_path {
            dump::write_modules(&format!("{path}.modules.txt"))?;
        }
        if let Path::D(_) = self.path {
            arm_d::start(self.hwnd, self.font.clone(), self.layout)?;
        }
        if self.options.launch_log.is_some() {
            // DwmFlush returns after the next desktop composition pass, so this
            // later stamp is a little closer to "on screen" than the return of
            // Present or BitBlt. It runs after the first stamp and cannot change it.
            dump::dwm_flush()?;
            self.flushed = qpc();
        }
        if !matches!(self.path, Path::D(_)) {
            self.finish_launch(None)?;
        }
        Ok(())
    }

    /// Writes the launch log and leaves if asked. Arm d waits for its takeover.
    fn finish_launch(&mut self, takeover: Option<i64>) -> Result<(), String> {
        if let Some(text) = self.options.self_type.take() {
            for c in text.chars() {
                self.on_char(c as usize)?;
            }
            if let Some(path) = self.options.dump_frame.clone() {
                self.draw(Some(&format!("{path}.typed.bmp")))?;
            }
        }
        let first = self.first_frame.ok_or("the launch ended with no frame")?;
        if let Some(path) = &self.options.launch_log {
            fs::write(path, launch_line(self.start, first, self.flushed, takeover))
                .map_err(|e| format!("launch log {path}: {e}"))?;
        }
        if self.options.exit_first {
            unsafe { ffi::PostQuitMessage(0) };
        }
        Ok(())
    }

    fn on_char(&mut self, wp: usize) -> Result<(), String> {
        let received = qpc();
        let Some(c) = char::from_u32(wp as u32) else {
            return Ok(());
        };
        if c.is_control() {
            return Ok(());
        }
        self.typed.push(c);
        self.draw(None)?;
        let after = qpc();
        if let Some(file) = &mut self.type_file {
            writeln!(file, "{received},{after},{wp}").map_err(|e| format!("type log: {e}"))?;
        }
        Ok(())
    }

    fn on_gpu_ready(&mut self, pointer: usize) -> Result<(), String> {
        let gpu = arm_d::gpu_ready(pointer);
        let dump_path = self
            .options
            .dump_frame
            .as_ref()
            .map(|p| format!("{p}.gpu.bmp"));
        let Path::D(arm) = &mut self.path else {
            return Err("a background result reached an arm with no background".into());
        };
        arm.take_over(gpu, &self.typed, dump_path.as_deref())?;
        let takeover = qpc();
        if let Some(path) = &dump_path {
            dump::write_modules(&format!("{path}.modules.txt"))?;
        }
        self.finish_launch(Some(takeover))
    }
}

static STATE: AtomicPtr<State> = AtomicPtr::new(ptr::null_mut());

unsafe extern "system" fn wndproc(hwnd: H, msg: u32, wp: usize, lp: isize) -> isize {
    let pointer = STATE.load(Ordering::Relaxed);
    if pointer.is_null() {
        return unsafe { ffi::DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let state = unsafe { &mut *pointer };
    let result = match msg {
        WM_PAINT => {
            let result = state.on_paint();
            unsafe { ffi::ValidateRect(hwnd, ptr::null()) };
            result
        }
        WM_CHAR => state.on_char(wp),
        arm_d::WM_GPU_READY => state.on_gpu_ready(wp),
        arm_d::WM_GPU_FAILED => Err(format!("arm d background: {}", arm_d::gpu_failed(wp))),
        // The layout is built once for one DPI. Timings after a change would
        // be for a blurry or wrong-size window, so the spike stops and says so.
        WM_DPICHANGED => Err("the window moved to a monitor with another DPI; run it again".into()),
        WM_DESTROY => {
            unsafe { ffi::PostQuitMessage(0) };
            Ok(())
        }
        _ => return unsafe { ffi::DefWindowProcW(hwnd, msg, wp, lp) },
    };
    if let Err(e) = result {
        state.fail(e);
    }
    0
}

fn run(start: i64, options: Options) -> Result<(), String> {
    let type_file = match &options.type_log {
        Some(path) => Some(File::create(path).map_err(|e| format!("type log {path}: {e}"))?),
        None => None,
    };
    // Per-monitor awareness, version 2. This call must come before any window.
    if unsafe { ffi::SetProcessDpiAwarenessContext(-4) } == 0 {
        return Err(format!(
            "SetProcessDpiAwarenessContext failed: error {}",
            unsafe { ffi::GetLastError() }
        ));
    }
    let class = ffi::wide("QeRendererSpike");
    let title = ffi::wide("QuickEdit renderer spike");
    let instance = unsafe { ffi::GetModuleHandleW(ptr::null()) };
    let wc = WndClass {
        style: 0,
        proc: Some(wndproc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: ptr::null_mut(),
        cursor: ptr::null_mut(),
        background: ptr::null_mut(),
        menu: ptr::null(),
        name: class.as_ptr(),
    };
    if unsafe { ffi::RegisterClassW(&wc) } == 0 {
        return Err("RegisterClassW failed".into());
    }
    let hwnd = unsafe {
        ffi::CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WINDOW_STYLE,
            100,
            100,
            900,
            700,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        return Err(format!("CreateWindowExW failed: error {}", unsafe {
            ffi::GetLastError()
        }));
    }
    let dpi = unsafe { ffi::GetDpiForWindow(hwnd) };
    let font = Arc::new(Font::open()?);
    let layout = font.layout(dpi)?;
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: layout.width,
        bottom: layout.height,
    };
    if unsafe { ffi::AdjustWindowRectExForDpi(&mut rect, WINDOW_STYLE, 0, 0, dpi) } == 0 {
        return Err("AdjustWindowRectExForDpi failed".into());
    }
    // SWP_NOMOVE | SWP_NOZORDER
    let sized = unsafe {
        ffi::SetWindowPos(
            hwnd,
            ptr::null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            0x0006,
        )
    };
    if sized == 0 {
        return Err("SetWindowPos failed".into());
    }
    let path = match options.arm {
        'a' => Path::A(ArmA::new(hwnd, font.clone(), layout)?),
        'b' => Path::B(ArmB::new(hwnd, font.clone(), layout)?),
        'c' => Path::C(ArmC::new(Output::Window(hwnd), font.clone(), layout)?),
        _ => Path::D(ArmD::new(hwnd, font.clone(), layout)?),
    };
    let mut state = Box::new(State {
        options,
        hwnd,
        start,
        first_frame: None,
        flushed: 0,
        font,
        layout,
        path,
        typed: Vec::new(),
        type_file,
        error: None,
    });
    STATE.store(&mut *state, Ordering::Relaxed);
    unsafe {
        ffi::ShowWindow(hwnd, 5);
        ffi::UpdateWindow(hwnd);
    }
    let mut msg = unsafe { mem::zeroed::<Msg>() };
    loop {
        let result = unsafe { ffi::GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result == 0 {
            break;
        }
        if result < 0 {
            return Err("GetMessageW failed".into());
        }
        unsafe {
            ffi::TranslateMessage(&msg);
            ffi::DispatchMessageW(&msg);
        }
    }
    STATE.store(ptr::null_mut(), Ordering::Relaxed);
    if let Some(e) = state.error.take() {
        return Err(e);
    }
    if msg.wp != 0 {
        return Err(format!("the window ended with code {}", msg.wp));
    }
    // Ends now, with no slow and pointless release of GPU objects at exit.
    std::process::exit(0);
}

fn report(error: &str, logs: &[Option<String>]) {
    eprintln!("spike-renderers: {error}");
    for path in logs.iter().flatten() {
        if let Err(e) = fs::write(format!("{path}.err"), error) {
            eprintln!("spike-renderers: writing {path}.err: {e}");
        }
    }
}

fn main() {
    let start = qpc();
    let options = match parse_options(env::args().skip(1)) {
        Ok(options) => options,
        Err(e) => {
            report(&e, &[]);
            std::process::exit(2);
        }
    };
    let logs = [options.launch_log.clone(), options.type_log.clone()];
    if let Err(e) = run(start, options) {
        report(&e, &logs);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(items: &[&str]) -> Result<Options, String> {
        parse_options(items.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn options_need_a_known_arm() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--arm", "e"]).is_err());
        assert!(parse(&["--arm"]).is_err());
        assert!(parse(&["--arm", "c", "--nope"]).is_err());
        let options = parse(&[
            "--arm",
            "d",
            "--launch-log",
            "x",
            "--exit-after-first-frame",
        ])
        .unwrap();
        assert_eq!(options.arm, 'd');
        assert!(options.exit_first);
    }

    #[test]
    fn the_launch_line_leaves_takeover_empty_when_there_is_none() {
        assert_eq!(launch_line(1, 2, 3, None), "1,2,3,\n");
        assert_eq!(launch_line(1, 2, 3, Some(4)), "1,2,3,4\n");
    }
}
