//! A QuickEdit window that accepts keyboard and mouse input.
//!
//! The window appears first and the file follows: a thread loads it, and its first
//! screen paints when it is ready.
//!
//! `--timings` prints what the plan's budgets ask for. `input to present` runs from
//! the moment the event handler gets a key to the end of the present call of the frame
//! that shows it. `first text frame` is the first frame that holds real text, and
//! `full first screen` is the first frame whose text reaches the text area's bottom.
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
    sync::{
        Arc,
        atomic::{AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SendMessageW, SetTimer, WM_PAINT,
};

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
    key_started: Cell<Option<Instant>>,
    key_samples: RefCell<Vec<f64>>,
    /// The frame being painted holds real text.
    frame_has_text: Cell<bool>,
    first_text_presented: Cell<Option<Instant>>,
    /// The frame being painted holds as many rows as the text area fits.
    frame_is_full: Cell<bool>,
    full_screen_presented: Cell<Option<Instant>>,
}
impl Timing {
    fn after_present(&self) {
        if let Some(started) = self.key_started.take() {
            self.key_samples
                .borrow_mut()
                .push(started.elapsed().as_secs_f64() * 1000.0);
        }
        if self.frame_has_text.get() && self.first_text_presented.get().is_none() {
            self.first_text_presented.set(Some(Instant::now()));
        }
        if self.frame_is_full.get() && self.full_screen_presented.get().is_none() {
            self.full_screen_presented.set(Some(Instant::now()));
        }
    }
    fn report_keys(&self, label: &str) {
        let mut samples = self.key_samples.borrow_mut();
        if samples.is_empty() {
            return;
        }
        samples.sort_by(f64::total_cmp);
        let at = |part: f64| {
            samples[((samples.len() as f64 * part).ceil() as usize)
                .saturating_sub(1)
                .min(samples.len() - 1)]
        };
        eprintln!(
            "input to present, {label}: {} keys, p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
            samples.len(),
            at(0.5),
            at(0.99),
            at(1.0)
        );
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
}
fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        file: None,
        hidden: false,
        timings: false,
        force_read_only: false,
        bench_keys: 0,
        hidden_loop: false,
    };
    let mut items = std::env::args_os().skip(1);
    while let Some(item) = items.next() {
        match item.to_str() {
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
                return Err(format!("unknown option {flag}"));
            }
            _ => args.file = Some(item),
        }
    }
    Ok(args)
}

/// Takes a finished load into the view. Returns true once the load has ended.
fn take_load(editor: &RefCell<EditorView>, load: &RefCell<Option<FileLoad>>) -> Option<bool> {
    let result = load.borrow_mut().as_mut()?.poll()?;
    *load.borrow_mut() = None;
    let mut editor = editor.borrow_mut();
    Some(match result {
        LoadResult::Loaded(file) => match editor.adopt(file) {
            Ok(()) => true,
            Err(e) => {
                editor.show_warning(e);
                false
            }
        },
        LoadResult::Failed(message) => {
            eprintln!("{message}");
            editor.show_warning(message);
            false
        }
        LoadResult::Cancelled => false,
    })
}

