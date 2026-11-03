//! What the Settings window needs from the app. Its code runs only when the user opens it.
use crate::app::Shared;
use crate::render::popup_renderer;
use qe_a11y::{UiThreadOnly, UiaHost, UiaSlot, WidgetAction};
use qe_api::{Renderer, TextMeasurer, Theme, ThemeKind, Widget, WidgetId};
use qe_settings::SettingsHost;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use windows::Win32::Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_BACK, VK_END};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, WM_CHAR, WM_DESTROY, WM_GETOBJECT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP,
};

thread_local! {
    static SLOTS: RefCell<HashMap<isize, Rc<UiaSlot>>> = RefCell::new(HashMap::new());
}

pub struct AppSettingsHost {
    pub shared: Weak<Shared>,
}

/// Lets a screen reader read Settings and its encoding panel, and work their controls.
struct SettingsUia {
    shared: UiThreadOnly<Weak<Shared>>,
    hwnd: isize,
}
impl SettingsUia {
    fn post(&self, message: u32, w: usize, l: isize) -> windows::core::Result<()> {
        // SAFETY: a window of this thread. Windows checks the handle.
        unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                message,
                WPARAM(w),
                LPARAM(l),
            )
        }
    }
    /// The same click a mouse gives, so a control behaves as it does for anyone else.
    fn click(&self, widget: &Widget) -> windows::core::Result<()> {
        // SAFETY: a plain query. A dead handle gives 0.
        let dpi = unsafe { GetDpiForWindow(HWND(self.hwnd as *mut _)) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let x = ((widget.bounds.x.0 + widget.bounds.width.0 / 2.0) * scale) as isize;
        let y = ((widget.bounds.y.0 + widget.bounds.height.0 / 2.0) * scale) as isize;
        let l = (x & 0xffff) | ((y & 0xffff) << 16);
        self.post(WM_LBUTTONDOWN, 1, l)?;
        self.post(WM_LBUTTONUP, 0, l)
    }
    /// A click on the field, then the same keys a person would press to retype it.
    fn set_value(&self, widget: &Widget, text: &str) -> windows::core::Result<()> {
        self.click(widget)?;
        self.post(WM_KEYDOWN, usize::from(VK_END.0), 0)?;
        self.post(WM_KEYUP, usize::from(VK_END.0), 0)?;
        let old = widget
            .value
            .as_deref()
            .map_or(0, |value| value.chars().count());
        for _ in 0..old {
            self.post(WM_KEYDOWN, usize::from(VK_BACK.0), 0)?;
            self.post(WM_KEYUP, usize::from(VK_BACK.0), 0)?;
        }
        for unit in text.encode_utf16() {
            self.post(WM_CHAR, usize::from(unit), 0)?;
        }
        Ok(())
    }
}
impl UiaHost for SettingsUia {
    fn widgets(&self) -> Option<Widget> {
        let shared = self.shared.get()?.upgrade()?;
        let window = shared.settings_window.try_borrow().ok()?;
        let window = window.as_ref()?;
        if window
            .encoding_panel_hwnd()
            .is_some_and(|h| h.0 as isize == self.hwnd)
        {
            window.encoding_panel_widgets()
        } else {
            window.widgets()
        }
    }
    fn action(&self, id: WidgetId, action: WidgetAction) -> windows::core::Result<()> {
        let tree = self
            .widgets()
            .ok_or_else(|| windows::core::Error::new(E_INVALIDARG, "Settings is not open"))?;
        let widget = crate::a11y::find(&tree, id).ok_or_else(|| {
            windows::core::Error::new(E_INVALIDARG, "That control is not on show in Settings")
        })?;
        match action {
            WidgetAction::SetValue(text) => self.set_value(widget, &text),
            _ => self.click(widget),
        }
    }
}

impl SettingsHost for AppSettingsHost {
    fn measurer(&self) -> Arc<dyn TextMeasurer> {
        match self.shared.upgrade() {
            Some(shared) => shared.measure.clone(),
            None => Arc::new(qe_ui_measure()),
        }
    }
    fn theme(&self) -> Theme {
        // With `App` busy, the last painted frame's colors, which are the app's own.
        self.shared
            .upgrade()
            .and_then(|shared| {
                let theme = shared.state.try_borrow().ok().and_then(|app| {
                    let window = app.store.last_used_window()?;
                    app.windows.get(&window)?.handle.theme().ok()
                });
                theme.or_else(|| shared.last_theme.get())
            })
            .unwrap_or_else(|| Theme::new(ThemeKind::Light))
    }
    fn make_renderer(
        &self,
        width: u32,
        height: u32,
        dpi: u32,
    ) -> Result<Box<dyn Renderer>, String> {
        let mut renderer = popup_renderer(width, height).map_err(|e| e.message)?;
        renderer.set_dpi(dpi).map_err(|e| e.message)?;
        Ok(renderer)
    }
    fn window_message(&self, hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        if message == WM_DESTROY {
            let slot = SLOTS.with(|slots| slots.borrow_mut().remove(&(hwnd.0 as isize)));
            if let Some(slot) = slot
                && let Err(e) = slot.shutdown()
            {
                eprintln!("A screen reader's link to Settings would not end: {e}");
            }
            return None;
        }
        let known = SLOTS.with(|slots| slots.borrow().get(&(hwnd.0 as isize)).cloned());
        let slot = match known {
            Some(slot) => slot,
            None if message == WM_GETOBJECT => {
                let slot = Rc::new(UiaSlot::new());
                SLOTS.with(|slots| {
                    slots.borrow_mut().insert(hwnd.0 as isize, slot.clone());
                });
                slot
            }
            None => return None,
        };
        let shared = self.shared.clone();
        slot.on_window_message(hwnd, message, w, l, || {
            Arc::new(SettingsUia {
                shared: UiThreadOnly::new(shared),
                hwnd: hwnd.0 as isize,
            })
        })
    }
    fn announce(&self, hwnd: HWND, text: &str) {
        let known = SLOTS.with(|slots| slots.borrow().get(&(hwnd.0 as isize)).cloned());
        if let Some(slot) = known
            && let Err(e) = slot.announce(text)
        {
            eprintln!("Telling a screen reader \"{text}\": {e}");
        }
    }
}

fn qe_ui_measure() -> qe_layout::DirectWriteLayout {
    match qe_layout::DirectWriteLayout::new() {
        Ok(layout) => layout,
        // The shared one already worked at launch, and without text nothing can paint.
        Err(e) => {
            eprintln!("Text measuring could not start: {e}");
            std::process::abort();
        }
    }
}
