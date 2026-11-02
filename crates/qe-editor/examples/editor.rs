//! A QuickEdit window that accepts keyboard and mouse input.
//!
//! The window appears first and the file follows: a thread loads it, and its first
//! screen paints when it is ready. The thread and the layout workers wake the window
//! with a posted message, which nothing the window does with its timers can replace.
//!
//! `--timings` prints what the plan's budgets ask for:
//! - `input to present` runs from the moment a key arrives to the end of the present
//!   call of the first frame that shows the new text. On a line that a worker lays out,
//!   that is a later frame than the first. A key that waits behind a layout or in the
//!   editor's queue counts too, with its wait, and the count of such keys is printed.
//! - `input to first frame` ends at the next frame, whatever it shows.
//! - Both count only keys that produced a frame. The others are counted apart.
//! - `first text frame` and `full first screen` measure a load, so they print only when
//!   a file was asked for. `first text frame` is the first frame that holds real text,
//!   and `full first screen` is the first frame whose text reaches the text area's
//!   bottom. A window with no file, or a file with no text, prints `first frame`.
//! - `from process creation` starts at the time Windows gives from GetProcessTimes, so
//!   it holds the loading of the program and its libraries.
//!
//! `--bench-keys N` runs in a hidden window, sends WM_PAINT itself, and prints the same
//! numbers, so no window appears on screen. Windows' own copy to a hidden window draws
//! nothing, so this measures our work up to and including the present call.
use qe_api::{
    Bitmap, Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseCursor,
    PixelRect, Point, Rect, RenderError, Renderer, View, ViewContext, ViewRequest, WidgetId,
};
use qe_buffer::Notify;
use qe_editor::{Clipboard, ClipboardPreparer, EditorView, FileLoad, LoadOptions, LoadResult};
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};
use qe_shell::{
    ShellEvent, ShellHandle, ShellWindow, Tab, VIEW_TIMER, WindowConfig, quit_message_loop,
    run_message_loop,
};
use qe_ui::{
    ClipboardAccess, MenuModel, MenuView, MenuWindows, POPUP_DISMISSED_MESSAGE, PopupDismissed,
    caret_menu, menu_bar_groups, place_popup,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetTickCount};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, SetFocus, VK_CONTROL, VK_F4, VK_MENU, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetSystemMetrics, MSG, PostQuitMessage, SC_KEYMENU,
    SM_CXDOUBLECLK, SM_CYDOUBLECLK, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SendMessageW, SystemParametersInfoW, TranslateMessage, WM_CHAR, WM_CLOSE, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SYSCOMMAND,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const HELP: &str = "\
editor [options] [file]
  (no option)        opens a real window on screen
  --hidden-smoke     hidden window; checks that the file's text reaches a frame
  --hidden-loop      hidden window; real message loop until the file has loaded
  --bench-keys N     hidden window; sends N keys and prints the timers
  --bench-wrap       with --bench-keys: long lines wrap
  --bench-middle     with --bench-keys: the keys go to the middle of the text
  --bench-no-pause   with --bench-keys: the next key does not wait for the last one's frame
  --bench-end-first  with --bench-keys: Ctrl+End and Home go first, so that keys wait behind them
  --timings          prints the timers when the window closes
  --force-mapped     opens the file read-only, whatever its size
  --help             prints this and opens nothing";

struct SystemClipboard(qe_ui::SystemClipboard);
impl Clipboard for SystemClipboard {
    fn read(&mut self) -> Result<Option<String>, String> {
        self.0.read_text()
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        self.0.write_text(value)
    }
    fn preparer(&self) -> Option<ClipboardPreparer> {
        Some(Arc::new(|text, cancel| {
            let prepared = qe_ui::prepare_clipboard_text_unless(text, cancel)?;
            Ok(Box::new(prepared) as Box<dyn std::any::Any + Send>)
        }))
    }
    fn write_prepared(&mut self, prepared: Box<dyn std::any::Any + Send>) -> Result<(), String> {
        let prepared = prepared
            .downcast::<qe_ui::PreparedClipboardText>()
            .map_err(|_| "the copied text was prepared for another clipboard".to_string())?;
        self.0.write_prepared(*prepared)
    }
}

