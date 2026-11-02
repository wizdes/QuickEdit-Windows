//! A QuickEdit window that accepts keyboard and mouse input.
use qe_api::{
    Bitmap, Dip, DisplayList, DrawCommand, MouseCursor, PixelRect, Rect, RenderError, Renderer,
    View, ViewContext,
};
use qe_buffer::{MappedStore, PieceStore};
use qe_editor::{Clipboard, EditorView, preferred_line_ending_bytes};
use qe_layout::DirectWriteLayout;
use qe_render::BitmapRenderer;
use qe_shell::{
    ShellEvent, ShellWindow, Tab, VIEW_TIMER, WindowConfig, quit_message_loop, run_message_loop,
};
use qe_ui::ClipboardAccess;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicIsize, Ordering},
    },
    time::Instant,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::SetTimer;

struct FontRenderer(BitmapRenderer);
struct SystemClipboard(qe_ui::SystemClipboard);
impl Clipboard for SystemClipboard {
    fn read(&mut self) -> Result<String, String> {
        Ok(self.0.read_text()?.unwrap_or_default())
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        self.0.write_text(value)
    }
}
impl FontRenderer {
    fn register(&mut self, list: &DisplayList) {
        for command in &list.commands {
            if let DrawCommand::GlyphRun { font_id, .. } = command
                && let Some(face) = qe_layout::font_face(*font_id)
            {
                self.0.register_font_face(*font_id, &face);
            }
        }
    }
}
impl Renderer for FontRenderer {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.0.begin_frame()
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        self.register(list);
        self.0.draw(list)
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        self.0.end_frame()
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.0.resize(width, height)
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.0.set_dpi(dpi)
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        self.register(list);
        self.0.render_to_bitmap(list, width, height)
    }
    fn invalidate(&mut self, rect: Rect) {
        self.0.invalidate(rect);
    }
    fn invalidate_all(&mut self) {
        self.0.invalidate_all();
    }
    fn scroll(&mut self, rect: Rect, dy: Dip) -> Result<(), RenderError> {
        self.0.scroll(rect, dy)
    }
    fn present(&mut self, window: *mut std::ffi::c_void) -> Result<(), RenderError> {
        <BitmapRenderer as Renderer>::present(&mut self.0, window)
    }
    fn present_rect(
        &mut self,
        dc: *mut std::ffi::c_void,
        rect: PixelRect,
    ) -> Result<(), RenderError> {
        <BitmapRenderer as Renderer>::present_rect(&mut self.0, dc, rect)
    }
}

