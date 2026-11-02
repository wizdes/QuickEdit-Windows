//! Manual check of the caret list in real popups. It needs a visible desktop, so root
//! runs it, not the test suite.
//!
//! Each open level of the list has its own window, sized to that level, from
//! `MenuWindows`. The first window holds the mouse. Every message goes through
//! `TranslateMessage`, so that typed letters reach the list as `WM_CHAR`.
use qe_api::{
    Dip, DisplayList, InputEvent, InputKind, Modifiers, MouseButton, Rect, Renderer, TextMeasurer,
    TextMetrics, Theme, ThemeKind, View, ViewContext,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_ui::{
    MenuBar, MenuView, MenuWindows, POPUP_DISMISSED_MESSAGE, menu_bar_groups, place_popup,
};
use std::time::Instant;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// A fixed measure, so that the example runs without another dependency.
struct Measure;
impl TextMeasurer for Measure {
    fn measure(&self, text: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(text.chars().count() as f32 * 7.0),
            height: Dip(18.0),
            baseline: Dip(14.0),
        }
    }
}
fn modifiers() -> Modifiers {
    // SAFETY: a plain query.
    let down = |key: i32| unsafe { GetKeyState(key) } < 0;
    Modifiers {
        ctrl: down(i32::from(VK_CONTROL.0)),
        alt: down(i32::from(VK_MENU.0)),
        shift: down(i32::from(VK_SHIFT.0)),
        win: false,
    }
}
/// Paints every open level into its own window.
fn repaint(view: &MenuView, windows: &MenuWindows) -> Result<(), String> {
    let theme = Theme::new(ThemeKind::Light);
    let rects = view.levels();
    for (depth, window) in windows.levels().iter().enumerate() {
        let Some(r) = rects.get(depth) else { break };
        let (width, height) = (r.width.0.ceil() as u32, r.height.0.ceil() as u32);
        let mut renderer = BitmapRenderer::new(width, height).map_err(|e| e.message)?;
        renderer
            .set_text_settings(TextSettings::reference())
            .map_err(|e| e.message)?;
        let mut list = DisplayList::default();
        view.draw_level(depth, &mut list, &theme, &Measure);
        renderer.begin_frame().map_err(|e| e.message)?;
        renderer.draw(&list).map_err(|e| e.message)?;
        renderer.end_frame().map_err(|e| e.message)?;
        renderer.present(window.hwnd()).map_err(|e| e.message)?;
    }
    Ok(())
}
fn main() -> Result<(), String> {
    let error = |e: windows::core::Error| format!("window: {e}");
    // SAFETY: creates a plain owner window of a built-in class.
    let owner = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Menu bar example"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            60,
            60,
            300,
            120,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(error)?;
    // SAFETY: `owner` is the window created above. A list opens over an active window.
    unsafe {
        let _ = SetForegroundWindow(owner);
    }
    let theme = Theme::new(ThemeKind::Light);
    let mut bar = MenuBar::new(qe_api::WidgetId(10));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(300.0),
            height: theme.menu_bar_height,
        },
        &Measure,
    );
    let mut bar_list = DisplayList::default();
    bar_list.push(qe_api::DrawCommand::FillRect {
        rect: Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(300.0),
            height: Dip(120.0),
        },
        color: theme.text_background,
    });
    bar.draw(&mut bar_list, &theme, &Measure);
    let mut bar_renderer = BitmapRenderer::new(300, 120).map_err(|e| e.message)?;
    bar_renderer
        .set_text_settings(TextSettings::reference())
        .map_err(|e| e.message)?;
    bar_renderer.begin_frame().map_err(|e| e.message)?;
    bar_renderer.draw(&bar_list).map_err(|e| e.message)?;
    bar_renderer.end_frame().map_err(|e| e.message)?;
    bar_renderer.present(owner).map_err(|e| e.message)?;
    let file = menu_bar_groups().remove(0);
    let model = file.submenu.unwrap();
    let (width, height) = model.preferred_size(&Measure);
    let label = bar.label_rect(0);
    let mut at = POINT {
        x: label.x.0 as i32,
        y: label.y.0 as i32,
    };
    if !unsafe { ClientToScreen(owner, &mut at) }.as_bool() {
        return Err("placing the File menu failed".into());
    }
    let anchor = Rect {
        x: Dip(at.x as f32),
        y: Dip(at.y as f32),
        ..label
    };
    let monitor = unsafe { MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err("reading the work area failed".into());
    }
    let work = Rect {
        x: Dip(info.rcWork.left as f32),
        y: Dip(info.rcWork.top as f32),
        width: Dip((info.rcWork.right - info.rcWork.left) as f32),
        height: Dip((info.rcWork.bottom - info.rcWork.top) as f32),
    };
    let bounds = place_popup(anchor, (width, height), work, false);
    let mut view = MenuView::new(qe_api::WidgetId(1), "File", model);
    view.layout(bounds, &Measure);
    view.open_keyboard();
    let mut windows = MenuWindows::new(owner, 1.0);
    windows.sync(&view)?;
    repaint(&view, &windows)?;
    let start = Instant::now();
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out pointer.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 || msg.message == POPUP_DISMISSED_MESSAGE {
            eprintln!(
                "list closed: message {:#x}, wparam {}",
                msg.message, msg.wParam.0
            );
            break;
        }
        // SAFETY: `msg` came from GetMessageW. It runs for every message, handled or not.
        unsafe {
            let _ = TranslateMessage(&msg);
        }
        let pointer = windows.pointer(msg.lParam);
        let kind = match msg.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(InputKind::KeyDown {
                virtual_key: msg.wParam.0 as u16,
                modifiers: modifiers(),
                repeat: msg.lParam.0 & (1 << 30) != 0,
            }),
            WM_KEYUP | WM_SYSKEYUP => Some(InputKind::KeyUp {
                virtual_key: msg.wParam.0 as u16,
                modifiers: modifiers(),
            }),
            WM_CHAR => char::from_u32(msg.wParam.0 as u32).map(InputKind::Char),
            WM_MOUSEMOVE => pointer.map(|at| InputKind::MouseMove { at }),
            WM_LBUTTONDOWN => pointer.map(|at| InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            }),
            WM_LBUTTONUP => pointer.map(|at| InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            }),
            _ => None,
        };
        // SAFETY: `msg` came from GetMessageW. The popup's own procedure judges each
        // press before the view hears of it.
        unsafe {
            DispatchMessageW(&msg);
        }
        if let Some(kind) = kind {
            let mut ctx = ViewContext::default();
            let event = InputEvent {
                time_us: start.elapsed().as_micros() as u64,
                kind,
            };
            view.handle(&event, &mut ctx);
            for request in ctx.requests {
                if let qe_api::ViewRequest::Run(command) = request {
                    println!("ran {command:?}");
                }
            }
            if view.closed {
                break;
            }
            windows.sync(&view)?;
            repaint(&view, &windows)?;
        }
    }
    drop(windows);
    // SAFETY: the owner window is still alive here.
    unsafe {
        let _ = DestroyWindow(owner);
    }
    Ok(())
}
