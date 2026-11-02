//! Win32 frame. The renderer stays owned by the caller.
use crate::{CaptionHit, ShellEvent, ShellTokens, Tab, TabStrip, caption_hit, maximized_insets};
use qe_api::{
    Color, Dip, DisplayList, InputEvent, InputKind, Modifiers, MouseButton, Point, Rect as ApiRect,
    Renderer, TextMeasurer, Theme, ThemeKind, View, ViewContext,
};
use std::ffi::c_void;
use std::ptr::{null, null_mut};

type Hwnd = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinPoint {
    x: i32,
    y: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
struct WinClass {
    style: u32,
    proc: Option<unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: Hwnd,
    icon: Hwnd,
    cursor: Hwnd,
    brush: Hwnd,
    menu: *const u16,
    class: *const u16,
}
#[repr(C)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w: usize,
    l: isize,
    time: u32,
    pt: WinPoint,
    private: u32,
}
#[repr(C)]
struct Paint {
    dc: Hwnd,
    erase: i32,
    rect: WinRect,
    restore: i32,
    inc: i32,
    reserved: [u8; 32],
}
#[repr(C)]
struct Margins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}
#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: WinRect,
    work: WinRect,
    flags: u32,
}
#[repr(C)]
struct HighContrast {
    size: u32,
    flags: u32,
    scheme: *mut u16,
}
#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bits: u16,
    compression: u32,
    image_size: u32,
    xppm: i32,
    yppm: i32,
    used: u32,
    important: u32,
}
#[repr(C)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 3],
}
#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(w: *const WinClass) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: Hwnd,
        instance: Hwnd,
        param: Hwnd,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
    fn DestroyWindow(hwnd: Hwnd) -> i32;
    fn ShowWindow(hwnd: Hwnd, cmd: i32) -> i32;
    fn UpdateWindow(hwnd: Hwnd) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
    fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    fn GetClientRect(hwnd: Hwnd, r: *mut WinRect) -> i32;
    fn GetWindowRect(hwnd: Hwnd, r: *mut WinRect) -> i32;
    fn MonitorFromWindow(hwnd: Hwnd, flags: u32) -> Hwnd;
    fn GetMonitorInfoW(monitor: Hwnd, info: *mut MonitorInfo) -> i32;
    fn ScreenToClient(hwnd: Hwnd, p: *mut WinPoint) -> i32;
    fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
    fn BeginPaint(hwnd: Hwnd, p: *mut Paint) -> Hwnd;
    fn EndPaint(hwnd: Hwnd, p: *const Paint) -> i32;
    fn InvalidateRect(hwnd: Hwnd, r: *const WinRect, erase: i32) -> i32;
    fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn IsZoomed(hwnd: Hwnd) -> i32;
    fn SendMessageW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
    fn SetCapture(hwnd: Hwnd) -> Hwnd;
    fn ReleaseCapture() -> i32;
    fn GetKeyState(key: i32) -> i16;
    fn LoadCursorW(instance: Hwnd, name: *const u16) -> Hwnd;
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void, flags: u32) -> i32;
    fn GetSysColor(index: i32) -> u32;
    fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Hwnd;
    fn GetLastError() -> u32;
}
#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmExtendFrameIntoClientArea(hwnd: Hwnd, m: *const Margins) -> i32;
    fn DwmSetWindowAttribute(hwnd: Hwnd, attr: u32, value: *const c_void, size: u32) -> i32;
    fn DwmDefWindowProc(hwnd: Hwnd, msg: u32, w: usize, l: isize, out: *mut isize) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn StretchDIBits(
        dc: Hwnd,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        sx: i32,
        sy: i32,
        sw: i32,
        sh: i32,
        bits: *const c_void,
        info: *const BitmapInfo,
        usage: u32,
        rop: u32,
    ) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        root: Hwnd,
        key: *const u16,
        name: *const u16,
        flags: u32,
        kind: *mut u32,
        data: Hwnd,
        size: *mut u32,
    ) -> i32;
}

