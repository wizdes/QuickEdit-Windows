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
use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{WM_DESTROY, WM_GETOBJECT};

thread_local! {
    static SLOTS: RefCell<HashMap<isize, Rc<UiaSlot>>> = RefCell::new(HashMap::new());
}

pub struct AppSettingsHost {
    pub shared: Weak<Shared>,
}

/// Lets a screen reader read Settings and its encoding panel.
struct SettingsUia {
    shared: UiThreadOnly<Weak<Shared>>,
    hwnd: isize,
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
    fn action(&self, _id: WidgetId, _action: WidgetAction) -> windows::core::Result<()> {
        // Settings' controls take the keyboard and the mouse. See the report's gaps.
        Err(windows::core::Error::new(
            E_NOTIMPL,
            "Settings' controls cannot yet be run by a screen reader's action",
        ))
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
        self.shared
            .upgrade()
            .and_then(|shared| {
                let app = shared.state.try_borrow().ok()?;
                let window = app.store.last_used_window()?;
                app.windows.get(&window)?.handle.theme().ok()
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