fn main() -> Result<(), String> {
    let start = Instant::now();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let hidden = args.iter().any(|a| a == "--hidden-smoke");
    let timings = args.iter().any(|a| a == "--timings");
    let force_mapped = args.iter().any(|a| a == "--force-mapped");
    let file = args
        .iter()
        .find(|a| *a != "--hidden-smoke" && *a != "--timings" && *a != "--force-mapped");
    let title = file.map_or_else(|| "Untitled".into(), |p| p.to_string_lossy().into_owned());
    let mapped_hwnd = Arc::new(AtomicIsize::new(0));
    let view = match file {
        Some(path)
            if force_mapped
                || std::fs::metadata(path).map_err(|e| e.to_string())?.len()
                    > 512 * 1024 * 1024 =>
        {
            let wake_window = mapped_hwnd.clone();
            let notify: qe_buffer::Notify = Arc::new(move || {
                let raw = wake_window.load(Ordering::Acquire);
                if raw != 0 {
                    let timer = unsafe {
                        SetTimer(
                            Some(HWND(raw as *mut std::ffi::c_void)),
                            VIEW_TIMER as usize,
                            1,
                            None,
                        )
                    };
                    if timer == 0 {
                        eprintln!("store notification could not wake editor window");
                    }
                }
            });
            EditorView::from_mapped(
                MappedStore::open_with_notify(path, notify).map_err(|e| e.to_string())?,
            )?
        }
        Some(path) => {
            let bytes =
                std::fs::read(path).map_err(|e| format!("open {}: {e}", path.to_string_lossy()))?;
            let ending = preferred_line_ending_bytes(&bytes);
            let task = PieceStore::build_async(bytes).map_err(|e| e.to_string())?;
            let store = task
                .join()
                .map_err(|_| "text worker panicked".to_string())?
                .map_err(|e| e.to_string())?;
            let mut view = EditorView::from_store(store)?;
            view.set_preferred_ending(ending);
            view
        }
        None => EditorView::from_text("")?,
    };
    let editor = Rc::new(RefCell::new(view));
    editor
        .borrow_mut()
        .set_clipboard(Box::new(SystemClipboard(qe_ui::SystemClipboard)));
    let window = ShellWindow::create_with_refresh(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            ..Default::default()
        },
        FontRenderer(BitmapRenderer::new(800, 600).map_err(|e| e.to_string())?),
        |renderer, hwnd| renderer.0.refresh_text_settings_for_window(HWND(hwnd)),
        Box::new(DirectWriteLayout::new().map_err(|e| e.to_string())?),
    )
    .map_err(|e| e.to_string())?;
    mapped_hwnd.store(
        window.hwnd().map_err(|e| e.to_string())? as isize,
        Ordering::Release,
    );
    window
        .add_tab(Tab::new(&title))
        .map_err(|e| e.to_string())?;
    window
        .set_content_cursor(MouseCursor::IBeam)
        .map_err(|e| e.to_string())?;
    let paint_editor = editor.clone();
    let measurer = DirectWriteLayout::new().map_err(|e| e.to_string())?;
    let mut first_frame = true;
    window.set_content_painter(move |list, frame| {
        paint_editor.borrow_mut().layout(frame.rect, &measurer);
        paint_editor.borrow().draw(list, &frame.theme, &measurer);
        if timings && first_frame {
            eprintln!(
                "first content frame: {:.3} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
            first_frame = false;
        }
    });
    let event_editor = editor.clone();
    let event_hwnd = mapped_hwnd.clone();
    let mut key_times_ms = Vec::<f64>::new();
    window.set_event_handler(move |handle, event| {
        let result = match event {
            ShellEvent::ContentInput(input) => {
                let started = Instant::now();
                let mut ctx = ViewContext::default();
                event_editor.borrow_mut().handle(&input, &mut ctx);
                if timings
                    && matches!(
                        input.kind,
                        qe_api::InputKind::KeyDown { .. } | qe_api::InputKind::Char(_)
                    )
                {
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    key_times_ms.push(elapsed);
                    eprintln!("key handling: {:.3} ms", elapsed);
                }
                if input.kind == qe_api::InputKind::Blur {
                    let _ = handle.kill_timer(VIEW_TIMER);
                }
                handle.apply_requests(ctx.requests)
            }
            ShellEvent::Timer(VIEW_TIMER) => {
                let _ = handle.kill_timer(VIEW_TIMER);
                let mut ctx = ViewContext::default();
                event_editor.borrow_mut().on_timer(&mut ctx);
                handle.apply_requests(ctx.requests)
            }
            ShellEvent::ContentCaptureLost => {
                event_editor.borrow_mut().cancel_drag();
                Ok(())
            }
            ShellEvent::CloseWindowRequested => handle.destroy(),
            ShellEvent::WindowDestroyed => {
                event_hwnd.store(0, Ordering::Release);
                if timings && !key_times_ms.is_empty() {
                    key_times_ms.sort_by(f64::total_cmp);
                    let index =
                        ((key_times_ms.len() as f64 * 0.99).ceil() as usize).saturating_sub(1);
                    eprintln!(
                        "key handling p99 over {} keys: {:.3} ms",
                        key_times_ms.len(),
                        key_times_ms[index]
                    );
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
    if hidden {
        let mut list = DisplayList::default();
        let frame = window.content_rect().map_err(|e| e.to_string())?;
        let theme = qe_api::Theme::new(qe_api::ThemeKind::Light);
        let measure = DirectWriteLayout::new().map_err(|e| e.to_string())?;
        editor.borrow_mut().layout(frame, &measure);
        let limit = Instant::now() + std::time::Duration::from_secs(2);
        while editor.borrow().pending_layout() && Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(5));
            editor.borrow_mut().poll_layout(&mut ViewContext::default());
        }
        editor.borrow().draw(&mut list, &theme, &measure);
        if file.is_some()
            && !list
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::GlyphRun { .. }))
        {
            return Err("hidden editor smoke drew no file text".into());
        }
        let mut renderer = FontRenderer(BitmapRenderer::new(800, 600).map_err(|e| e.to_string())?);
        renderer
            .render_to_bitmap(&list, 800, 600)
            .map_err(|e| e.to_string())?;
        window.destroy().map_err(|e| e.to_string())?;
        println!("hidden editor smoke passed");
        return Ok(());
    }
    window.show().map_err(|e| e.to_string())?;
    run_message_loop().map_err(|e| e.to_string())?;
    Ok(())
}
