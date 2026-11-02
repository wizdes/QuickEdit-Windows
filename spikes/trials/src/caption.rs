use crate::win::*;
use std::ffi::c_void;
use std::ptr::{null, null_mut};

const ROW: i32 = 40;
const GWLP_USERDATA: i32 = -21;
const SC_MINIMIZE: usize = 0xf020;
const SC_MAXIMIZE: usize = 0xf030;
const SC_RESTORE: usize = 0xf120;
const SC_CLOSE: usize = 0xf060;
const WM_SYSCOMMAND: u32 = 0x112;
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        root: *mut c_void,
        sub: *const u16,
        value: *const u16,
        flags: u32,
        typ: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
}

struct State {
    hover: i32,
    popup: Hwnd,
    dark: bool,
}
fn dark() -> bool {
    unsafe {
        let mut v: u32 = 1;
        let mut size = 4;
        let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let name = wide("AppsUseLightTheme");
        let status = RegGetValueW(
            0x80000001usize as *mut c_void,
            key.as_ptr(),
            name.as_ptr(),
            0x10,
            null_mut(),
            (&mut v as *mut u32).cast(),
            &mut size,
        );
        status == 0 && v == 0
    }
}
fn scale(hwnd: Hwnd, n: i32) -> i32 {
    unsafe { (n * GetDpiForWindow(hwnd) as i32 + 48) / 96 }
}
unsafe fn state(hwnd: Hwnd) -> Option<&'static mut State> {
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State).as_mut() }
}
unsafe fn theme(hwnd: Hwnd, s: &mut State) {
    unsafe {
        s.dark = dark();
        let enabled = if s.dark { 1i32 } else { 0 };
        let _ = DwmSetWindowAttribute(hwnd, 20, (&enabled as *const i32).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, 33, (&2i32 as *const i32).cast(), 4);
        let _ = DwmExtendFrameIntoClientArea(
            hwnd,
            &Margins {
                left: 0,
                right: 0,
                top: 1,
                bottom: 0,
            },
        );
        InvalidateRect(hwnd, null(), 1);
    }
}
fn button_at(hwnd: Hwnd, x: i32, width: i32) -> i32 {
    let b = scale(hwnd, 46);
    if x >= width - b {
        6
    } else if x >= width - 2 * b {
        5
    } else if x >= width - 3 * b {
        4
    } else if x >= scale(hwnd, 270) && x < scale(hwnd, 304) {
        3
    } else if x >= scale(hwnd, 236) && x < scale(hwnd, 270) {
        2
    } else {
        0
    }
}
unsafe fn popup(hwnd: Hwnd, s: &mut State) {
    unsafe {
        if !s.popup.is_null() {
            DestroyWindow(s.popup);
            s.popup = null_mut();
            return;
        }
        let name = wide("SpikeCaptionPopup");
        let wc = Wndclass {
            style: 0,
            proc: Some(popup_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance: null_mut(),
            icon: null_mut(),
            cursor: null_mut(),
            brush: null_mut(),
            class: name.as_ptr(),
            menu: null(),
        };
        RegisterClassW(&wc);
        let mut r = Rect::default();
        GetWindowRect(hwnd, &mut r);
        let p = CreateWindowExW(
            0x80 | 0x8,
            name.as_ptr(),
            wide("Menu").as_ptr(),
            WS_POPUP | WS_VISIBLE,
            r.left + scale(hwnd, 269),
            r.top + scale(hwnd, ROW),
            scale(hwnd, 210),
            scale(hwnd, 100),
            hwnd,
            null_mut(),
            null_mut(),
            null_mut(),
        );
        if p.is_null() {
            eprintln!("CreateWindowExW popup failed");
        } else {
            s.popup = p;
            ShowWindow(p, SW_SHOW);
            UpdateWindow(p);
        }
    }
}
unsafe extern "system" fn popup_proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut p = std::mem::zeroed::<Paint>();
                let dc = BeginPaint(hwnd, &mut p);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                fill(dc, r, rgb(246, 246, 246));
                text(
                    dc,
                    "New tab       Ctrl+T",
                    Rect {
                        left: 12,
                        top: 8,
                        right: r.right - 8,
                        bottom: 36,
                    },
                    rgb(20, 20, 20),
                    DT_VCENTER | DT_SINGLELINE,
                );
                text(
                    dc,
                    "Settings",
                    Rect {
                        left: 12,
                        top: 42,
                        right: r.right - 8,
                        bottom: 74,
                    },
                    rgb(20, 20, 20),
                    DT_VCENTER | DT_SINGLELINE,
                );
                EndPaint(hwnd, &p);
                0
            }
            WM_LBUTTONDOWN => {
                DestroyWindow(hwnd);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
unsafe extern "system" fn proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    unsafe {
        match msg {
            WM_NCCALCSIZE if w != 0 => {
                if IsZoomed(hwnd) != 0 {
                    let r = &mut *(l as *mut Rect);
                    let dpi = GetDpiForWindow(hwnd);
                    let inset = GetSystemMetricsForDpi(32, dpi) + GetSystemMetricsForDpi(92, dpi);
                    r.left += inset;
                    r.right -= inset;
                    r.top += inset;
                    r.bottom -= inset;
                }
                0
            }
            WM_NCHITTEST => {
                let mut out = 0;
                if DwmDefWindowProc(hwnd, msg, w, l, &mut out) != 0 && out != HTCLIENT {
                    return out;
                }
                let mut p = xy(l);
                ScreenToClient(hwnd, &mut p);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                let edge = scale(hwnd, 8);
                if IsZoomed(hwnd) == 0 {
                    let left = p.x < edge;
                    let right = p.x >= r.right - edge;
                    let top = p.y < edge;
                    let bottom = p.y >= r.bottom - edge;
                    if top && left {
                        return HTTOPLEFT;
                    }
                    if top && right {
                        return HTTOPRIGHT;
                    }
                    if bottom && left {
                        return HTBOTTOMLEFT;
                    }
                    if bottom && right {
                        return HTBOTTOMRIGHT;
                    }
                    if left {
                        return HTLEFT;
                    }
                    if right {
                        return HTRIGHT;
                    }
                    if top {
                        return HTTOP;
                    }
                    if bottom {
                        return HTBOTTOM;
                    }
                }
                if p.y < scale(hwnd, ROW) {
                    match button_at(hwnd, p.x, r.right) {
                        4 => HTMINBUTTON,
                        5 => HTMAXBUTTON,
                        6 => HTCLOSE,
                        2 | 3 => HTCLIENT,
                        _ => HTCAPTION,
                    }
                } else {
                    HTCLIENT
                }
            }
            WM_SIZE => {
                InvalidateRect(hwnd, null(), 1);
                0
            }
            WM_THEMECHANGED | WM_SETTINGCHANGE => {
                if let Some(s) = state(hwnd) {
                    theme(hwnd, s);
                }
                0
            }
            WM_DPICHANGED => {
                let suggested = &*(l as *const Rect);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    4 | 16,
                );
                0
            }
            WM_MOUSEMOVE => {
                let p = xy(l);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                if let Some(s) = state(hwnd) {
                    let n = if p.y < scale(hwnd, ROW) {
                        button_at(hwnd, p.x, r.right)
                    } else {
                        0
                    };
                    if n != s.hover {
                        s.hover = n;
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            WM_LBUTTONDOWN => {
                let p = xy(l);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                if p.y < scale(hwnd, ROW) && button_at(hwnd, p.x, r.right) == 3 {
                    if let Some(s) = state(hwnd) {
                        popup(hwnd, s);
                    }
                }
                0
            }
            WM_NCLBUTTONDOWN => {
                let command = match w as isize {
                    HTMINBUTTON => SC_MINIMIZE,
                    HTMAXBUTTON => {
                        if IsZoomed(hwnd) != 0 {
                            SC_RESTORE
                        } else {
                            SC_MAXIMIZE
                        }
                    }
                    HTCLOSE => SC_CLOSE,
                    _ => 0,
                };
                if command != 0 {
                    SendMessageW(hwnd, WM_SYSCOMMAND, command, 0);
                    0
                } else {
                    DefWindowProcW(hwnd, msg, w, l)
                }
            }
            WM_PAINT => {
                let mut p = std::mem::zeroed::<Paint>();
                let dc = BeginPaint(hwnd, &mut p);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                let (bg, tab, fg, hover) = if state(hwnd).is_some_and(|s| s.dark) {
                    (
                        rgb(32, 32, 32),
                        rgb(50, 50, 50),
                        rgb(240, 240, 240),
                        rgb(72, 72, 72),
                    )
                } else {
                    (
                        rgb(243, 243, 243),
                        rgb(255, 255, 255),
                        rgb(32, 32, 32),
                        rgb(223, 229, 238),
                    )
                };
                fill(dc, r, tab);
                let row = scale(hwnd, ROW);
                fill(
                    dc,
                    Rect {
                        left: 0,
                        top: 0,
                        right: r.right,
                        bottom: row,
                    },
                    bg,
                );
                fill(
                    dc,
                    Rect {
                        left: scale(hwnd, 10),
                        top: scale(hwnd, 7),
                        right: scale(hwnd, 232),
                        bottom: row,
                    },
                    tab,
                );
                text(
                    dc,
                    "QuickEdit    ×",
                    Rect {
                        left: scale(hwnd, 22),
                        top: 0,
                        right: scale(hwnd, 223),
                        bottom: row,
                    },
                    fg,
                    DT_VCENTER | DT_SINGLELINE,
                );
                for (id, label) in [
                    (2, "+"),
                    (3, "⌄"),
                    (4, "─"),
                    (5, if IsZoomed(hwnd) != 0 { "❐" } else { "□" }),
                    (6, "×"),
                ] {
                    let bounds = if id <= 3 {
                        let left = scale(hwnd, 236 + (id - 2) * 34);
                        Rect {
                            left,
                            top: 0,
                            right: left + scale(hwnd, 34),
                            bottom: row,
                        }
                    } else {
                        let b = scale(hwnd, 46);
                        let left = r.right - (7 - id) * b;
                        Rect {
                            left,
                            top: 0,
                            right: left + b,
                            bottom: row,
                        }
                    };
                    if state(hwnd).is_some_and(|s| s.hover == id) {
                        fill(dc, bounds, if id == 6 { rgb(232, 17, 35) } else { hover });
                    }
                    text(
                        dc,
                        label,
                        bounds,
                        fg,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                }
                text(
                    dc,
                    "Caption trial: hover maximize for Snap Layouts",
                    Rect {
                        left: 20,
                        top: row + 30,
                        right: r.right - 20,
                        bottom: row + 80,
                    },
                    fg,
                    DT_SINGLELINE,
                );
                EndPaint(hwnd, &p);
                0
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
pub fn run() -> Result<(), String> {
    unsafe {
        let hwnd = make_window("SpikeCaption", "Caption trial", proc, 900, 560)?;
        let state = Box::into_raw(Box::new(State {
            hover: 0,
            popup: null_mut(),
            dark: false,
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        theme(hwnd, &mut *state);
        loop_messages();
        Ok(())
    }
}
