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
    Tab, TabId, TabStrip, caption_hit, maximized_insets,
};
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers, MouseButton,
    MouseCursor, PixelRect, Point, Rect as ApiRect, Renderer, TextMeasurer, Theme, ThemeKind, View,
    ViewContext, ViewRequest,
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
    BLACK_BRUSH, BeginPaint, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWTEXT, EndPaint, FillRect, GetMonitorInfoW, GetStockObject, GetSysColor, HBRUSH,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT,
    SYS_COLOR_INDEX, ScreenToClient, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, AreDpiAwarenessContextsEqual,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, GetSystemMetricsForDpi,
    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::Ime::{
    GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, IME_COMPOSITION_STRING, ImmGetCompositionStringW,
    ImmGetContext, ImmReleaseContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_F10, VK_F11, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
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

/// A failure a test can force, because these calls do not fail on their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestFault {
    /// `create_with` fails after the window exists.
    AfterCreate,
    /// `SetWindowPos` fails on the way into full screen.
    FullScreenMove,
    /// Every `WM_MOVE` counts as a move to another monitor.
    MoveChangesMonitor,
    /// `WM_CREATE` returns -1, so that Windows destroys the window inside
    /// `CreateWindowExW`, which then fails.
    FailWmCreate,
}

/// How a new window starts.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowConfig {
    pub cell_width: f32,
    pub line_height: f32,
    /// For tests: this DPI takes the place of the monitor's, from the first message on.
    pub fake_dpi: Option<u32>,
    /// For tests: `WM_SYSCOMMAND` is listed in `system_commands` and stops there, so that
    /// a hidden window never shows. The real `SendMessageW` still runs. Messages that
    /// reach `DefWindowProcW` are listed in `default_messages`.
    pub record_system_commands: bool,
    /// For tests.
    pub fault: Option<TestFault>,
}

/// What a key in the table does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellAction {
    NewTab,
    ToggleFullScreen,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    pub virtual_key: u16,
    pub modifiers: Modifiers,
    pub action: ShellAction,
}
/// The keys the window acts on by itself. Parity row W08 says keys can change, and M8
/// owns the real table: it replaces this one with `ShellHandle::set_key_table`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyTable {
    pub bindings: Vec<KeyBinding>,
}
impl Default for KeyTable {
    fn default() -> Self {
        Self {
            bindings: vec![
                KeyBinding {
                    virtual_key: 0x54,
                    modifiers: Modifiers {
                        ctrl: true,
                        ..Default::default()
                    },
                    action: ShellAction::NewTab,
                },
                KeyBinding {
                    virtual_key: VK_F11.0,
                    modifiers: Modifiers::default(),
                    action: ShellAction::ToggleFullScreen,
                },
            ],
        }
    }
}
impl KeyTable {
    fn action(&self, virtual_key: u16, modifiers: Modifiers) -> Option<ShellAction> {
        self.bindings
            .iter()
            .find(|b| b.virtual_key == virtual_key && b.modifiers == modifiers)
            .map(|b| b.action)
    }
}

/// What the owner gets to paint the text area with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentFrame {
    pub rect: ApiRect,
    pub theme: Theme,
}