fn file_time_100ns(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
/// The moment Windows created this process, on the clock the timers use.
fn process_creation() -> Result<Instant, String> {
    let empty = FILETIME::default();
    let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
    // SAFETY: the pseudo handle needs no closing, and the four values live through the call.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|e| format!("could not read the process creation time: {e}"))?;
    // SAFETY: a plain query.
    let now = file_time_100ns(unsafe { GetSystemTimeAsFileTime() });
    let age = now.saturating_sub(file_time_100ns(created));
    Instant::now()
        .checked_sub(Duration::from_nanos(age.saturating_mul(100)))
        .ok_or_else(|| "the process creation time is out of range".to_string())
}
/// When a key really came, if Windows held it for longer than its clock's 16 ms grain.
fn input_time(time_us: u64, handled: Instant) -> Instant {
    // SAFETY: a plain query.
    let now_ms = unsafe { GetTickCount() };
    let behind = now_ms.wrapping_sub((time_us / 1000) as u32);
    if (17..60_000).contains(&behind) {
        handled
            .checked_sub(Duration::from_millis(u64::from(behind)))
            .unwrap_or(handled)
    } else {
        handled
    }
}

/// Timers that end at the present call, which only the renderer can see.
#[derive(Default)]
struct Timing {
    /// Keys that asked for a repaint and have not reached a frame with their new text.
    keys_waiting: RefCell<Vec<Instant>>,
    /// The same keys, until any frame at all.
    keys_before_frame: RefCell<Vec<Instant>>,
    to_new_text: RefCell<Vec<f64>>,
    to_first_frame: RefCell<Vec<f64>>,
    keys_with_no_frame: Cell<u64>,
    /// Keys that had to wait behind a layout or in the editor's queue.
    keys_that_waited: Cell<u64>,
    /// The frame being painted shows the text as it is now.
    frame_is_current: Cell<bool>,
    /// The frame being painted holds real text.
    frame_has_text: Cell<bool>,
    /// The frame being painted holds as many rows as the text area fits.
    frame_is_full: Cell<bool>,
    first_frame_presented: Cell<Option<Instant>>,
    first_text_presented: Cell<Option<Instant>>,
    full_screen_presented: Cell<Option<Instant>>,
}
impl Timing {
    fn after_present(&self) {
        let now = Instant::now();
        for started in self.keys_before_frame.borrow_mut().drain(..) {
            self.to_first_frame
                .borrow_mut()
                .push((now - started).as_secs_f64() * 1000.0);
        }
        if self.frame_is_current.get() {
            for started in self.keys_waiting.borrow_mut().drain(..) {
                self.to_new_text
                    .borrow_mut()
                    .push((now - started).as_secs_f64() * 1000.0);
            }
        }
        if self.first_frame_presented.get().is_none() {
            self.first_frame_presented.set(Some(now));
        }
        if self.frame_has_text.get() && self.first_text_presented.get().is_none() {
            self.first_text_presented.set(Some(now));
        }
        if self.frame_is_full.get() && self.full_screen_presented.get().is_none() {
            self.full_screen_presented.set(Some(now));
        }
    }
    /// `queued`: the key has not run yet, or it has run but keys ahead of later ones wait.
    /// Its frame comes after that wait, so it counts as waiting for its frame.
    fn note_key(&self, started: Instant, repaint: bool, queued: bool) {
        if queued {
            self.keys_that_waited.set(self.keys_that_waited.get() + 1);
        }
        if repaint || queued {
            self.keys_waiting.borrow_mut().push(started);
            self.keys_before_frame.borrow_mut().push(started);
        } else {
            self.keys_with_no_frame
                .set(self.keys_with_no_frame.get() + 1);
        }
    }
    fn report_keys(&self, label: &str) {
        let summary = |samples: &RefCell<Vec<f64>>| -> Option<String> {
            let mut samples = samples.borrow_mut();
            if samples.is_empty() {
                return None;
            }
            samples.sort_by(f64::total_cmp);
            let at = |part: f64| {
                samples[((samples.len() as f64 * part).ceil() as usize)
                    .saturating_sub(1)
                    .min(samples.len() - 1)]
            };
            Some(format!(
                "p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
                at(0.5),
                at(0.99),
                at(1.0)
            ))
        };
        eprintln!(
            "keys, {label}: {} with a frame, {} with no frame, {} still waiting for their text",
            self.to_first_frame.borrow().len(),
            self.keys_with_no_frame.get(),
            self.keys_waiting.borrow().len(),
        );
        eprintln!("keys that waited, {label}: {}", self.keys_that_waited.get());
        if let Some(text) = summary(&self.to_new_text) {
            eprintln!("input to present, {label}: {text}");
        }
        if let Some(text) = summary(&self.to_first_frame) {
            eprintln!("input to first frame, {label}: {text}");
        }
    }
    /// `has_file` says a load ran. With none, text on screen is only what the user typed,
    /// and a time for it would say nothing about the program.
    fn report_first_frame(&self, process_start: Instant, load_start: Instant, has_file: bool) {
        let from_process = |at: Instant| (at - process_start).as_secs_f64() * 1000.0;
        let times = |at: Instant| {
            format!(
                "{:.3} ms from process creation, {:.3} ms from load start",
                from_process(at),
                (at - load_start).as_secs_f64() * 1000.0,
            )
        };
        match self.first_text_presented.get() {
            Some(at) if has_file => {
                eprintln!("first text frame: {}", times(at));
                if let Some(full) = self.full_screen_presented.get() {
                    eprintln!("full first screen: {}", times(full));
                } else {
                    eprintln!("full first screen: the text did not fill the text area");
                }
            }
            _ => {
                if let Some(at) = self.first_frame_presented.get() {
                    eprintln!(
                        "first frame: {:.3} ms from process creation",
                        from_process(at)
                    );
                }
            }
        }
    }
}

struct FontRenderer {
    inner: BitmapRenderer,
    timing: Rc<Timing>,
}
impl FontRenderer {
    fn register(&mut self, list: &DisplayList) {
        for command in &list.commands {
            if let DrawCommand::GlyphRun { font_id, .. } = command
                && let Some(face) = qe_layout::font_face(*font_id)
            {
                self.inner.register_font_face(*font_id, &face);
            }
        }
    }
}
impl Renderer for FontRenderer {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.inner.begin_frame()
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        self.register(list);
        self.inner.draw(list)
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        self.inner.end_frame()
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.inner.resize(width, height)
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.inner.set_dpi(dpi)
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        self.register(list);
        self.inner.render_to_bitmap(list, width, height)
    }
    fn invalidate(&mut self, rect: Rect) {
        self.inner.invalidate(rect);
    }
    fn invalidate_all(&mut self) {
        self.inner.invalidate_all();
    }
    fn scroll(&mut self, rect: Rect, dy: Dip) -> Result<(), RenderError> {
        self.inner.scroll(rect, dy)
    }
    fn present(&mut self, window: *mut std::ffi::c_void) -> Result<(), RenderError> {
        let result = <BitmapRenderer as Renderer>::present(&mut self.inner, window);
        self.timing.after_present();
        result
    }
    fn present_rect(
        &mut self,
        dc: *mut std::ffi::c_void,
        rect: PixelRect,
    ) -> Result<(), RenderError> {
        let result = <BitmapRenderer as Renderer>::present_rect(&mut self.inner, dc, rect);
        self.timing.after_present();
        result
    }
}

