//! The Win32 frame, copied from Windows Terminal's `NonClientIslandWindow`.
//!
//! The left, right and bottom resize zones stay in the invisible frame outside the
//! client area. Only the top of the frame is removed. `TabStrip` answers every hit test.
//!
//! No reference to the state lives across a call that can send a message. `RefCell`
//! enforces this: each borrow is short, and a message that finds the state in use takes
//! the default path and names itself in `ShellError::Reentered`.
use crate::{
    CONTENT_WIDGET, CaptionHit, ContrastColors, FrameMode, ShellError, ShellEvent, ShellTokens,
    Tab, TabStrip, caption_hit, maximized_insets,
};
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers, MouseButton,
    PixelRect, Point, Rect as ApiRect, Renderer, TextMeasurer, Theme, ThemeKind, View, ViewContext,
    ViewRequest,
};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::size_of;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWTEXT, EndPaint, GetMonitorInfoW, GetSysColor, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, SYS_COLOR_INDEX,
    ScreenToClient, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    GetSystemMetricsForDpi, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_F11, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETAUTOHIDEBAREX, ABM_GETSTATE, ABS_AUTOHIDE,
    APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

/// The most problems a window keeps. Later distinct ones still reach the owner.
const PROBLEM_LIMIT: usize = 64;
/// Terminal's `AutohideTaskbarSize`: a maximized window leaves this many pixels free on
/// the auto-hide taskbar's edge, so that the pointer can reach and reveal the taskbar.
pub const AUTOHIDE_TASKBAR_PX: i32 = 2;

/// How a new window starts.
#[derive(Clone, Copy, Debug)]
pub struct WindowConfig {
    pub cell_width: f32,
    pub line_height: f32,
    /// For tests: this DPI takes the place of the monitor's, from the first message on.
    pub fake_dpi: Option<u32>,
    /// For tests: window-button commands go to `system_commands` instead of the system,
    /// so that a hidden window never shows.
    pub record_system_commands: bool,
}

/// Which edges of a monitor hold an auto-hide taskbar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoHideEdges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}
/// Shrinks a maximized client rectangle on each edge with an auto-hide taskbar.
pub fn apply_autohide(mut rect: RECT, edges: AutoHideEdges) -> RECT {
    if edges.top {
        rect.top += AUTOHIDE_TASKBAR_PX;
    }
    if edges.bottom {
        rect.bottom -= AUTOHIDE_TASKBAR_PX;
    }
    if edges.left {
        rect.left += AUTOHIDE_TASKBAR_PX;
    }
    if edges.right {
        rect.right -= AUTOHIDE_TASKBAR_PX;
    }
    rect
}

/// Turns the result of reading `AppsUseLightTheme` into "is dark". A missing value is
/// the normal light case. Any other failure is a problem to report.
pub fn theme_from_status(status: u32, value: u32) -> Result<bool, ShellError> {
    match status {
        0 => Ok(value == 0),
        2 => Ok(false),
        status => Err(ShellError::ThemeRead { status }),
    }
}

