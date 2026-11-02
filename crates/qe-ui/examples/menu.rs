//! Manual popup inspection example. A visible desktop is required.
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseButton, Point,
    Rect, TextMeasurer, TextMetrics, Theme, ThemeKind, View, ViewContext,
};
use qe_ui::{MenuItem, MenuModel, MenuView, PopupWindow};
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

struct Measure;
impl TextMeasurer for Measure {
    fn measure(&self, text: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(text.chars().count() as f32 * 7.0),
            height: Dip(16.0),
            baseline: Dip(12.0),
        }
    }
}
fn rgb(c: Color) -> COLORREF {
    COLORREF(c.r as u32 | ((c.g as u32) << 8) | ((c.b as u32) << 16))
}
fn rect(r: Rect) -> RECT {
    RECT {
        left: r.x.0 as i32,
        top: r.y.0 as i32,
        right: (r.x.0 + r.width.0) as i32,
        bottom: (r.y.0 + r.height.0) as i32,
    }
}
fn paint(hwnd: windows::Win32::Foundation::HWND, view: &MenuView) -> Result<(), String> {
    let mut list = DisplayList::default();
    view.draw(&mut list, &Theme::new(ThemeKind::Light), &Measure);
    unsafe {
        let dc = GetDC(Some(hwnd));
        if dc.0.is_null() {
            return Err("GetDC failed".into());
        }
        for command in list.commands {
            match command {
                DrawCommand::RoundedRect { rect: r, color, .. }
                | DrawCommand::FillRect { rect: r, color } => {
                    let brush = CreateSolidBrush(rgb(color));
                    FillRect(dc, &rect(r), brush);
                    let _ = DeleteObject(brush.into());
                }
                DrawCommand::TextRun {
                    at,
                    text,
                    font,
                    color,
                } => {
                    let face: Vec<u16> = font.encode_utf16().chain(std::iter::once(0)).collect();
                    let f = CreateFontW(
                        -17,
                        0,
                        0,
                        0,
                        400,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        DEFAULT_QUALITY,
                        0,
                        PCWSTR(face.as_ptr()),
                    );
                    let old = SelectObject(dc, f.into());
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, rgb(color));
                    let wide: Vec<u16> = text.encode_utf16().collect();
                    let _ = TextOutW(dc, at.x.0 as i32, at.y.0 as i32, &wide);
                    SelectObject(dc, old);
                    let _ = DeleteObject(f.into());
                }
                _ => {}
            }
        }
        ReleaseDC(Some(hwnd), dc);
    }
    Ok(())
}
fn event(kind: InputKind) -> InputEvent {
    InputEvent { time_us: 0, kind }
}
fn main() -> Result<(), String> {
    let mut file = MenuItem::command("File", qe_api::CommandId::Open);
    file.submenu = Some(MenuModel {
        items: vec![
            MenuItem::command("New", qe_api::CommandId::New),
            MenuItem::command("Open…", qe_api::CommandId::Open),
            MenuItem::separator(),
            MenuItem::command("Save", qe_api::CommandId::Save),
            MenuItem::command("Save As…", qe_api::CommandId::SaveAs),
        ],
    });
    let mut view = MenuView::new(
        qe_api::WidgetId(1),
        MenuModel {
            items: vec![
                file,
                MenuItem::command("Edit", qe_api::CommandId::Undo),
                MenuItem::command("Settings", qe_api::CommandId::Settings),
            ],
        },
    );
    view.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(230.0),
            height: Dip(120.0),
        },
        &Measure,
    );
    view.open_keyboard();
    view.handle(
        &event(InputKind::KeyDown {
            virtual_key: 0x27,
            modifiers: Modifiers::default(),
            repeat: false,
        }),
        &mut ViewContext::default(),
    );
    let popup =
        PopupWindow::create(100, 100, 460, 220).map_err(|e| format!("Create popup: {e}"))?;
    let hwnd = popup.hwnd();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        SetCapture(hwnd);
    }
    paint(hwnd, &view)?;
    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        let kind = match msg.message {
            WM_KEYDOWN => Some(InputKind::KeyDown {
                virtual_key: msg.wParam.0 as u16,
                modifiers: Modifiers::default(),
                repeat: false,
            }),
            WM_CHAR => char::from_u32(msg.wParam.0 as u32).map(InputKind::Char),
            WM_MOUSEMOVE => {
                let x = msg.lParam.0 as i16 as f32;
                let y = (msg.lParam.0 >> 16) as i16 as f32;
                Some(InputKind::MouseMove {
                    at: Point {
                        x: Dip(x),
                        y: Dip(y),
                    },
                })
            }
            WM_LBUTTONUP => {
                let x = msg.lParam.0 as i16 as f32;
                let y = (msg.lParam.0 >> 16) as i16 as f32;
                if !(0.0..460.0).contains(&x) || !(0.0..220.0).contains(&y) {
                    break;
                }
                Some(InputKind::MouseUp {
                    at: Point {
                        x: Dip(x),
                        y: Dip(y),
                    },
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                })
            }
            WM_KILLFOCUS => break,
            _ => None,
        };
        if let Some(kind) = kind {
            view.handle(&event(kind), &mut ViewContext::default());
            if view.closed {
                break;
            }
            paint(hwnd, &view)?;
        } else {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    unsafe {
        let _ = ReleaseCapture();
    }
    Ok(())
}
