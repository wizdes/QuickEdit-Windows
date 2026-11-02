//! Manual check of the caret list in a real popup. It needs a visible desktop, so
//! root runs it, not the test suite.
//!
//! Every message goes through `TranslateMessage`, so that typed letters reach the
//! list as `WM_CHAR`. Focus loss arrives at the window procedure, because Windows
//! sends `WM_KILLFOCUS` directly and it never passes through this loop.
use qe_api::{
    Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseButton, Point, Rect,
    Renderer, TextMeasurer, TextMetrics, Theme, ThemeKind, View, ViewContext,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_ui::{MenuView, POPUP_DISMISSED_MESSAGE, PopupWindow, caret_menu};
use std::cell::RefCell;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LRESULT, RECT};
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
fn event(kind: InputKind) -> InputEvent {
    InputEvent { time_us: 0, kind }
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
fn point(lparam: isize) -> Point {
    Point {
        x: Dip(lparam as i16 as f32),
        y: Dip((lparam >> 16) as i16 as f32),
    }
}
struct Shared {
    view: MenuView,
    renderer: BitmapRenderer,
    width: i32,
    height: i32,
}
impl Shared {
    fn repaint(&mut self, hwnd: HWND) -> Result<(), String> {
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(self.width as f32),
                height: Dip(self.height as f32),
            },
            color: qe_ui::UiTokens::from_theme(&Theme::new(ThemeKind::Light)).background,
        });
        self.view
            .draw(&mut list, &Theme::new(ThemeKind::Light), &Measure);
        self.renderer.invalidate_all();
        self.renderer.begin_frame().map_err(|e| e.message)?;
        self.renderer.draw(&list).map_err(|e| e.message)?;
        self.renderer.end_frame().map_err(|e| e.message)?;
        self.renderer.present(hwnd).map_err(|e| e.message)
    }
}
fn main() -> Result<(), String> {
    let error = |e: windows::core::Error| format!("window: {e}");
    // SAFETY: creates a plain hidden owner window of a built-in class.
    let owner = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Caret list example"),
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
    let (width, height) = (520, 460);
    let popup = PopupWindow::create(owner, 100, 100, width, height).map_err(error)?;
    if let Some(problem) = popup.decoration_error() {
        eprintln!("The popup has square corners: {problem}");
    }
    let model = caret_menu();
    let (w, h) = model.preferred_size(&Measure);
    let mut view = MenuView::new(qe_api::WidgetId(1), model).named("All menus");
    view.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(w),
            height: Dip(h),
        },
        &Measure,
    );
    view.open_keyboard();
    let mut renderer = BitmapRenderer::new(width as u32, height as u32).map_err(|e| e.message)?;
    renderer
        .set_text_settings(TextSettings::reference())
        .map_err(|e| e.message)?;
    let shared = Rc::new(RefCell::new(Shared {
        view,
        renderer,
        width,
        height,
    }));
    let hwnd = popup.hwnd();
    let for_messages = shared.clone();
    popup.set_message_handler(Box::new(move |hwnd, msg, _, l| {
        let kind = match msg {
            WM_KILLFOCUS => Some(InputKind::Blur),
            WM_MOUSEMOVE => Some(InputKind::MouseMove { at: point(l.0) }),
            WM_LBUTTONDOWN => Some(InputKind::MouseDown {
                at: point(l.0),
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            }),
            WM_LBUTTONUP => Some(InputKind::MouseUp {
                at: point(l.0),
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            }),
            _ => None,
        }?;
        let mut state = for_messages.borrow_mut();
        state.view.handle(&event(kind), &mut ViewContext::default());
        if let Err(problem) = state.repaint(hwnd) {
            eprintln!("Painting failed: {problem}");
        }
        Some(LRESULT(0))
    }));
    popup.show();
    shared.borrow_mut().repaint(hwnd)?;
    let _ = RECT::default();
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out pointer.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 || msg.message == POPUP_DISMISSED_MESSAGE {
            break;
        }
        // SAFETY: `msg` came from GetMessageW. It runs for every message, handled or not.
        unsafe {
            let _ = TranslateMessage(&msg);
        }
        let kind = match msg.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(InputKind::KeyDown {
                virtual_key: msg.wParam.0 as u16,
                modifiers: modifiers(),
                repeat: msg.lParam.0 & (1 << 30) != 0,
            }),
            WM_CHAR => char::from_u32(msg.wParam.0 as u32).map(InputKind::Char),
            _ => None,
        };
        if let Some(kind) = kind {
            let mut state = shared.borrow_mut();
            let mut ctx = ViewContext::default();
            state.view.handle(&event(kind), &mut ctx);
            for request in ctx.requests {
                if let qe_api::ViewRequest::Run(command) = request {
                    println!("ran {command:?}");
                }
            }
            if state.view.closed {
                break;
            }
            state.repaint(hwnd)?;
        } else {
            // SAFETY: `msg` came from GetMessageW.
            unsafe {
                DispatchMessageW(&msg);
            }
        }
        if shared.borrow().view.closed {
            break;
        }
    }
    drop(popup);
    // SAFETY: the owner window is still alive here.
    unsafe {
        let _ = DestroyWindow(owner);
    }
    Ok(())
}