struct Args {
    file: Option<std::ffi::OsString>,
    hidden: bool,
    timings: bool,
    force_read_only: bool,
    bench_keys: usize,
    bench_wrap: bool,
    bench_middle: bool,
    bench_no_pause: bool,
    bench_end_first: bool,
    /// Runs the real message loop in a hidden window until the file has loaded.
    hidden_loop: bool,
    help: bool,
}
fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        file: None,
        hidden: false,
        timings: false,
        force_read_only: false,
        bench_keys: 0,
        bench_wrap: false,
        bench_middle: false,
        bench_no_pause: false,
        bench_end_first: false,
        hidden_loop: false,
        help: false,
    };
    let mut items = std::env::args_os().skip(1);
    while let Some(item) = items.next() {
        match item.to_str() {
            Some("--help") => args.help = true,
            Some("--hidden-smoke") => args.hidden = true,
            Some("--timings") => args.timings = true,
            Some("--force-mapped") => args.force_read_only = true,
            Some("--hidden-loop") => args.hidden_loop = true,
            Some("--bench-wrap") => args.bench_wrap = true,
            Some("--bench-middle") => args.bench_middle = true,
            Some("--bench-no-pause") => args.bench_no_pause = true,
            Some("--bench-end-first") => args.bench_end_first = true,
            Some("--bench-keys") => {
                args.bench_keys = items
                    .next()
                    .and_then(|n| n.to_str().and_then(|n| n.parse().ok()))
                    .ok_or("--bench-keys needs a number")?;
                args.hidden = true;
                args.timings = true;
            }
            // A mistyped flag must not open a window on screen.
            Some(flag) if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{HELP}"));
            }
            _ => args.file = Some(item),
        }
    }
    let bench_option =
        args.bench_wrap || args.bench_middle || args.bench_no_pause || args.bench_end_first;
    if bench_option && args.bench_keys == 0 {
        // Alone, these would open a window on screen.
        return Err(format!("this option needs --bench-keys\n{HELP}"));
    }
    Ok(args)
}

/// Takes a finished load into the view. `None` means it has not ended.
fn take_load(editor: &RefCell<EditorView>, load: &RefCell<Option<FileLoad>>) -> Option<bool> {
    let result = load.borrow_mut().as_mut()?.poll()?;
    *load.borrow_mut() = None;
    let mut editor = editor.borrow_mut();
    Some(match result {
        LoadResult::Loaded(file) => match editor.adopt(file) {
            Ok(()) => true,
            Err(e) => {
                editor.end_load();
                editor.show_warning(e);
                false
            }
        },
        LoadResult::Failed(message) => {
            eprintln!("{message}");
            editor.end_load();
            editor.show_warning(message);
            false
        }
        LoadResult::Cancelled => {
            editor.end_load();
            false
        }
    })
}

