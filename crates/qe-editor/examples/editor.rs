//! A QuickEdit window that accepts keyboard and mouse input.
//!
//! The window appears first and the file follows: a thread loads it, and its first
//! screen paints when it is ready. The thread and the layout workers wake the window
//! with a posted message, which nothing the window does with its timers can replace.
//!
//! `--timings` prints what the plan's budgets ask for:
//! - `input to present` runs from the moment a key arrives to the end of the present
//!   call of the first frame that shows the new text. On a line that a worker lays out,
//!   that is a later frame than the first.
//! - `input to first frame` ends at the next frame, whatever it shows.
//! - Both count only keys that produced a frame. The others are counted apart.
//! - `first text frame` is the first frame that holds real text, and `full first screen`
//!   is the first frame whose text reaches the text area's bottom. A document with no
//!   text prints `first frame` instead.
//!
//! `--bench-keys N` runs in a hidden window, sends WM_PAINT itself, and prints the same
//! numbers, so no window appears on screen. Windows' own copy to a hidden window draws
//! nothing, so this measures our work up to and including the present call.
use qe_api::{
    Bitmap, Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseCursor,
    PixelRect, Rect, RenderError, Renderer, View, ViewContext, ViewRequest,
};
use qe_buffer::Notify;
use qe_editor::{Clipboard, EditorView, FileLoad, LoadOptions, LoadResult};
use qe_layout::DirectWriteLayout;
use qe_render::BitmapRenderer;
use qe_shell::{
    ShellEvent, ShellHandle, ShellWindow, Tab, VIEW_TIMER, WindowConfig, quit_message_loop,
    run_message_loop,
};
use qe_ui::ClipboardAccess;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SPI_GETWHEELSCROLLLINES,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW, SystemParametersInfoW, WM_KILLFOCUS,
    WM_PAINT,
};

const HELP: &str = "\
editor [options] [file]
  (no option)        opens a real window on screen
  --hidden-smoke     hidden window; checks that the file's text reaches a frame
  --hidden-loop      hidden window; real message loop until the file has loaded
  --bench-keys N     hidden window; sends N keys and prints the timers
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
    fn note_key(&self, started: Instant, repaint: bool) {
        if repaint {
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
        if let Some(text) = summary(&self.to_new_text) {
            eprintln!("input to present, {label}: {text}");
        }
        if let Some(text) = summary(&self.to_first_frame) {
            eprintln!("input to first frame, {label}: {text}");
        }
    }
    fn report_first_frame(&self, process_start: Instant, load_start: Instant) {
        let times = |at: Instant| {
            format!(
                "{:.3} ms from process start, {:.3} ms from load start",
                (at - process_start).as_secs_f64() * 1000.0,
                (at - load_start).as_secs_f64() * 1000.0,
            )
        };
        if let Some(at) = self.first_text_presented.get() {
            eprintln!("first text frame: {}", times(at));
            if let Some(full) = self.full_screen_presented.get() {
                eprintln!("full first screen: {}", times(full));
            } else {
                eprintln!("full first screen: the text did not fill the text area");
            }
        } else if let Some(at) = self.first_frame_presented.get() {
            eprintln!("first frame: {}; it holds no text", times(at));
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
    if args.timings {
        timing.report_first_frame(process_start, load_start);
    }
    if args.bench_keys > 0 {
        run_key_bench(args, window, editor, timing)?;
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
    let mut handled = Vec::with_capacity(args.bench_keys);
    for i in 0..args.bench_keys {
        let kind = match (can_edit, i % 2 == 0) {
            (true, true) => InputKind::Char('q'),
            (true, false) => key(0x08),
            (false, true) => key(0x28),
            (false, false) => key(0x26),
        };
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
        timing.note_key(started, repaint);
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        if repaint {
            paint_now(&handle)?;
        }
        // A key on a shaped line ends when its worker's result is on screen.
        let limit = Instant::now() + Duration::from_secs(1);
        while !timing.keys_waiting.borrow().is_empty() && Instant::now() < limit {
            std::thread::sleep(Duration::from_micros(100));
            editor.borrow_mut().poll_layout(&mut ctx);
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
        }
        // Workers run while a person's fingers move. Their results are not key work.
        std::thread::sleep(Duration::from_millis(3));
        editor.borrow_mut().poll_layout(&mut ctx);
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        paint_now(&handle)?;
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

fn main() -> Result<(), String> {
    let process_start = Instant::now();
    let args = parse_args()?;
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
        paint_timing
            .frame_is_current
            .set(paint_editor.borrow().caret_is_laid_out());
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
    window.set_event_handler(move |handle, event| {
        // Destroying a window sends it a last focus change and other events, and they
        // arrive here after it has gone. They have nothing left to act on.
        if event_closing.get() && event != ShellEvent::WindowDestroyed {
            return;
        }
        let result = match event {
            ShellEvent::ContentInput(input) => {
                let is_key = matches!(input.kind, InputKind::KeyDown { .. } | InputKind::Char(_));
                let started = Instant::now();
                let mut ctx = ViewContext::default();
                event_editor.borrow_mut().handle(&input, &mut ctx);
                if timings && is_key {
                    let repaint = ctx
                        .requests
                        .iter()
                        .any(|r| matches!(r, ViewRequest::Invalidate(_)));
                    event_timing.note_key(started, repaint);
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
                if timings {
                    event_timing.report_keys("this window");
                    event_timing.report_first_frame(process_start, load_start);
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
