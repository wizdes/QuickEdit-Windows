//! The Settings window: a native title bar around the painted panel. Nothing here runs
//! until the user opens Settings. No timer runs: painting follows input and store events.
use crate::font_dialog::choose_plain_font;
use crate::panel::{PanelRequest, SETTINGS_WIDTH, SettingsPanel};
use crate::store::{FontChoice, SettingsStore};
use qe_api::{
    Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseButton, MouseCursor,
    PixelRect, Point, Rect, Renderer, TextEncoding, TextMeasurer, Theme, View, ViewContext,
    ViewRequest, Widget, WidgetId,
};
use qe_ui::{
    DialogWindow, ENCODING_PANEL_HEIGHT, ENCODING_PANEL_WIDTH, EncodingPanel, MessageHandler,
    UiTokens,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HDC, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    ScreenToClient,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the window when the store's writer has finished a write.
const WRITE_DONE_MESSAGE: u32 = WM_APP + 0x61;
/// `WinUser.h`. The `windows` crate keeps it behind a feature nothing else here needs.
const WM_MOUSELEAVE: u32 = 0x02a3;
const ENCODING_PANEL_ID: u64 = 0x5e_0000;
const PANEL_ID: u64 = 0x5d_0000;

/// What the app gives the window. `qe-settings` paints through these and links no
/// renderer of its own. The tests use the same trait with hidden windows.
pub trait SettingsHost {
    fn measurer(&self) -> Arc<dyn TextMeasurer>;
    /// The theme the app paints with now, also for high contrast.
    fn theme(&self) -> Theme;
    /// A renderer for a window of this size in device pixels, at this DPI.
    fn make_renderer(&self, width: u32, height: u32, dpi: u32)
    -> Result<Box<dyn Renderer>, String>;
    /// Shows the font dialog. A test host answers without one.
    fn choose_font(
        &self,
        owner: Option<HWND>,
        current: &FontChoice,
    ) -> Result<Option<FontChoice>, String> {
        choose_plain_font(owner, current)
    }
    /// A message before the window sees it. The app answers `WM_GETOBJECT` here, from
    /// `SettingsWindow::widgets`, for UI Automation.
    fn window_message(&self, _: HWND, _: u32, _: WPARAM, _: LPARAM) -> Option<LRESULT> {
        None
    }
    /// Something a screen reader should say, such as why a key was refused.
    fn announce(&self, _: HWND, _: &str) {}
}

/// Whether the window goes on screen. Tests keep it hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Shown,
    Hidden,
}

fn modifiers() -> Modifiers {
    let down = |key: u16| {
        // SAFETY: a plain query about this thread's key state.
        unsafe { GetKeyState(i32::from(key)) < 0 }
    };
    Modifiers {
        ctrl: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        shift: down(VK_SHIFT.0),
        win: down(VK_LWIN.0) || down(VK_RWIN.0),
    }
}
fn at_pixels(x: i32, y: i32, scale: f32) -> Point {
    Point {
        x: Dip(x as f32 / scale),
        y: Dip(y as f32 / scale),
    }
}
fn at_lparam(l: LPARAM, scale: f32) -> Point {
    at_pixels(i32::from(l.0 as i16), i32::from((l.0 >> 16) as i16), scale)
}
fn button_of(message: u32) -> Option<(MouseButton, bool)> {
    Some(match message {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => (MouseButton::Left, true),
        WM_LBUTTONUP => (MouseButton::Left, false),
        WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => (MouseButton::Right, true),
        WM_RBUTTONUP => (MouseButton::Right, false),
        WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => (MouseButton::Middle, true),
        WM_MBUTTONUP => (MouseButton::Middle, false),
        _ => return None,
    })
}
/// The event a window message means for a painted view, in dips.
fn input_from_message(
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    scale: f32,
    surrogate: &Cell<Option<u16>>,
) -> Option<InputKind> {
    let mods = || {
        let mut held = modifiers();
        held.alt |= matches!(message, WM_SYSKEYDOWN | WM_SYSKEYUP) && l.0 & (1 << 29) != 0;
        held
    };
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(InputKind::KeyDown {
            virtual_key: w.0 as u16,
            modifiers: mods(),
            repeat: l.0 & (1 << 30) != 0,
        }),
        WM_KEYUP | WM_SYSKEYUP => Some(InputKind::KeyUp {
            virtual_key: w.0 as u16,
            modifiers: mods(),
        }),
        WM_CHAR => {
            let unit = w.0 as u16;
            let text = match (surrogate.take(), unit) {
                (None, 0xd800..=0xdbff) => {
                    surrogate.set(Some(unit));
                    return None;
                }
                (Some(high), 0xdc00..=0xdfff) => String::from_utf16(&[high, unit]).ok()?,
                (_, unit) => String::from_utf16(&[unit]).ok()?,
            };
            text.chars().next().map(InputKind::Char)
        }
        WM_MOUSEMOVE => Some(InputKind::MouseMove {
            at: at_lparam(l, scale),
        }),
        WM_MOUSEWHEEL => {
            let mut point = POINT {
                x: i32::from(l.0 as i16),
                y: i32::from((l.0 >> 16) as i16),
            };
            // SAFETY: `point` is a valid in and out pointer.
            let _ = unsafe { ScreenToClient(hwnd, &mut point) };
            Some(InputKind::Wheel {
                at: at_pixels(point.x, point.y, scale),
                delta_x: 0.0,
                delta_y: f32::from((w.0 >> 16) as i16),
                modifiers: modifiers(),
            })
        }
        WM_SETFOCUS => Some(InputKind::Focus),
        WM_KILLFOCUS => Some(InputKind::Blur),
        _ => {
            let (button, down) = button_of(message)?;
            let at = at_lparam(l, scale);
            let modifiers = modifiers();
            Some(if down {
                InputKind::MouseDown {
                    at,
                    button,
                    modifiers,
                }
            } else {
                InputKind::MouseUp {
                    at,
                    button,
                    modifiers,
                }
            })
        }
    }
}

