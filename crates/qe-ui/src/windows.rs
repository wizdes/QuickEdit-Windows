use crate::{ClipboardAccess, MenuView, SystemPalette, place_popup};
use qe_api::{Color, Dip, DisplayList, Rect, RenderError, Renderer};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{
    COLORREF, E_FAIL, E_INVALIDARG, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT,
    RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, GetMonitorInfoW, HDC, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, PAINTSTRUCT,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, GetFocus, IsWindowEnabled, ReleaseCapture, SetCapture, SetFocus,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// Posted to the owner when a popup goes away on its own. `wParam` is the `HWND` of
/// the popup and `lParam` is a `PopupDismissed` value as a number.
pub const POPUP_DISMISSED_MESSAGE: u32 = WM_APP + 0x51;
/// Posted to the owner when a dialog's own close button closed it. `wParam` is the
/// `HWND` of the dialog.
pub const DIALOG_CLOSED_MESSAGE: u32 = WM_APP + 0x52;

/// Runs for a message that the wrapper does not answer itself, such as a key or a
/// mouse message. `Some` is the answer. `None` passes it on to Windows.
pub type MessageHandler = Box<dyn FnMut(HWND, u32, WPARAM, LPARAM) -> Option<LRESULT>>;
/// Runs on `WM_PAINT` with the window's device context and the area to paint. A
/// host with a `qe_render::BitmapRenderer` calls `present_rect_to_dc` here.
pub type PaintHandler = Box<dyn FnMut(HDC, RECT)>;

#[derive(Default)]
struct Handlers {
    paint: RefCell<Option<PaintHandler>>,
    message: RefCell<Option<MessageHandler>>,
    shut: Cell<bool>,
}
impl Handlers {
    fn paint(&self, hwnd: HWND) {
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: `hwnd` is the window that gets this WM_PAINT.
        let dc = unsafe { BeginPaint(hwnd, &mut paint) };
        // The handler is taken out, so that a message it causes finds none running.
        let handler = self.paint.borrow_mut().take();
        if let Some(mut handler) = handler {
            handler(dc, paint.rcPaint);
            self.put_back(&self.paint, handler);
        }
        // SAFETY: pairs with BeginPaint above.
        let _ = unsafe { EndPaint(hwnd, &paint) };
    }
    fn message(&self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        let handler = self.message.borrow_mut().take();
        let mut handler = handler?;
        let answer = handler(hwnd, msg, w, l);
        self.put_back(&self.message, handler);
        answer
    }
    /// Returns a handler to its slot after it ran. The handler may have destroyed the
    /// window or set a new handler, and in those cases it is dropped here instead.
    fn put_back<T>(&self, slot: &RefCell<Option<T>>, handler: T) {
        if self.shut.get() || slot.borrow().is_some() {
            drop(handler);
        } else {
            *slot.borrow_mut() = Some(handler);
        }
    }
    /// The window is going away, so its handlers go too. Each slot is emptied first,
    /// because dropping a handler can send more messages here.
    fn shut(&self) {
        self.shut.set(true);
        let paint = self.paint.borrow_mut().take();
        let message = self.message.borrow_mut().take();
        drop(paint);
        drop(message);
    }
}

fn module() -> windows::core::Result<windows::Win32::Foundation::HINSTANCE> {
    // SAFETY: a null name asks for this process's own module.
    unsafe { GetModuleHandleW(None) }.map(Into::into)
}
fn register_class(
    slot: &OnceLock<Result<(), String>>,
    name: PCWSTR,
    procedure: WNDPROC,
    style: WNDCLASS_STYLES,
) -> windows::core::Result<()> {
    let outcome = slot.get_or_init(|| {
        let instance = module().map_err(|e| e.to_string())?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style,
            lpfnWndProc: procedure,
            hInstance: instance,
            // SAFETY: loads a stock cursor.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.map_err(|e| e.to_string())?,
            lpszClassName: name,
            ..Default::default()
        };
        // SAFETY: the class struct is complete, and its name is a static string.
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(windows::core::Error::from_thread().to_string());
        }
        Ok(())
    });
    outcome
        .clone()
        .map_err(|message| windows::core::Error::new(E_FAIL, format!("window class: {message}")))
}

/// Why a popup went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
pub enum PopupDismissed {
    /// A mouse press landed outside the popup.
    OutsideClick = 1,
    /// Another program became the active one.
    Deactivated = 2,
    /// Something else took the mouse capture.
    CaptureLost = 3,
    /// The owner's code called `dismiss`.
    Caller = 4,
}