const WS_OVERLAPPEDWINDOW: u32 = 0x00cf0000;
const WS_POPUP: u32 = 0x80000000;
const WM_NCCALCSIZE: u32 = 0x83;
const WM_NCHITTEST: u32 = 0x84;
const WM_SIZE: u32 = 5;
const WM_PAINT: u32 = 15;
const WM_DESTROY: u32 = 2;
const WM_SETTINGCHANGE: u32 = 0x1a;
const WM_THEMECHANGED: u32 = 0x31a;
const WM_DPICHANGED: u32 = 0x2e0;
const WM_MOUSEMOVE: u32 = 0x200;
const WM_NCMOUSEMOVE: u32 = 0xa0;
const WM_LBUTTONDOWN: u32 = 0x201;
const WM_LBUTTONUP: u32 = 0x202;
const WM_NCLBUTTONDOWN: u32 = 0xa1;
const WM_KEYDOWN: u32 = 0x100;
const WM_SYSKEYDOWN: u32 = 0x104;
const WM_SETFOCUS: u32 = 7;
const WM_KILLFOCUS: u32 = 8;
const WM_GETMINMAXINFO: u32 = 0x24;
const WM_SYSCOMMAND: u32 = 0x112;
const HTCLIENT: isize = 1;
const HTCAPTION: isize = 2;
const HTMINBUTTON: isize = 8;
const HTMAXBUTTON: isize = 9;
const HTLEFT: isize = 10;
const HTRIGHT: isize = 11;
const HTTOP: isize = 12;
const HTTOPLEFT: isize = 13;
const HTTOPRIGHT: isize = 14;
const HTBOTTOM: isize = 15;
const HTBOTTOMLEFT: isize = 16;
const HTBOTTOMRIGHT: isize = 17;
const HTCLOSE: isize = 20;
const GWLP_USERDATA: i32 = -21;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn xy(l: isize) -> WinPoint {
    WinPoint {
        x: (l as i16) as i32,
        y: ((l >> 16) as i16) as i32,
    }
}
fn scale(hwnd: Hwnd) -> f32 {
    unsafe { GetDpiForWindow(hwnd) as f32 / 96.0 }
}
fn system_dark() -> bool {
    let mut value = 1u32;
    let mut size = 4u32;
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let name = wide("AppsUseLightTheme");
    unsafe {
        RegGetValueW(
            0x80000001usize as Hwnd,
            key.as_ptr(),
            name.as_ptr(),
            0x10,
            null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        ) == 0
            && value == 0
    }
}
fn system_high_contrast() -> bool {
    let mut info = HighContrast {
        size: std::mem::size_of::<HighContrast>() as u32,
        flags: 0,
        scheme: null_mut(),
    };
    unsafe {
        SystemParametersInfoW(0x42, info.size, (&mut info as *mut HighContrast).cast(), 0) != 0
            && info.flags & 1 != 0
    }
}
fn system_color(index: i32) -> Color {
    let value = unsafe { GetSysColor(index) };
    Color::rgb(value as u8, (value >> 8) as u8, (value >> 16) as u8)
}
fn theme(hwnd: Hwnd, dark: bool) {
    let enabled = if dark { 1i32 } else { 0 };
    let corners = 2i32;
    let margin = Margins {
        left: 0,
        right: 0,
        top: 1,
        bottom: 0,
    };
    unsafe {
        DwmSetWindowAttribute(hwnd, 20, (&enabled as *const i32).cast(), 4);
        DwmSetWindowAttribute(hwnd, 33, (&corners as *const i32).cast(), 4);
        DwmExtendFrameIntoClientArea(hwnd, &margin);
    }
}
fn class_registration_ok(atom: u16, error: u32) -> bool {
    atom != 0 || error == 1410
}