/// The timer id of `ViewRequest::RequestTimer`, which fires once.
pub const VIEW_TIMER: u32 = u32::MAX;

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
/// A `COLORREF` is 0x00BBGGRR.
pub fn color_from_colorref(value: u32) -> Color {
    Color::rgb(value as u8, (value >> 8) as u8, (value >> 16) as u8)
}
fn system_color(index: SYS_COLOR_INDEX) -> Color {
    color_from_colorref(unsafe { GetSysColor(index) })
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
    if let Err(process) =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
    {
        // Windows refuses this once a process has its awareness, as in some accounts. The
        // window lives on this thread, so the thread's awareness gives the same window.
        let before =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if before.is_invalid() {
            return Err(win32("SetProcessDpiAwarenessContext", process));
        }
    }
    Ok(())
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
fn cursor_id(cursor: MouseCursor) -> PCWSTR {
    match cursor {
        MouseCursor::Arrow => IDC_ARROW,
        MouseCursor::IBeam => IDC_IBEAM,
        MouseCursor::Hand => IDC_HAND,
        MouseCursor::SizeHorizontal => IDC_SIZEWE,
        MouseCursor::SizeVertical => IDC_SIZENS,
        MouseCursor::Wait => IDC_WAIT,
    }
}
fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 4,
        MouseButton::X1 => 8,
        MouseButton::X2 => 16,
    }
}
/// Terminal's `_GetTopBorderHeight`: 1 pixel, and none when maximized or full screen.
pub fn top_border_height(mode: FrameMode) -> i32 {
    i32::from(mode == FrameMode::Restored)
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
/// `Renderer` has no call for the monitor's text settings, and `BitmapRenderer` lives in a
/// crate this one cannot name. So the owner gives a function for its own renderer type.
pub type TextSettingsRefresh<R> = fn(&mut R, *mut c_void) -> Result<bool, qe_api::RenderError>;
trait RendererHolder {
    fn renderer(&mut self) -> &mut dyn Renderer;
    fn refresh(&mut self, hwnd: *mut c_void) -> Result<bool, qe_api::RenderError>;
}
struct WithRefresh<R> {
    renderer: R,
    refresh: TextSettingsRefresh<R>,
}
impl<R: Renderer> RendererHolder for WithRefresh<R> {
    fn renderer(&mut self) -> &mut dyn Renderer {
        &mut self.renderer
    }
    fn refresh(&mut self, hwnd: *mut c_void) -> Result<bool, qe_api::RenderError> {
        (self.refresh)(&mut self.renderer, hwnd)
    }
}
/// `create` and `create_with` have no refresh function: text keeps the settings it has.
impl RendererHolder for Box<dyn Renderer> {
    fn renderer(&mut self) -> &mut dyn Renderer {
        self.as_mut()
    }
    fn refresh(&mut self, _hwnd: *mut c_void) -> Result<bool, qe_api::RenderError> {
        Ok(false)
    }
}
struct State {
    strip: TabStrip,
    renderer: Box<dyn RendererHolder>,
    measurer: Box<dyn TextMeasurer>,
    theme: Theme,
    dark: bool,
    contrast: Option<ContrastColors>,
    needs_draw: bool,
    size: (u32, u32),
    client_dips: (f32, f32),
    saved: Option<SavedFrame>,
    commands: Vec<u32>,
    content_cursor: MouseCursor,
    list: DisplayList,
}
impl State {
    fn content_rect(&self) -> ApiRect {
        let row = self.strip.row_rect();
        let top = row.y.0 + row.height.0;
        ApiRect {
            x: Dip(0.0),
            y: Dip(top),
            width: Dip(self.client_dips.0),
            height: Dip((self.client_dips.1 - top).max(0.0)),
        }
    }
    /// Draws `list`, and the row over it, then copies the rectangle that Windows asked
    /// for. A frame with a problem still ends and still copies: its pixels are valid.
    fn paint(
        &mut self,
        list: Option<DisplayList>,
        dc: *mut c_void,
        rect: PixelRect,
    ) -> Vec<ShellError> {
        let mut problems = Vec::new();
        if let Some(mut list) = list {
            // The top border's row, which `on_paint` then paints black on the window.
            let row = self.strip.row_rect();
            if row.y.0 > 0.0 {
                list.push(DrawCommand::FillRect {
                    rect: ApiRect {
                        x: Dip(0.0),
                        y: Dip(0.0),
                        width: row.width,
                        height: row.y,
                    },
                    color: self.theme.tab_background,
                });
            }
            self.strip
                .draw(&mut list, &self.theme, self.measurer.as_ref());
            if let Err(e) = self.renderer.renderer().begin_frame() {
                problems.push(renderer_error("begin_frame", e));
            }
            if let Err(e) = self.renderer.renderer().draw(&list) {
                problems.push(renderer_error("draw", e));
            }
            if let Err(e) = self.renderer.renderer().end_frame() {
                problems.push(renderer_error("end_frame", e));
            }
            self.list = list;
        }
        if let Err(e) = self.renderer.renderer().present_rect(dc, rect) {
            problems.push(renderer_error("present_rect", e));
        }
        problems
    }
}
type ContentPainter = Box<dyn FnMut(&mut DisplayList, ContentFrame)>;
type EventHandler = Box<dyn FnMut(&ShellHandle, ShellEvent)>;
struct Shared {
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    fake_dpi: Option<u32>,
    record_commands: bool,
    tracking: Cell<Tracking>,
    fault: Option<TestFault>,
    monitor: Cell<isize>,
    /// The window took the count from `Rc::into_raw`. From then on, `WM_NCDESTROY` gives
    /// it back. A null handle cannot say this, because `WM_NCDESTROY` also clears it.
    adopted: Cell<bool>,
    top_margin: Cell<i32>,
    default_log: RefCell<Vec<u32>>,
    invalidated: RefCell<Vec<RECT>>,
    /// The buttons held since a press in the text area. While any is held, the text area
    /// holds the mouse: every move and release goes there, even over the row.
    content_buttons: Cell<u8>,
    high_surrogate: Cell<Option<u16>>,
    /// A key the strip or the key table used. Its `WM_CHAR` must not reach the content.
    swallow_char: Cell<bool>,
    /// A key the key table used. Its key-up must not reach the content either.
    swallow_up: Cell<Option<u16>>,
    last_mouse: Cell<POINT>,
    ime_open: Cell<bool>,
    keys: RefCell<KeyTable>,
    painter: RefCell<Option<ContentPainter>>,
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
                state.renderer.renderer().invalidate(*b);
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
            let done = unsafe { InvalidateRect(Some(self.hwnd.get()), Some(&rect), false) };
            if self.record_commands && done.as_bool() {
                self.invalidated.borrow_mut().push(rect);
            }
        }
    }
    fn invalidate_all(&self) {
        let done = self.with_state(WM_PAINT, |state| {
            state.renderer.renderer().invalidate_all();
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
                ViewRequest::SetFocus(_) => {}
                ViewRequest::Run(command) => self
                    .queue
                    .borrow_mut()
                    .push_back(ShellEvent::RunCommand(command)),
                ViewRequest::SetCursor(cursor) => {
                    self.with_state(WM_SETCURSOR, |state| state.content_cursor = cursor);
                }
                ViewRequest::RequestTimer(milliseconds) => {
                    let id =
                        unsafe { SetTimer(Some(hwnd), VIEW_TIMER as usize, milliseconds, None) };
                    if id == 0 {
                        self.report(win32("SetTimer", windows::core::Error::from_thread()));
                    }
                }
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
    fn make_event(&self, kind: InputKind) -> InputEvent {
        InputEvent {
            time_us: u64::from(unsafe { GetMessageTime() } as u32) * 1000,
            kind,
        }
    }
    fn to_content(&self, kind: InputKind) {
        let event = self.make_event(kind);
        self.queue
            .borrow_mut()
            .push_back(ShellEvent::ContentInput(event));
    }
    /// Who holds the mouse decides. A press in the text area keeps every later move and
    /// release there, with capture, so that a selection drag can cross the row.
    fn mouse_button(&self, message: u32, button: MouseButton, at: Point, down: bool) {
        let bit = button_bit(button);
        let held = self.content_buttons.get();
        let modifiers = modifiers();
        if down {
            let kind = InputKind::MouseDown {
                at,
                button,
                modifiers,
            };
            if held != 0 {
                self.content_buttons.set(held | bit);
                self.to_content(kind);
                return;
            }
            let (handled, event) = self.input(message, kind);
            if handled == Handled::No {
                self.content_buttons.set(bit);
                unsafe {
                    SetCapture(self.hwnd.get());
                }
                self.queue
                    .borrow_mut()
                    .push_back(ShellEvent::ContentInput(event));
            }
            return;
        }
        let kind = InputKind::MouseUp {
            at,
            button,
            modifiers,
        };
        if held & bit != 0 {
            self.content_buttons.set(held & !bit);
            if held & !bit == 0
                && let Err(e) = unsafe { ReleaseCapture() }
            {
                self.report(win32("ReleaseCapture", e));
            }
            self.to_content(kind);
        } else {
            self.content_input(message, kind);
        }
    }
    fn input(&self, message: u32, kind: InputKind) -> (Handled, InputEvent) {
        let event = self.make_event(kind);
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
        unsafe {
            SendMessageW(
                self.hwnd.get(),
                WM_SYSCOMMAND,
                Some(WPARAM(command as usize)),
                Some(LPARAM(0)),
            );
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
        let moved = if self.fault == Some(TestFault::FullScreenMove) {
            Err(windows::core::Error::empty())
        } else {
            unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOP),
                    b.left,
                    b.top,
                    b.right - b.left,
                    b.bottom - b.top,
                    SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
                )
            }
        };
        if let Err(e) = moved {
            // The window did not move, so it must not stay styled as full screen.
            self.report(win32("SetWindowPos", e));
            unsafe {
                SetWindowLongPtrW(hwnd, GWL_STYLE, style);
            }
            self.with_state(WM_KEYDOWN, |state| {
                state.saved = None;
                state.strip.set_frame_mode(if was_maximized {
                    FrameMode::Maximized
                } else {
                    FrameMode::Restored
                });
            });
            return;
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
        // Terminal's `_UpdateFrameMargins`: the whole top of the frame, because a margin of
        // only the 1-pixel border has a transparency bug when the window is inactive.
        // Our opaque pixels then hide all of it except the row that `on_paint` paints black.
        let mut top = 0;
        if top_border_height(mode) != 0 {
            let mut frame = RECT::default();
            match unsafe {
                AdjustWindowRectExForDpi(
                    &mut frame,
                    WS_OVERLAPPEDWINDOW,
                    false,
                    WINDOW_EX_STYLE(0),
                    self.dpi.get(),
                )
            } {
                Ok(()) => top = -frame.top,
                Err(e) => self.report(win32("AdjustWindowRectExForDpi", e)),
            }
        }
        self.top_margin.set(top);
        let margins = MARGINS {
            cyTopHeight: top,
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
        self.apply_theme(dark, contrast);
    }
    fn apply_theme(&self, dark: bool, contrast: Option<ContrastColors>) {
        let theme = build_theme(dark, contrast);
        let changed = self.with_state(WM_THEMECHANGED, |state| {
            // `Theme` has no highlight text or gray text, so the colors compare too.
            let changed = state.dark != dark || state.theme != theme || state.contrast != contrast;
            state.dark = dark;
            state.theme = theme;
            state.contrast = contrast;
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
                match state.renderer.renderer().resize(size.0, size.1) {
                    Ok(()) => state.size = size,
                    Err(e) => problem = Some(renderer_error("resize", e)),
                }
            }
            let content_before = state.content_rect();
            state.client_dips = (width as f32 / scale, height as f32 / scale);
            state.strip.set_scale(scale);
            // Terminal places its whole island, tabs and window buttons alike, below the
            // 1-pixel top border when restored (`_UpdateIslandPosition`). Its -1 when
            // maximized works around a XAML click bug that this window does not have.
            let border = top_border_height(state.strip.frame_mode()) as f32 / scale;
            let row = ApiRect {
                x: Dip(0.0),
                y: Dip(border),
                width: Dip(state.client_dips.0),
                height: Dip(state.strip.row_height()),
            };
            state.strip.layout(row, state.measurer.as_ref());
            let content = state.content_rect();
            let moved = content != content_before;
            (problem, mode_changed, size_changed, moved, content)
        });
        let Some((problem, mode_changed, size_changed, moved, content)) = result else {
            return;
        };
        if moved {
            self.to_content(InputKind::Resize {
                width: content.width,
                height: content.height,
            });
        }
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
        // Both forms start with the rectangle: the `RECT` itself when `w` is 0, and
        // `rgrc[0]` of `NCCALCSIZE_PARAMS` otherwise. Microsoft documents client-area output
        // for both, so both get the same answer. Terminal returns early for the first form.
        let first = l.0 as *mut RECT;
        let original_top = unsafe { (*first).top };
        let result = unsafe { DefWindowProcW(hwnd, WM_NCCALCSIZE, w, l) };
        if result.0 != 0 {
            return result;
        }
        let mut rect = unsafe { *first };
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
            *first = rect;
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
    /// A monitor can have its own text settings. If they changed, the renderer has asked
    /// for a full repaint, and Windows must be told too.
    fn refresh_text_settings(&self, hwnd: HWND) {
        match self.with_state(WM_DISPLAYCHANGE, |s| s.renderer.refresh(hwnd.0)) {
            Some(Ok(true)) => {
                self.with_state(WM_DISPLAYCHANGE, |s| s.needs_draw = true);
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            Some(Err(e)) => self.report(renderer_error("refresh_text_settings", e)),
            Some(Ok(false)) | None => {}
        }
    }
    fn on_dpi_changed(&self, hwnd: HWND, w: WPARAM, l: LPARAM) {
        let dpi = (w.0 & 0xffff) as u32;
        self.dpi.set(dpi);
        if let Some(Err(e)) = self.with_state(WM_DPICHANGED, |s| s.renderer.renderer().set_dpi(dpi))
        {
            self.report(renderer_error("set_dpi", e));
        }
        self.to_content(InputKind::DpiChange { dpi });
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
        // The owner paints with no borrow held, so that its painter can send messages.
        let job = self.with_state(WM_PAINT, |state| {
            let mode = state.strip.frame_mode();
            if !state.needs_draw {
                return (None, mode);
            }
            state.needs_draw = false;
            let mut list = std::mem::take(&mut state.list);
            list.commands.clear();
            let frame = ContentFrame {
                rect: state.content_rect(),
                theme: state.theme,
            };
            (Some((list, frame)), mode)
        });
        let Some((job, mode)) = job else {
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            return;
        };
        let list = job.map(|(mut list, frame)| {
            list.push(DrawCommand::FillRect {
                rect: frame.rect,
                color: frame.theme.text_background,
            });
            let painter = self.painter.borrow_mut().take();
            if let Some(mut painter) = painter {
                list.push(DrawCommand::PushClip(frame.rect));
                painter(&mut list, frame);
                list.push(DrawCommand::PopClip);
                let mut slot = self.painter.borrow_mut();
                if slot.is_none() {
                    *slot = Some(painter);
                }
            }
            list
        });
        let problems = self.with_state(WM_PAINT, |state| state.paint(list, dc.0, rect));
        for problem in problems.unwrap_or_default() {
            self.report(problem);
        }
        // Terminal's `_OnPaint`: black has alpha 0, so the top border from DWM shows here.
        let border = top_border_height(mode);
        if paint.rcPaint.top < border {
            let top = RECT {
                bottom: border,
                ..paint.rcPaint
            };
            unsafe {
                FillRect(dc, &top, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            }
        }
        unsafe {
            let _ = EndPaint(hwnd, &paint);
        }
    }
    fn on_ime_composition(&self, hwnd: HWND, l: LPARAM) -> bool {
        let read = |context, kind: IME_COMPOSITION_STRING| -> Option<String> {
            let bytes = unsafe { ImmGetCompositionStringW(context, kind, None, 0) };
            let mut units = vec![0u16; usize::try_from(bytes).ok()? / 2];
            unsafe {
                ImmGetCompositionStringW(
                    context,
                    kind,
                    Some(units.as_mut_ptr().cast()),
                    bytes as u32,
                );
            }
            Some(String::from_utf16_lossy(&units))
        };
        let context = unsafe { ImmGetContext(hwnd) };
        if context.is_invalid() {
            return false;
        }
        let flags = l.0 as u32;
        let mut consumed = false;
        if flags & GCS_RESULTSTR.0 != 0
            && let Some(text) = read(context, GCS_RESULTSTR)
        {
            self.ime_open.set(false);
            self.to_content(InputKind::ImeEnd { text });
            consumed = true;
        } else if flags & GCS_COMPSTR.0 != 0
            && let Some(text) = read(context, GCS_COMPSTR)
        {
            let cursor = unsafe { ImmGetCompositionStringW(context, GCS_CURSORPOS, None, 0) };
            self.to_content(InputKind::ImeUpdate {
                text,
                cursor_utf16: usize::try_from(cursor).unwrap_or(0),
            });
        }
        unsafe {
            let _ = ImmReleaseContext(hwnd, context);
        }
        consumed
    }
    fn on_key_table(&self, msg: u32, vk: u16, repeat: bool) -> bool {
        let Some(action) = self.keys.borrow().action(vk, modifiers()) else {
            return false;
        };
        self.swallow_up.set(Some(vk));
        self.swallow_char.set(true);
        self.strip_action(msg, |strip, _| strip.disarm_alt());
        if !repeat {
            match action {
                ShellAction::NewTab => self.queue.borrow_mut().push_back(ShellEvent::NewTab),
                ShellAction::ToggleFullScreen => self.toggle_full_screen(),
            }
        }
        true
    }
    fn on_char(&self, unit: u16) {
        if self.swallow_char.take() {
            return;
        }
        let high = self.high_surrogate.take();
        match unit {
            0xd800..=0xdbff => {
                if let Some(high) = high {
                    self.report(ShellError::LoneSurrogate { unit: high });
                }
                self.high_surrogate.set(Some(unit));
            }
            0xdc00..=0xdfff => match high {
                Some(high) => {
                    let code =
                        0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
                    if let Some(c) = char::from_u32(code) {
                        self.to_content(InputKind::Char(c));
                    }
                }
                None => self.report(ShellError::LoneSurrogate { unit }),
            },
            _ => {
                if let Some(high) = high {
                    self.report(ShellError::LoneSurrogate { unit: high });
                }
                if let Some(c) = char::from_u32(u32::from(unit)) {
                    self.to_content(InputKind::Char(c));
                }
            }
        }
    }
    fn on_message(&self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        let default = || {
            if self.record_commands {
                self.default_log.borrow_mut().push(msg);
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        };
        match msg {
            WM_NCCALCSIZE => self.on_nccalcsize(hwnd, w, l),
            WM_NCHITTEST => self.on_nchittest(hwnd, w, l),
            WM_CREATE if self.fault == Some(TestFault::FailWmCreate) => LRESULT(-1),
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                if w.0 as u32 != SIZE_MINIMIZED {
                    self.on_size(false);
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                self.refresh_theme();
                if msg == WM_SETTINGCHANGE {
                    self.refresh_text_settings(hwnd);
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                self.refresh_text_settings(hwnd);
                default()
            }
            WM_MOVE => {
                let now = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }.0 as isize;
                let moved = self.monitor.replace(now) != now
                    || self.fault == Some(TestFault::MoveChangesMonitor);
                if moved {
                    self.refresh_text_settings(hwnd);
                }
                default()
            }
            WM_DPICHANGED => {
                self.on_dpi_changed(hwnd, w, l);
                self.refresh_text_settings(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                self.track_leave(Tracking::Client);
                self.last_mouse.set(xy(l));
                let at = self.dips(xy(l));
                if self.content_buttons.get() != 0 {
                    self.to_content(InputKind::MouseMove { at });
                } else {
                    self.content_input(msg, InputKind::MouseMove { at });
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN | WM_LBUTTONUP
            | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP => {
                let button = match msg {
                    WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
                    WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
                    WM_MBUTTONDOWN | WM_MBUTTONUP => MouseButton::Middle,
                    _ if (w.0 >> 16) & 0xffff == 1 => MouseButton::X1,
                    _ => MouseButton::X2,
                };
                let down = matches!(
                    msg,
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
                );
                self.mouse_button(msg, button, self.dips(xy(l)), down);
                LRESULT(if matches!(msg, WM_XBUTTONDOWN | WM_XBUTTONUP) {
                    1
                } else {
                    0
                })
            }
            WM_SETCURSOR => {
                if (l.0 & 0xffff) as u32 != HTCLIENT {
                    return default();
                }
                let at = self.dips(self.last_mouse.get());
                let id = self.with_state(msg, |state| {
                    let b = state.content_rect();
                    if at.y.0 >= b.y.0 {
                        state.content_cursor
                    } else {
                        MouseCursor::Arrow
                    }
                });
                match unsafe { LoadCursorW(None, cursor_id(id.unwrap_or(MouseCursor::Arrow))) } {
                    Ok(cursor) => unsafe {
                        SetCursor(Some(cursor));
                    },
                    Err(e) => self.report(win32("LoadCursorW", e)),
                }
                LRESULT(1)
            }
            WM_TIMER => {
                let id = w.0 as u32;
                if id == VIEW_TIMER
                    && let Err(e) = unsafe { KillTimer(Some(hwnd), w.0) }
                {
                    self.report(win32("KillTimer", e));
                }
                self.queue.borrow_mut().push_back(ShellEvent::Timer(id));
                LRESULT(0)
            }
            WM_SYSCOMMAND if self.record_commands => {
                let command = (w.0 as u32) & 0xfff0;
                self.with_state(msg, |state| state.commands.push(command));
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
                // Terminal lets this reach `DefWindowProc` for every hit code, window
                // buttons too (`NonClientIslandWindow.cpp`, the `WM_NCMOUSEMOVE` case and
                // the `DefWindowProc` at the end of its handler). The Snap flyout may
                // depend on it.
                default()
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
                // The same handler in Terminal: its `break` reaches `DefWindowProc`.
                default()
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
                    if self.content_buttons.take() != 0 {
                        self.queue
                            .borrow_mut()
                            .push_back(ShellEvent::ContentCaptureLost);
                    }
                    self.strip_action(msg, |strip, ctx| strip.capture_lost(ctx));
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                // Bit 30 says that the key was already down: this is a repeat.
                if self.on_key_table(msg, w.0 as u16, l.0 & (1 << 30) != 0) {
                    return LRESULT(0);
                }
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
                if self.swallow_up.get() == Some(w.0 as u16) {
                    self.swallow_up.set(None);
                    return LRESULT(0);
                }
                let (handled, event) = self.input(
                    msg,
                    InputKind::KeyUp {
                        virtual_key: w.0 as u16,
                        modifiers: modifiers(),
                    },
                );
                self.after_key(msg, w, handled, event, default)
            }
            WM_CHAR => {
                self.on_char(w.0 as u16);
                LRESULT(0)
            }
            WM_SYSCHAR => {
                // Alt+Space opens the system menu. For any other Alt+letter, the default
                // handler looks for a menu, finds none and beeps.
                if w.0 == usize::from(b' ') {
                    default()
                } else {
                    LRESULT(0)
                }
            }
            WM_IME_STARTCOMPOSITION => {
                self.ime_open.set(true);
                self.to_content(InputKind::ImeStart);
                default()
            }
            WM_IME_COMPOSITION => {
                if self.on_ime_composition(hwnd, l) {
                    LRESULT(0)
                } else {
                    default()
                }
            }
            WM_IME_ENDCOMPOSITION => {
                if self.ime_open.take() {
                    self.to_content(InputKind::ImeEnd {
                        text: String::new(),
                    });
                }
                default()
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                let kind = if msg == WM_SETFOCUS {
                    InputKind::Focus
                } else {
                    InputKind::Blur
                };
                self.input(msg, kind.clone());
                self.to_content(kind);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let info = l.0 as *mut MINMAXINFO;
                let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
                // The limits are for the client area, so the frame comes on top.
                let (mut outer, mut client) = (RECT::default(), RECT::default());
                unsafe {
                    let _ = GetWindowRect(hwnd, &mut outer);
                    let _ = GetClientRect(hwnd, &mut client);
                    (*info).ptMinTrackSize.x = (t.min_window_width * self.scale()).ceil() as i32
                        + (outer.right - outer.left)
                        - client.right;
                    (*info).ptMinTrackSize.y = (t.min_window_height * self.scale()).ceil() as i32
                        + (outer.bottom - outer.top)
                        - client.bottom;
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
    /// Every key the strip does not take reaches the owner, chords too. Alt alone and F10
    /// never reach `DefWindowProcW`, which would start the invisible menu loop. Other
    /// system keys, such as Alt+F4, must reach it.
    fn after_key(
        &self,
        msg: u32,
        w: WPARAM,
        handled: Handled,
        event: InputEvent,
        default: impl FnOnce() -> LRESULT,
    ) -> LRESULT {
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        if handled == Handled::Yes {
            if down {
                self.swallow_char.set(true);
            }
            return LRESULT(0);
        }
        if down {
            self.swallow_char.set(false);
        }
        self.queue
            .borrow_mut()
            .push_back(ShellEvent::ContentInput(event));
        let menu_key = w.0 == usize::from(VK_MENU.0) || w.0 == usize::from(VK_F10.0);
        if (msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP) && !menu_key {
            return default();
        }
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
            (*raw).adopted.set(true);
            (*raw).hwnd.set(hwnd);
            let dpi = (*raw).fake_dpi.unwrap_or_else(|| GetDpiForWindow(hwnd));
            (*raw).dpi.set(dpi);
            if let Some(()) = (*raw).with_state(msg, |s| s.strip.set_scale(dpi as f32 / 96.0)) {}
            // The DPI is set before anything can draw.
            if let Some(Err(e)) = (*raw).with_state(msg, |s| s.renderer.renderer().set_dpi(dpi)) {
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
        let (result, row, events, release) = {
            let mut state = shared
                .state
                .try_borrow_mut()
                .map_err(|_| ShellError::Reentered { message: 0 })?;
            let result = f(&mut state.strip);
            (
                result,
                state.strip.row_rect(),
                state.strip.take_events(),
                state.strip.take_capture_release(),
            )
        };
        let mut requests = vec![ViewRequest::Invalidate(row)];
        if release {
            requests.push(ViewRequest::CaptureMouse(false));
        }
        shared.apply(requests, events);
        shared.dispatch();
        Ok(result)
    }
    pub fn add_tab(&self, tab: Tab) -> Result<TabId, ShellError> {
        self.with_strip(|strip| strip.add_tab(tab))
    }
    /// Removes a tab, as the answer to `CloseTabRequested`. An id that has already gone
    /// does nothing.
    pub fn close_tab(&self, id: TabId) -> Result<(), ShellError> {
        self.with_strip(|strip| strip.close(id))
    }
    /// The text area in dips: the client area under the row.
    pub fn content_rect(&self) -> Result<ApiRect, ShellError> {
        let shared = self.live()?;
        let state = shared
            .state
            .try_borrow()
            .map_err(|_| ShellError::Reentered { message: 0 })?;
        Ok(state.content_rect())
    }
    /// Asks the next paint to repaint the whole text area.
    pub fn invalidate_content(&self) -> Result<(), ShellError> {
        let rect = self.content_rect()?;
        self.live()?.invalidate(&[rect]);
        Ok(())
    }
    /// Asks the next paint to repaint this part of the window, in dips.
    pub fn invalidate_rect(&self, rect: ApiRect) -> Result<(), ShellError> {
        self.live()?.invalidate(&[rect]);
        Ok(())
    }
    /// The cursor over the text area, such as `MouseCursor::IBeam`.
    pub fn set_content_cursor(&self, cursor: MouseCursor) -> Result<(), ShellError> {
        let shared = self.live()?;
        shared.with_state(WM_SETCURSOR, |state| state.content_cursor = cursor);
        Ok(())
    }
    /// Starts a timer that sends `ShellEvent::Timer(id)` until `kill_timer`.
    pub fn set_timer(&self, id: u32, milliseconds: u32) -> Result<(), ShellError> {
        let shared = self.live()?;
        let set = unsafe { SetTimer(Some(shared.hwnd.get()), id as usize, milliseconds, None) };
        if set == 0 {
            return Err(win32("SetTimer", windows::core::Error::from_thread()));
        }
        Ok(())
    }
    pub fn kill_timer(&self, id: u32) -> Result<(), ShellError> {
        let shared = self.live()?;
        unsafe { KillTimer(Some(shared.hwnd.get()), id as usize) }
            .map_err(|e| win32("KillTimer", e))
    }
    /// Carries out what an owner's own view asked for in its `ViewContext`.
    pub fn apply_requests(&self, requests: Vec<ViewRequest>) -> Result<(), ShellError> {
        let shared = self.live()?;
        shared.apply(requests, Vec::new());
        shared.dispatch();
        Ok(())
    }
    /// Replaces the keys the window acts on by itself. An empty table sends them all to
    /// the owner as `ContentInput`.
    pub fn set_key_table(&self, table: KeyTable) -> Result<(), ShellError> {
        *self.live()?.keys.borrow_mut() = table;
        Ok(())
    }
    /// Applies a theme as `WM_THEMECHANGED` would. For tests, and to follow a setting
    /// the owner reads itself.
    pub fn apply_theme(
        &self,
        dark: bool,
        contrast: Option<ContrastColors>,
    ) -> Result<(), ShellError> {
        self.live()?.apply_theme(dark, contrast);
        Ok(())
    }
    /// The top margin last given to `DwmExtendFrameIntoClientArea`, in pixels.
    pub fn frame_top_margin(&self) -> Result<i32, ShellError> {
        Ok(self.live()?.top_margin.get())
    }
    /// With `record_system_commands`, the rectangles for which `InvalidateRect` returned
    /// success since the last call. A hidden window keeps no update region to read.
    pub fn invalidated_rects(&self) -> Result<Vec<RECT>, ShellError> {
        Ok(std::mem::take(&mut *self.live()?.invalidated.borrow_mut()))
    }
    /// With `record_system_commands`, the messages that reached `DefWindowProcW`.
    pub fn default_messages(&self) -> Result<Vec<u32>, ShellError> {
        Ok(self.live()?.default_log.borrow().clone())
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
    /// Events that no handler took. With a handler set, this is empty. It still works
    /// after the window is destroyed, so that a polling owner hears `WindowDestroyed`.
    pub fn take_events(&self) -> Result<Vec<ShellEvent>, ShellError> {
        let shared = self.shared.upgrade().ok_or(ShellError::WindowGone)?;
        let events = shared.queue.borrow_mut().drain(..).collect();
        Ok(events)
    }
    /// Every distinct problem so far. Also delivered as `ShellEvent::Problem`.
    pub fn problems(&self) -> Result<Vec<ShellError>, ShellError> {
        let shared = self.shared.upgrade().ok_or(ShellError::WindowGone)?;
        Ok(shared.problems.borrow().clone())
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
                ..Default::default()
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
        Self::create_inner(config, Box::new(renderer), measurer)
    }
    /// `create_with`, and the window calls `refresh` with its `HWND` after
    /// `WM_SETTINGCHANGE`, `WM_DISPLAYCHANGE`, `WM_DPICHANGED` and a move to another
    /// monitor. For `qe-render`, pass a function that calls
    /// `BitmapRenderer::refresh_text_settings_for_window`.
    pub fn create_with_refresh<R: Renderer + 'static>(
        config: WindowConfig,
        renderer: R,
        refresh: TextSettingsRefresh<R>,
        measurer: Box<dyn TextMeasurer>,
    ) -> Result<Self, ShellError> {
        Self::create_inner(
            config,
            Box::new(WithRefresh { renderer, refresh }),
            measurer,
        )
    }
    fn create_inner(
        config: WindowConfig,
        renderer: Box<dyn RendererHolder>,
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
            fault: config.fault,
            monitor: Cell::new(0),
            adopted: Cell::new(false),
            top_margin: Cell::new(0),
            default_log: RefCell::new(Vec::new()),
            invalidated: RefCell::new(Vec::new()),
            content_buttons: Cell::new(0),
            high_surrogate: Cell::new(None),
            swallow_char: Cell::new(false),
            swallow_up: Cell::new(None),
            last_mouse: Cell::new(POINT::default()),
            ime_open: Cell::new(false),
            keys: RefCell::new(KeyTable::default()),
            painter: RefCell::new(None),
            tracking: Cell::new(Tracking::None),
            state: RefCell::new(State {
                strip: TabStrip::new(vec![Tab::new("Untitled")]),
                renderer,
                measurer,
                theme: Theme::new(ThemeKind::Light),
                dark: false,
                contrast: None,
                needs_draw: true,
                size: (0, 0),
                client_dips: (0.0, 0.0),
                saved: None,
                commands: Vec::new(),
                content_cursor: MouseCursor::Arrow,
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
                if !shared.adopted.get() {
                    // The window never took its count, so this gives it back.
                    drop(unsafe { Rc::from_raw(raw) });
                }
                // Only `shared` is left. Any other count here is a leak or a double release.
                debug_assert_eq!(Rc::strong_count(&shared), 1);
                return Err(win32("CreateWindowExW", e));
            }
        };
        // From here on, dropping this value destroys the window, so a late failure
        // leaves no hidden window behind.
        let window = Self {
            handle: ShellHandle {
                shared: Rc::downgrade(&shared),
            },
            shared,
        };
        window.finish_create(hwnd, config, awareness, t)?;
        Ok(window)
    }
    fn finish_create(
        &self,
        hwnd: HWND,
        config: WindowConfig,
        awareness: Result<(), ShellError>,
        t: ShellTokens,
    ) -> Result<(), ShellError> {
        let shared = &self.shared;
        if let Err(problem) = awareness {
            shared.report(problem);
        }
        if config.fault == Some(TestFault::AfterCreate) {
            return Err(ShellError::Win32 {
                call: "finish_create",
                message: "forced by a test".into(),
            });
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
        Ok(())
    }
    pub fn handle(&self) -> ShellHandle {
        self.handle.clone()
    }
    /// The handler gets a handle, not a reference to the strip, so it can run a dialog.
    /// Paints the text area, under the row and clipped to the rectangle in the frame. It
    /// runs with no borrow held, so that it can send messages. The window has already
    /// filled the rectangle with the theme's text background.
    pub fn set_content_painter(
        &self,
        painter: impl FnMut(&mut DisplayList, ContentFrame) + 'static,
    ) {
        *self.shared.painter.borrow_mut() = Some(Box::new(painter));
    }
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