/// One group of popups: a menu and its open submenus. They go away together, and the
/// first popup holds the mouse for all of them. A second popup taking the capture would
/// make the first close itself, so a level never takes it.
struct PopupGroup {
    owner: HWND,
    root: Cell<HWND>,
    members: RefCell<Vec<Weak<PopupState>>>,
    restore_focus: Cell<Option<HWND>>,
    armed: Cell<bool>,
    dismissed: Cell<Option<PopupDismissed>>,
    problems: RefCell<Vec<String>>,
}
struct PopupState {
    hwnd: Cell<HWND>,
    group: Rc<PopupGroup>,
    is_root: bool,
    shown: Cell<bool>,
    alive: Cell<bool>,
    adopted: Cell<bool>,
    handlers: Handlers,
}
thread_local! {
    static WATCHED_GROUPS: RefCell<Vec<Rc<PopupGroup>>> = const { RefCell::new(Vec::new()) };
    static FOREGROUND_HOOK: Cell<Option<HWINEVENTHOOK>> = const { Cell::new(None) };
    static MODAL_OWNERS: RefCell<HashMap<isize, ModalOwner>> = RefCell::new(HashMap::new());
    static CLIPBOARD_OWNER: Cell<Option<HWND>> = const { Cell::new(None) };
}
impl PopupGroup {
    fn members(&self) -> Vec<Rc<PopupState>> {
        self.members
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
    fn holds(&self, hwnd: HWND) -> bool {
        self.members().iter().any(|m| m.hwnd.get() == hwnd)
    }
    /// True when a screen point is on any showing level of this group.
    fn on_a_level(&self, point: POINT) -> bool {
        self.members().iter().any(|member| {
            let mut rect = RECT::default();
            // SAFETY: `rect` is a valid out pointer. A dead handle returns an error.
            member.shown.get()
                && unsafe { GetWindowRect(member.hwnd.get(), &mut rect) }.is_ok()
                && point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
        })
    }
    fn note(&self, problem: String) {
        self.problems.borrow_mut().push(problem);
    }
    /// Hides every level, lets go of the mouse, gives focus back, and tells the owner.
    fn dismiss(self: &Rc<Self>, reason: PopupDismissed) {
        // First of all, because releasing the capture sends WM_CAPTURECHANGED here.
        if !self.armed.replace(false) {
            return;
        }
        self.dismissed.set(Some(reason));
        stop_watching(self);
        let root = self.root.get();
        // SAFETY: these calls take any handle, and they run on the window's thread.
        unsafe {
            if GetCapture() == root {
                let _ = ReleaseCapture();
            }
            for member in self.members() {
                if member.shown.replace(false) && member.alive.get() {
                    let _ = ShowWindow(member.hwnd.get(), SW_HIDE);
                }
            }
            if let Some(focus) = self.restore_focus.take()
                && IsWindow(Some(focus)).as_bool()
                && GetFocus() != focus
            {
                let _ = SetFocus(Some(focus));
            }
            let _ = PostMessageW(
                Some(self.owner),
                POPUP_DISMISSED_MESSAGE,
                WPARAM(root.0 as usize),
                LPARAM(reason as isize),
            );
        }
    }
}
fn stop_watching(group: &Rc<PopupGroup>) {
    let hook = WATCHED_GROUPS.with(|list| {
        let mut list = list.borrow_mut();
        list.retain(|g| !Rc::ptr_eq(g, group));
        if list.is_empty() {
            FOREGROUND_HOOK.take()
        } else {
            None
        }
    });
    if let Some(hook) = hook {
        // SAFETY: the hook came from SetWinEventHook on this thread.
        let _ = unsafe { UnhookWinEvent(hook) };
    }
}
/// Dismisses each watched group, unless its owner or one of its levels is in front.
fn foreground_is(foreground: HWND) {
    let groups: Vec<Rc<PopupGroup>> = WATCHED_GROUPS.with(|list| list.borrow().clone());
    for group in groups {
        if group.owner != foreground && !group.holds(foreground) {
            group.dismiss(PopupDismissed::Deactivated);
        }
    }
}
unsafe extern "system" fn foreground_changed(
    _: HWINEVENTHOOK,
    _: u32,
    foreground: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    foreground_is(foreground);
}
/// What the system's foreground notice does. A test with no desktop cannot make
/// another program active, so it calls this.
#[doc(hidden)]
pub fn foreground_changed_for_test(foreground: HWND) {
    foreground_is(foreground);
}
/// True while the system's foreground notice is hooked.
#[doc(hidden)]
pub fn foreground_watch_active() -> bool {
    let hook = FOREGROUND_HOOK.take();
    FOREGROUND_HOOK.set(hook);
    hook.is_some()
}

/// A strong reference to a window's state for the length of one message. A handler
/// may drop the wrapper that owns the window. The state then lasts until this message
/// is finished, so nothing below writes into freed memory.
///
/// # Safety
/// The window's user data must be null, or come from `Rc::into_raw` of a `T`.
unsafe fn state_for_message<T>(hwnd: HWND) -> Option<Rc<T>> {
    // SAFETY: as this function's contract says.
    unsafe {
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const T;
        if pointer.is_null() {
            return None;
        }
        Rc::increment_strong_count(pointer);
        Some(Rc::from_raw(pointer))
    }
}
/// Ends a window's own reference to its state, on `WM_NCDESTROY`.
///
/// # Safety
/// As for `state_for_message`. Runs once for each window.
unsafe fn release_window_state<T>(hwnd: HWND) {
    // SAFETY: as this function's contract says.
    unsafe {
        let pointer = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *const T;
        if !pointer.is_null() {
            drop(Rc::from_raw(pointer));
        }
    }
}
unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: Windows passes valid arguments. The user data is set on WM_NCCREATE from
    // `Rc::into_raw`, and is released once on WM_NCDESTROY.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(l.0 as *const CREATESTRUCTW);
            let pointer = create.lpCreateParams as *const PopupState;
            (*pointer).adopted.set(true);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, pointer as isize);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let Some(state) = state_for_message::<PopupState>(hwnd) else {
            return DefWindowProcW(hwnd, msg, w, l);
        };
        match msg {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                state.handlers.paint(hwnd);
                return LRESULT(0);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                // With the capture, a press anywhere arrives here. It counts as inside
                // when it lands on any level of the group.
                let mut point = POINT {
                    x: l.0 as i16 as i32,
                    y: (l.0 >> 16) as i16 as i32,
                };
                let _ = ClientToScreen(hwnd, &mut point);
                if !state.group.on_a_level(point) {
                    state.group.dismiss(PopupDismissed::OutsideClick);
                    return LRESULT(0);
                }
            }
            WM_CAPTURECHANGED => {
                if state.is_root && !state.group.holds(HWND(l.0 as *mut _)) {
                    state.group.dismiss(PopupDismissed::CaptureLost);
                }
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                if state.is_root {
                    stop_watching(&state.group);
                    state.group.armed.set(false);
                }
                state.alive.set(false);
                state.shown.set(false);
                state.handlers.shut();
                release_window_state::<PopupState>(hwnd);
                return DefWindowProcW(hwnd, msg, w, l);
            }
            _ => {}
        }
        if let Some(answer) = state.handlers.message(hwnd, msg, w, l) {
            return answer;
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}