fn win32(call: &'static str, error: windows::core::Error) -> ShellError {
    ShellError::Win32 {
        call,
        message: error.to_string(),
    }
}
fn dwm(call: &'static str, error: windows::core::Error) -> ShellError {
    ShellError::Dwm {
        call,
        message: error.to_string(),
    }
}
fn renderer_error(stage: &'static str, error: qe_api::RenderError) -> ShellError {
    ShellError::Renderer {
        stage,
        message: error.message,
    }
}
fn read_dark() -> Result<bool, ShellError> {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    theme_from_status(status.0, value)
}
fn system_color(index: SYS_COLOR_INDEX) -> Color {
    let value = unsafe { GetSysColor(index) };
    Color::rgb(value as u8, (value >> 8) as u8, (value >> 16) as u8)
}
fn read_contrast() -> Result<Option<ContrastColors>, ShellError> {
    let mut info = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            info.cbSize,
            Some((&mut info as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .map_err(|e| win32("SystemParametersInfoW", e))?;
    if info.dwFlags.0 & HCF_HIGHCONTRASTON.0 == 0 {
        return Ok(None);
    }
    Ok(Some(ContrastColors {
        window: system_color(COLOR_WINDOW),
        window_text: system_color(COLOR_WINDOWTEXT),
        highlight: system_color(COLOR_HIGHLIGHT),
        highlight_text: system_color(COLOR_HIGHLIGHTTEXT),
        gray_text: system_color(COLOR_GRAYTEXT),
    }))
}
fn build_theme(dark: bool, contrast: Option<ContrastColors>) -> Theme {
    let mut theme = Theme::new(if dark {
        ThemeKind::Dark
    } else {
        ThemeKind::Light
    });
    if let Some(c) = contrast {
        theme.text_background = c.window;
        theme.tab_background = c.window;
        theme.tab_selected = c.highlight;
        theme.text = c.window_text;
        theme.dim_text = c.window_text;
        theme.border = c.window_text;
    }
    theme
}
fn ensure_dpi_awareness() -> Result<(), ShellError> {
    let current = unsafe { GetThreadDpiAwarenessContext() };
    let already = unsafe {
        AreDpiAwarenessContextsEqual(current, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    };
    if already.as_bool() {
        return Ok(());
    }
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .map_err(|e| win32("SetProcessDpiAwarenessContext", e))
}
fn class_registration_ok(atom: u16, error: u32) -> bool {
    atom != 0 || error == ERROR_CLASS_ALREADY_EXISTS.0
}
fn xy(l: LPARAM) -> POINT {
    POINT {
        x: i32::from(l.0 as i16),
        y: i32::from((l.0 >> 16) as i16),
    }
}
fn is_down(key: i32) -> bool {
    unsafe { GetKeyState(key) < 0 }
}
fn modifiers() -> Modifiers {
    Modifiers {
        ctrl: is_down(i32::from(VK_CONTROL.0)),
        alt: is_down(i32::from(VK_MENU.0)),
        shift: is_down(i32::from(VK_SHIFT.0)),
        win: is_down(i32::from(VK_LWIN.0)) || is_down(i32::from(VK_RWIN.0)),
    }
}
fn is_caption_code(code: usize) -> bool {
    let code = code as u32;
    code == HTMINBUTTON || code == HTMAXBUTTON || code == HTCLOSE
}
fn hit_code(hit: CaptionHit, mode: FrameMode) -> u32 {
    match hit {
        CaptionHit::Close => HTCLOSE,
        // Snap Layouts opens from this answer, as Microsoft's guidance requires.
        CaptionHit::Maximize => HTMAXBUTTON,
        CaptionHit::Minimize => HTMINBUTTON,
        CaptionHit::ResizeTop => HTTOP,
        // A full-screen window does not move.
        CaptionHit::Drag if mode != FrameMode::FullScreen => HTCAPTION,
        _ => HTCLIENT,
    }
}

type AppBarMessage = unsafe extern "system" fn(u32, *mut APPBARDATA) -> usize;
/// `shell32` is heavy and only a maximized window needs it, so it loads on first use.
fn app_bar_message() -> Result<AppBarMessage, ShellError> {
    static FUNCTION: OnceLock<Result<usize, ShellError>> = OnceLock::new();
    let address = FUNCTION.get_or_init(|| unsafe {
        let module = LoadLibraryW(w!("shell32.dll")).map_err(|e| win32("LoadLibraryW", e))?;
        GetProcAddress(module, windows::core::s!("SHAppBarMessage"))
            .map(|f| f as usize)
            .ok_or_else(|| ShellError::Win32 {
                call: "GetProcAddress",
                message: "SHAppBarMessage is missing".into(),
            })
    });
    address
        .clone()
        .map(|a| unsafe { std::mem::transmute::<*const c_void, AppBarMessage>(a as *const c_void) })
}
fn autohide_edges(hwnd: HWND) -> Result<AutoHideEdges, ShellError> {
    let send = app_bar_message()?;
    let mut data = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    if unsafe { send(ABM_GETSTATE, &mut data) } as u32 & ABS_AUTOHIDE == 0 {
        return Ok(AutoHideEdges::default());
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(ShellError::Win32 {
            call: "GetMonitorInfoW",
            message: "it returned false".into(),
        });
    }
    let has = |edge: u32| {
        let mut data = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            uEdge: edge,
            rc: info.rcMonitor,
            ..Default::default()
        };
        unsafe { send(ABM_GETAUTOHIDEBAREX, &mut data) != 0 }
    };
    Ok(AutoHideEdges {
        top: has(ABE_TOP),
        bottom: has(ABE_BOTTOM),
        left: has(ABE_LEFT),
        right: has(ABE_RIGHT),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tracking {
    None,
    Client,
    NonClient,
}
struct SavedFrame {
    style: isize,
    placement: WINDOWPLACEMENT,
    was_maximized: bool,
}
struct State {
    strip: TabStrip,
    renderer: Box<dyn Renderer>,
    measurer: Box<dyn TextMeasurer>,
    theme: Theme,
    dark: bool,
    needs_draw: bool,
    size: (u32, u32),
    client_dips: (f32, f32),
    saved: Option<SavedFrame>,
    commands: Vec<u32>,
    list: DisplayList,
}
impl State {
    /// Draws what is waiting, then copies the rectangle that Windows asked for. A frame
    /// with a problem still ends and still copies, because its pixels are valid.
    fn paint(&mut self, dc: *mut c_void, rect: PixelRect) -> Vec<ShellError> {
        let mut problems = Vec::new();
        if self.needs_draw {
            self.needs_draw = false;
            let mut list = std::mem::take(&mut self.list);
            list.commands.clear();
            let row = self.strip.row_height();
            list.push(DrawCommand::FillRect {
                rect: ApiRect {
                    x: Dip(0.0),
                    y: Dip(row),
                    width: Dip(self.client_dips.0),
                    height: Dip((self.client_dips.1 - row).max(0.0)),
                },
                color: self.theme.text_background,
            });
            self.strip
                .draw(&mut list, &self.theme, self.measurer.as_ref());
            if let Err(e) = self.renderer.begin_frame() {
                problems.push(renderer_error("begin_frame", e));
            }
            if let Err(e) = self.renderer.draw(&list) {
                problems.push(renderer_error("draw", e));
            }
            if let Err(e) = self.renderer.end_frame() {
                problems.push(renderer_error("end_frame", e));
            }
            self.list = list;
        }
        if let Err(e) = self.renderer.present_rect(dc, rect) {
            problems.push(renderer_error("present_rect", e));
        }
        problems
    }
}
type EventHandler = Box<dyn FnMut(&ShellHandle, ShellEvent)>;
struct Shared {
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    fake_dpi: Option<u32>,
    record_commands: bool,
    tracking: Cell<Tracking>,
    state: RefCell<State>,
    handler: RefCell<Option<EventHandler>>,
    queue: RefCell<VecDeque<ShellEvent>>,
    problems: RefCell<Vec<ShellError>>,
    dispatching: Cell<bool>,
}
impl Shared {
    fn report(&self, problem: ShellError) {
        {
            let mut problems = self.problems.borrow_mut();
            if problems.contains(&problem) {
                return;
            }
            if problems.len() < PROBLEM_LIMIT {
                problems.push(problem.clone());
            }
        }
        self.queue
            .borrow_mut()
            .push_back(ShellEvent::Problem(problem));
    }
    /// The only way to the state. `f` must not call anything that sends a message.
    fn with_state<R>(&self, message: u32, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        match self.state.try_borrow_mut() {
            Ok(mut state) => Some(f(&mut state)),
            Err(_) => {
                self.report(ShellError::Reentered { message });
                None
            }
        }
    }
    fn scale(&self) -> f32 {
        self.dpi.get() as f32 / 96.0
    }
    fn dips(&self, px: POINT) -> Point {
        let s = self.scale();
        Point {
            x: Dip(px.x as f32 / s),
            y: Dip(px.y as f32 / s),
        }
    }
    fn screen_to_client(&self, mut at: POINT) -> POINT {
        unsafe {
            let _ = ScreenToClient(self.hwnd.get(), &mut at);
        }
        at
    }
    /// Asks for a repaint of only these rectangles.
    fn invalidate(&self, rects: &[ApiRect]) {
        let s = self.scale();
        let live: Vec<ApiRect> = rects
            .iter()
            .copied()
            .filter(|b| b.width.0 > 0.0 && b.height.0 > 0.0)
            .collect();
        if live.is_empty() {
            return;
        }
        let done = self.with_state(WM_PAINT, |state| {
            for b in &live {
                state.renderer.invalidate(*b);
            }
            state.needs_draw = true;
        });
        if done.is_none() {
            return;
        }
        for b in live {
            let rect = RECT {
                left: (b.x.0 * s).floor() as i32,
                top: (b.y.0 * s).floor() as i32,
                right: ((b.x.0 + b.width.0) * s).ceil() as i32,
                bottom: ((b.y.0 + b.height.0) * s).ceil() as i32,
            };
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd.get()), Some(&rect), false);
            }
        }
    }
    fn invalidate_all(&self) {
        let done = self.with_state(WM_PAINT, |state| {
            state.renderer.invalidate_all();
            state.needs_draw = true;
        });
        if done.is_some() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
            }
        }
    }
    fn apply(&self, requests: Vec<ViewRequest>, events: Vec<ShellEvent>) {
        let hwnd = self.hwnd.get();
        let mut rects = Vec::new();
        for request in requests {
            match request {
                ViewRequest::Invalidate(b) => rects.push(b),
                ViewRequest::CaptureMouse(true) => unsafe {
                    SetCapture(hwnd);
                },
                ViewRequest::CaptureMouse(false) => {
                    if let Err(e) = unsafe { ReleaseCapture() } {
                        self.report(win32("ReleaseCapture", e));
                    }
                }
                ViewRequest::SetFocus(id) if id != CONTENT_WIDGET => {
                    if let Err(e) = unsafe { SetFocus(Some(hwnd)) } {
                        self.report(win32("SetFocus", e));
                    }
                }
                // `FocusContent` tells the owner. The text area paints in this window.
                ViewRequest::SetFocus(_)
                | ViewRequest::Run(_)
                | ViewRequest::SetCursor(_)
                | ViewRequest::RequestTimer(_) => {}
            }
        }
        self.invalidate(&rects);
        for event in events {
            if let ShellEvent::CaptionCommand(hit) = event {
                self.caption_command(hit);
            } else {
                self.queue.borrow_mut().push_back(event);
            }
        }
    }
    fn input(&self, message: u32, kind: InputKind) -> (Handled, InputEvent) {
        let event = InputEvent {
            time_us: u64::from(unsafe { GetMessageTime() } as u32) * 1000,
            kind,
        };
        let result = self.with_state(message, |state| {
            let mut ctx = ViewContext::default();
            let handled = state.strip.handle(&event, &mut ctx);
            (handled, ctx.requests, state.strip.take_events())
        });
        let Some((handled, requests, events)) = result else {
            return (Handled::No, event);
        };
        self.apply(requests, events);
        (handled, event)
    }
    /// Sends input to the strip, and to the text area if the strip does not take it.
    fn content_input(&self, message: u32, kind: InputKind) {
        let (handled, event) = self.input(message, kind);
        if handled == Handled::No {
            self.queue
                .borrow_mut()
                .push_back(ShellEvent::ContentInput(event));
        }
    }
    fn strip_action(&self, message: u32, f: impl FnOnce(&mut TabStrip, &mut ViewContext)) {
        let result = self.with_state(message, |state| {
            let mut ctx = ViewContext::default();
            f(&mut state.strip, &mut ctx);
            (ctx.requests, state.strip.take_events())
        });
        if let Some((requests, events)) = result {
            self.apply(requests, events);
        }
    }
    fn track_leave(&self, wanted: Tracking) {
        if self.tracking.get() == wanted {
            return;
        }
        let mut flags = TME_LEAVE;
        if wanted == Tracking::NonClient {
            flags |= TME_NONCLIENT;
        }
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: flags,
            hwndTrack: self.hwnd.get(),
            dwHoverTime: 0,
        };
        match unsafe { TrackMouseEvent(&mut track) } {
            Ok(()) => self.tracking.set(wanted),
            Err(e) => self.report(win32("TrackMouseEvent", e)),
        }
    }
    fn system_command(&self, command: u32) {
        if self.record_commands {
            self.with_state(WM_SYSCOMMAND, |state| state.commands.push(command));
        } else {
            unsafe {
                SendMessageW(
                    self.hwnd.get(),
                    WM_SYSCOMMAND,
                    Some(WPARAM(command as usize)),
                    Some(LPARAM(0)),
                );
            }
        }
    }
    fn frame_mode(&self, message: u32) -> Option<FrameMode> {
        self.with_state(message, |state| state.strip.frame_mode())
    }
    fn caption_command(&self, hit: CaptionHit) {
        match hit {
            CaptionHit::Minimize => self.system_command(SC_MINIMIZE),
            CaptionHit::Close => self.system_command(SC_CLOSE),
            CaptionHit::Maximize => match self.frame_mode(WM_SYSCOMMAND) {
                Some(FrameMode::FullScreen) => {
                    self.leave_full_screen();
                    if !unsafe { IsZoomed(self.hwnd.get()) }.as_bool() {
                        self.system_command(SC_MAXIMIZE);
                    }
                }
                Some(FrameMode::Maximized) => self.system_command(SC_RESTORE),
                Some(FrameMode::Restored) => self.system_command(SC_MAXIMIZE),
                None => {}
            },
            _ => {}
        }
    }
    fn toggle_full_screen(&self) {
        match self.frame_mode(WM_KEYDOWN) {
            Some(FrameMode::FullScreen) => self.leave_full_screen(),
            Some(_) => self.enter_full_screen(),
            None => {}
        }
    }
    fn enter_full_screen(&self) {
        let hwnd = self.hwnd.get();
        // Everything that can fail runs first, so that a failure changes nothing.
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            self.report(ShellError::Win32 {
                call: "GetMonitorInfoW",
                message: "it returned false".into(),
            });
            return;
        }
        let mut placement = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if let Err(e) = unsafe { GetWindowPlacement(hwnd, &mut placement) } {
            self.report(win32("GetWindowPlacement", e));
            return;
        }
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
        let was_maximized = unsafe { IsZoomed(hwnd) }.as_bool();
        let done = self.with_state(WM_KEYDOWN, |state| {
            state.saved = Some(SavedFrame {
                style,
                placement,
                was_maximized,
            });
            state.strip.set_frame_mode(FrameMode::FullScreen);
        });
        if done.is_none() {
            return;
        }
        let new_style =
            (style & !((WS_OVERLAPPEDWINDOW.0 | WS_MAXIMIZE.0) as isize)) | WS_POPUP.0 as isize;
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, new_style);
        }
        let b = info.rcMonitor;
        if let Err(e) = unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                b.left,
                b.top,
                b.right - b.left,
                b.bottom - b.top,
                SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
            )
        } {
            self.report(win32("SetWindowPos", e));
        }
        self.apply_frame();
        self.on_size(true);
    }
    fn leave_full_screen(&self) {
        let hwnd = self.hwnd.get();
        let saved = self.with_state(WM_KEYDOWN, |state| {
            let saved = state.saved.take();
            if let Some(saved) = &saved {
                state.strip.set_frame_mode(if saved.was_maximized {
                    FrameMode::Maximized
                } else {
                    FrameMode::Restored
                });
            }
            saved
        });
        let Some(Some(saved)) = saved else {
            return;
        };
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        let plain = saved.style & !(WS_MAXIMIZE.0 as isize);
        let mut placement = saved.placement;
        if !visible {
            // A placement's show command would show a hidden window.
            placement.showCmd = SW_HIDE.0 as u32;
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, plain);
        }
        if let Err(e) = unsafe { SetWindowPlacement(hwnd, &placement) } {
            self.report(win32("SetWindowPlacement", e));
        }
        if !visible && saved.was_maximized {
            unsafe {
                SetWindowLongPtrW(hwnd, GWL_STYLE, saved.style);
            }
        }
        if let Err(e) = unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            )
        } {
            self.report(win32("SetWindowPos", e));
        }
        self.apply_frame();
        self.on_size(true);
    }
    /// Sets the dark frame, the corners and the 1-pixel frame extension.
    fn apply_frame(&self) {
        let hwnd = self.hwnd.get();
        let Some((dark, mode)) =
            self.with_state(WM_THEMECHANGED, |s| (s.dark, s.strip.frame_mode()))
        else {
            return;
        };
        let enabled = BOOL::from(dark);
        let corners = if mode == FrameMode::FullScreen {
            DWMWCP_DONOTROUND
        } else {
            DWMWCP_ROUND
        };
        let margins = MARGINS {
            cyTopHeight: i32::from(mode == FrameMode::Restored),
            ..Default::default()
        };
        unsafe {
            if let Err(e) = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&enabled as *const BOOL).cast(),
                size_of::<BOOL>() as u32,
            ) {
                self.report(dwm("DwmSetWindowAttribute(dark)", e));
            }
            if let Err(e) = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corners as *const _ as *const c_void).cast(),
                size_of::<i32>() as u32,
            ) {
                self.report(dwm("DwmSetWindowAttribute(corners)", e));
            }
            if let Err(e) = DwmExtendFrameIntoClientArea(hwnd, &margins) {
                self.report(dwm("DwmExtendFrameIntoClientArea", e));
            }
        }
    }
    fn refresh_theme(&self) {
        let dark = read_dark().unwrap_or_else(|problem| {
            self.report(problem);
            false
        });
        let contrast = read_contrast().unwrap_or_else(|problem| {
            self.report(problem);
            None
        });
        let theme = build_theme(dark, contrast);
        let changed = self.with_state(WM_THEMECHANGED, |state| {
            let changed = state.dark != dark || state.theme != theme;
            state.dark = dark;
            state.theme = theme;
            state.strip.set_high_contrast(contrast);
            changed
        });
        self.apply_frame();
        if changed == Some(true) {
            self.invalidate_all();
        }
    }
    /// Matches the frame mode, renderer and layout to the client area. A minimized or
    /// empty window keeps what it has: the renderer refuses an empty bitmap.
    fn on_size(&self, force: bool) {
        let hwnd = self.hwnd.get();
        let mut client = RECT::default();
        if let Err(e) = unsafe { GetClientRect(hwnd, &mut client) } {
            self.report(win32("GetClientRect", e));
            return;
        }
        let (width, height) = (client.right - client.left, client.bottom - client.top);
        if width <= 0 || height <= 0 {
            return;
        }
        let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
        let scale = self.scale();
        let result = self.with_state(WM_SIZE, |state| {
            let before = state.strip.frame_mode();
            if before != FrameMode::FullScreen {
                state.strip.set_frame_mode(if zoomed {
                    FrameMode::Maximized
                } else {
                    FrameMode::Restored
                });
            }
            let mode_changed = before != state.strip.frame_mode();
            let size = (width as u32, height as u32);
            let mut problem = None;
            let size_changed = size != state.size;
            if size_changed {
                match state.renderer.resize(size.0, size.1) {
                    Ok(()) => state.size = size,
                    Err(e) => problem = Some(renderer_error("resize", e)),
                }
            }
            state.client_dips = (width as f32 / scale, height as f32 / scale);
            let row = ApiRect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(state.client_dips.0),
                height: Dip(state.strip.row_height()),
            };
            state.strip.layout(row, state.measurer.as_ref());
            (problem, mode_changed, size_changed)
        });
        let Some((problem, mode_changed, size_changed)) = result else {
            return;
        };
        if let Some(problem) = problem {
            self.report(problem);
        }
        if mode_changed {
            self.apply_frame();
        }
        if force || mode_changed || size_changed {
            self.invalidate_all();
        }
    }
    fn on_nccalcsize(&self, hwnd: HWND, w: WPARAM, l: LPARAM) -> LRESULT {
        if w.0 == 0 {
            return LRESULT(0);
        }
        let params = l.0 as *mut NCCALCSIZE_PARAMS;
        let original_top = unsafe { (*params).rgrc[0].top };
        let result = unsafe { DefWindowProcW(hwnd, WM_NCCALCSIZE, w, l) };
        if result.0 != 0 {
            return result;
        }
        let mut rect = unsafe { (*params).rgrc[0] };
        rect.top = original_top;
        // This message comes before WM_SIZE, so the window's own state is the newest.
        // Full screen clears WS_MAXIMIZE, so that it never counts here.
        if unsafe { IsZoomed(hwnd) }.as_bool() {
            let dpi = self.dpi.get();
            rect.top += unsafe {
                maximized_insets(
                    GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi),
                    GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi),
                )
            };
            match autohide_edges(hwnd) {
                Ok(edges) => rect = apply_autohide(rect, edges),
                Err(problem) => self.report(problem),
            }
        }
        unsafe {
            (*params).rgrc[0] = rect;
        }
        LRESULT(0)
    }
    fn on_nchittest(&self, hwnd: HWND, w: WPARAM, l: LPARAM) -> LRESULT {
        // The left, right and bottom of the frame are outside the client area.
        let outer = unsafe { DefWindowProcW(hwnd, WM_NCHITTEST, w, l) };
        if outer.0 != HTCLIENT as isize {
            return outer;
        }
        let client = self.screen_to_client(xy(l));
        let at = Point {
            x: Dip(client.x as f32),
            y: Dip(client.y as f32),
        };
        let scale = self.scale();
        let code = self.with_state(WM_NCHITTEST, |state| {
            hit_code(
                caption_hit(at, scale, &state.strip),
                state.strip.frame_mode(),
            )
        });
        LRESULT(code.unwrap_or(HTCLIENT) as isize)
    }
    fn on_dpi_changed(&self, hwnd: HWND, w: WPARAM, l: LPARAM) {
        let dpi = (w.0 & 0xffff) as u32;
        self.dpi.set(dpi);
        if let Some(Err(e)) = self.with_state(WM_DPICHANGED, |s| s.renderer.set_dpi(dpi)) {
            self.report(renderer_error("set_dpi", e));
        }
        let b = unsafe { *(l.0 as *const RECT) };
        if let Err(e) = unsafe {
            SetWindowPos(
                hwnd,
                None,
                b.left,
                b.top,
                b.right - b.left,
                b.bottom - b.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        } {
            self.report(win32("SetWindowPos", e));
        }
        self.on_size(true);
    }
    fn on_paint(&self, hwnd: HWND) {
        let mut paint = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut paint) };
        let rect = PixelRect {
            left: paint.rcPaint.left,
            top: paint.rcPaint.top,
            right: paint.rcPaint.right,
            bottom: paint.rcPaint.bottom,
        };
        let problems = self.with_state(WM_PAINT, |state| state.paint(dc.0, rect));
        for problem in problems.unwrap_or_default() {
            self.report(problem);
        }
        unsafe {
            let _ = EndPaint(hwnd, &paint);
        }
    }
    fn on_message(&self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        let default = || unsafe { DefWindowProcW(hwnd, msg, w, l) };
        match msg {
            WM_NCCALCSIZE => self.on_nccalcsize(hwnd, w, l),
            WM_NCHITTEST => self.on_nchittest(hwnd, w, l),
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                if w.0 as u32 != SIZE_MINIMIZED {
                    self.on_size(false);
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                self.refresh_theme();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                self.on_dpi_changed(hwnd, w, l);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                self.track_leave(Tracking::Client);
                let at = self.dips(xy(l));
                self.content_input(msg, InputKind::MouseMove { at });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let at = self.dips(xy(l));
                self.content_input(
                    msg,
                    InputKind::MouseDown {
                        at,
                        button: MouseButton::Left,
                        modifiers: modifiers(),
                    },
                );
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let at = self.dips(xy(l));
                self.content_input(
                    msg,
                    InputKind::MouseUp {
                        at,
                        button: MouseButton::Left,
                        modifiers: modifiers(),
                    },
                );
                LRESULT(0)
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let at = self.dips(self.screen_to_client(xy(l)));
                let notches = f32::from((w.0 >> 16) as i16) / WHEEL_DELTA as f32;
                let (delta_x, delta_y) = if msg == WM_MOUSEWHEEL {
                    (0.0, notches)
                } else {
                    (notches, 0.0)
                };
                self.content_input(
                    msg,
                    InputKind::Wheel {
                        at,
                        delta_x,
                        delta_y,
                        modifiers: modifiers(),
                    },
                );
                LRESULT(0)
            }
            WM_NCMOUSEMOVE => {
                self.track_leave(Tracking::NonClient);
                let at = self.dips(self.screen_to_client(xy(l)));
                self.input(msg, InputKind::MouseMove { at });
                if is_caption_code(w.0) {
                    LRESULT(0)
                } else {
                    default()
                }
            }
            WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
                let ended = if msg == WM_MOUSELEAVE {
                    Tracking::Client
                } else {
                    Tracking::NonClient
                };
                if self.tracking.get() == ended {
                    self.tracking.set(Tracking::None);
                }
                self.strip_action(msg, |strip, ctx| strip.pointer_left(ctx));
                LRESULT(0)
            }
            // A fast second click arrives as a double click. It presses the button too.
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if is_caption_code(w.0) => {
                let at = self.dips(self.screen_to_client(xy(l)));
                self.input(
                    msg,
                    InputKind::MouseDown {
                        at,
                        button: MouseButton::Left,
                        modifiers: modifiers(),
                    },
                );
                LRESULT(0)
            }
            WM_NCLBUTTONUP => {
                let at = self.dips(self.screen_to_client(xy(l)));
                let (handled, _) = self.input(
                    msg,
                    InputKind::MouseUp {
                        at,
                        button: MouseButton::Left,
                        modifiers: modifiers(),
                    },
                );
                if handled == Handled::Yes || is_caption_code(w.0) {
                    LRESULT(0)
                } else {
                    default()
                }
            }
            WM_CAPTURECHANGED => {
                if l.0 != hwnd.0 as isize {
                    self.strip_action(msg, |strip, ctx| strip.capture_lost(ctx));
                }
                LRESULT(0)
            }
            WM_KEYDOWN if w.0 == usize::from(VK_F11.0) => {
                // Bit 30 says that the key was already down: this is a repeat.
                if l.0 & (1 << 30) == 0 {
                    self.toggle_full_screen();
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let (handled, event) = self.input(
                    msg,
                    InputKind::KeyDown {
                        virtual_key: w.0 as u16,
                        modifiers: modifiers(),
                        repeat: l.0 & (1 << 30) != 0,
                    },
                );
                self.after_key(msg, w, handled, event, default)
            }
            WM_KEYUP | WM_SYSKEYUP => {
                let (handled, event) = self.input(
                    msg,
                    InputKind::KeyUp {
                        virtual_key: w.0 as u16,
                        modifiers: modifiers(),
                    },
                );
                self.after_key(msg, w, handled, event, default)
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                self.input(
                    msg,
                    if msg == WM_SETFOCUS {
                        InputKind::Focus
                    } else {
                        InputKind::Blur
                    },
                );
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let info = l.0 as *mut MINMAXINFO;
                let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
                unsafe {
                    (*info).ptMinTrackSize.x = (t.min_window_width * self.scale()).ceil() as i32;
                }
                LRESULT(0)
            }
            WM_PAINT => {
                self.on_paint(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                // The owner answers. Unsaved work may need a question first.
                self.queue
                    .borrow_mut()
                    .push_back(ShellEvent::CloseWindowRequested);
                LRESULT(0)
            }
            WM_DESTROY => {
                self.queue
                    .borrow_mut()
                    .push_back(ShellEvent::WindowDestroyed);
                LRESULT(0)
            }
            _ => default(),
        }
    }
    /// Alt alone never reaches `DefWindowProcW`, which would start the system menu loop.
    /// Other system keys, such as Alt+F4 and Alt+Space, must reach it.
    fn after_key(
        &self,
        msg: u32,
        w: WPARAM,
        handled: Handled,
        event: InputEvent,
        default: impl FnOnce() -> LRESULT,
    ) -> LRESULT {
        if w.0 == usize::from(VK_MENU.0) || handled == Handled::Yes {
            return LRESULT(0);
        }
        if msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP {
            return default();
        }
        self.queue
            .borrow_mut()
            .push_back(ShellEvent::ContentInput(event));
        LRESULT(0)
    }
    /// Gives waiting events to the owner, with no borrow held. The handler may send
    /// messages or run a dialog: a nested call finds `dispatching` set and leaves its
    /// events for this loop.
    fn dispatch(self: &Rc<Self>) {
        if self.dispatching.get() {
            return;
        }
        let Some(mut handler) = self.handler.borrow_mut().take() else {
            return;
        };
        self.dispatching.set(true);
        let handle = ShellHandle {
            shared: Rc::downgrade(self),
        };
        loop {
            let event = self.queue.borrow_mut().pop_front();
            let Some(event) = event else {
                break;
            };
            handler(&handle, event);
        }
        self.dispatching.set(false);
        let mut slot = self.handler.borrow_mut();
        if slot.is_none() {
            *slot = Some(handler);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = l.0 as *const CREATESTRUCTW;
            let raw = (*create).lpCreateParams as *const Shared;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
            (*raw).hwnd.set(hwnd);
            let dpi = (*raw).fake_dpi.unwrap_or_else(|| GetDpiForWindow(hwnd));
            (*raw).dpi.set(dpi);
            // The DPI is set before anything can draw.
            if let Some(Err(e)) = (*raw).with_state(msg, |s| s.renderer.set_dpi(dpi)) {
                (*raw).report(renderer_error("set_dpi", e));
            }
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared;
        if raw.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        // This count keeps the state alive even if the handler destroys the window.
        Rc::increment_strong_count(raw);
        let shared = Rc::from_raw(raw);
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            shared.hwnd.set(HWND::default());
            // This drop ends the count that GWLP_USERDATA held.
            drop(Rc::from_raw(raw));
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let result = shared.on_message(hwnd, msg, w, l);
        shared.dispatch();
        result
    }
}

/// A way to reach a window from an event handler or an owner. It keeps nothing alive,
/// and after the window is gone its calls return `ShellError::WindowGone`.
#[derive(Clone)]
pub struct ShellHandle {
    shared: Weak<Shared>,
}
impl ShellHandle {
    fn live(&self) -> Result<Rc<Shared>, ShellError> {
        let shared = self.shared.upgrade().ok_or(ShellError::WindowGone)?;
        if shared.hwnd.get().is_invalid() {
            return Err(ShellError::WindowGone);
        }
        Ok(shared)
    }
    pub fn hwnd(&self) -> Result<*mut c_void, ShellError> {
        Ok(self.live()?.hwnd.get().0)
    }
    /// Reads or changes the strip, then repaints the row. The closure must not run a
    /// dialog or a message loop: messages sent meanwhile take the default path and
    /// report `ShellError::Reentered`.
    pub fn with_strip<R>(&self, f: impl FnOnce(&mut TabStrip) -> R) -> Result<R, ShellError> {
        let shared = self.live()?;
        let (result, row, events) = {
            let mut state = shared
                .state
                .try_borrow_mut()
                .map_err(|_| ShellError::Reentered { message: 0 })?;
            let result = f(&mut state.strip);
            (result, state.strip.row_rect(), state.strip.take_events())
        };
        shared.apply(vec![ViewRequest::Invalidate(row)], events);
        shared.dispatch();
        Ok(result)
    }
    pub fn add_tab(&self, tab: Tab) -> Result<(), ShellError> {
        self.with_strip(|strip| strip.add_tab(tab))
    }
    /// Removes a tab, as the answer to `CloseTabRequested`.
    pub fn close_tab(&self, index: usize) -> Result<(), ShellError> {
        self.with_strip(|strip| strip.close(index))
    }
    pub fn focus_strip(&self) -> Result<(), ShellError> {
        self.with_strip(TabStrip::focus_strip)
    }
    pub fn frame_mode(&self) -> Result<FrameMode, ShellError> {
        let shared = self.live()?;
        let state = shared
            .state
            .try_borrow()
            .map_err(|_| ShellError::Reentered { message: 0 })?;
        Ok(state.strip.frame_mode())
    }
    pub fn dpi(&self) -> Result<u32, ShellError> {
        Ok(self.live()?.dpi.get())
    }
    /// Shows the window. Nothing else shows it.
    pub fn show(&self) -> Result<(), ShellError> {
        let shared = self.live()?;
        let hwnd = shared.hwnd.get();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }
        shared.dispatch();
        Ok(())
    }
    /// Destroys the window, as the answer to `CloseWindowRequested`.
    pub fn destroy(&self) -> Result<(), ShellError> {
        let shared = self.live()?;
        unsafe { DestroyWindow(shared.hwnd.get()) }.map_err(|e| win32("DestroyWindow", e))
    }
    /// Events that no handler took. With a handler set, this is empty.
    pub fn take_events(&self) -> Result<Vec<ShellEvent>, ShellError> {
        let shared = self.live()?;
        let events = shared.queue.borrow_mut().drain(..).collect();
        Ok(events)
    }
    /// Every distinct problem so far. Also delivered as `ShellEvent::Problem`.
    pub fn problems(&self) -> Result<Vec<ShellError>, ShellError> {
        Ok(self.live()?.problems.borrow().clone())
    }
    /// With `record_system_commands`, the commands that the window buttons asked for.
    pub fn system_commands(&self) -> Result<Vec<u32>, ShellError> {
        let shared = self.live()?;
        let state = shared
            .state
            .try_borrow()
            .map_err(|_| ShellError::Reentered { message: 0 })?;
        Ok(state.commands.clone())
    }
}

