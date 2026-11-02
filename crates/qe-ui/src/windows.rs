use crate::place_popup;
use qe_api::{Dip, DisplayList, Rect, RenderError, Renderer};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetWindowRect, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CAPTION,
    WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};
use windows::core::w;

/// Owns a real top-level popup. The caller paints it with its Renderer.
pub struct PopupWindow {
    hwnd: HWND,
}
impl PopupWindow {
    pub fn create_anchored(
        anchor: RECT,
        width: i32,
        height: i32,
        submenu: bool,
    ) -> windows::core::Result<Self> {
        let monitor = unsafe { MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
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
            placement.x.0 as i32,
            placement.y.0 as i32,
            placement.width.0 as i32,
            placement.height.0 as i32,
        )
    }
    pub fn create(x: i32, y: i32, width: i32, height: i32) -> windows::core::Result<Self> {
        // SAFETY: built-in window class and null parent; the returned HWND is owned here.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                x,
                y,
                width,
                height,
                None,
                None,
                None,
                None,
            )
        }?;
        // SAFETY: the attribute points to a valid integer and the HWND was just created.
        let preference = DWMWCP_ROUND.0;
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as _,
                std::mem::size_of_val(&preference) as u32,
            )
        };
        Ok(Self { hwnd })
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
    pub fn bounds(&self) -> windows::core::Result<RECT> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut rect)? };
        Ok(rect)
    }
    pub fn paint(
        &self,
        renderer: &mut dyn Renderer,
        list: &DisplayList,
    ) -> Result<(), RenderError> {
        renderer.begin_frame()?;
        renderer.draw(list)?;
        renderer.end_frame()
    }
}
impl Drop for PopupWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Owns a native-titled top-level dialog. The caller routes keys and focus.
pub struct DialogWindow {
    hwnd: HWND,
    owner: Option<HWND>,
    pub modal: bool,
    pub resizable: bool,
}
impl DialogWindow {
    pub fn create(
        title: &str,
        width: i32,
        height: i32,
        modal: bool,
        resizable: bool,
    ) -> windows::core::Result<Self> {
        Self::create_with_owner(title, width, height, modal, resizable, None)
    }
    pub fn create_with_owner(
        title: &str,
        width: i32,
        height: i32,
        modal: bool,
        resizable: bool,
        owner: Option<HWND>,
    ) -> windows::core::Result<Self> {
        let mut light = 1u32;
        let mut size = std::mem::size_of::<u32>() as u32;
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
        if status.0 != 0 {
            return Err(windows::core::Error::from_hresult(
                windows::core::HRESULT::from_win32(status.0),
            ));
        }
        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let style = WS_CAPTION
            | WS_SYSMENU
            | if resizable {
                WS_THICKFRAME
            } else {
                WINDOW_STYLE(0)
            };
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                windows::core::PCWSTR(wide.as_ptr()),
                style,
                0,
                0,
                width,
                height,
                owner,
                None,
                None,
                None,
            )
        }?;
        let dark = i32::from(light == 0);
        if let Err(error) = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as _,
                std::mem::size_of_val(&dark) as u32,
            )
        } {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(error);
        }
        if modal && let Some(owner) = owner {
            unsafe {
                let _ = EnableWindow(owner, false);
            }
        }
        Ok(Self {
            hwnd,
            owner,
            modal,
            resizable,
        })
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}
impl Drop for DialogWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            if self.modal
                && let Some(owner) = self.owner
            {
                let _ = EnableWindow(owner, true);
            }
        }
    }
}

pub struct AlertModel {
    pub title: String,
    pub message: String,
    pub buttons: Vec<String>,
}
impl AlertModel {
    pub fn new(title: impl Into<String>, message: impl Into<String>, buttons: Vec<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            buttons: buttons.into_iter().take(3).collect(),
        }
    }
}