type EventHandler = Box<dyn FnMut(&mut TabStrip, ShellEvent)>;
pub struct ShellWindow {
    hwnd: Hwnd,
    strip: TabStrip,
    renderer: Box<dyn Renderer>,
    measurer: Box<dyn TextMeasurer>,
    dark: bool,
    full_screen: bool,
    saved_style: isize,
    saved_rect: WinRect,
    event_handler: Option<EventHandler>,
    pending_events: Vec<ShellEvent>,
}
impl ShellWindow {
    /// Create a native frame. The supplied renderer prepares pixels for each paint.
    pub fn create(
        renderer: Box<dyn Renderer>,
        measurer: Box<dyn TextMeasurer>,
        cell_width: f32,
        line_height: f32,
    ) -> Result<Self, String> {
        unsafe {
            SetProcessDpiAwarenessContext(-4);
        }
        let class = wide("QuickEditShellWindow");
        let title = wide("QuickEdit");
        let instance = unsafe { GetModuleHandleW(null()) };
        let wc = WinClass {
            style: 3,
            proc: Some(window_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: null_mut(),
            cursor: unsafe { LoadCursorW(null_mut(), 32512usize as *const u16) },
            brush: null_mut(),
            menu: null(),
            class: class.as_ptr(),
        };
        let atom = unsafe { RegisterClassW(&wc) };
        let error = if atom == 0 {
            unsafe { GetLastError() }
        } else {
            0
        };
        if !class_registration_ok(atom, error) {
            return Err(format!("RegisterClassW: {error}"));
        }
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        let width = (t.new_window_columns * cell_width + t.caption_width * 3.0).ceil() as i32;
        let height = (t.new_window_rows * line_height + t.row_height).ceil() as i32;
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                150,
                120,
                width.max(t.min_window_width as i32),
                height,
                null_mut(),
                null_mut(),
                instance,
                null_mut(),
            )
        };
        if hwnd.is_null() {
            return Err(format!("CreateWindowExW: {}", unsafe { GetLastError() }));
        }
        let dark = system_dark();
        let mut out = Self {
            hwnd,
            strip: TabStrip::new(vec![Tab::new("Untitled")]),
            renderer,
            measurer,
            dark,
            full_screen: false,
            saved_style: 0,
            saved_rect: WinRect::default(),
            event_handler: None,
            pending_events: Vec::new(),
        };
        theme(hwnd, dark);
        out.strip.set_high_contrast(system_high_contrast());
        out.relayout();
        Ok(out)
    }
    pub fn hwnd(&self) -> *mut c_void {
        self.hwnd
    }
    pub fn strip(&self) -> &TabStrip {
        &self.strip
    }
    pub fn strip_mut(&mut self) -> &mut TabStrip {
        &mut self.strip
    }
    pub fn set_event_handler(&mut self, handler: impl FnMut(&mut TabStrip, ShellEvent) + 'static) {
        self.event_handler = Some(Box::new(handler));
    }
    pub fn take_events(&mut self) -> Vec<ShellEvent> {
        std::mem::take(&mut self.pending_events)
    }
    pub fn show_and_run(mut self) -> Result<(), String> {
        self.relayout();
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, (&mut self as *mut Self) as isize);
            ShowWindow(self.hwnd, 5);
            UpdateWindow(self.hwnd);
        }
        let mut msg = Msg {
            hwnd: null_mut(),
            message: 0,
            w: 0,
            l: 0,
            time: 0,
            pt: WinPoint::default(),
            private: 0,
        };
        loop {
            let result = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
            if result < 0 {
                return Err(format!("GetMessageW: {}", unsafe { GetLastError() }));
            }
            if result == 0 {
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
    fn relayout(&mut self) {
        let mut b = WinRect::default();
        unsafe {
            GetClientRect(self.hwnd, &mut b);
        }
        let s = scale(self.hwnd);
        self.strip.layout(
            ApiRect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip((b.right - b.left) as f32 / s),
                height: Dip((b.bottom - b.top) as f32 / s),
            },
            self.measurer.as_ref(),
        );
    }
    fn input(&mut self, kind: InputKind) {
        let mut ctx = ViewContext::default();
        self.strip
            .handle(&InputEvent { time_us: 0, kind }, &mut ctx);
        for request in ctx.requests {
            match request {
                qe_api::ViewRequest::CaptureMouse(true) => unsafe {
                    SetCapture(self.hwnd);
                },
                qe_api::ViewRequest::CaptureMouse(false) => unsafe {
                    ReleaseCapture();
                },
                _ => {}
            }
        }
        for event in self.strip.take_events() {
            if let Some(handler) = &mut self.event_handler {
                handler(&mut self.strip, event);
            } else {
                self.pending_events.push(event);
            }
        }
        self.relayout();
        unsafe {
            InvalidateRect(self.hwnd, null(), 0);
        }
    }
    fn paint(&mut self, dc: Hwnd, width: i32, height: i32) {
        let mut list = DisplayList::default();
        let mut theme = Theme::new(if self.dark {
            ThemeKind::Dark
        } else {
            ThemeKind::Light
        });
        if system_high_contrast() {
            theme.text_background = system_color(5);
            theme.tab_background = theme.text_background;
            theme.tab_selected = theme.text_background;
            theme.text = system_color(8);
            theme.dim_text = theme.text;
            theme.border = theme.text;
        }
        self.strip.draw(&mut list, &theme, self.measurer.as_ref());
        if let Err(e) = self
            .renderer
            .begin_frame()
            .and_then(|()| self.renderer.draw(&list))
            .and_then(|()| self.renderer.end_frame())
        {
            eprintln!("renderer frame: {e}");
        }
        match self
            .renderer
            .render_to_bitmap(&list, width.max(1) as u32, height.max(1) as u32)
        {
            Ok(bitmap)
                if bitmap.bgra.len() == bitmap.width as usize * bitmap.height as usize * 4 =>
            {
                let info = BitmapInfo {
                    header: BitmapInfoHeader {
                        size: 40,
                        width: bitmap.width as i32,
                        height: -(bitmap.height as i32),
                        planes: 1,
                        bits: 32,
                        compression: 0,
                        image_size: 0,
                        xppm: 0,
                        yppm: 0,
                        used: 0,
                        important: 0,
                    },
                    colors: [0; 3],
                };
                unsafe {
                    StretchDIBits(
                        dc,
                        0,
                        0,
                        width,
                        height,
                        0,
                        0,
                        bitmap.width as i32,
                        bitmap.height as i32,
                        bitmap.bgra.as_ptr().cast(),
                        &info,
                        0,
                        0x00cc0020,
                    );
                }
            }
            Ok(_) => eprintln!("renderer returned an invalid BGRA buffer"),
            Err(e) => eprintln!("renderer bitmap: {e}"),
        }
    }
}
unsafe fn state(hwnd: Hwnd) -> Option<&'static mut ShellWindow> {
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ShellWindow).as_mut() }
}
fn hit_code(hit: CaptionHit) -> isize {
    match hit {
        CaptionHit::Close => HTCLOSE,
        CaptionHit::Maximize => HTMAXBUTTON,
        CaptionHit::Minimize => HTMINBUTTON,
        CaptionHit::ResizeLeft => HTLEFT,
        CaptionHit::ResizeRight => HTRIGHT,
        CaptionHit::ResizeTop => HTTOP,
        CaptionHit::ResizeBottom => HTBOTTOM,
        CaptionHit::ResizeTopLeft => HTTOPLEFT,
        CaptionHit::ResizeTopRight => HTTOPRIGHT,
        CaptionHit::ResizeBottomLeft => HTBOTTOMLEFT,
        CaptionHit::ResizeBottomRight => HTBOTTOMRIGHT,
        CaptionHit::Drag => HTCAPTION,
        _ => HTCLIENT,
    }
}
unsafe extern "system" fn window_proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    unsafe {
        match msg {
            WM_NCCALCSIZE if w != 0 => {
                if IsZoomed(hwnd) != 0 {
                    let b = &mut *(l as *mut WinRect);
                    let dpi = GetDpiForWindow(hwnd);
                    let inset_x = maximized_insets(
                        GetSystemMetricsForDpi(32, dpi),
                        GetSystemMetricsForDpi(92, dpi),
                    );
                    let inset_y = maximized_insets(
                        GetSystemMetricsForDpi(33, dpi),
                        GetSystemMetricsForDpi(92, dpi),
                    );
                    b.left += inset_x;
                    b.right -= inset_x;
                    b.top += inset_y;
                    b.bottom -= inset_y;
                }
                0
            }
            WM_NCHITTEST => {
                let mut result = 0;
                if DwmDefWindowProc(hwnd, msg, w, l, &mut result) != 0 && result != HTCLIENT {
                    return result;
                }
                let mut pt = xy(l);
                ScreenToClient(hwnd, &mut pt);
                let mut b = WinRect::default();
                GetClientRect(hwnd, &mut b);
                if let Some(s) = state(hwnd) {
                    hit_code(caption_hit(
                        Point {
                            x: Dip(pt.x as f32),
                            y: Dip(pt.y as f32),
                        },
                        b.right as f32,
                        b.bottom as f32,
                        scale(hwnd),
                        IsZoomed(hwnd) != 0,
                        &s.strip,
                    ))
                } else {
                    HTCLIENT
                }
            }
            WM_SIZE => {
                if let Some(s) = state(hwnd) {
                    s.strip.set_maximized(IsZoomed(hwnd) != 0 || s.full_screen);
                    s.relayout();
                    let mut b = WinRect::default();
                    GetClientRect(hwnd, &mut b);
                    if let Err(e) = s.renderer.resize(b.right as u32, b.bottom as u32) {
                        eprintln!("renderer resize: {e}");
                    }
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                if let Some(s) = state(hwnd) {
                    s.dark = system_dark();
                    s.strip.set_high_contrast(system_high_contrast());
                    theme(hwnd, s.dark);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_DPICHANGED => {
                let b = &*(l as *const WinRect);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    b.left,
                    b.top,
                    b.right - b.left,
                    b.bottom - b.top,
                    4 | 16,
                );
                if let Some(s) = state(hwnd) {
                    if let Err(e) = s.renderer.set_dpi(GetDpiForWindow(hwnd)) {
                        eprintln!("renderer dpi: {e}");
                    }
                    s.relayout();
                }
                0
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
                if let Some(s) = state(hwnd) {
                    let pt = xy(l);
                    let at = Point {
                        x: Dip(pt.x as f32 / scale(hwnd)),
                        y: Dip(pt.y as f32 / scale(hwnd)),
                    };
                    let kind = match msg {
                        WM_MOUSEMOVE => InputKind::MouseMove { at },
                        WM_LBUTTONDOWN => InputKind::MouseDown {
                            at,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                        },
                        _ => InputKind::MouseUp {
                            at,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                        },
                    };
                    s.input(kind);
                }
                0
            }
            WM_NCMOUSEMOVE => {
                if let Some(s) = state(hwnd) {
                    let mut pt = xy(l);
                    ScreenToClient(hwnd, &mut pt);
                    s.input(InputKind::MouseMove {
                        at: Point {
                            x: Dip(pt.x as f32 / scale(hwnd)),
                            y: Dip(pt.y as f32 / scale(hwnd)),
                        },
                    });
                }
                DefWindowProcW(hwnd, msg, w, l)
            }
            WM_NCLBUTTONDOWN if [HTMINBUTTON, HTMAXBUTTON, HTCLOSE].contains(&(w as isize)) => {
                let cmd = match w as isize {
                    HTMINBUTTON => 0xf020,
                    HTMAXBUTTON if IsZoomed(hwnd) != 0 => 0xf120,
                    HTMAXBUTTON => 0xf030,
                    _ => 0xf060,
                };
                SendMessageW(hwnd, WM_SYSCOMMAND, cmd, 0);
                0
            }
            WM_KEYDOWN if w == 0x7a => {
                let enter = if let Some(s) = state(hwnd) {
                    s.full_screen = !s.full_screen;
                    if s.full_screen {
                        s.saved_style = GetWindowLongPtrW(hwnd, -16);
                        GetWindowRect(hwnd, &mut s.saved_rect);
                    }
                    Some((s.full_screen, s.saved_style, s.saved_rect))
                } else {
                    None
                };
                if let Some((true, style, _)) = enter {
                    let monitor = MonitorFromWindow(hwnd, 2);
                    let mut info = MonitorInfo {
                        size: std::mem::size_of::<MonitorInfo>() as u32,
                        monitor: WinRect::default(),
                        work: WinRect::default(),
                        flags: 0,
                    };
                    if GetMonitorInfoW(monitor, &mut info) != 0 {
                        SetWindowLongPtrW(
                            hwnd,
                            -16,
                            (style & !(WS_OVERLAPPEDWINDOW as isize)) | WS_POPUP as isize,
                        );
                        let b = info.monitor;
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            b.left,
                            b.top,
                            b.right - b.left,
                            b.bottom - b.top,
                            0x20,
                        );
                    }
                } else if let Some((false, style, b)) = enter {
                    SetWindowLongPtrW(hwnd, -16, style);
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        b.left,
                        b.top,
                        b.right - b.left,
                        b.bottom - b.top,
                        0x20,
                    );
                }
                0
            }
            WM_KEYDOWN | WM_SYSKEYDOWN if msg == WM_KEYDOWN || w == 0x12 => {
                if let Some(s) = state(hwnd) {
                    let modifiers = Modifiers {
                        ctrl: GetKeyState(0x11) < 0,
                        alt: GetKeyState(0x12) < 0,
                        shift: GetKeyState(0x10) < 0,
                        win: false,
                    };
                    s.input(InputKind::KeyDown {
                        virtual_key: w as u16,
                        modifiers,
                        repeat: l & 0x40000000 != 0,
                    });
                }
                0
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                if let Some(s) = state(hwnd) {
                    s.input(if msg == WM_SETFOCUS {
                        InputKind::Focus
                    } else {
                        InputKind::Blur
                    });
                }
                0
            }
            WM_GETMINMAXINFO => {
                let info = l as *mut i32;
                let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
                let min = (t.min_window_width * scale(hwnd)) as i32;
                *info.add(6) = min;
                0
            }
            WM_PAINT => {
                let mut paint = std::mem::zeroed::<Paint>();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut b = WinRect::default();
                GetClientRect(hwnd, &mut b);
                if let Some(s) = state(hwnd) {
                    s.paint(dc, b.right, b.bottom);
                }
                EndPaint(hwnd, &paint);
                0
            }
            WM_DESTROY => {
                if let Some(s) = state(hwnd) {
                    s.hwnd = null_mut();
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
impl Drop for ShellWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_null() {
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
            self.hwnd = null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn existing_window_class_allows_second_window() {
        assert!(super::class_registration_ok(0, 1410));
        assert!(!super::class_registration_ok(0, 5));
    }
}
