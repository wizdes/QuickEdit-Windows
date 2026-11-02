#![allow(dead_code)]
use std::ffi::c_void;
use std::ptr::{null, null_mut};
pub type Hwnd = *mut c_void;
pub type Hdc = *mut c_void;
pub type Hr = i32;
pub type Lresult = isize;
pub type Wparam = usize;
pub type Lparam = isize;
pub type Wndproc = unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[repr(C)]
pub struct Paint {
    pub hdc: Hdc,
    pub erase: i32,
    pub rc: Rect,
    pub restore: i32,
    pub inc: i32,
    pub reserved: [u8; 32],
}
#[repr(C)]
pub struct Msg {
    pub hwnd: Hwnd,
    pub message: u32,
    pub wparam: Wparam,
    pub lparam: Lparam,
    pub time: u32,
    pub pt: Point,
    pub private: u32,
}
#[repr(C)]
pub struct Wndclass {
    pub style: u32,
    pub proc: Option<Wndproc>,
    pub cls_extra: i32,
    pub wnd_extra: i32,
    pub instance: *mut c_void,
    pub icon: *mut c_void,
    pub cursor: *mut c_void,
    pub brush: *mut c_void,
    pub menu: *const u16,
    pub class: *const u16,
}
#[repr(C)]
pub struct Margins {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}
#[repr(C)]
#[derive(Default)]
pub struct Size {
    pub cx: i32,
    pub cy: i32,
}
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);
impl Guid {
    pub const fn new(a: u32, b: u16, c: u16, d: [u8; 8]) -> Self {
        Self(a, b, c, d)
    }
}
pub const WM_NCLBUTTONDOWN: u32 = 0xa1;
pub const WM_APP_LAYOUT: u32 = 0x8001;
pub const WM_DESTROY: u32 = 2;
pub const WM_SIZE: u32 = 5;
pub const WM_PAINT: u32 = 15;
pub const WM_CLOSE: u32 = 16;
pub const WM_GETOBJECT: u32 = 0x3d;
pub const WM_NCCALCSIZE: u32 = 0x83;
pub const WM_NCHITTEST: u32 = 0x84;
pub const WM_MOUSEMOVE: u32 = 0x200;
pub const WM_LBUTTONDOWN: u32 = 0x201;
pub const WM_LBUTTONUP: u32 = 0x202;
pub const WM_KEYDOWN: u32 = 0x100;
pub const WM_CHAR: u32 = 0x102;
pub const WM_THEMECHANGED: u32 = 0x031a;
pub const WM_SETTINGCHANGE: u32 = 0x001a;
pub const WM_DPICHANGED: u32 = 0x02e0;
pub const WS_OVERLAPPEDWINDOW: u32 = 0x00cf0000;
pub const WS_VISIBLE: u32 = 0x10000000;
pub const WS_POPUP: u32 = 0x80000000;
pub const SW_SHOW: i32 = 5;
pub const HTCLIENT: isize = 1;
pub const HTCAPTION: isize = 2;
pub const HTMINBUTTON: isize = 8;
pub const HTMAXBUTTON: isize = 9;
pub const HTLEFT: isize = 10;
pub const HTRIGHT: isize = 11;
pub const HTTOP: isize = 12;
pub const HTTOPLEFT: isize = 13;
pub const HTTOPRIGHT: isize = 14;
pub const HTBOTTOM: isize = 15;
pub const HTBOTTOMLEFT: isize = 16;
pub const HTBOTTOMRIGHT: isize = 17;
pub const HTCLOSE: isize = 20;
pub const DT_CENTER: u32 = 1;
pub const DT_VCENTER: u32 = 4;
pub const DT_SINGLELINE: u32 = 32;
pub const TRANSPARENT: i32 = 1;
#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    pub fn GetLastError() -> u32;
    pub fn LoadLibraryW(name: *const u16) -> *mut c_void;
    pub fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}