/// What both windows share: a renderer and the sizes it paints for.
struct Surface {
    hwnd: HWND,
    renderer: Box<dyn Renderer>,
    measure: Arc<dyn TextMeasurer>,
    theme: Theme,
    scale: f32,
    size: (u32, u32),
    cursor: MouseCursor,
    surrogate: Cell<Option<u16>>,
    /// True while Windows will tell this window that the pointer left it.
    tracking_leave: bool,
    /// The last paint problem, for the window to show. Painting goes on trying.
    problem: Option<String>,
}
impl Surface {
    fn dips(&self) -> Rect {
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(self.size.0 as f32 / self.scale),
            height: Dip(self.size.1 as f32 / self.scale),
        }
    }
    fn paint_view(&mut self, view: &dyn View) {
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: self.dips(),
            color: UiTokens::from_theme(&self.theme).background,
        });
        view.draw(&mut list, &self.theme, self.measure.as_ref());
        self.renderer.invalidate_all();
        let frame = self
            .renderer
            .begin_frame()
            .and_then(|()| self.renderer.draw(&list))
            .and_then(|()| self.renderer.end_frame());
        self.problem = frame.err().map(|e| format!("Painting failed: {e}"));
        // SAFETY: the window is alive while it gets messages.
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }
    /// Applies what a view asked for. Painting is the caller's, after all of them.
    fn apply(&mut self, requests: Vec<ViewRequest>) -> bool {
        let mut repaint = false;
        for request in requests {
            match request {
                ViewRequest::Invalidate(_) => repaint = true,
                ViewRequest::CaptureMouse(true) => {
                    // SAFETY: the window belongs to this thread.
                    unsafe { SetCapture(self.hwnd) };
                }
                ViewRequest::CaptureMouse(false) => {
                    // SAFETY: a plain call on this thread.
                    let _ = unsafe { ReleaseCapture() };
                }
                ViewRequest::SetCursor(cursor) => self.cursor = cursor,
                // The controls here ask for nothing else. No timer can start.
                ViewRequest::Run(_)
                | ViewRequest::RequestTimer(_)
                | ViewRequest::SetFocus(_)
                | ViewRequest::CancelComposition
                | ViewRequest::ContextMenu(_) => {}
            }
        }
        repaint
    }
    /// The pointer left: hover must clear, so a move far outside stands in for it. The
    /// first move in asks Windows to send that notice.
    fn pointer_notice(&mut self, message: u32) -> Option<InputKind> {
        match message {
            WM_MOUSEMOVE if !self.tracking_leave => {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: the struct has its size set. A failure only keeps a hover mark.
                self.tracking_leave = unsafe { TrackMouseEvent(&mut track) }.is_ok();
                None
            }
            WM_MOUSELEAVE => {
                self.tracking_leave = false;
                Some(InputKind::MouseMove {
                    at: Point {
                        x: Dip(-1.0e6),
                        y: Dip(-1.0e6),
                    },
                })
            }
            _ => None,
        }
    }
    fn set_dark_title(&self) {
        let dark = i32::from(self.theme.kind == qe_api::ThemeKind::Dark);
        // SAFETY: the attribute points to a valid integer for the call. A failure leaves
        // the title bar as it was, which is only a look.
        let _ = unsafe {
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const i32 as *const _,
                std::mem::size_of::<i32>() as u32,
            )
        };
    }
    fn resized(&mut self, width: u32, height: u32) {
        if self.size == (width, height) || width == 0 || height == 0 {
            return;
        }
        self.size = (width, height);
        if let Err(error) = self.renderer.resize(width, height) {
            self.problem = Some(format!("Painting failed: {error}"));
        }
    }
    fn present(&mut self, dc: HDC, rect: RECT) {
        let done = self.renderer.present_rect(
            dc.0,
            PixelRect {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
        );
        if let Err(error) = done {
            self.problem = Some(format!("Painting failed: {error}"));
        }
    }
}
fn window_dpi(hwnd: HWND) -> u32 {
    // SAFETY: a plain query. A dead handle gives 0.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}
/// Sets the painted area's size in pixels, keeping the top left corner. The height stays
/// inside the monitor's work area, so the footer never falls off a small screen.
fn set_client_size(hwnd: HWND, width: i32, height: i32) -> Result<(i32, i32), String> {
    let (mut outer, mut client) = (RECT::default(), RECT::default());
    // SAFETY: the out pointers are valid. A dead handle returns an error.
    unsafe {
        GetWindowRect(hwnd, &mut outer).map_err(|e| e.to_string())?;
        GetClientRect(hwnd, &mut client).map_err(|e| e.to_string())?;
    }
    let extra_w = (outer.right - outer.left) - (client.right - client.left);
    let extra_h = (outer.bottom - outer.top) - (client.bottom - client.top);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` has its size set, and the monitor comes from the window.
    let found =
        unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) }
            .as_bool();
    let (mut top, mut room) = (outer.top, i32::MAX);
    if found {
        room = info.rcWork.bottom - info.rcWork.top - extra_h;
        top = top
            .min(info.rcWork.bottom - (height.min(room) + extra_h))
            .max(info.rcWork.top);
    }
    let height = height.min(room);
    // SAFETY: a plain call on a window of this thread.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            outer.left,
            top,
            width + extra_w,
            height + extra_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| e.to_string())?;
    Ok((width, height))
}

struct EncodingDialog {
    dialog: DialogWindow,
    state: Rc<RefCell<EncodingState>>,
}
struct OpenState {
    panel: SettingsPanel,
    surface: Surface,
    store: SettingsStore,
    host: Rc<dyn SettingsHost>,
    encoding: Option<EncodingDialog>,
    presentation: Presentation,
    /// What was announced, for `SettingsWindow::take_announcements`.
    announcements: Vec<String>,
    /// The window, to keep its message handler in place while a dialog's loop runs.
    dialog: Weak<DialogWindow>,
    subscription: u64,
    write_wake: u64,
    /// The size last asked of Windows, which may have clamped it to a short screen.
    asked_size: Option<(i32, i32)>,
    resize_problem: Option<String>,
    /// An Alt key this window took: Windows sends a `WM_SYSCHAR` for it, which beeps if
    /// nothing answers.
    sys_char_owed: bool,
    font_dialog_open: bool,
}
impl OpenState {
    fn fit(&mut self) {
        let measure = self.surface.measure.clone();
        self.panel.fit_to_text(measure.as_ref());
        let scale = self.surface.scale;
        let want_w = (SETTINGS_WIDTH * scale).round() as i32;
        let want_h = (self.panel.wanted_height() * scale).round() as i32;
        if self.asked_size != Some((want_w, want_h)) {
            match set_client_size(self.surface.hwnd, want_w, want_h) {
                Ok((w, h)) => {
                    self.surface.resized(w as u32, h as u32);
                    self.asked_size = Some((want_w, want_h));
                    self.resize_problem = None;
                }
                Err(error) => self.resize_problem = Some(format!("Resizing failed: {error}")),
            }
        }
        let bounds = self.surface.dips();
        self.panel.layout(bounds, measure.as_ref());
    }
    /// Paints, and again if this paint changed what the footer must say. Each problem goes
    /// with the next paint that does not meet it.
    fn paint(&mut self) {
        self.fit();
        self.surface.paint_view(&self.panel);
        let problem = self
            .resize_problem
            .clone()
            .or_else(|| self.surface.problem.clone());
        if self.panel.set_surface_problem(problem) {
            self.fit();
            self.surface.paint_view(&self.panel);
        }
    }
    /// Takes what a screen reader should say. The caller says it with no borrow held,
    /// because the app's UI Automation code may ask for the widget tree during the call.
    fn take_announcement(&mut self) -> Option<String> {
        let announcement = self.panel.take_announcement()?;
        self.announcements.push(announcement.clone());
        Some(announcement)
    }
}

/// The open window's parts. Dropping this destroys the window.
struct OpenWindow {
    dialog: Rc<DialogWindow>,
    state: Rc<RefCell<OpenState>>,
    subscription: u64,
    write_wake: u64,
}

/// Settings, made once and opened from `CommandId::Settings`. Opening it again brings
/// the open one forward.
pub struct SettingsWindow {
    store: SettingsStore,
    host: Rc<dyn SettingsHost>,
    open: Option<OpenWindow>,
    opened: Cell<usize>,
    brought_forward: Cell<usize>,
}
impl SettingsWindow {
    /// Makes no window and reads nothing.
    pub fn new(store: SettingsStore, host: Rc<dyn SettingsHost>) -> Self {
        Self {
            store,
            host,
            open: None,
            opened: Cell::new(0),
            brought_forward: Cell::new(0),
        }
    }
    /// Opens the window, on screen and in front.
    pub fn open(&mut self) -> Result<(), String> {
        self.open_as(Presentation::Shown)
    }
    /// Opens the window, or brings the open one forward.
    pub fn open_as(&mut self, presentation: Presentation) -> Result<(), String> {
        self.forget_closed();
        if let Some(open) = &self.open {
            self.brought_forward.set(self.brought_forward.get() + 1);
            if presentation == Presentation::Shown {
                let hwnd = open.dialog.hwnd();
                // SAFETY: plain calls on a live window of this thread.
                unsafe {
                    if IsIconic(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            return Ok(());
        }
        self.open = Some(self.create(presentation)?);
        self.opened.set(self.opened.get() + 1);
        Ok(())
    }
    /// How many windows were made, and how many opens found one, for tests.
    pub fn open_counts(&self) -> (usize, usize) {
        (self.opened.get(), self.brought_forward.get())
    }
    pub fn is_open(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.dialog.is_alive())
    }
    pub fn hwnd(&self) -> Option<HWND> {
        self.open
            .as_ref()
            .filter(|o| o.dialog.is_alive())
            .map(|o| o.dialog.hwnd())
    }
    /// The window's widget tree, for the app's UI Automation provider.
    pub fn widgets(&self) -> Option<Widget> {
        let open = self.open.as_ref().filter(|o| o.dialog.is_alive())?;
        open.state.try_borrow().ok()?.panel.widgets()
    }
    /// What a screen reader should say. Reading clears it.
    pub fn take_announcements(&self) -> Vec<String> {
        self.open.as_ref().map_or_else(Vec::new, |open| {
            open.state.try_borrow_mut().map_or_else(
                |_| Vec::new(),
                |mut state| std::mem::take(&mut state.announcements),
            )
        })
    }
    /// Runs `read` on the panel, for tests.
    pub fn with_panel<T>(&self, read: impl FnOnce(&SettingsPanel) -> T) -> Option<T> {
        let open = self.open.as_ref().filter(|o| o.dialog.is_alive())?;
        Some(read(&open.state.try_borrow().ok()?.panel))
    }
    /// The painted area in device pixels, for tests.
    pub fn client_size(&self) -> Option<(i32, i32)> {
        self.open.as_ref()?.dialog.client_size().ok()
    }
    /// The Customize Encodings window, while it is open.
    pub fn encoding_panel_hwnd(&self) -> Option<HWND> {
        let open = self.open.as_ref()?;
        let state = open.state.try_borrow().ok()?;
        let dialog = &state.encoding.as_ref()?.dialog;
        dialog.is_alive().then(|| dialog.hwnd())
    }
    /// The Customize Encodings window's widget tree, while it is open.
    pub fn encoding_panel_widgets(&self) -> Option<Widget> {
        let open = self.open.as_ref()?;
        let state = open.state.try_borrow().ok()?;
        let encoding = state.encoding.as_ref()?;
        if !encoding.dialog.is_alive() {
            return None;
        }
        encoding.state.try_borrow().ok()?.panel.widgets()
    }
    /// Closes the window. Typed text is kept, as when the user closes it.
    pub fn close(&mut self) {
        if let Some(open) = self.open.take() {
            self.finish(open);
        }
    }
    fn finish(&self, open: OpenWindow) {
        self.store.unsubscribe(open.subscription);
        self.store.remove_write_wake(open.write_wake);
        drop(open);
    }
    fn forget_closed(&mut self) {
        if self.open.as_ref().is_some_and(|o| !o.dialog.is_alive())
            && let Some(open) = self.open.take()
        {
            self.finish(open);
        }
    }
    fn create(&self, presentation: Presentation) -> Result<OpenWindow, String> {
        let problem = |what: &str, why: String| format!("Settings could not open: {what}: {why}");
        let store = self.store.clone();
        let host = self.host.clone();
        // A write that ended with nothing polling shows now, in this window's footer.
        store.poll_writes();
        let mut panel = SettingsPanel::new(WidgetId(PANEL_ID), store.clone());
        let measure = host.measurer();
        panel.fit_to_text(measure.as_ref());
        // At 96 DPI first. The real size follows, once the window knows its monitor.
        let start_h = panel.wanted_height().round() as i32;
        let dialog = DialogWindow::create("Settings", SETTINGS_WIDTH as i32, start_h, false, false)
            .map_err(|e| problem("the window", e.to_string()))?;
        let dialog = Rc::new(dialog);
        let hwnd = dialog.hwnd();
        let dpi = window_dpi(hwnd);
        let scale = dpi as f32 / 96.0;
        let want = (
            (SETTINGS_WIDTH * scale).round() as i32,
            (panel.wanted_height() * scale).round() as i32,
        );
        let (width, height) =
            set_client_size(hwnd, want.0, want.1).map_err(|e| problem("its size", e))?;
        let renderer = host
            .make_renderer(width as u32, height as u32, dpi)
            .map_err(|e| problem("its renderer", e))?;
        let surface = Surface {
            hwnd,
            renderer,
            measure: measure.clone(),
            theme: host.theme(),
            scale,
            size: (width as u32, height as u32),
            cursor: MouseCursor::Arrow,
            surrogate: Cell::new(None),
            tracking_leave: false,
            problem: None,
        };
        surface.set_dark_title();
        panel.layout(surface.dips(), measure.as_ref());
        let state = Rc::new(RefCell::new(OpenState {
            panel,
            surface,
            store: store.clone(),
            host,
            encoding: None,
            presentation,
            announcements: Vec::new(),
            dialog: Rc::downgrade(&dialog),
            subscription: 0,
            write_wake: 0,
            asked_size: Some(want),
            resize_problem: None,
            sys_char_owed: false,
            font_dialog_open: false,
        }));

        let painter = state.clone();
        dialog.set_paint_handler(Box::new(move |dc, rect| {
            // A modal dialog's own loop can paint while a handler holds the state. The
            // next paint catches up, so this one is left out.
            if let Ok(mut state) = painter.try_borrow_mut() {
                state.surface.present(dc, rect);
            }
        }));
        dialog.set_message_handler(message_handler(Rc::downgrade(&state), self.host.clone()));
        let listener = Rc::downgrade(&state);
        let subscription = store.subscribe(move |change| {
            if let Some(state) = listener.upgrade()
                && let Ok(mut state) = state.try_borrow_mut()
            {
                state.panel.store_changed(change);
                state.paint();
            }
        });
        // The writer's thread only posts a message. A handle is a number to it.
        let target = hwnd.0 as isize;
        let write_wake = store.add_write_wake(Arc::new(move || {
            let hwnd = HWND(target as *mut _);
            // SAFETY: posting to a window that has gone returns an error, which means
            // that no one is left to tell.
            let _ = unsafe { PostMessageW(Some(hwnd), WRITE_DONE_MESSAGE, WPARAM(0), LPARAM(0)) };
        }));
        {
            let mut open = state.borrow_mut();
            open.subscription = subscription;
            open.write_wake = write_wake;
            open.paint();
        }
        if presentation == Presentation::Shown {
            // SAFETY: plain calls on a window of this thread.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
        }
        Ok(OpenWindow {
            dialog,
            state,
            subscription,
            write_wake,
        })
    }
}
impl Drop for SettingsWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// The Settings window's message handler. `DialogWindow` takes a handler out of its slot
/// while it runs, so a dialog's loop needs a fresh one in the slot: see `run_request`.
fn message_handler(state: Weak<RefCell<OpenState>>, host: Rc<dyn SettingsHost>) -> MessageHandler {
    Box::new(move |hwnd, message, w, l| {
        if let Some(answer) = host.window_message(hwnd, message, w, l) {
            return Some(answer);
        }
        let state = state.upgrade()?;
        settings_message(&state, hwnd, message, w, l)
    })
}
/// Moves the window where a `WM_DPICHANGED` says. Windows always sends a rectangle.
fn take_suggested_rect(hwnd: HWND, l: LPARAM) -> Result<(), String> {
    if l.0 == 0 {
        return Ok(());
    }
    // SAFETY: for this message Windows passes a valid RECT that lives through the call.
    let rect = unsafe { *(l.0 as *const RECT) };
    // SAFETY: a plain call on a window of this thread.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| format!("Resizing failed: {e}"))
}
/// One message for the Settings window. `Some` answers it, and `None` passes it to Windows.
fn settings_message(
    shared: &Rc<RefCell<OpenState>>,
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> Option<LRESULT> {
    let mut state = shared.try_borrow_mut().ok()?;
    match message {
        WRITE_DONE_MESSAGE => {
            let store = state.store.clone();
            // The store's event runs a listener that needs the state.
            drop(state);
            store.poll_writes();
            return Some(LRESULT(0));
        }
        WM_SIZE => {
            let (width, height) = ((l.0 & 0xffff) as u32, ((l.0 >> 16) & 0xffff) as u32);
            state.surface.resized(width, height);
            return None;
        }
        WM_DPICHANGED => {
            let dpi = (w.0 & 0xffff) as u32;
            state.surface.scale = dpi as f32 / 96.0;
            if let Err(error) = state.surface.renderer.set_dpi(dpi) {
                state
                    .panel
                    .show_problem(format!("Painting failed: {error}"));
            }
            if let Err(problem) = take_suggested_rect(hwnd, l) {
                state.panel.show_problem(problem);
            }
            // The rectangle has the place. `fit` sets the exact size from that corner.
            state.asked_size = None;
            state.paint();
            return Some(LRESULT(0));
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            state.surface.theme = state.host.theme();
            state.surface.set_dark_title();
            state.paint();
            return None;
        }
        WM_SETCURSOR if (l.0 & 0xffff) as u32 == HTCLIENT => {
            let id = match state.surface.cursor {
                MouseCursor::IBeam => IDC_IBEAM,
                MouseCursor::Hand => IDC_HAND,
                MouseCursor::SizeHorizontal => IDC_SIZEWE,
                MouseCursor::SizeVertical => IDC_SIZENS,
                MouseCursor::Wait => IDC_WAIT,
                MouseCursor::Arrow => IDC_ARROW,
            };
            // SAFETY: loads and sets a stock cursor.
            unsafe {
                if let Ok(cursor) = LoadCursorW(None, id) {
                    SetCursor(Some(cursor));
                }
            }
            return Some(LRESULT(1));
        }
        WM_DESTROY => {
            state.panel.commit_typed();
            // A closed window must hear no more: no paints, and no posts to a dead handle.
            state.store.unsubscribe(state.subscription);
            state.store.remove_write_wake(state.write_wake);
            return None;
        }
        WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN | WM_NCMBUTTONDOWN => {
            if state.panel.end_recording() {
                state.paint();
            }
            return None;
        }
        _ => {}
    }
    let scale = state.surface.scale;
    let notice = state.surface.pointer_notice(message);
    let kind = notice.or_else(|| {
        let surrogate = &state.surface.surrogate;
        input_from_message(hwnd, message, w, l, scale, surrogate)
    });
    let Some(kind) = kind else {
        // WM_SYSCHAR beeps if nothing answers it, while a recording takes the Alt chord.
        // The chord that ended a recording, and its repeats, count too.
        if message == WM_SYSCHAR
            && (state.sys_char_owed || state.panel.table().is_some_and(|t| t.is_recording()))
        {
            return Some(LRESULT(0));
        }
        return None;
    };
    let is_sys_key = matches!(message, WM_SYSKEYDOWN | WM_SYSKEYUP);
    let was_recording = state.panel.table().is_some_and(|t| t.is_recording());
    let mut ctx = ViewContext::default();
    // The panel's store calls run listeners. They skip a state in use, so this repaints.
    let event = InputEvent {
        time_us: time_now(),
        kind,
    };
    let handled = state.panel.handle(&event, &mut ctx);
    if message == WM_SYSKEYDOWN {
        state.sys_char_owed = was_recording || handled == qe_api::Handled::Yes;
    }
    let announcement = state.take_announcement();
    let host = state.host.clone();
    let repaint = state.surface.apply(ctx.requests);
    let request = state.panel.take_request();
    let closed = state.panel.closed();
    if repaint || request.is_some() || state.surface.problem.is_some() {
        state.paint();
    }
    drop(state);
    if let Some(announcement) = announcement {
        host.announce(hwnd, &announcement);
    }
    if let Some(request) = request {
        run_request(shared, request);
    }
    if closed {
        // SAFETY: posting to this window. The close comes after this message ends.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    if is_sys_key {
        // Alt+F4 and the rest go on to Windows, except while a key is being recorded.
        return (handled == qe_api::Handled::Yes || was_recording).then_some(LRESULT(0));
    }
    Some(LRESULT(0))
}
fn time_now() -> u64 {
    // SAFETY: a plain query. Its milliseconds wrap every 49 days, as double clicks allow.
    u64::from(unsafe { GetMessageTime() } as u32) * 1000
}
/// Runs a dialog with no borrow held, because its own message loop paints this window.
///
/// This runs inside a message handler, which `DialogWindow` has taken out of its slot, so
/// the dialog's loop would send this window's messages straight to Windows. A fresh handler
/// goes in the slot first. Writes, focus, DPI and theme messages then arrive as usual, and
/// `DialogWindow` drops the handler that is running when it returns.
fn run_request(shared: &Rc<RefCell<OpenState>>, request: PanelRequest) {
    match request {
        PanelRequest::ChooseFont => {
            let (host, hwnd, store, dialog, already_open) = {
                let mut state = shared.borrow_mut();
                let already_open = std::mem::replace(&mut state.font_dialog_open, true);
                (
                    state.host.clone(),
                    state.surface.hwnd,
                    state.store.clone(),
                    state.dialog.upgrade(),
                    already_open,
                )
            };
            if already_open {
                return;
            }
            if let Some(dialog) = dialog {
                dialog.set_message_handler(message_handler(Rc::downgrade(shared), host.clone()));
            }
            let current = store.font();
            let outcome = host.choose_font(Some(hwnd), &current);
            let mut problem = None;
            match outcome {
                Ok(Some(font)) => {
                    if let Err(refused) = store.set_font(&font) {
                        problem = Some(refused.0);
                    }
                }
                Ok(None) => {}
                Err(failed) => problem = Some(failed),
            }
            let mut state = shared.borrow_mut();
            state.font_dialog_open = false;
            if let Some(problem) = problem {
                state.panel.show_problem(problem);
            }
            state.panel.refresh();
            state.paint();
        }
        PanelRequest::CustomizeEncodings => {
            if let Err(problem) = open_encoding_panel(shared) {
                let mut state = shared.borrow_mut();
                state.panel.show_problem(problem);
                state.paint();
            }
        }
    }
}
/// The panel that `qe-ui` already has, in its own modal window. Changes apply as they
/// happen, as on the Mac.
fn open_encoding_panel(shared: &Rc<RefCell<OpenState>>) -> Result<(), String> {
    let (host, owner, store, scale, presentation) = {
        let state = shared.borrow();
        (
            state.host.clone(),
            state.surface.hwnd,
            state.store.clone(),
            state.surface.scale,
            state.presentation,
        )
    };
    let width = (ENCODING_PANEL_WIDTH * scale).round() as i32;
    let height = (ENCODING_PANEL_HEIGHT * scale).round() as i32;
    let problem = |e: String| format!("Customize Encodings could not open: {e}");
    let dialog = DialogWindow::create_with_owner(
        "Customize Encodings List",
        width,
        height,
        true,
        true,
        Some(owner),
    )
    .map_err(|e| problem(e.to_string()))?;
    let hwnd = dialog.hwnd();
    let dpi = window_dpi(hwnd);
    let (real_w, real_h) = dialog.client_size().map_err(|e| problem(e.to_string()))?;
    let renderer = host
        .make_renderer(real_w as u32, real_h as u32, dpi)
        .map_err(problem)?;
    let measure = host.measurer();
    let surface = Surface {
        hwnd,
        renderer,
        measure: measure.clone(),
        theme: host.theme(),
        scale,
        size: (real_w as u32, real_h as u32),
        cursor: MouseCursor::Arrow,
        surrogate: Cell::new(None),
        tracking_leave: false,
        problem: None,
    };
    let enabled = store.enabled_encodings();
    let mut panel = EncodingPanel::new(WidgetId(ENCODING_PANEL_ID), &enabled, surface.dips());
    panel.layout(surface.dips(), measure.as_ref());
    surface.set_dark_title();
    let inner = Rc::new(RefCell::new(EncodingState { panel, surface }));
    inner.borrow_mut().paint();
    let painter = inner.clone();
    dialog.set_paint_handler(Box::new(move |dc, rect| {
        if let Ok(mut state) = painter.try_borrow_mut() {
            state.surface.present(dc, rect);
        }
    }));
    let keep = inner.clone();
    let settings: Weak<RefCell<OpenState>> = Rc::downgrade(shared);
    let outer = host.clone();
    dialog.set_message_handler(Box::new(move |hwnd, message, w, l| {
        if let Some(answer) = outer.window_message(hwnd, message, w, l) {
            return Some(answer);
        }
        encoding_message(&keep, &settings, &outer, hwnd, message, w, l)
    }));
    if presentation == Presentation::Shown {
        // SAFETY: plain calls on a window of this thread.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
        }
    }
    shared.borrow_mut().encoding = Some(EncodingDialog {
        dialog,
        state: inner,
    });
    Ok(())
}
struct EncodingState {
    panel: EncodingPanel,
    surface: Surface,
}
impl EncodingState {
    fn paint(&mut self) {
        let measure = self.surface.measure.clone();
        let bounds = self.surface.dips();
        self.panel.layout(bounds, measure.as_ref());
        self.surface.paint_view(&self.panel);
    }
}
fn encoding_message(
    shared: &Rc<RefCell<EncodingState>>,
    settings: &Weak<RefCell<OpenState>>,
    host: &Rc<dyn SettingsHost>,
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> Option<LRESULT> {
    let mut state = shared.try_borrow_mut().ok()?;
    if message == WM_SIZE {
        let (width, height) = ((l.0 & 0xffff) as u32, ((l.0 >> 16) & 0xffff) as u32);
        state.surface.resized(width, height);
        state.paint();
        return None;
    }
    if message == WM_DPICHANGED {
        let dpi = (w.0 & 0xffff) as u32;
        state.surface.scale = dpi as f32 / 96.0;
        let width = (ENCODING_PANEL_WIDTH * state.surface.scale).round() as i32;
        let height = (ENCODING_PANEL_HEIGHT * state.surface.scale).round() as i32;
        // A failure keeps the old size, and the panel still works at it.
        let moved =
            take_suggested_rect(hwnd, l).and_then(|()| set_client_size(hwnd, width, height));
        let failed = state
            .surface
            .renderer
            .set_dpi(dpi)
            .map_err(|e| e.to_string());
        match moved {
            Ok((w, h)) => state.surface.resized(w as u32, h as u32),
            Err(error) => state.surface.problem = Some(error),
        }
        if let Err(error) = failed {
            state.surface.problem = Some(format!("Painting failed: {error}"));
        }
        state.paint();
        return Some(LRESULT(0));
    }
    if matches!(message, WM_SETTINGCHANGE | WM_THEMECHANGED) {
        state.surface.theme = host.theme();
        state.surface.set_dark_title();
        state.paint();
        return None;
    }
    let scale = state.surface.scale;
    let notice = state.surface.pointer_notice(message);
    let kind = notice.or_else(|| {
        let surrogate = &state.surface.surrogate;
        input_from_message(hwnd, message, w, l, scale, surrogate)
    })?;
    let before: Vec<TextEncoding> = state.panel.enabled_encodings();
    let mut ctx = ViewContext::default();
    let event = InputEvent {
        time_us: time_now(),
        kind,
    };
    state.panel.handle(&event, &mut ctx);
    if state.surface.apply(ctx.requests) {
        state.paint();
    }
    let after = state.panel.enabled_encodings();
    let closed = state.panel.closed();
    drop(state);
    if before != after
        && let Some(settings) = settings.upgrade()
    {
        // The store's listener repaints Settings with the new lists.
        let store = settings.borrow().store.clone();
        if let Err(refused) = store.set_enabled_encodings(&after) {
            let mut settings = settings.borrow_mut();
            settings.panel.show_problem(refused.0);
            settings.paint();
        }
    }
    if closed {
        // SAFETY: posting to this window. The close comes after this message ends.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    if matches!(message, WM_SYSKEYDOWN | WM_SYSKEYUP) {
        return None;
    }
    Some(LRESULT(0))
}