/// Owns a real top-level popup with its own window procedure.
///
/// It belongs to an owner window, stays above it, shows without taking activation
/// and has no taskbar entry. An outside press, another program becoming active, or
/// a lost capture hides it, gives focus back, and posts `POPUP_DISMISSED_MESSAGE`.
///
/// A submenu is a second popup from `create_level`, which joins this one's group. The
/// first popup keeps the mouse for all of them, and they are dismissed as one.
pub struct PopupWindow {
    state: Rc<PopupState>,
    decoration_error: Option<windows::core::Error>,
}
impl PopupWindow {
    /// Placed next to `anchor` inside the work area of the anchor's monitor.
    pub fn create_anchored(
        owner: HWND,
        anchor: RECT,
        width: i32,
        height: i32,
        submenu: bool,
    ) -> windows::core::Result<Self> {
        // SAFETY: a plain query with a valid rectangle.
        let monitor = unsafe { MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` has its size set, and the monitor came from the call above.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return Err(windows::core::Error::from_thread());
        }
        let work = info.rcWork;
        let placement = place_popup(
            Rect {
                x: Dip(anchor.left as f32),
                y: Dip(anchor.top as f32),
                width: Dip((anchor.right - anchor.left) as f32),
                height: Dip((anchor.bottom - anchor.top) as f32),
            },
            (width as f32, height as f32),
            Rect {
                x: Dip(work.left as f32),
                y: Dip(work.top as f32),
                width: Dip((work.right - work.left) as f32),
                height: Dip((work.bottom - work.top) as f32),
            },
            submenu,
        );
        Self::create(
            owner,
            placement.x.0 as i32,
            placement.y.0 as i32,
            placement.width.0 as i32,
            placement.height.0 as i32,
        )
    }
    /// Creates the first popup of a group, hidden. `show` puts it on screen.
    pub fn create(
        owner: HWND,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> windows::core::Result<Self> {
        let group = Rc::new(PopupGroup {
            owner,
            root: Cell::new(HWND::default()),
            members: RefCell::new(Vec::new()),
            restore_focus: Cell::new(None),
            armed: Cell::new(false),
            dismissed: Cell::new(None),
            problems: RefCell::new(Vec::new()),
        });
        Self::create_in(
            group,
            owner,
            true,
            RECT {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            },
        )
    }
    /// Creates another level of this popup's group, hidden, owned by this popup.
    pub fn create_level(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> windows::core::Result<Self> {
        Self::create_in(
            self.state.group.clone(),
            self.state.hwnd.get(),
            false,
            RECT {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            },
        )
    }
    fn create_in(
        group: Rc<PopupGroup>,
        parent: HWND,
        is_root: bool,
        at: RECT,
    ) -> windows::core::Result<Self> {
        static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
        let class = w!("QuickEditPopup");
        register_class(&CLASS, class, Some(popup_proc), CS_DROPSHADOW)?;
        let state = Rc::new(PopupState {
            hwnd: Cell::new(HWND::default()),
            group: group.clone(),
            is_root,
            shown: Cell::new(false),
            alive: Cell::new(true),
            adopted: Cell::new(false),
            handlers: Handlers::default(),
        });
        let raw = Rc::into_raw(state.clone());
        // SAFETY: the class is registered. The window takes over one reference from
        // WM_NCCREATE on, and gives it back on WM_NCDESTROY.
        let created = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!(""),
                WS_POPUP,
                at.left,
                at.top,
                at.right - at.left,
                at.bottom - at.top,
                Some(parent),
                None,
                Some(module()?),
                Some(raw as _),
            )
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(error) => {
                if !state.adopted.get() {
                    // SAFETY: no window took the reference, so it is still ours.
                    drop(unsafe { Rc::from_raw(raw) });
                }
                return Err(error);
            }
        };
        state.hwnd.set(hwnd);
        if is_root {
            group.root.set(hwnd);
        }
        group.members.borrow_mut().push(Rc::downgrade(&state));
        let preference = DWMWCP_ROUND.0;
        // SAFETY: the attribute points to a valid integer and the HWND was just created.
        let decoration_error = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as _,
                std::mem::size_of_val(&preference) as u32,
            )
        }
        .err();
        Ok(Self {
            state,
            decoration_error,
        })
    }
    pub fn hwnd(&self) -> HWND {
        self.state.hwnd.get()
    }
    pub fn owner(&self) -> HWND {
        self.state.group.owner
    }
    /// The DWM error, if the popup could not ask for rounded corners. The popup still
    /// works, with square corners.
    pub fn decoration_error(&self) -> Option<&windows::core::Error> {
        self.decoration_error.as_ref()
    }
    /// False after the window is gone, for example because its owner was destroyed.
    pub fn is_alive(&self) -> bool {
        self.state.alive.get()
    }
    /// Why the group went away. `None` while it is open.
    pub fn dismissed(&self) -> Option<PopupDismissed> {
        self.state.group.dismissed.get()
    }
    /// What went wrong while showing the group, in words: a capture or a foreground
    /// watch that Windows refused. The popup still shows, but it can stay open too long.
    pub fn problems(&self) -> Vec<String> {
        self.state.group.problems.borrow().clone()
    }
    pub fn set_paint_handler(&self, handler: PaintHandler) {
        *self.state.handlers.paint.borrow_mut() = Some(handler);
    }
    pub fn set_message_handler(&self, handler: MessageHandler) {
        *self.state.handlers.message.borrow_mut() = Some(handler);
    }
    /// Shows the popup without activating it. The first popup also takes the mouse so
    /// that an outside press reaches it. `Err` names what Windows refused.
    pub fn show(&self) -> Result<(), String> {
        self.begin(true)
    }
    /// Does all that `show` does but make the window visible, for tests on hidden
    /// windows: the capture, the focus record and the foreground watch are all real.
    #[doc(hidden)]
    pub fn begin_hidden(&self) -> Result<(), String> {
        self.begin(false)
    }
    fn begin(&self, on_screen: bool) -> Result<(), String> {
        let state = &*self.state;
        if !state.alive.get() || state.shown.replace(true) {
            return Ok(());
        }
        let hwnd = state.hwnd.get();
        if on_screen {
            // SAFETY: the window is alive, and this runs on its thread.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        if !state.is_root {
            return Ok(());
        }
        let group = &state.group;
        group.dismissed.set(None);
        group.armed.set(true);
        let mut problem = None;
        // SAFETY: the window is alive, and these calls run on its thread.
        unsafe {
            let focus = GetFocus();
            group
                .restore_focus
                .set((!focus.0.is_null()).then_some(focus));
            SetCapture(hwnd);
            if GetCapture() != hwnd {
                problem = Some(
                    "Windows did not give the popup the mouse, so a press outside it may not \
                     close it"
                        .to_owned(),
                );
            }
        }
        WATCHED_GROUPS.with(|list| list.borrow_mut().push(group.clone()));
        if FOREGROUND_HOOK.get().is_none() {
            // SAFETY: the callback stays valid for the life of the process.
            let hook = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(foreground_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if hook.is_invalid() {
                problem = Some(
                    "Windows refused the foreground watch, so the popup may stay open when \
                     another program becomes active"
                        .to_owned(),
                );
            } else {
                FOREGROUND_HOOK.set(Some(hook));
            }
        }
        match problem {
            Some(problem) => {
                group.note(problem.clone());
                Err(problem)
            }
            None => Ok(()),
        }
    }
    /// Hides this level alone, as when a submenu closes and its parent stays.
    pub fn hide_level(&self) {
        if self.state.shown.replace(false) {
            // SAFETY: the call takes any handle.
            unsafe {
                let _ = ShowWindow(self.state.hwnd.get(), SW_HIDE);
            }
        }
    }
    /// Moves and sizes this level, with no change to activation or stacking.
    pub fn move_to(&self, x: i32, y: i32, width: i32, height: i32) -> windows::core::Result<()> {
        // SAFETY: the call takes any handle and reports a dead one.
        unsafe {
            SetWindowPos(
                self.state.hwnd.get(),
                None,
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
    }
    /// Hides all levels of the group and gives focus back.
    pub fn dismiss(&self) {
        self.state.group.dismiss(PopupDismissed::Caller);
    }
    pub fn bounds(&self) -> windows::core::Result<RECT> {
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid out pointer. A dead handle returns an error.
        unsafe { GetWindowRect(self.state.hwnd.get(), &mut rect)? };
        Ok(rect)
    }
    /// Draws into the renderer and asks this window to repaint. The paint handler
    /// then copies the pixels to the window.
    pub fn paint(
        &self,
        renderer: &mut dyn Renderer,
        list: &DisplayList,
    ) -> Result<(), RenderError> {
        renderer.begin_frame()?;
        renderer.draw(list)?;
        renderer.end_frame()?;
        // SAFETY: an invalid handle only makes the call return false.
        let ok = unsafe { InvalidateRect(Some(self.state.hwnd.get()), None, false) };
        if !ok.as_bool() {
            return Err(RenderError::new(
                "InvalidateRect failed on the popup window",
            ));
        }
        Ok(())
    }
}
impl Drop for PopupWindow {
    fn drop(&mut self) {
        if self.state.alive.get() {
            // SAFETY: the window is still alive. WM_NCDESTROY ends its reference, and
            // this wrapper's own reference keeps the state until this function ends.
            unsafe {
                let _ = DestroyWindow(self.state.hwnd.get());
            }
        }
    }
}

/// The windows of a caret list: one popup for each open level of a `MenuView`. Call
/// `sync` after each event. The first window keeps the mouse throughout, so opening or
/// closing a submenu never dismisses the list.
pub struct MenuWindows {
    owner: HWND,
    scale: f32,
    on_screen: bool,
    levels: Vec<PopupWindow>,
}
impl MenuWindows {
    /// `scale` is device pixels per dip.
    pub fn new(owner: HWND, scale: f32) -> Self {
        Self {
            owner,
            scale,
            on_screen: true,
            levels: Vec::new(),
        }
    }
    /// The same windows with none of them made visible, for tests.
    #[doc(hidden)]
    pub fn new_hidden(owner: HWND, scale: f32) -> Self {
        Self {
            on_screen: false,
            ..Self::new(owner, scale)
        }
    }
    pub fn levels(&self) -> &[PopupWindow] {
        &self.levels
    }
    /// Why the list went away. `None` while it is open.
    pub fn dismissed(&self) -> Option<PopupDismissed> {
        self.levels.first().and_then(PopupWindow::dismissed)
    }
    /// Makes the windows match the menu's open levels. A closed menu hides them all.
    /// `Err` names a window that could not be made, or a refusal from `show`.
    pub fn sync(&mut self, menu: &MenuView) -> Result<(), String> {
        if menu.closed {
            if let Some(root) = self.levels.first() {
                root.dismiss();
            }
            return Ok(());
        }
        if self.dismissed().is_some() {
            return Ok(());
        }
        let rects = menu.levels();
        // Deepest first, so that a parent never goes while its child is listed.
        self.levels.truncate(rects.len());
        let mut problem = None;
        for (depth, r) in rects.iter().enumerate() {
            let (x, y, w, h) = (
                (r.x.0 * self.scale).round() as i32,
                (r.y.0 * self.scale).round() as i32,
                (r.width.0 * self.scale).round() as i32,
                (r.height.0 * self.scale).round() as i32,
            );
            if let Some(level) = self.levels.get(depth) {
                level
                    .move_to(x, y, w, h)
                    .map_err(|e| format!("moving a menu level: {e}"))?;
            } else {
                let made = match self.levels.last() {
                    Some(parent) => parent.create_level(x, y, w, h),
                    None => PopupWindow::create(self.owner, x, y, w, h),
                }
                .map_err(|e| format!("making a menu level: {e}"))?;
                let shown = if self.on_screen {
                    made.show()
                } else {
                    made.begin_hidden()
                };
                if let Err(refused) = shown {
                    problem = Some(refused);
                }
                self.levels.push(made);
            }
        }
        problem.map_or(Ok(()), Err)
    }
    /// A mouse message's place on the first window, in the menu's own dips. Mouse
    /// messages for every level arrive there, because it holds the capture.
    pub fn pointer(&self, l: LPARAM) -> Option<qe_api::Point> {
        let root = self.levels.first()?;
        let mut point = POINT {
            x: l.0 as i16 as i32,
            y: (l.0 >> 16) as i16 as i32,
        };
        // SAFETY: `point` is a valid in and out pointer.
        unsafe { ClientToScreen(root.hwnd(), &mut point) }
            .as_bool()
            .then(|| qe_api::Point {
                x: Dip(point.x as f32 / self.scale),
                y: Dip(point.y as f32 / self.scale),
            })
    }
}

struct ModalOwner {
    count: usize,
    was_disabled: bool,
}
/// Disables an owner for one more modal dialog. The first one records whether the
/// owner was already disabled.
fn acquire_modal(owner: HWND) {
    // SAFETY: a plain query.
    let enabled = unsafe { IsWindowEnabled(owner) }.as_bool();
    MODAL_OWNERS.with(|map| {
        let mut map = map.borrow_mut();
        let entry = map.entry(owner.0 as isize).or_insert(ModalOwner {
            count: 0,
            was_disabled: !enabled,
        });
        entry.count += 1;
    });
    // No borrow is held here: EnableWindow sends WM_ENABLE, and its handler may open
    // another modal dialog.
    // SAFETY: EnableWindow takes any window handle on this thread.
    unsafe {
        let _ = EnableWindow(owner, false);
    }
}
/// Ends one modal dialog on an owner. Only the last one restores the owner, and
/// only if the owner was enabled before the first.
fn release_modal(owner: HWND) {
    let restore = MODAL_OWNERS.with(|map| {
        let mut map = map.borrow_mut();
        let key = owner.0 as isize;
        let entry = map.get_mut(&key)?;
        entry.count -= 1;
        if entry.count > 0 {
            return None;
        }
        let restore = !entry.was_disabled;
        map.remove(&key);
        Some(restore)
    });
    // No borrow is held here, for the same reason as in `acquire_modal`.
    // SAFETY: the check covers an owner that is already gone.
    unsafe {
        if restore == Some(true) && IsWindow(Some(owner)).as_bool() {
            let _ = EnableWindow(owner, true);
        }
    }
}

struct DialogState {
    owner: Option<HWND>,
    modal: bool,
    holds_owner: Cell<bool>,
    alive: Cell<bool>,
    adopted: Cell<bool>,
    closed_by_user: Cell<bool>,
    handlers: Handlers,
}
impl DialogState {
    /// Gives the owner back. This comes before the dialog is destroyed, so that
    /// Windows activates the owner and not some other program.
    fn let_go_of_owner(&self) {
        if self.holds_owner.replace(false)
            && let Some(owner) = self.owner
        {
            release_modal(owner);
        }
    }
}
unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: as in `popup_proc`.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(l.0 as *const CREATESTRUCTW);
            let pointer = create.lpCreateParams as *const DialogState;
            (*pointer).adopted.set(true);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, pointer as isize);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let Some(state) = state_for_message::<DialogState>(hwnd) else {
            return DefWindowProcW(hwnd, msg, w, l);
        };
        match msg {
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                state.handlers.paint(hwnd);
                return LRESULT(0);
            }
            WM_CLOSE => {
                state.closed_by_user.set(true);
                state.let_go_of_owner();
                if let Some(owner) = state.owner {
                    let _ = PostMessageW(
                        Some(owner),
                        DIALOG_CLOSED_MESSAGE,
                        WPARAM(hwnd.0 as usize),
                        LPARAM(0),
                    );
                }
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                state.let_go_of_owner();
                state.alive.set(false);
                state.handlers.shut();
                release_window_state::<DialogState>(hwnd);
                return DefWindowProcW(hwnd, msg, w, l);
            }
            _ => {}
        }
        if let Some(answer) = state.handlers.message(hwnd, msg, w, l) {
            return answer;
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}

/// Owns a native-titled top-level dialog. The caller routes keys and focus.
///
/// The window's own close button closes it and gives a modal owner back. The caller
/// learns of it from `closed_by_user` and from `DIALOG_CLOSED_MESSAGE`.
pub struct DialogWindow {
    hwnd: HWND,
    state: Rc<DialogState>,
    resizable: bool,
    theme_note: Option<String>,
    decoration_error: Option<windows::core::Error>,
}
impl DialogWindow {
    /// A dialog with no owner. It cannot be modal, because nothing would be disabled.
    /// `width` and `height` are the size of the painted area, as in `create_with_owner`.
    pub fn create(
        title: &str,
        width: i32,
        height: i32,
        modal: bool,
        resizable: bool,
    ) -> windows::core::Result<Self> {
        Self::create_with_owner(title, width, height, modal, resizable, None)
    }
    /// Creates the dialog hidden, centered on its owner or on the primary monitor, and
    /// kept on that monitor's work area. `width` and `height` are the painted area in
    /// device pixels, not the outer window, so a size such as row I39's 336 by 187 is
    /// the size the user gets. The caller shows it.
    pub fn create_with_owner(
        title: &str,
        width: i32,
        height: i32,
        modal: bool,
        resizable: bool,
        owner: Option<HWND>,
    ) -> windows::core::Result<Self> {
        if modal && owner.is_none() {
            return Err(windows::core::Error::new(
                E_INVALIDARG,
                "a modal dialog needs an owner window to disable",
            ));
        }
        let (dark, theme_note) = match apps_use_light_theme() {
            Ok(light) => (!light, None),
            Err(note) => (false, Some(note)),
        };
        static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
        let class = w!("QuickEditDialog");
        register_class(&CLASS, class, Some(dialog_proc), WNDCLASS_STYLES(0))?;
        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let style = WS_CAPTION
            | WS_SYSMENU
            | if resizable {
                WS_THICKFRAME
            } else {
                WINDOW_STYLE(0)
            };
        let state = Rc::new(DialogState {
            owner,
            modal,
            holds_owner: Cell::new(false),
            alive: Cell::new(true),
            adopted: Cell::new(false),
            closed_by_user: Cell::new(false),
            handlers: Handlers::default(),
        });
        let mut outer = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: `outer` is a valid in and out pointer.
        unsafe { AdjustWindowRectEx(&mut outer, style, false, WINDOW_EX_STYLE(0))? };
        let (outer_w, outer_h) = (outer.right - outer.left, outer.bottom - outer.top);
        let (x, y) = centered_place(owner, outer_w, outer_h);
        let raw = Rc::into_raw(state.clone());
        // SAFETY: the class is registered. The window takes over one reference from
        // WM_NCCREATE on, and gives it back on WM_NCDESTROY.
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(wide.as_ptr()),
                style,
                x,
                y,
                outer_w,
                outer_h,
                owner,
                None,
                Some(module()?),
                Some(raw as _),
            )
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(error) => {
                if !state.adopted.get() {
                    // SAFETY: no window took the reference, so it is still ours.
                    drop(unsafe { Rc::from_raw(raw) });
                }
                return Err(error);
            }
        };
        let dark_value = i32::from(dark);
        // SAFETY: the attribute points to a valid integer and the HWND was just created.
        let decoration_error = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_value as *const _ as _,
                std::mem::size_of_val(&dark_value) as u32,
            )
        }
        .err();
        if modal && let Some(owner) = owner {
            acquire_modal(owner);
            state.holds_owner.set(true);
        }
        Ok(Self {
            hwnd,
            state,
            resizable,
            theme_note,
            decoration_error,
        })
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
    /// The size of the painted area in device pixels.
    pub fn client_size(&self) -> windows::core::Result<(i32, i32)> {
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid out pointer. A dead handle returns an error.
        unsafe { GetClientRect(self.hwnd, &mut rect)? };
        Ok((rect.right - rect.left, rect.bottom - rect.top))
    }
    /// Fixed when the dialog is made, so that the owner is always given back.
    pub fn modal(&self) -> bool {
        self.state.modal
    }
    pub fn resizable(&self) -> bool {
        self.resizable
    }
    /// False after the window is gone.
    pub fn is_alive(&self) -> bool {
        self.state.alive.get()
    }
    /// True after the window's own close button closed it.
    pub fn closed_by_user(&self) -> bool {
        self.state.closed_by_user.get()
    }
    /// Why the dialog could not read the app theme. Its title bar is then light.
    pub fn theme_note(&self) -> Option<&str> {
        self.theme_note.as_deref()
    }
    /// The DWM error, if the title bar could not be set to the theme. The dialog
    /// still works, with a light title bar.
    pub fn decoration_error(&self) -> Option<&windows::core::Error> {
        self.decoration_error.as_ref()
    }
    pub fn set_paint_handler(&self, handler: PaintHandler) {
        *self.state.handlers.paint.borrow_mut() = Some(handler);
    }
    pub fn set_message_handler(&self, handler: MessageHandler) {
        *self.state.handlers.message.borrow_mut() = Some(handler);
    }
}
impl Drop for DialogWindow {
    fn drop(&mut self) {
        self.state.let_go_of_owner();
        if self.state.alive.get() {
            // SAFETY: the window is alive, and WM_NCDESTROY clears its pointer.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Where a dialog of this outer size goes: the middle of its owner, or of the primary
/// monitor, moved as little as needed to stay on that monitor's work area.
fn centered_place(owner: Option<HWND>, width: i32, height: i32) -> (i32, i32) {
    let mut around = RECT::default();
    let has_owner = owner.is_some_and(|owner| {
        // SAFETY: `around` is a valid out pointer. A dead handle returns an error.
        unsafe { GetWindowRect(owner, &mut around) }.is_ok()
    });
    if !has_owner {
        around = RECT::default();
    }
    // SAFETY: a plain query. The zero rectangle gives the primary monitor.
    let monitor = unsafe {
        MonitorFromRect(
            &around,
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
        )
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` has its size set, and the monitor came from the call above.
    let found = unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool();
    let work = info.rcWork;
    if !has_owner && found {
        around = work;
    }
    let mut x = (around.left + around.right - width) / 2;
    let mut y = (around.top + around.bottom - height) / 2;
    if found {
        x = x.min(work.right - width).max(work.left);
        y = y.min(work.bottom - height).max(work.top);
    }
    (x, y)
}

/// The app theme from the registry. A missing value is a normal case on some
/// systems, so it means light, the Windows default.
fn apps_use_light_theme() -> Result<bool, String> {
    let mut light = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the out pointers are valid and sized for a DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut light as *mut _ as _),
            Some(&mut size),
        )
    };
    match status.0 {
        0 => Ok(light != 0),
        2 | 3 => Ok(true),
        code => Err(format!(
            "reading AppsUseLightTheme failed with code {code}; the title bar stays light"
        )),
    }
}