#[link(name = "user32")]
unsafe extern "system" {
    pub fn RegisterClassW(c: *const Wndclass) -> u16;
    pub fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: *mut c_void,
        inst: *mut c_void,
        param: *mut c_void,
    ) -> Hwnd;
    pub fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: Wparam, l: Lparam) -> Lresult;
    pub fn DestroyWindow(hwnd: Hwnd) -> i32;
    pub fn ShowWindow(hwnd: Hwnd, cmd: i32) -> i32;
    pub fn UpdateWindow(hwnd: Hwnd) -> i32;
    pub fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    pub fn PeekMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32, remove: u32) -> i32;
    pub fn TranslateMessage(msg: *const Msg) -> i32;
    pub fn DispatchMessageW(msg: *const Msg) -> Lresult;
    pub fn PostQuitMessage(code: i32);
    pub fn PostMessageW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> i32;
    pub fn SetProcessDpiAwarenessContext(value: isize) -> i32;
    pub fn SetThreadDpiAwarenessContext(value: isize) -> isize;
    pub fn GetThreadDpiAwarenessContext() -> isize;
    pub fn AreDpiAwarenessContextsEqual(a: isize, b: isize) -> i32;
    pub fn BeginPaint(hwnd: Hwnd, p: *mut Paint) -> Hdc;
    pub fn EndPaint(hwnd: Hwnd, p: *const Paint) -> i32;
    pub fn GetClientRect(hwnd: Hwnd, r: *mut Rect) -> i32;
    pub fn GetWindowRect(hwnd: Hwnd, r: *mut Rect) -> i32;
    pub fn ScreenToClient(hwnd: Hwnd, p: *mut Point) -> i32;
    pub fn ClientToScreen(hwnd: Hwnd, p: *mut Point) -> i32;
    pub fn GetDC(hwnd: Hwnd) -> Hdc;
    pub fn ReleaseDC(hwnd: Hwnd, dc: Hdc) -> i32;
    pub fn InvalidateRect(hwnd: Hwnd, r: *const Rect, erase: i32) -> i32;
    pub fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, w: i32, h: i32, flags: u32)
    -> i32;
    pub fn SetFocus(hwnd: Hwnd) -> Hwnd;
    pub fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    pub fn IsZoomed(hwnd: Hwnd) -> i32;
    pub fn SendMessageW(hwnd: Hwnd, msg: u32, w: Wparam, l: Lparam) -> Lresult;
    pub fn FillRect(hdc: Hdc, r: *const Rect, brush: *mut c_void) -> i32;
    pub fn DrawTextW(hdc: Hdc, text: *const u16, count: i32, r: *mut Rect, fmt: u32) -> i32;
    pub fn SetTextColor(hdc: Hdc, color: u32) -> u32;
    pub fn SetBkMode(hdc: Hdc, mode: i32) -> i32;
    pub fn LoadCursorW(inst: *mut c_void, name: *const u16) -> *mut c_void;
    pub fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
    pub fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
    pub fn SetCapture(hwnd: Hwnd) -> Hwnd;
    pub fn ReleaseCapture() -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn CreateSolidBrush(color: u32) -> *mut c_void;
    pub fn DeleteObject(obj: *mut c_void) -> i32;
    pub fn MoveToEx(hdc: Hdc, x: i32, y: i32, old: *mut Point) -> i32;
    pub fn LineTo(hdc: Hdc, x: i32, y: i32) -> i32;
    pub fn GetTextExtentPoint32W(dc: Hdc, text: *const u16, count: i32, size: *mut Size) -> i32;
}
#[link(name = "dwmapi")]
unsafe extern "system" {
    pub fn DwmExtendFrameIntoClientArea(hwnd: Hwnd, m: *const Margins) -> Hr;
    pub fn DwmSetWindowAttribute(hwnd: Hwnd, attr: u32, value: *const c_void, size: u32) -> Hr;
    pub fn DwmDefWindowProc(hwnd: Hwnd, msg: u32, w: Wparam, l: Lparam, out: *mut Lresult) -> i32;
}
#[link(name = "ole32")]
unsafe extern "system" {
    pub fn CoInitializeEx(res: *mut c_void, flags: u32) -> Hr;
    pub fn CoUninitialize();
    pub fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        clsctx: u32,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> Hr;
}
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}
pub unsafe fn fill(hdc: Hdc, r: Rect, color: u32) {
    unsafe {
        let b = CreateSolidBrush(color);
        FillRect(hdc, &r, b);
        DeleteObject(b);
    }
}
pub unsafe fn text(hdc: Hdc, s: &str, mut r: Rect, color: u32, fmt: u32) {
    unsafe {
        let utf: Vec<u16> = s.encode_utf16().collect();
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        DrawTextW(hdc, utf.as_ptr(), utf.len() as i32, &mut r, fmt);
    }
}
pub fn xy(l: Lparam) -> Point {
    Point {
        x: (l as i16) as i32,
        y: ((l >> 16) as i16) as i32,
    }
}
pub fn ok(hr: Hr, what: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{what}: HRESULT 0x{:08x}", hr as u32))
    } else {
        Ok(())
    }
}
pub unsafe fn loop_messages() {
    unsafe {
        let mut m = std::mem::zeroed::<Msg>();
        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}
pub unsafe fn make_window(
    class: &str,
    title: &str,
    proc: Wndproc,
    w: i32,
    h: i32,
) -> Result<Hwnd, String> {
    unsafe {
        let c = wide(class);
        let t = wide(title);
        let wc = Wndclass {
            style: 3,
            proc: Some(proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance: GetModuleHandleW(null()),
            icon: null_mut(),
            cursor: LoadCursorW(null_mut(), 32512usize as *const u16),
            brush: null_mut(),
            class: c.as_ptr(),
            menu: null(),
        };
        if RegisterClassW(&wc) == 0 {
            return Err(format!(
                "RegisterClassW({class}) failed: GetLastError={} module={:?} cursor={:?}",
                GetLastError(),
                wc.instance,
                wc.cursor
            ));
        }
        let hwnd = CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            150,
            120,
            w,
            h,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null_mut(),
        );
        if hwnd.is_null() {
            Err("CreateWindowExW failed".into())
        } else {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            Ok(hwnd)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "system" fn proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }
    #[test]
    fn window_registration_uses_exe_module() {
        unsafe {
            let hwnd = make_window("SpikeRegistrationRegression", "regression", proc, 100, 100)
                .expect("RegisterClassW and CreateWindowExW");
            assert_ne!(DestroyWindow(hwnd), 0);
        }
    }
}
