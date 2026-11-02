//! One open menu: its view, its popup windows and their renderers.
//!
//! No message loop lives here. Popup input runs through the window's normal message loop,
//! so timers, wakes, layout results and `WM_CLOSE` keep reaching the owner while a menu is
//! open. `ShellHandle::open_menu` is the public way in.
use crate::ShellError;
use qe_api::{DisplayList, PixelRect, RenderError, Renderer, TextMeasurer, Theme};
use qe_ui::{MenuView, MenuWindows};
use std::cell::RefCell;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

/// Makes the renderer for one popup level, at this size in device pixels. The window sets
/// its DPI. `qe-shell` cannot name `qe_render::BitmapRenderer`, so the owner gives this.
pub type PopupRendererFactory = Box<dyn FnMut(u32, u32) -> Result<Box<dyn Renderer>, RenderError>>;
/// Gets each popup's mouse messages. `None` passes the message on to Windows.
pub(crate) type PopupMessages = Rc<dyn Fn(HWND, u32, WPARAM, LPARAM) -> Option<LRESULT>>;

struct Level {
    renderer: Box<dyn Renderer>,
    size: (u32, u32),
}
pub(crate) struct MenuRun {
    pub view: MenuView,
    pub windows: MenuWindows,
    pub timer_set: bool,
    levels: Rc<RefCell<Vec<Option<Level>>>>,
    scale: f32,
}
impl MenuRun {
    pub fn new(
        owner: HWND,
        scale: f32,
        on_screen: bool,
        view: MenuView,
        messages: PopupMessages,
    ) -> Self {
        let levels: Rc<RefCell<Vec<Option<Level>>>> = Rc::default();
        let mut windows = if on_screen {
            MenuWindows::new(owner, scale)
        } else {
            MenuWindows::new_hidden(owner, scale)
        };
        let painted = levels.clone();
        windows.set_level_setup(Box::new(move |depth, popup| {
            let messages = messages.clone();
            popup.set_message_handler(Box::new(move |hwnd, msg, w, l| messages(hwnd, msg, w, l)));
            let painted = painted.clone();
            popup.set_paint_handler(Box::new(move |dc, rect| {
                // `paint` holds this only while it draws, and drawing sends no message.
                let Ok(mut levels) = painted.try_borrow_mut() else {
                    return;
                };
                if let Some(Some(level)) = levels.get_mut(depth)
                    && let Err(error) = level.renderer.present_rect(
                        dc.0,
                        PixelRect {
                            left: rect.left,
                            top: rect.top,
                            right: rect.right,
                            bottom: rect.bottom,
                        },
                    )
                {
                    eprintln!("copying a menu to its window failed: {}", error.message);
                }
            }));
        }));
        Self {
            view,
            windows,
            timer_set: false,
            levels,
            scale,
        }
    }
    pub fn root(&self) -> Option<HWND> {
        self.windows.levels().first().map(|level| level.hwnd())
    }
    /// Draws every open level and copies it to its window.
    pub fn paint(
        &mut self,
        factory: &mut PopupRendererFactory,
        theme: &Theme,
        measure: &dyn TextMeasurer,
    ) -> Result<(), ShellError> {
        let failed = |stage: &'static str, error: RenderError| ShellError::Renderer {
            stage,
            message: error.message,
        };
        let rects = self.view.levels();
        let mut levels = self.levels.borrow_mut();
        let wanted = rects.len().max(levels.len());
        levels.resize_with(wanted, || None);
        for (depth, popup) in self.windows.levels().iter().enumerate() {
            let rect = rects[depth];
            let size = (
                (rect.width.0 * self.scale).round() as u32,
                (rect.height.0 * self.scale).round() as u32,
            );
            let slot = &mut levels[depth];
            if slot.is_none() {
                let mut renderer =
                    factory(size.0, size.1).map_err(|e| failed("menu renderer", e))?;
                renderer
                    .set_dpi((self.scale * 96.0).round() as u32)
                    .map_err(|e| failed("menu set_dpi", e))?;
                *slot = Some(Level { renderer, size });
            }
            let Some(level) = slot else { continue };
            if level.size != size {
                level
                    .renderer
                    .resize(size.0, size.1)
                    .map_err(|e| failed("menu resize", e))?;
                level.size = size;
            }
            let mut list = DisplayList::default();
            self.view.draw_level(depth, &mut list, theme, measure);
            let renderer = &mut level.renderer;
            renderer.invalidate_all();
            renderer
                .begin_frame()
                .map_err(|e| failed("menu begin_frame", e))?;
            renderer.draw(&list).map_err(|e| failed("menu draw", e))?;
            renderer
                .end_frame()
                .map_err(|e| failed("menu end_frame", e))?;
            renderer
                .present(popup.hwnd().0)
                .map_err(|e| failed("menu present", e))?;
        }
        Ok(())
    }
}