/// The current Windows system colors.
pub fn system_palette() -> SystemPalette {
    use windows::Win32::Graphics::Gdi::{
        COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT,
        COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
    };
    let get = |index| {
        // SAFETY: GetSysColor takes any of these color names.
        let value: COLORREF = COLORREF(unsafe { GetSysColor(index) });
        Color::rgb(
            (value.0 & 0xff) as u8,
            ((value.0 >> 8) & 0xff) as u8,
            ((value.0 >> 16) & 0xff) as u8,
        )
    };
    SystemPalette {
        window: get(COLOR_WINDOW),
        window_text: get(COLOR_WINDOWTEXT),
        highlight: get(COLOR_HIGHLIGHT),
        highlight_text: get(COLOR_HIGHLIGHTTEXT),
        button_face: get(COLOR_BTNFACE),
        gray_text: get(COLOR_GRAYTEXT),
        hotlight: get(COLOR_HOTLIGHT),
    }
}

/// True while Windows is in a high contrast theme.
pub fn high_contrast_active() -> windows::core::Result<bool> {
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    let mut info = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` has its size set.
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            info.cbSize,
            Some(&mut info as *mut _ as _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )?;
    }
    Ok(info.dwFlags.contains(HCF_HIGHCONTRASTON))
}

/// The Windows clipboard, for plain text.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClipboard;

/// Holds the clipboard open and closes it on every path out.
struct OpenedClipboard;
/// A hidden window on this thread that owns the clipboard while it writes. Microsoft's
/// `OpenClipboard` page says that opening with a null window makes `EmptyClipboard` set
/// a null owner, "this causes SetClipboardData to fail". That would wipe the user's
/// clipboard and write nothing, so a write never opens without this window.
#[doc(hidden)]
pub fn clipboard_owner_window() -> Result<HWND, String> {
    let held = CLIPBOARD_OWNER.get();
    if let Some(hwnd) = held
        // SAFETY: a plain query.
        && unsafe { IsWindow(Some(hwnd)) }.as_bool()
    {
        return Ok(hwnd);
    }
    // SAFETY: a built-in class and a message-only parent: it never shows.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("could not make the clipboard's owner window: {e}"))?;
    CLIPBOARD_OWNER.set(Some(hwnd));
    Ok(hwnd)
}

impl OpenedClipboard {
    fn open() -> Result<Self, String> {
        let owner = clipboard_owner_window()?;
        // Another program can hold the clipboard for a moment, so it tries a few times.
        let mut last = String::new();
        for attempt in 0..5 {
            // SAFETY: the owner is a live window on this thread.
            match unsafe { OpenClipboard(Some(owner)) } {
                Ok(()) => return Ok(Self),
                Err(error) => last = error.to_string(),
            }
            if attempt < 4 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Err(format!("could not open the clipboard: {last}"))
    }
}
impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        // SAFETY: this value exists only while this thread has the clipboard open.
        let _ = unsafe { CloseClipboard() };
    }
}
/// Makes room for decoded text, or says in plain words that the memory is not there.
#[doc(hidden)]
pub fn reserve_text(bytes: usize) -> Result<String, String> {
    let mut text = String::new();
    text.try_reserve_exact(bytes).map_err(|_| {
        format!(
            "There is not enough memory to paste this text ({} MiB).",
            bytes >> 20
        )
    })?;
    Ok(text)
}
/// Turns UTF-16 into a String, with one exact allocation that may fail softly. Text
/// that is not valid UTF-16 is an error, not a silent change to what the user copied.
#[doc(hidden)]
pub fn decode_units(units: &[u16]) -> Result<String, String> {
    let invalid = || "the clipboard text is not valid Unicode, so it was not pasted".to_string();
    let mut bytes = 0usize;
    for unit in char::decode_utf16(units.iter().copied()) {
        bytes += unit.map_err(|_| invalid())?.len_utf8();
    }
    let mut text = reserve_text(bytes)?;
    text.extend(char::decode_utf16(units.iter().copied()).flatten());
    Ok(text)
}
/// Reads text from a memory block, never past the block's own size, whether or not
/// the text ends in a zero.
///
/// # Safety
/// `memory` must be a live global memory block that nothing else writes to meanwhile.
#[doc(hidden)]
pub unsafe fn read_text_block(memory: HGLOBAL) -> Result<String, String> {
    // SAFETY: as this function's contract says. The slice stays inside the block.
    unsafe {
        let units = GlobalSize(memory) / 2;
        let pointer = GlobalLock(memory) as *const u16;
        if pointer.is_null() {
            return Err("could not lock the clipboard text".into());
        }
        let block = std::slice::from_raw_parts(pointer, units);
        let end = block.iter().position(|unit| *unit == 0).unwrap_or(units);
        let text = decode_units(&block[..end]);
        let _ = GlobalUnlock(memory);
        text
    }
}
/// Reads the private format: a `u64` count of UTF-16 units, then the units. Zeros are
/// text here. A block whose count does not fit it is an error, because pasting only a
/// part of it would lose text with no word to the user.
///
/// # Safety
/// `memory` must be a live global memory block that nothing else writes to meanwhile.
#[doc(hidden)]
pub unsafe fn read_full_text_block(memory: HGLOBAL) -> Result<String, String> {
    const DAMAGED: &str = "the clipboard's full text is damaged, so it was not pasted";
    // SAFETY: as this function's contract says. The header is read only after the size
    // check, and the slice length is checked against the block's size.
    unsafe {
        let size = GlobalSize(memory);
        if size < 8 {
            return Err(DAMAGED.into());
        }
        let pointer = GlobalLock(memory) as *const u8;
        if pointer.is_null() {
            return Err("could not lock the clipboard text".into());
        }
        let count = (pointer as *const u64).read_unaligned();
        let fits = usize::try_from(count)
            .ok()
            .and_then(|units| units.checked_mul(2))
            .is_some_and(|bytes| bytes <= size - 8);
        let text = if fits {
            let units = std::slice::from_raw_parts(pointer.add(8) as *const u16, count as usize);
            decode_units(units)
        } else {
            Err(DAMAGED.into())
        };
        let _ = GlobalUnlock(memory);
        text
    }
}
const CANCELLED: &str = "The copy was replaced by a newer one.";
/// A movable memory block this code still owns.
struct OwnedBlock(HGLOBAL);
// SAFETY: a movable global block is a process-wide handle, not tied to a thread. Nothing
// holds it locked between calls, and this value is its only owner.
unsafe impl Send for OwnedBlock {}
impl OwnedBlock {
    fn allocate(bytes: usize) -> Result<Self, String> {
        // SAFETY: a plain allocation, and a failure is returned.
        unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
            .map(Self)
            .map_err(|_| {
                format!(
                    "There is not enough memory to copy this text ({} MiB).",
                    bytes >> 20
                )
            })
    }
    /// Gives the block to the clipboard. On success Windows owns it.
    ///
    /// # Safety
    /// The clipboard must be open on this thread and already emptied.
    unsafe fn hand_over(self, format: u32) -> Result<(), String> {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: as this function's contract says.
        match unsafe { SetClipboardData(format, Some(HANDLE(this.0.0))) } {
            Ok(_) => Ok(()),
            Err(error) => {
                // SAFETY: Windows refused it, so this code still owns the block.
                let _ = unsafe { GlobalFree(Some(this.0)) };
                Err(format!("could not write the clipboard: {error}"))
            }
        }
    }
}
impl Drop for OwnedBlock {
    fn drop(&mut self) {
        // SAFETY: only an owned block reaches this drop.
        let _ = unsafe { GlobalFree(Some(self.0)) };
    }
}
/// Text ready for the clipboard. Making it takes time and memory in proportion to the
/// text and touches no clipboard state, so it can run on any thread. Handing it over
/// with `SystemClipboard::write_prepared` costs the same for any size of text.
pub struct PreparedClipboardText {
    text: OwnedBlock,
    /// Present only when the text holds a zero, which the normal format stops at.
    full: Option<OwnedBlock>,
}
impl std::fmt::Debug for PreparedClipboardText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedClipboardText")
            .field("has_full_text", &self.full.is_some())
            .finish()
    }
}
/// Writes `text` as UTF-16 into a new block. With `count`, the block starts with that
/// count and has no end zero; without it, the text ends in a zero.
fn fill_block(
    block: &OwnedBlock,
    text: &str,
    count: Option<u64>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    // SAFETY: `prepare_clipboard_text` sized the block for the header or the end zero,
    // and one unit for each unit of `text`. Nothing else can reach the block yet.
    unsafe {
        let pointer = GlobalLock(block.0) as *mut u8;
        if pointer.is_null() {
            return Err("could not lock clipboard memory".into());
        }
        let mut to = pointer as *mut u16;
        if let Some(count) = count {
            (pointer as *mut u64).write_unaligned(count);
            to = pointer.add(8) as *mut u16;
        }
        let mut stopped = false;
        for (written, unit) in text.encode_utf16().enumerate() {
            // Each 256 KiB of text takes well under a millisecond.
            if written & 0x3_ffff == 0 && cancel.load(Ordering::Acquire) {
                stopped = true;
                break;
            }
            to.write_unaligned(unit);
            to = to.add(1);
        }
        if count.is_none() && !stopped {
            to.write_unaligned(0);
        }
        let _ = GlobalUnlock(block.0);
        if stopped {
            return Err(CANCELLED.to_string());
        }
    }
    Ok(())
}
/// Makes the memory blocks for `text`. Safe to run on any thread.
pub fn prepare_clipboard_text(text: &str) -> Result<PreparedClipboardText, String> {
    prepare_clipboard_text_unless(text, &AtomicBool::new(false))
}
/// The same, but it stops soon after `cancel` turns true, frees what it made, and
/// returns an error that nobody needs to read.
pub fn prepare_clipboard_text_unless(
    text: &str,
    cancel: &AtomicBool,
) -> Result<PreparedClipboardText, String> {
    let units = text.encode_utf16().count();
    let too_big = || "This text is too large for the clipboard.".to_string();
    let bytes = units.checked_mul(2).ok_or_else(too_big)?;
    let block = OwnedBlock::allocate(bytes.checked_add(2).ok_or_else(too_big)?)?;
    fill_block(&block, text, None, cancel)?;
    let full = if text.as_bytes().contains(&0) {
        let full = OwnedBlock::allocate(bytes.checked_add(8).ok_or_else(too_big)?)?;
        fill_block(&full, text, Some(units as u64), cancel)?;
        Some(full)
    } else {
        None
    };
    Ok(PreparedClipboardText { text: block, full })
}
impl PreparedClipboardText {
    /// Lets a test look at what a write would give the clipboard, with no clipboard.
    #[doc(hidden)]
    #[allow(clippy::type_complexity)]
    pub fn test_read(&self) -> (Result<String, String>, Option<Result<String, String>>) {
        // SAFETY: this value owns both blocks and nothing else writes to them.
        unsafe {
            (
                read_text_block(self.text.0),
                self.full.as_ref().map(|full| read_full_text_block(full.0)),
            )
        }
    }
}
/// The private format that keeps text a zero would cut short.
fn full_text_format() -> Result<u32, String> {
    // SAFETY: a plain call with a constant name. It adds a name to a system table and
    // does not touch what the clipboard holds.
    match unsafe { RegisterClipboardFormatW(w!("QuickEdit Full Text")) } {
        0 => Err(format!(
            "could not name the clipboard's full text format: {}",
            windows::core::Error::from_thread()
        )),
        format => Ok(format),
    }
}
impl SystemClipboard {
    /// Replaces the clipboard with prepared text. Its cost does not depend on the size
    /// of the text, apart from up to 40 ms of retries if another program holds the
    /// clipboard.
    pub fn write_prepared(&self, prepared: PreparedClipboardText) -> Result<(), String> {
        let full_format = match prepared.full {
            Some(_) => Some(full_text_format()?),
            None => None,
        };
        let _open = OpenedClipboard::open()?;
        // SAFETY: the clipboard is open, and it is emptied before the blocks go in.
        unsafe {
            EmptyClipboard().map_err(|e| format!("could not empty the clipboard: {e}"))?;
            prepared.text.hand_over(u32::from(CF_UNICODETEXT.0))?;
            if let (Some(full), Some(format)) = (prepared.full, full_format)
                && let Err(error) = full.hand_over(format)
            {
                // Half a write would paste a part of the text with no word to anyone.
                if let Err(empty) = EmptyClipboard() {
                    return Err(format!("{error}; could not empty the clipboard: {empty}"));
                }
                return Err(error);
            }
        }
        Ok(())
    }
}
/// Which format a Paste reads: the private one if it is there, or else plain text. With
/// no name for the private format, nobody can have written it, so plain text is safe.
#[doc(hidden)]
pub fn clipboard_read_format(
    full: Option<u32>,
    plain: u32,
    available: impl Fn(u32) -> bool,
) -> Option<u32> {
    full.filter(|format| available(*format))
        .or_else(|| available(plain).then_some(plain))
}
impl ClipboardAccess for SystemClipboard {
    fn read_text(&self) -> Result<Option<String>, String> {
        let full_format = full_text_format().ok();
        let plain = u32::from(CF_UNICODETEXT.0);
        // SAFETY: a plain query.
        let available = |format| unsafe { IsClipboardFormatAvailable(format) }.is_ok();
        let Some(format) = clipboard_read_format(full_format, plain, available) else {
            return Ok(None);
        };
        let _open = OpenedClipboard::open()?;
        // SAFETY: the clipboard is open, so the data stays valid until it closes.
        unsafe {
            let handle = GetClipboardData(format)
                .map_err(|e| format!("could not read the clipboard: {e}"))?;
            if format != plain {
                read_full_text_block(HGLOBAL(handle.0)).map(Some)
            } else {
                read_text_block(HGLOBAL(handle.0)).map(Some)
            }
        }
    }
    fn write_text(&self, text: &str) -> Result<(), String> {
        self.write_prepared(prepare_clipboard_text(text)?)
    }
}

/// An alert has at most this many buttons, as on the Mac.
pub const ALERT_MAX_BUTTONS: usize = 3;

pub struct AlertModel {
    pub title: String,
    pub message: String,
    pub buttons: Vec<String>,
    /// Buttons past the third, which have no room. A caller can show or log them.
    pub dropped_buttons: Vec<String>,
}
impl AlertModel {
    pub fn new(title: impl Into<String>, message: impl Into<String>, buttons: Vec<String>) -> Self {
        let mut buttons = buttons;
        let dropped_buttons = if buttons.len() > ALERT_MAX_BUTTONS {
            buttons.split_off(ALERT_MAX_BUTTONS)
        } else {
            vec![]
        };
        let title = title.into();
        if !dropped_buttons.is_empty() {
            eprintln!(
                "alert {title:?} has more than {ALERT_MAX_BUTTONS} buttons; left out: {dropped_buttons:?}"
            );
        }
        Self {
            title,
            message: message.into(),
            buttons,
            dropped_buttons,
        }
    }
}