fn paint_now(handle: &ShellHandle) -> Result<(), String> {
    let hwnd = handle.hwnd().map_err(|e| e.to_string())?;
    // SAFETY: the handle is live, and this thread owns the window.
    unsafe {
        SendMessageW(HWND(hwnd), WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    Ok(())
}

struct Hidden<'a> {
    args: &'a Args,
    window: &'a ShellWindow,
    editor: &'a Rc<RefCell<EditorView>>,
    load: &'a Rc<RefCell<Option<FileLoad>>>,
    timing: &'a Rc<Timing>,
    closing: &'a Rc<Cell<bool>>,
    process_start: Instant,
    load_start: Instant,
}

/// The hidden run. It measures the first screen and then, on request, the keys.
fn run_hidden(run: Hidden) -> Result<(), String> {
    let Hidden {
        args,
        window,
        editor,
        load,
        timing,
        closing,
        process_start,
        load_start,
    } = run;
    let handle = window.handle();
    let limit = Instant::now() + Duration::from_secs(60);
    while load.borrow().is_some() && Instant::now() < limit {
        if take_load(editor, load).is_none() {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    let limit = Instant::now() + Duration::from_secs(5);
    let mut ctx = ViewContext::default();
    loop {
        ctx.requests.clear();
        editor.borrow_mut().poll_layout(&mut ctx);
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        handle.invalidate_content().map_err(|e| e.to_string())?;
        paint_now(&handle)?;
        // A short file never fills the text area, and then its layout's end ends this.
        let settled = !editor.borrow().pending_layout();
        if timing.full_screen_presented.get().is_some() || settled || Instant::now() >= limit {
            break;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    if let Some(warning) = editor.borrow().warning() {
        eprintln!("warning: {warning}");
    }
    if args.file.is_some() && timing.first_text_presented.get().is_none() {
        return Err("hidden editor smoke drew no file text".into());
    }
    if args.bench_keys > 0 {
        run_key_bench(args, window, editor, timing)?;
    }
    if args.timings {
        timing.report_first_frame(process_start, load_start, args.file.is_some());
    }
    if args.timings && !editor.borrow().can_edit() {
        let limit = Instant::now() + Duration::from_secs(60);
        while !editor.borrow().store_status().complete && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = editor.borrow().store_status();
        eprintln!(
            "line index: complete {}, at {:.3} ms from load start, {} lines",
            status.complete,
            load_start.elapsed().as_secs_f64() * 1000.0,
            status.known_lines
        );
    }
    closing.set(true);
    window.destroy().map_err(|e| e.to_string())?;
    println!("hidden editor smoke passed");
    Ok(())
}

fn run_key_bench(
    args: &Args,
    window: &ShellWindow,
    editor: &Rc<RefCell<EditorView>>,
    timing: &Rc<Timing>,
) -> Result<(), String> {
    let handle = window.handle();
    let measure = DirectWriteLayout::new().map_err(|e| e.to_string())?;
    editor
        .borrow_mut()
        .layout(window.content_rect().map_err(|e| e.to_string())?, &measure);
    let can_edit = editor.borrow().can_edit();
    let key = |code: u16| InputKind::KeyDown {
        virtual_key: code,
        modifiers: Modifiers::default(),
        repeat: false,
    };
    let focus = InputEvent {
        time_us: 0,
        kind: InputKind::Focus,
    };
    let mut ctx = ViewContext::default();
    editor.borrow_mut().handle(&focus, &mut ctx);
    handle
        .apply_requests(std::mem::take(&mut ctx.requests))
        .map_err(|e| e.to_string())?;
    let pump = |ctx: &mut ViewContext| -> Result<(), String> {
        editor.borrow_mut().poll_layout(ctx);
        let more = ctx
            .requests
            .iter()
            .any(|r| matches!(r, ViewRequest::Invalidate(_)));
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        if more {
            paint_now(&handle)?;
        }
        Ok(())
    };
    if args.bench_wrap {
        editor.borrow_mut().set_wrap(true)?;
        editor
            .borrow_mut()
            .layout(window.content_rect().map_err(|e| e.to_string())?, &measure);
    }
    if args.bench_middle {
        // A file still being indexed would give the middle of what is known so far.
        let limit = Instant::now() + Duration::from_secs(60);
        while !editor.borrow().store_status().complete && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        let middle = editor.borrow().store_status().known_bytes.0 / 2;
        editor.borrow_mut().select(
            qe_api::ByteOffset(middle)..qe_api::ByteOffset(middle),
            &mut ctx,
        );
        // The way there is not key work: it lays out the place the keys go to.
        let limit = Instant::now() + Duration::from_secs(10);
        pump(&mut ctx)?;
        while editor.borrow().pending_layout() && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(1));
            pump(&mut ctx)?;
        }
        eprintln!("the keys go to byte {}", editor.borrow().caret().0);
    }
    let mut first_keys = Vec::new();
    if args.bench_end_first {
        first_keys.push(InputKind::KeyDown {
            virtual_key: 0x23,
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
        });
        first_keys.push(key(0x24));
    }
    let mut handled = Vec::with_capacity(args.bench_keys);
    for i in 0..args.bench_keys {
        let kind = first_keys
            .get(i)
            .cloned()
            .unwrap_or(match (can_edit, i % 2 == 0) {
                (true, true) => InputKind::Char('q'),
                (true, false) => key(0x08),
                (false, true) => key(0x28),
                (false, false) => key(0x26),
            });
        let event = InputEvent {
            time_us: i as u64,
            kind,
        };
        let started = Instant::now();
        editor.borrow_mut().handle(&event, &mut ctx);
        handled.push(started.elapsed().as_secs_f64() * 1000.0);
        let repaint = ctx
            .requests
            .iter()
            .any(|r| matches!(r, ViewRequest::Invalidate(_)));
        let queued = editor.borrow().queued_input_count() > 0;
        timing.note_key(started, repaint, queued);
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        // The first keys arrive together, as a person's fast keys do: no result can come
        // in between them, so a key that needs one has to wait.
        if i < first_keys.len() {
            continue;
        }
        if repaint {
            paint_now(&handle)?;
        }
        if args.bench_no_pause {
            pump(&mut ctx)?;
            continue;
        }
        // A key on a shaped line ends when its worker's result is on screen.
        let limit = Instant::now() + Duration::from_secs(1);
        while !timing.keys_waiting.borrow().is_empty() && Instant::now() < limit {
            std::thread::sleep(Duration::from_micros(100));
            pump(&mut ctx)?;
        }
        // Workers run while a person's fingers move. Their results are not key work.
        std::thread::sleep(Duration::from_millis(3));
        editor.borrow_mut().poll_layout(&mut ctx);
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        paint_now(&handle)?;
    }
    // Keys sent with no pause may still wait for their frame.
    let limit = Instant::now() + Duration::from_secs(10);
    while !timing.keys_waiting.borrow().is_empty() && Instant::now() < limit {
        std::thread::sleep(Duration::from_micros(100));
        pump(&mut ctx)?;
    }
    handled.sort_by(f64::total_cmp);
    eprintln!(
        "of which the editor's handle: p50 {:.3} ms, p99 {:.3} ms",
        handled[handled.len() / 2],
        handled[(handled.len() * 99).div_ceil(100) - 1]
    );
    timing.report_keys(if can_edit {
        "typing and Backspace, editable file"
    } else {
        "Down and Up, read-only file"
    });
    Ok(())
}

fn show_menu_popup(handle: &ShellHandle, anchor: Rect, model: MenuModel) -> Result<(), String> {
    let owner = HWND(handle.hwnd().map_err(|e| e.to_string())?);
    let scale = handle.dpi().map_err(|e| e.to_string())? as f32 / 96.0;
    let theme = handle.theme().map_err(|e| e.to_string())?;
    let measure = DirectWriteLayout::new().map_err(|e| e.to_string())?;
    let mut screen = POINT {
        x: (anchor.x.0 * scale).round() as i32,
        y: (anchor.y.0 * scale).round() as i32,
    };
    if !unsafe { ClientToScreen(owner, &mut screen) }.as_bool() {
        return Err("placing the menu on screen failed".into());
    }
    let monitor = unsafe { MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err("reading the menu work area failed".into());
    }
    let work = Rect {
        x: Dip(info.rcWork.left as f32 / scale),
        y: Dip(info.rcWork.top as f32 / scale),
        width: Dip((info.rcWork.right - info.rcWork.left) as f32 / scale),
        height: Dip((info.rcWork.bottom - info.rcWork.top) as f32 / scale),
    };
    let anchor = Rect {
        x: Dip(screen.x as f32 / scale),
        y: Dip(screen.y as f32 / scale),
        width: anchor.width,
        height: anchor.height,
    };
    let (width, height) = model.preferred_size(&measure);
    let bounds = place_popup(anchor, (width, height), work, false);
    let mut view = MenuView::new(WidgetId(20_000), "QuickEdit menu", model);
    view.work_area = Some(work);
    view.layout(bounds, &measure);
    view.open_keyboard();
    let mut windows = MenuWindows::new(owner, scale);
    windows.sync(&view)?;
    let repaint = |view: &MenuView, windows: &MenuWindows| -> Result<(), String> {
        for (depth, popup) in windows.levels().iter().enumerate() {
            let rect = view.levels()[depth];
            let mut renderer = BitmapRenderer::new(
                (rect.width.0 * scale).ceil() as u32,
                (rect.height.0 * scale).ceil() as u32,
            )
            .map_err(|e| e.message)?;
            renderer
                .set_dpi((scale * 96.0) as u32)
                .map_err(|e| e.message)?;
            renderer
                .set_text_settings(TextSettings::reference())
                .map_err(|e| e.message)?;
            let mut list = DisplayList::default();
            view.draw_level(depth, &mut list, &theme, &measure);
            renderer.begin_frame().map_err(|e| e.message)?;
            renderer.draw(&list).map_err(|e| e.message)?;
            renderer.end_frame().map_err(|e| e.message)?;
            renderer.present(popup.hwnd()).map_err(|e| e.message)?;
        }
        Ok(())
    };
    repaint(&view, &windows)?;
    let mut origin = POINT { x: 0, y: 0 };
    if !unsafe { ClientToScreen(owner, &mut origin) }.as_bool() {
        return Err("reading the owner position failed".into());
    }
    let bar_tree = handle.widgets().map_err(|e| e.to_string())?;
    let bar = bar_tree
        .children
        .iter()
        .find(|node| node.name == "Menu bar");
    let current_group =
        bar.and_then(|node| node.children.iter().position(|child| child.states.expanded));
    let mut switched = false;
    let mut chosen = false;
    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 == -1 {
            return Err(format!(
                "reading menu messages failed: {}",
                windows::core::Error::from_thread()
            ));
        }
        if got.0 == 0 {
            unsafe {
                PostQuitMessage(msg.wParam.0 as i32);
            }
            break;
        }
        if msg.message == POPUP_DISMISSED_MESSAGE {
            if windows
                .levels()
                .first()
                .is_some_and(|root| root.hwnd().0 as usize == msg.wParam.0)
            {
                break;
            }
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
        }
        let pointer = windows.pointer(msg.lParam);
        let down = |key: i32| unsafe { GetKeyState(key) } < 0;
        let modifiers = Modifiers {
            ctrl: down(i32::from(VK_CONTROL.0)),
            alt: down(i32::from(VK_MENU.0)),
            shift: down(i32::from(VK_SHIFT.0)),
            win: false,
        };
        if msg.message == WM_SYSKEYDOWN
            && modifiers.alt
            && !modifiers.ctrl
            && (msg.wParam.0 == usize::from(VK_F4.0) || msg.wParam.0 == usize::from(VK_SPACE.0))
        {
            view.closed = true;
            windows.sync(&view)?;
            if msg.wParam.0 == usize::from(VK_F4.0) {
                unsafe {
                    SendMessageW(owner, WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
                }
            } else {
                unsafe {
                    SendMessageW(
                        owner,
                        WM_SYSCOMMAND,
                        Some(WPARAM(SC_KEYMENU as usize)),
                        Some(LPARAM(0)),
                    );
                }
            }
            break;
        }
        if msg.message == WM_MOUSEMOVE
            && let (Some(at), Some(bar)) = (pointer, bar)
        {
            let client = Point {
                x: Dip(at.x.0 - origin.x as f32 / scale),
                y: Dip(at.y.0 - origin.y as f32 / scale),
            };
            let next = bar.children.iter().position(|label| {
                let r = label.bounds;
                client.x.0 >= r.x.0
                    && client.x.0 < r.x.0 + r.width.0
                    && client.y.0 >= r.y.0
                    && client.y.0 < r.y.0 + r.height.0
            });
            if current_group.is_some() && next.is_some() && next != current_group {
                view.closed = true;
                windows.sync(&view)?;
                handle
                    .menu_bar_input(InputKind::MouseMove { at: client })
                    .map_err(|e| e.to_string())?;
                switched = true;
                break;
            }
        }
        if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN)
            && current_group.is_some()
            && view.child.is_none()
            && (msg.wParam.0 == 0x25 || msg.wParam.0 == 0x27)
            && (msg.wParam.0 == 0x25
                || view
                    .selected
                    .and_then(|i| view.model.items.get(i))
                    .is_none_or(|item| item.submenu.is_none()))
        {
            view.closed = true;
            windows.sync(&view)?;
            handle
                .menu_bar_input(InputKind::KeyDown {
                    virtual_key: msg.wParam.0 as u16,
                    modifiers: Modifiers::default(),
                    repeat: false,
                })
                .map_err(|e| e.to_string())?;
            switched = true;
            break;
        }
        let kind = match msg.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(InputKind::KeyDown {
                virtual_key: msg.wParam.0 as u16,
                modifiers,
                repeat: msg.lParam.0 & (1 << 30) != 0,
            }),
            WM_KEYUP | WM_SYSKEYUP => Some(InputKind::KeyUp {
                virtual_key: msg.wParam.0 as u16,
                modifiers,
            }),
            WM_CHAR => char::from_u32(msg.wParam.0 as u32).map(InputKind::Char),
            WM_MOUSEMOVE => pointer.map(|at| InputKind::MouseMove { at }),
            WM_LBUTTONDOWN => pointer.map(|at| InputKind::MouseDown {
                at,
                button: qe_api::MouseButton::Left,
                modifiers,
            }),
            WM_LBUTTONUP => pointer.map(|at| InputKind::MouseUp {
                at,
                button: qe_api::MouseButton::Left,
                modifiers,
            }),
            _ => None,
        };
        unsafe {
            DispatchMessageW(&msg);
        }
        if let Some(kind) = kind {
            let mut ctx = ViewContext::default();
            view.handle(&InputEvent { time_us: 0, kind }, &mut ctx);
            for request in ctx.requests {
                if let ViewRequest::Run(command) = request {
                    chosen = true;
                    println!("{command:?}");
                }
            }
            if view.closed {
                break;
            }
            windows.sync(&view)?;
            repaint(&view, &windows)?;
        }
    }
    if switched {
        return Ok(());
    }
    if chosen || windows.dismissed() == Some(PopupDismissed::OutsideClick) {
        handle
            .menu_bar_input(InputKind::MouseDown {
                at: Point {
                    x: Dip(-1.0),
                    y: Dip(-1.0),
                },
                button: qe_api::MouseButton::Left,
                modifiers: Modifiers::default(),
            })
            .map_err(|e| e.to_string())
    } else {
        handle.close_menu_popup().map_err(|e| e.to_string())
    }
}

fn main() -> Result<(), String> {
    let args = parse_args()?;
    // Only the timers use this, so its failure must not stop the program from opening.
    let process_start = match process_creation() {
        Ok(start) => start,
        Err(problem) => {
            if args.timings {
                eprintln!("{problem}. The times below count from the start of main.");
            }
            Instant::now()
        }
    };
    if args.help {
        println!("{HELP}");
        return Ok(());
    }
    let title = args
        .file
        .as_ref()
        .map_or_else(|| "Untitled".into(), |p| p.to_string_lossy().into_owned());
    let editor = Rc::new(RefCell::new(EditorView::from_text("")?));
    editor
        .borrow_mut()
        .set_clipboard(Box::new(SystemClipboard(qe_ui::SystemClipboard)));
    let timing = Rc::new(Timing::default());
    let window = ShellWindow::create_with_refresh(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            ..Default::default()
        },
        FontRenderer {
            inner: BitmapRenderer::new(800, 600).map_err(|e| e.to_string())?,
            timing: timing.clone(),
        },
        |renderer, hwnd| renderer.inner.refresh_text_settings_for_window(HWND(hwnd)),
        Box::new(DirectWriteLayout::new().map_err(|e| e.to_string())?),
    )
    .map_err(|e| e.to_string())?;
    let waker = window.handle().waker().map_err(|e| e.to_string())?;
    let wake: Notify = Arc::new(move || {
        // A window that has gone has nothing to wake, and a refused post is covered by
        // the view's own timer while it waits for work.
        let _ = waker.wake();
    });
    editor.borrow_mut().set_wake(wake.clone());
    window
        .add_tab(Tab::new(&title))
        .map_err(|e| e.to_string())?;
    window
        .set_content_cursor(MouseCursor::IBeam)
        .map_err(|e| e.to_string())?;
    {
        let dpi = window.dpi().map_err(|e| e.to_string())? as f32;
        let scale = 96.0 / dpi.max(1.0);
        // SAFETY: plain queries of system settings, and `lines` lives through the call.
        let (time_ms, width, height) = unsafe {
            (
                GetDoubleClickTime(),
                GetSystemMetrics(SM_CXDOUBLECLK),
                GetSystemMetrics(SM_CYDOUBLECLK),
            )
        };
        let mut editor = editor.borrow_mut();
        editor.set_double_click_limits(
            u64::from(time_ms) * 1000,
            width as f32 * scale,
            height as f32 * scale,
        );
        let mut lines = 3_u32;
        // SAFETY: the call writes one u32 to `lines`, which lives through the call.
        let read = unsafe {
            SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                Some((&raw mut lines).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        match read {
            Ok(()) => editor.set_wheel_lines(lines),
            Err(e) => eprintln!("reading the wheel setting failed, so one notch is 3 rows: {e}"),
        }
    }
    let paint_editor = editor.clone();
    let paint_timing = timing.clone();
    let measurer = DirectWriteLayout::new().map_err(|e| e.to_string())?;
    window.set_content_painter(move |list, frame| {
        paint_editor.borrow_mut().layout(frame.rect, &measurer);
        let start = list.commands.len();
        paint_editor.borrow().draw(list, &frame.theme, &measurer);
        let rows: std::collections::BTreeSet<i32> = list.commands[start..]
            .iter()
            .filter_map(|c| match c {
                DrawCommand::GlyphRun { at, .. } => Some(at.y.0 as i32),
                _ => None,
            })
            .collect();
        if !rows.is_empty() {
            paint_timing.frame_has_text.set(true);
        }
        // Full: the bottom row is within two rows of the text area's bottom edge.
        let bottom = frame.rect.y.0 + frame.rect.height.0;
        if rows.last().is_some_and(|y| *y as f32 + 40.0 >= bottom) {
            paint_timing.frame_is_full.set(true);
        }
        paint_timing.frame_is_current.set(
            paint_editor.borrow().caret_is_laid_out()
                && paint_editor.borrow().queued_input_count() == 0,
        );
    });

    let load_start = Instant::now();
    let load: Rc<RefCell<Option<FileLoad>>> = Rc::new(RefCell::new(None));
    if let Some(path) = &args.file {
        let options = LoadOptions {
            force_read_only: args.force_read_only,
            ..Default::default()
        };
        editor.borrow_mut().begin_load();
        let started = FileLoad::start(path, options, wake).map_err(|e| e.to_string())?;
        *load.borrow_mut() = Some(started);
    }

    let event_editor = editor.clone();
    let event_timing = timing.clone();
    let event_load = load.clone();
    let closing = Rc::new(Cell::new(false));
    let event_closing = closing.clone();
    let timings = args.timings;
    let hidden = args.hidden;
    let hidden_loop = args.hidden_loop;
    let has_file = args.file.is_some();
    window.set_event_handler(move |handle, event| {
        // Destroying a window sends it a last focus change and other events, and they
        // arrive here after it has gone. They have nothing left to act on.
        if event_closing.get() && event != ShellEvent::WindowDestroyed {
            return;
        }
        let result = match event {
            ShellEvent::OpenMenuGroup(index, anchor) => {
                if let Some(group) = menu_bar_groups().into_iter().nth(index) {
                    if let Err(problem) = show_menu_popup(handle, anchor, group.submenu.unwrap_or_default()) {
                        eprintln!("{problem}");
                    }
                } else { eprintln!("menu group {index} does not exist"); }
                Ok(())
            }
            ShellEvent::OpenCaretList(anchor) => {
                if let Err(problem) = show_menu_popup(handle, anchor, caret_menu()) { eprintln!("{problem}"); }
                Ok(())
            }
            ShellEvent::ContentInput(input) => {
                let is_key = matches!(input.kind, InputKind::KeyDown { .. } | InputKind::Char(_));
                let started = input_time(input.time_us, Instant::now());
                let mut ctx = ViewContext::default();
                event_editor.borrow_mut().handle(&input, &mut ctx);
                if timings && is_key {
                    let repaint = ctx
                        .requests
                        .iter()
                        .any(|r| matches!(r, ViewRequest::Invalidate(_)));
                    let queued = event_editor.borrow().queued_input_count() > 0;
                    event_timing.note_key(started, repaint, queued);
                }
                if input.kind == InputKind::Blur {
                    let _ = handle.kill_timer(VIEW_TIMER);
                }
                handle.apply_requests(ctx.requests)
            }
            ShellEvent::Timer(VIEW_TIMER) | ShellEvent::Wake => {
                let mut ctx = ViewContext::default();
                let took = if hidden {
                    None
                } else {
                    take_load(&event_editor, &event_load)
                };
                if took.is_some() {
                    ctx.invalidate(handle.content_rect().unwrap_or(Rect {
                        x: Dip(0.0),
                        y: Dip(0.0),
                        width: Dip(0.0),
                        height: Dip(0.0),
                    }));
                }
                event_editor.borrow_mut().on_timer(&mut ctx);
                let applied = handle.apply_requests(ctx.requests);
                if hidden_loop && took.is_some() {
                    println!(
                        "hidden loop: the loading thread woke the window, {} bytes, {} lines, after {} ms",
                        event_editor.borrow().store_status().known_bytes.0,
                        event_editor.borrow().line_count(),
                        load_start.elapsed().as_millis(),
                    );
                    // A real window loses the focus while it is destroyed. A hidden one has
                    // no real focus to lose, so this sends what Windows would send.
                    if let Ok(hwnd) = handle.hwnd() {
                        // SAFETY: the handle is live, and this thread owns the window.
                        unsafe {
                            SendMessageW(HWND(hwnd), WM_KILLFOCUS, Some(WPARAM(0)), Some(LPARAM(0)));
                        }
                    }
                    event_closing.set(true);
                    handle.destroy()
                } else {
                    applied
                }
            }
            ShellEvent::ContentCaptureLost => {
                event_editor.borrow_mut().cancel_drag();
                Ok(())
            }
            ShellEvent::CloseWindowRequested => {
                event_closing.set(true);
                handle.destroy()
            }
            ShellEvent::WindowDestroyed => {
                // Closing cancels a load that is still running.
                *event_load.borrow_mut() = None;
                // A hidden run has printed its own numbers.
                if timings && !hidden {
                    event_timing.report_keys("this window");
                    event_timing.report_first_frame(process_start, load_start, has_file);
                }
                quit_message_loop(0);
                Ok(())
            }
            ShellEvent::Problem(problem) => {
                eprintln!("{problem}");
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            eprintln!("{error}");
        }
    });
    let mut initial = ViewContext::default();
    editor.borrow_mut().layout(
        window.content_rect().map_err(|e| e.to_string())?,
        &DirectWriteLayout::new().map_err(|e| e.to_string())?,
    );
    editor.borrow_mut().poll_layout(&mut initial);
    window
        .apply_requests(initial.requests)
        .map_err(|e| e.to_string())?;
    if args.hidden {
        return run_hidden(Hidden {
            args: &args,
            window: &window,
            editor: &editor,
            load: &load,
            timing: &timing,
            closing: &closing,
            process_start,
            load_start,
        });
    }
    if args.hidden_loop {
        // What showing a window does: it takes the focus, and the caret asks for its
        // blink timer. The load's wake must still reach the window right away.
        std::thread::sleep(Duration::from_millis(20));
        // SAFETY: this thread owns the window, and a hidden window takes no user input.
        if let Err(e) = unsafe { SetFocus(Some(HWND(window.hwnd().map_err(|e| e.to_string())?))) } {
            eprintln!("SetFocus on the hidden window failed: {e}");
        }
        let mut ctx = ViewContext::default();
        let focus = InputEvent {
            time_us: 0,
            kind: InputKind::Focus,
        };
        editor.borrow_mut().handle(&focus, &mut ctx);
        window
            .apply_requests(ctx.requests)
            .map_err(|e| e.to_string())?;
    } else {
        window.show().map_err(|e| e.to_string())?;
    }
    run_message_loop().map_err(|e| e.to_string())?;
    Ok(())
}
