use crate::{ClipboardAccess, SystemPalette, place_popup};
use qe_api::{Color, Dip, DisplayList, Rect, RenderError, Renderer};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;
use windows::Win32::Foundation::{
    COLORREF, E_FAIL, E_INVALIDARG, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, RECT,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, HDC, InvalidateRect, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromRect, PAINTSTRUCT,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
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
            *self.paint.borrow_mut() = Some(handler);
        }
        // SAFETY: pairs with BeginPaint above.
        let _ = unsafe { EndPaint(hwnd, &paint) };
    }
    fn message(&self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        let handler = self.message.borrow_mut().take();
        let mut handler = handler?;
        let answer = handler(hwnd, msg, w, l);
        *self.message.borrow_mut() = Some(handler);
        answer
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

struct PopupState {
    hwnd: Cell<HWND>,
    owner: HWND,
    restore_focus: Cell<Option<HWND>>,
    shown: Cell<bool>,
    alive: Cell<bool>,
    dismissed: Cell<Option<PopupDismissed>>,
    handlers: Handlers,
}
thread_local! {
    static SHOWN_POPUPS: RefCell<Vec<*const PopupState>> = const { RefCell::new(Vec::new()) };
    static FOREGROUND_HOOK: Cell<Option<HWINEVENTHOOK>> = const { Cell::new(None) };
    static MODAL_OWNERS: RefCell<HashMap<isize, ModalOwner>> = RefCell::new(HashMap::new());
}
impl PopupState {
    fn dismiss(&self, reason: PopupDismissed) {
        if !self.shown.replace(false) {
            return;
        }
        self.dismissed.set(Some(reason));
        remove_from_foreground_watch(self);
        let hwnd = self.hwnd.get();
        // SAFETY: the window is alive while `alive` is true, and this runs on its thread.
        unsafe {
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some(focus) = self.restore_focus.take()
                && IsWindow(Some(focus)).as_bool()
                && GetFocus() != focus
            {
                let _ = SetFocus(Some(focus));
            }
            let _ = PostMessageW(
                Some(self.owner),
                POPUP_DISMISSED_MESSAGE,
                WPARAM(hwnd.0 as usize),
                LPARAM(reason as isize),
            );
        }
    }
}
fn remove_from_foreground_watch(state: &PopupState) {
    SHOWN_POPUPS.with(|list| {
        let mut list = list.borrow_mut();
        list.retain(|p| !std::ptr::eq(*p, state));
        if list.is_empty()
            && let Some(hook) = FOREGROUND_HOOK.take()
        {
            // SAFETY: the hook came from SetWinEventHook on this thread.
            let _ = unsafe { UnhookWinEvent(hook) };
        }
    });
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
    let popups: Vec<*const PopupState> = SHOWN_POPUPS.with(|list| list.borrow().clone());
    for popup in popups {
        // SAFETY: a popup leaves this list before its state is freed.
        let state = unsafe { &*popup };
        if state.owner != foreground && state.hwnd.get() != foreground {
            state.dismiss(PopupDismissed::Deactivated);
        }
    }
}
unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: Windows passes valid arguments. The state pointer is set on WM_NCCREATE
    // and is cleared on WM_NCDESTROY, before the state is freed.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PopupState;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let state = &*pointer;
        match msg {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                state.handlers.paint(hwnd);
                return LRESULT(0);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                let (x, y) = (l.0 as i16 as i32, (l.0 >> 16) as i16 as i32);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                if x < client.left || y < client.top || x >= client.right || y >= client.bottom {
                    state.dismiss(PopupDismissed::OutsideClick);
                    return LRESULT(0);
                }
            }
            WM_CAPTURECHANGED => {
                if HWND(l.0 as *mut _) != hwnd {
                    state.dismiss(PopupDismissed::CaptureLost);
                }
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                remove_from_foreground_watch(state);
                state.alive.set(false);
                state.shown.set(false);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
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
pub struct PopupWindow {
    state: Box<PopupState>,
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
    /// Creates the window hidden. `show` puts it on screen.
    pub fn create(
        owner: HWND,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> windows::core::Result<Self> {
        static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
        let class = w!("QuickEditPopup");
        register_class(&CLASS, class, Some(popup_proc), CS_DROPSHADOW)?;
        let state = Box::new(PopupState {
            hwnd: Cell::new(HWND::default()),
            owner,
            restore_focus: Cell::new(None),
            shown: Cell::new(false),
            alive: Cell::new(true),
            dismissed: Cell::new(None),
            handlers: Handlers::default(),
        });
        let pointer: *const PopupState = &*state;
        // SAFETY: the class is registered, and the state outlives the window: Drop
        // destroys the window before the box is freed.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!(""),
                WS_POPUP,
                x,
                y,
                width,
                height,
                Some(owner),
                None,
                Some(module()?),
                Some(pointer as _),
            )
        }?;
        state.hwnd.set(hwnd);
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
        self.state.owner
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
    pub fn dismissed(&self) -> Option<PopupDismissed> {
        self.state.dismissed.get()
    }
    pub fn set_paint_handler(&self, handler: PaintHandler) {
        *self.state.handlers.paint.borrow_mut() = Some(handler);
    }
    pub fn set_message_handler(&self, handler: MessageHandler) {
        *self.state.handlers.message.borrow_mut() = Some(handler);
    }
    /// Shows the popup without activating it, and takes the mouse so that an outside
    /// press reaches it.
    pub fn show(&self) {
        let state = &*self.state;
        if !state.alive.get() || state.shown.replace(true) {
            return;
        }
        state.dismissed.set(None);
        let hwnd = state.hwnd.get();
        // SAFETY: the window is alive, and these calls run on its thread.
        unsafe {
            let focus = GetFocus();
            state
                .restore_focus
                .set((!focus.0.is_null()).then_some(focus));
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetCapture(hwnd);
        }
        SHOWN_POPUPS.with(|list| list.borrow_mut().push(&*self.state));
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
            if !hook.is_invalid() {
                FOREGROUND_HOOK.set(Some(hook));
            }
        }
    }
    /// Marks the popup as showing without putting it on screen, so that a test with
    /// no desktop can check how it goes away.
    #[doc(hidden)]
    pub fn show_for_test(&self) {
        self.state.shown.set(true);
        self.state.dismissed.set(None);
    }
    /// Hides the popup and gives focus back.
    pub fn dismiss(&self) {
        self.state.dismiss(PopupDismissed::Caller);
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
        remove_from_foreground_watch(&self.state);
        if self.state.alive.get() {
            // SAFETY: the window is still alive, and WM_NCDESTROY clears its pointer.
            unsafe {
                let _ = DestroyWindow(self.state.hwnd.get());
            }
        }
    }
}

struct ModalOwner {
    count: usize,
    was_disabled: bool,
}
/// Disables an owner for one more modal dialog. The first one records whether the
/// owner was already disabled.
fn acquire_modal(owner: HWND) {
    MODAL_OWNERS.with(|map| {
        let mut map = map.borrow_mut();
        let entry = map.entry(owner.0 as isize).or_insert_with(|| ModalOwner {
            count: 0,
            // SAFETY: a plain query.
            was_disabled: !unsafe { IsWindowEnabled(owner) }.as_bool(),
        });
        entry.count += 1;
        // SAFETY: EnableWindow takes any window handle on this thread.
        unsafe {
            let _ = EnableWindow(owner, false);
        }
    });
}
/// Ends one modal dialog on an owner. Only the last one restores the owner, and
/// only if the owner was enabled before the first.
fn release_modal(owner: HWND) {
    MODAL_OWNERS.with(|map| {
        let mut map = map.borrow_mut();
        let key = owner.0 as isize;
        let Some(entry) = map.get_mut(&key) else {
            return;
        };
        entry.count -= 1;
        if entry.count == 0 {
            let restore = !entry.was_disabled;
            map.remove(&key);
            // SAFETY: the check covers an owner that is already gone.
            unsafe {
                if restore && IsWindow(Some(owner)).as_bool() {
                    let _ = EnableWindow(owner, true);
                }
            }
        }
    });
}

struct DialogState {
    owner: Option<HWND>,
    modal: bool,
    holds_owner: Cell<bool>,
    alive: Cell<bool>,
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
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DialogState;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let state = &*pointer;
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
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
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
    state: Box<DialogState>,
    resizable: bool,
    theme_note: Option<String>,
    decoration_error: Option<windows::core::Error>,
}
impl DialogWindow {
    /// A dialog with no owner. It cannot be modal, because nothing would be disabled.
    pub fn create(
        title: &str,
        width: i32,
        height: i32,
        modal: bool,
        resizable: bool,
    ) -> windows::core::Result<Self> {
        Self::create_with_owner(title, width, height, modal, resizable, None)
    }
    /// Creates the dialog hidden. The caller shows it.
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
        let state = Box::new(DialogState {
            owner,
            modal,
            holds_owner: Cell::new(false),
            alive: Cell::new(true),
            closed_by_user: Cell::new(false),
            handlers: Handlers::default(),
        });
        let pointer: *const DialogState = &*state;
        // SAFETY: the class is registered, and the state outlives the window.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(wide.as_ptr()),
                style,
                0,
                0,
                width,
                height,
                owner,
                None,
                Some(module()?),
                Some(pointer as _),
            )
        }?;
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
impl OpenedClipboard {
    fn open() -> Result<Self, String> {
        // Another program can hold the clipboard for a moment, so it tries a few times.
        let mut last = String::new();
        for attempt in 0..5 {
            // SAFETY: a null owner is allowed.
            match unsafe { OpenClipboard(None) } {
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
impl ClipboardAccess for SystemClipboard {
    fn read_text(&self) -> Result<Option<String>, String> {
        // SAFETY: a plain query.
        if unsafe { IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)) }.is_err() {
            return Ok(None);
        }
        let _open = OpenedClipboard::open()?;
        // SAFETY: the clipboard is open. The data stays valid until it closes. The
        // lock gives a pointer to text that ends in a zero.
        unsafe {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0))
                .map_err(|e| format!("could not read the clipboard: {e}"))?;
            let memory = HGLOBAL(handle.0);
            let pointer = GlobalLock(memory) as *const u16;
            if pointer.is_null() {
                return Err("could not lock the clipboard text".into());
            }
            let mut length = 0;
            while *pointer.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
            let _ = GlobalUnlock(memory);
            Ok(Some(text))
        }
    }
    fn write_text(&self, text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let _open = OpenedClipboard::open()?;
        // SAFETY: the clipboard is open. The block is large enough for `wide`. After
        // a successful SetClipboardData, Windows owns the block.
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)
                .map_err(|e| format!("could not allocate clipboard memory: {e}"))?;
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err("could not lock clipboard memory".into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
            let _ = GlobalUnlock(memory);
            if let Err(error) = EmptyClipboard() {
                let _ = GlobalFree(Some(memory));
                return Err(format!("could not empty the clipboard: {error}"));
            }
            if let Err(error) =
                SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0)))
            {
                let _ = GlobalFree(Some(memory));
                return Err(format!("could not write the clipboard: {error}"));
            }
        }
        Ok(())
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