/// Owns one window. Dropping this destroys the window. Many can live on one thread:
/// `run_message_loop` serves them all, and the owner decides when to quit.
pub struct ShellWindow {
    handle: ShellHandle,
    shared: Rc<Shared>,
}
impl std::ops::Deref for ShellWindow {
    type Target = ShellHandle;
    fn deref(&self) -> &ShellHandle {
        &self.handle
    }
}
impl ShellWindow {
    /// Creates a hidden window. Call `show` when it is ready.
    pub fn create(
        renderer: Box<dyn Renderer>,
        measurer: Box<dyn TextMeasurer>,
        cell_width: f32,
        line_height: f32,
    ) -> Result<Self, ShellError> {
        Self::create_with(
            WindowConfig {
                cell_width,
                line_height,
                fake_dpi: None,
                record_system_commands: false,
            },
            renderer,
            measurer,
        )
    }
    pub fn create_with(
        config: WindowConfig,
        renderer: Box<dyn Renderer>,
        measurer: Box<dyn TextMeasurer>,
    ) -> Result<Self, ShellError> {
        let awareness = ensure_dpi_awareness();
        let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .map_err(|e| win32("GetModuleHandleW", e))?;
        let class = w!("QuickEditShellWindow");
        let cursor =
            unsafe { LoadCursorW(None, IDC_ARROW) }.map_err(|e| win32("LoadCursorW", e))?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: cursor,
            lpszClassName: class,
            ..Default::default()
        };
        let atom = unsafe { RegisterClassW(&wc) };
        if atom == 0 {
            let error = unsafe { GetLastError() }.0;
            if !class_registration_ok(atom, error) {
                return Err(ShellError::Win32 {
                    call: "RegisterClassW",
                    message: format!("error {error}"),
                });
            }
        }
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        let shared = Rc::new(Shared {
            hwnd: Cell::new(HWND::default()),
            dpi: Cell::new(96),
            fake_dpi: config.fake_dpi,
            record_commands: config.record_system_commands,
            tracking: Cell::new(Tracking::None),
            state: RefCell::new(State {
                strip: TabStrip::new(vec![Tab::new("Untitled")]),
                renderer,
                measurer,
                theme: Theme::new(ThemeKind::Light),
                dark: false,
                needs_draw: true,
                size: (0, 0),
                client_dips: (0.0, 0.0),
                saved: None,
                commands: Vec::new(),
                list: DisplayList::default(),
            }),
            handler: RefCell::new(None),
            queue: RefCell::new(VecDeque::new()),
            problems: RefCell::new(Vec::new()),
            dispatching: Cell::new(false),
        });
        let raw = Rc::into_raw(shared.clone());
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("QuickEdit"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                t.min_window_width as i32,
                t.row_height as i32,
                None,
                None,
                Some(instance.into()),
                Some(raw.cast()),
            )
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(e) => {
                if shared.hwnd.get().is_invalid() {
                    // The window never took its count, so this gives it back.
                    drop(unsafe { Rc::from_raw(raw) });
                }
                return Err(win32("CreateWindowExW", e));
            }
        };
        if let Err(problem) = awareness {
            shared.report(problem);
        }
        shared.refresh_theme();
        // Without this the first frame keeps the standard title bar until a resize.
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                flags | SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
            )
        }
        .map_err(|e| win32("SetWindowPos", e))?;
        let (mut outer, mut client) = (RECT::default(), RECT::default());
        unsafe {
            GetWindowRect(hwnd, &mut outer).map_err(|e| win32("GetWindowRect", e))?;
            GetClientRect(hwnd, &mut client).map_err(|e| win32("GetClientRect", e))?;
        }
        let frame_w = (outer.right - outer.left) - client.right;
        let frame_h = (outer.bottom - outer.top) - client.bottom;
        let scale = shared.scale();
        let width_dips = (t.new_window_columns * config.cell_width + t.caption_width * 3.0)
            .max(t.min_window_width);
        let height_dips = t.new_window_rows * config.line_height + t.row_height;
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                (width_dips * scale).ceil() as i32 + frame_w,
                (height_dips * scale).ceil() as i32 + frame_h,
                flags | SWP_NOMOVE,
            )
        }
        .map_err(|e| win32("SetWindowPos", e))?;
        shared.on_size(true);
        Ok(Self {
            handle: ShellHandle {
                shared: Rc::downgrade(&shared),
            },
            shared,
        })
    }
    pub fn handle(&self) -> ShellHandle {
        self.handle.clone()
    }
    /// The handler gets a handle, not a reference to the strip, so it can run a dialog.
    pub fn set_event_handler(&self, handler: impl FnMut(&ShellHandle, ShellEvent) + 'static) {
        *self.shared.handler.borrow_mut() = Some(Box::new(handler));
        self.shared.dispatch();
    }
}
impl Drop for ShellWindow {
    fn drop(&mut self) {
        let hwnd = self.shared.hwnd.get();
        if !hwnd.is_invalid()
            && let Err(e) = unsafe { DestroyWindow(hwnd) }
        {
            eprintln!("DestroyWindow while dropping a ShellWindow: {e}");
        }
    }
}

/// Serves every window on this thread until `quit_message_loop`. It returns the code.
pub fn run_message_loop() -> Result<i32, ShellError> {
    let mut msg = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 == -1 {
            return Err(win32("GetMessageW", windows::core::Error::from_thread()));
        }
        if result.0 == 0 {
            return Ok(msg.wParam.0 as i32);
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
/// Ends `run_message_loop`. No window does this on its own.
pub fn quit_message_loop(code: i32) {
    unsafe { PostQuitMessage(code) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn existing_window_class_allows_second_window() {
        assert!(super::class_registration_ok(0, 1410));
        assert!(!super::class_registration_ok(0, 5));
    }
}