fn paint_now(handle: &ShellHandle) -> Result<(), String> {
    let hwnd = handle.hwnd().map_err(|e| e.to_string())?;
    unsafe {
        SendMessageW(HWND(hwnd), WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    Ok(())
}

/// The hidden run. It measures the first screen and then, on request, the keys.
fn run_hidden(
    args: &Args,
    window: &ShellWindow,
    editor: &Rc<RefCell<EditorView>>,
    load: &Rc<RefCell<Option<FileLoad>>>,
    timing: &Rc<Timing>,
    process_start: Instant,
    load_start: Instant,
) -> Result<(), String> {
    let handle = window.handle();
    let limit = Instant::now() + Duration::from_secs(60);
    while load.borrow().is_some() && Instant::now() < limit {
        if take_load(editor, load).is_none() {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    let loaded = Instant::now();
    let limit = loaded + Duration::from_secs(5);
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
        let settled =
            timing.first_text_presented.get().is_some() && !editor.borrow().pending_layout();
        if timing.full_screen_presented.get().is_some()
            || settled
            || (args.file.is_none() && !editor.borrow().pending_layout())
            || Instant::now() >= limit
        {
            break;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    if let Some(warning) = editor.borrow().warning() {
        eprintln!("warning: {warning}");
    }
    if args.file.is_some() {
        let Some(at) = timing.first_text_presented.get() else {
            return Err("hidden editor smoke drew no file text".into());
        };
        if args.timings {
            let size = editor.borrow().store_status().known_bytes.0;
            eprintln!(
                "first text frame: {:.3} ms from process start, {:.3} ms from load start; file loaded at {:.3} ms; {} bytes known",
                (at - process_start).as_secs_f64() * 1000.0,
                (at - load_start).as_secs_f64() * 1000.0,
                (loaded - process_start).as_secs_f64() * 1000.0,
                size
            );
            match timing.full_screen_presented.get() {
                Some(full) => eprintln!(
                    "full first screen: {:.3} ms from process start",
                    (full - process_start).as_secs_f64() * 1000.0
                ),
                None => eprintln!("full first screen: the text did not fill the text area"),
            }
        }
    }
    if args.bench_keys > 0 {
        run_key_bench(args, window, editor, timing, load_start)?;
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
    window.destroy().map_err(|e| e.to_string())?;
    println!("hidden editor smoke passed");
    Ok(())
}

fn run_key_bench(
    args: &Args,
    window: &ShellWindow,
    editor: &Rc<RefCell<EditorView>>,
    timing: &Rc<Timing>,
    _load_start: Instant,
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
    timing.key_samples.borrow_mut().clear();
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
        timing.key_started.set(Some(started));
        editor.borrow_mut().handle(&event, &mut ctx);
        handled.push(started.elapsed().as_secs_f64() * 1000.0);
        let repaint = ctx
            .requests
            .iter()
            .any(|r| matches!(r, ViewRequest::Invalidate(_)));
        handle
            .apply_requests(std::mem::take(&mut ctx.requests))
            .map_err(|e| e.to_string())?;
        if repaint {
            paint_now(&handle)?;
        } else {
            timing.key_started.set(None);
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
    let hwnd_bits = Arc::new(AtomicIsize::new(
        window.hwnd().map_err(|e| e.to_string())? as isize
    ));
    window
        .add_tab(Tab::new(&title))
        .map_err(|e| e.to_string())?;
    window
        .set_content_cursor(MouseCursor::IBeam)
        .map_err(|e| e.to_string())?;
    {
        let dpi = window.dpi().map_err(|e| e.to_string())? as f32;
        let scale = 96.0 / dpi.max(1.0);
        // SAFETY: plain queries of system settings.
        let (time_ms, width, height) = unsafe {
            (
                GetDoubleClickTime(),
                GetSystemMetrics(SM_CXDOUBLECLK),
                GetSystemMetrics(SM_CYDOUBLECLK),
            )
        };
        editor.borrow_mut().set_double_click_limits(
            u64::from(time_ms) * 1000,
            width as f32 * scale,
            height as f32 * scale,
        );
    }
    let paint_editor = editor.clone();
    let paint_timing = timing.clone();
    let measurer = DirectWriteLayout::new().map_err(|e| e.to_string())?;
    let has_file = args.file.is_some();
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
        if !has_file || !rows.is_empty() {
            paint_timing.frame_has_text.set(true);
        }
        // Fit: the bottom row is within two rows of the text area's bottom edge.
        let bottom = frame.rect.y.0 + frame.rect.height.0;
        if !has_file || rows.last().is_some_and(|y| *y as f32 + 40.0 >= bottom) {
            paint_timing.frame_is_full.set(true);
        }
    });

    let load_start = Instant::now();
    let load: Rc<RefCell<Option<FileLoad>>> = Rc::new(RefCell::new(None));
    if let Some(path) = &args.file {
        let wake_window = hwnd_bits.clone();
        let wake: Notify = Arc::new(move || {
            let raw = wake_window.load(Ordering::Acquire);
            if raw != 0 {
                // SAFETY: Windows checks the handle, and the window clears it on destroy.
                let timer = unsafe {
                    SetTimer(
                        Some(HWND(raw as *mut std::ffi::c_void)),
                        VIEW_TIMER as usize,
                        1,
                        None,
                    )
                };
                if timer == 0 {
                    eprintln!("the file loader could not wake the editor window");
                }
            }
        });
        let options = LoadOptions {
            force_read_only: args.force_read_only,
            ..Default::default()
        };
        let started = FileLoad::start(path, options, wake).map_err(|e| e.to_string())?;
        *load.borrow_mut() = Some(started);
    }

    let event_editor = editor.clone();
    let event_hwnd = hwnd_bits.clone();
    let event_timing = timing.clone();
    let event_load = load.clone();
    let timings = args.timings;
    let hidden = args.hidden;
    let hidden_loop = args.hidden_loop;
    window.set_event_handler(move |handle, event| {
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
                    if repaint {
                        event_timing.key_started.set(Some(started));
                    } else {
                        // Nothing to present, so the key's work ends here.
                        event_timing
                            .key_samples
                            .borrow_mut()
                            .push(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                if input.kind == InputKind::Blur {
                    let _ = handle.kill_timer(VIEW_TIMER);
                }
                handle.apply_requests(ctx.requests)
            }
            ShellEvent::Timer(VIEW_TIMER) => {
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
                        "hidden loop: the loading thread woke the window, {} bytes, {} lines",
                        event_editor.borrow().store_status().known_bytes.0,
                        event_editor.borrow().line_count()
                    );
                    handle.destroy()
                } else {
                    applied
                }
            }
            ShellEvent::ContentCaptureLost => {
                event_editor.borrow_mut().cancel_drag();
                Ok(())
            }
            ShellEvent::CloseWindowRequested => handle.destroy(),
            ShellEvent::WindowDestroyed => {
                event_hwnd.store(0, Ordering::Release);
                // Closing cancels a load that is still running.
                *event_load.borrow_mut() = None;
                if timings {
                    event_timing.report_keys("this window");
                    if let Some(at) = event_timing.first_text_presented.get() {
                        eprintln!(
                            "first text frame: {:.3} ms from process start, {:.3} ms from load start",
                            (at - process_start).as_secs_f64() * 1000.0,
                            (at - load_start).as_secs_f64() * 1000.0,
                        );
                    }
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
        return run_hidden(
            &args,
            &window,
            &editor,
            &load,
            &timing,
            process_start,
            load_start,
        );
    }
    if !args.hidden_loop {
        window.show().map_err(|e| e.to_string())?;
    }
    run_message_loop().map_err(|e| e.to_string())?;
    Ok(())
}
