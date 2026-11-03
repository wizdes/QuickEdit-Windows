//! One text window: its `ShellWindow`, its tools, its message hook and its tab row.
use crate::app::{App, Effect, Incoming, Shared, push};
use crate::render::{AppRenderer, Capture, popup_renderer};
use qe_a11y::UiaSlot;
use qe_api::{MouseCursor, Rect, Renderer, ViewContext, ViewRequest};
use qe_docs::{DocId, ShowState, WindowFrame, WindowId};
use qe_layout::DirectWriteLayout;
use qe_render::BitmapRenderer;
use qe_settings::SettingsStore;
use qe_shell::{KeyTable, ShellHandle, ShellWindow, Tab, TabId, WindowConfig};
use qe_tools::ToolsHost;
use std::cell::Cell;
use std::rc::{Rc, Weak};
use windows::Win32::Foundation::{HWND, LRESULT, POINT, RECT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::HSTRING;

pub struct Win {
    pub shell: ShellWindow,
    pub handle: ShellHandle,
    pub hwnd: HWND,
    pub tools: ToolsHost,
    pub slot: Rc<UiaSlot>,
    pub tabs: Vec<(DocId, TabId)>,
    /// The document on show, whose view has been told so.
    pub shown: Option<DocId>,
    pub content: Rect,
    pub dpi: Rc<Cell<u32>>,
    pub capture: Capture,
    pub show_state: ShowState,
    pub paint_queued: bool,
    /// This window has the keyboard, by the last Focus or Blur.
    pub has_keys: bool,
    /// The tools held the keyboard at the last look, so TSF has left this window's text.
    pub tools_had_keys: bool,
    /// The text of the last painted frame, for the test channel's check of what really shows.
    pub painted_text: Vec<String>,
    /// The find count and the tools' message that a screen reader last heard.
    pub said: (Option<String>, Option<String>),
}
impl Win {
    /// Ends the window. A failure means it has gone already.
    pub fn close(self) {
        if let Err(e) = self.handle.destroy() {
            eprintln!("Closing a window: {e}");
        }
        drop(self.shell);
    }
}

/// The size of a new window, from the settings' characters and lines, in device pixels (P6).
fn window_pixels(app: &App, dpi: u32, top_dips: f32) -> (i32, i32) {
    let token = app.settings.font().setting_text();
    let measure = |text: &str| qe_api::TextMeasurer::measure(app.measure.as_ref(), text, &token);
    let chars = app
        .settings
        .get_integer(qe_api::SettingKey::WindowWidthCharacters)
        .unwrap_or(90)
        .clamp(10, 1000) as f32;
    let lines = app
        .settings
        .get_integer(qe_api::SettingKey::WindowHeightLines)
        .unwrap_or(30)
        .clamp(5, 500) as f32;
    let scale = dpi as f32 / 96.0;
    // Two lines less one line is one row with its gap. The map takes 100 dips on the right,
    // the gutter takes its width when it shows, and the text has its padding on both sides,
    // as the Mac's `initialContentSize` has.
    let row = (measure("0\n0").height.0 - measure("0").height.0).max(1.0);
    let key = qe_api::SettingKey::ShowLineNumbers;
    let gutter = if app.settings.get_bool(key).unwrap_or(false) {
        let points = qe_views::gutter_font_points(app.settings.font().points);
        let digit = qe_api::TextMeasurer::measure(
            app.measure.as_ref(),
            "8",
            &format!("{} {points}", qe_views::GUTTER_FONT),
        );
        qe_views::gutter_width(qe_views::gutter_digits(0), digit.width.0)
    } else {
        0.0
    };
    let width = chars * measure("0000000000").width.0 / 10.0
        + qe_views::TEXT_PADDING * 2.0
        + gutter
        + qe_views_map_width();
    let height = lines * row + top_dips;
    (
        (width * scale).ceil() as i32,
        (height * scale).ceil() as i32,
    )
}
const fn qe_views_map_width() -> f32 {
    100.0
}

fn place_window(hwnd: HWND, frame: WindowFrame) -> Result<(), String> {
    // SAFETY: a window of this thread, and no flag here shows it.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            frame.x,
            frame.y,
            frame.width,
            frame.height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| e.to_string())
}

fn on_a_monitor(frame: WindowFrame) -> bool {
    let rect = RECT {
        left: frame.x,
        top: frame.y,
        right: frame.x + frame.width,
        bottom: frame.y + frame.height,
    };
    // SAFETY: `rect` lives through the call.
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// The system's own record of a window's place, as the session keeps it.
pub fn read_placement(hwnd: HWND) -> Option<(WindowFrame, ShowState)> {
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: `placement` lives through the call, with its length set.
    if let Err(e) = unsafe { GetWindowPlacement(hwnd, &mut placement) } {
        eprintln!("GetWindowPlacement: {e}");
        return None;
    }
    let RECT {
        left,
        top,
        right,
        bottom,
    } = placement.rcNormalPosition;
    let show = if placement.showCmd == SW_SHOWMAXIMIZED.0 as u32 {
        ShowState::Maximized
    } else {
        ShowState::Normal
    };
    Some((
        WindowFrame {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        },
        show,
    ))
}

fn message_hook(
    weak: Weak<Shared>,
    window: WindowId,
    home: HWND,
    slot: Rc<UiaSlot>,
    drag: Rc<Cell<Option<(WindowId, DocId)>>>,
) -> qe_shell::MessageHook {
    Rc::new(move |hwnd, message, w, l| {
        let host_weak = weak.clone();
        // A popup forwards `WM_GETOBJECT` here. The provider stays this window's, because only
        // this window's procedure runs the provider's work.
        if let Some(answer) = slot.on_message_for_home(home, hwnd, message, w, l, || {
            crate::a11y::make_host(host_weak, window)
        }) {
            return Some(answer);
        }
        // The link ends even when the app has already let go of its shared state.
        if message == WM_DESTROY
            && hwnd == home
            && let Err(e) = slot.shutdown()
        {
            match weak.upgrade() {
                Some(shared) => crate::app::queue_note(
                    &shared,
                    format!("A screen reader's link to a closing window would not end: {e}"),
                ),
                None => eprintln!("A screen reader's link to a closing window would not end: {e}"),
            }
        }
        // A cheap test first: most messages are for no one here.
        if !matches!(
            message,
            WM_ACTIVATE
                | WM_ACTIVATEAPP
                | WM_EXITSIZEMOVE
                | WM_SIZE
                | WM_DROPFILES
                | WM_QUERYENDSESSION
                | WM_ENDSESSION
                | WM_SETTINGCHANGE
                | WM_MOUSEMOVE
                | WM_LBUTTONUP
                | WM_CAPTURECHANGED
                | WM_KEYDOWN
        ) {
            return None;
        }
        // While a torn-off tab is held, the pointer belongs to that drag.
        if let Some((from, doc)) = drag.get()
            && from == window
        {
            match message {
                WM_MOUSEMOVE => return Some(LRESULT(0)),
                WM_LBUTTONUP => {
                    drag.set(None);
                    // SAFETY: a plain call. Nothing depends on its answer here.
                    if let Err(e) = unsafe { ReleaseCapture() } {
                        eprintln!("ReleaseCapture after a tab drag: {e}");
                    }
                    if let (Some(shared), Ok(at)) = (weak.upgrade(), crate::platform::cursor_pos())
                    {
                        push(&shared, Incoming::TabDrop { doc, at });
                    }
                    return Some(LRESULT(0));
                }
                WM_CAPTURECHANGED => drag.set(None),
                WM_KEYDOWN if w.0 == usize::from(VK_ESCAPE.0) => {
                    drag.set(None);
                    // SAFETY: as above.
                    if let Err(e) = unsafe { ReleaseCapture() } {
                        eprintln!("ReleaseCapture after a tab drag: {e}");
                    }
                    return Some(LRESULT(0));
                }
                _ => {}
            }
        }
        let shared = weak.upgrade()?;
        match message {
            WM_ACTIVATE if w.0 & 0xffff != 0 => push(&shared, Incoming::Activated(window)),
            WM_ACTIVATEAPP if w.0 != 0 => push(&shared, Incoming::AppActivated),
            WM_EXITSIZEMOVE => push(&shared, Incoming::FrameChanged(window, true)),
            WM_SIZE => push(&shared, Incoming::FrameChanged(window, false)),
            WM_DROPFILES => {
                let paths = crate::platform::dropped_paths(w);
                push(&shared, Incoming::Dropped(window, paths));
                return Some(LRESULT(0));
            }
            WM_QUERYENDSESSION => return Some(LRESULT(1)),
            WM_ENDSESSION if w.0 != 0 => {
                push(&shared, Incoming::EndSession);
                return Some(LRESULT(0));
            }
            WM_SETTINGCHANGE => push(&shared, Incoming::SystemSetting),
            _ => {}
        }
        None
    })
}

impl App {
    /// Makes the window the store has just added. It stays hidden until the effect shows it.
    pub fn create_window(&mut self, id: WindowId) -> Result<(), String> {
        if self.fail_windows > 0 {
            self.fail_windows -= 1;
            return Err("this test asked for a window that fails".to_owned());
        }
        let capture = Capture::default();
        let shell = ShellWindow::create_with_refresh(
            WindowConfig {
                cell_width: 8.0,
                line_height: 17.0,
                ..Default::default()
            },
            AppRenderer {
                inner: BitmapRenderer::new(800, 600).map_err(|e| e.to_string())?,
                timing: self.timing.clone(),
                capture: capture.clone(),
            },
            |renderer, hwnd| renderer.inner.refresh_text_settings_for_window(HWND(hwnd)),
            Box::new(DirectWriteLayout::new().map_err(|e| e.to_string())?),
        )
        .map_err(|e| e.to_string())?;
        let handle = shell.handle();
        let hwnd = HWND(handle.hwnd().map_err(|e| e.to_string())?);
        let oops = |e: qe_shell::ShellError| e.to_string();
        handle
            .set_content_cursor(MouseCursor::IBeam)
            .map_err(oops)?;
        handle
            .set_popup_renderers(|width, height| {
                popup_renderer(width, height).map(|r: Box<dyn Renderer>| r)
            })
            .map_err(oops)?;
        // Keys go through the store's table, so no key stays with the window itself.
        handle
            .set_key_table(KeyTable { bindings: vec![] })
            .map_err(oops)?;
        let keys: SettingsStore = self.settings.clone();
        handle
            .set_chord_filter(Box::new(move |key, modifiers| {
                keys.command_for_press(key, modifiers).is_some()
            }))
            .map_err(oops)?;
        let slot = Rc::new(UiaSlot::new());
        handle
            .set_message_hook(message_hook(
                self.me.clone(),
                id,
                hwnd,
                slot.clone(),
                self.tab_drag.clone(),
            ))
            .map_err(oops)?;
        let weak = self.me.clone();
        shell.set_content_painter(move |list, frame| {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let Ok(mut app) = shared.state.try_borrow_mut() else {
                shared.missed_paint.borrow_mut().push(id);
                return;
            };
            app.paint(id, list, &frame);
        });
        let weak = self.me.clone();
        let mut tools = match &self.clipboard {
            Some(memory) => ToolsHost::with_clipboard(memory.clone()),
            None => ToolsHost::new(),
        };
        let wake = self.wake.clone();
        tools.set_wake(std::sync::Arc::new(move || wake()));
        let dpi = Rc::new(Cell::new(handle.dpi().map_err(oops)?));
        let content = handle.content_rect().map_err(oops)?;
        // The window style says the same as `DragAcceptFiles`, with no need to load shell32.
        // SAFETY: a window of this thread.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_ACCEPTFILES.0 as isize);
        }
        let saved = self.store.window_frame(id);
        let frame = match saved {
            Some((frame, _)) if frame.width > 0 && frame.height > 0 => frame,
            _ => {
                let (width, height) = window_pixels(self, dpi.get(), content.y.0);
                let mut rect = RECT::default();
                let mut client = RECT::default();
                // SAFETY: both rectangles live through their calls.
                if let Err(e) = unsafe {
                    GetWindowRect(hwnd, &mut rect).and_then(|()| GetClientRect(hwnd, &mut client))
                } {
                    eprintln!("GetWindowRect for a new window: {e}");
                }
                // The numbers are for the client area. Windows' resize edges lie outside it.
                let edges_x = (rect.right - rect.left) - (client.right - client.left);
                let edges_y = (rect.bottom - rect.top) - (client.bottom - client.top);
                WindowFrame {
                    x: rect.left,
                    y: rect.top,
                    width: width + edges_x,
                    height: height + edges_y,
                }
            }
        };
        // A saved place on a monitor that has gone keeps its size, and lands where Windows chose.
        let frame = if on_a_monitor(frame) {
            frame
        } else {
            let mut rect = RECT::default();
            // SAFETY: `rect` lives through the call.
            if let Err(e) = unsafe { GetWindowRect(hwnd, &mut rect) } {
                eprintln!("GetWindowRect for a new window: {e}");
            }
            WindowFrame {
                x: rect.left,
                y: rect.top,
                ..frame
            }
        };
        place_window(hwnd, frame)?;
        let show_state = saved.map_or(ShowState::Normal, |(_, show)| show);
        self.windows.insert(
            id,
            Win {
                shell,
                handle: handle.clone(),
                hwnd,
                tools,
                slot,
                tabs: Vec::new(),
                shown: None,
                content,
                dpi,
                capture,
                show_state,
                paint_queued: false,
                has_keys: false,
                tools_had_keys: false,
                painted_text: Vec::new(),
                said: (None, None),
            },
        );
        // The handler goes last: messages sent above wait in the shell's queue until now.
        let shell_ref = &self.windows[&id].shell;
        shell_ref.set_event_handler(move |_, event| {
            if let Some(shared) = weak.upgrade() {
                push(&shared, Incoming::Shell(id, event));
            }
        });
        self.effects.push(Effect::ShowWindow(id));
        Ok(())
    }

    /// Makes the tab row match the store, then shows the active document.
    pub fn sync_tabs(&mut self, window: WindowId) {
        let want = self.store.tabs(window);
        let active = self.store.active_tab(window);
        let titles: Vec<(String, bool)> = want
            .iter()
            .map(|doc| (self.tab_title(*doc), self.store.is_dirty(*doc)))
            .collect();
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        let mut tabs = std::mem::take(&mut win.tabs);
        let done = win.handle.with_strip(|strip| {
            // The strip starts with one tab of its own, and that is the first document's.
            if tabs.is_empty()
                && let (Some(doc), Some(id)) = (want.first(), strip.tab_id(0))
            {
                tabs.push((*doc, id));
            }
            let gone: Vec<TabId> = tabs
                .iter()
                .filter(|(doc, _)| !want.contains(doc))
                .map(|(_, id)| *id)
                .collect();
            tabs.retain(|(doc, _)| want.contains(doc));
            for (at, (doc, (title, dirty))) in want.iter().zip(&titles).enumerate() {
                let id = match tabs.iter().find(|(known, _)| known == doc) {
                    Some((_, id)) => *id,
                    None => {
                        let id = strip.add_tab(Tab::new(title.clone()));
                        tabs.push((*doc, id));
                        id
                    }
                };
                if strip.index_of(id) != Some(at) {
                    strip.reorder(id, at);
                }
                if let Some(index) = strip.index_of(id) {
                    let tab = &mut strip.tabs_mut()[index];
                    tab.title.clone_from(title);
                    tab.dirty = *dirty;
                }
            }
            // After the new tabs: the last tab of a strip cannot close.
            for id in gone {
                strip.close(id);
            }
            if let Some(active) = active
                && let Some((_, id)) = tabs.iter().find(|(doc, _)| *doc == active)
                && let Some(index) = strip.index_of(*id)
                && strip.active() != index
            {
                strip.select(*id);
            }
            // The row now says what the store says. These events would only say it back.
            drop(strip.take_events());
        });
        if let Err(e) = done {
            eprintln!("Updating the tab row: {e}");
        }
        win.tabs = tabs;
        self.show_active(window);
        self.update_window_title(window);
    }

    /// The name on a tab, with what the store says about its state.
    pub fn tab_title(&self, doc: DocId) -> String {
        let name = self.store.display_name(doc);
        let name = if self.autosave_failing.contains(&doc) {
            format!("{name} - no safe copy")
        } else {
            name
        };
        match self.store.title_note(doc) {
            Some(note) => format!("{name} - {note}"),
            None => name,
        }
    }

    pub fn update_window_title(&mut self, window: WindowId) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        let dirty = self.store.is_dirty(doc);
        let text = format!(
            "{}{} - QuickEdit",
            if dirty { "* " } else { "" },
            self.tab_title(doc)
        );
        if let Some(win) = self.windows.get(&window) {
            // SAFETY: the string lives through the call.
            if let Err(e) = unsafe { SetWindowTextW(win.hwnd, &HSTRING::from(text)) } {
                eprintln!("Setting a window's title: {e}");
            }
        }
    }

    /// Shows a tab's title and dirty mark, with no change to the row's order.
    pub fn update_tab(&mut self, doc: DocId) {
        let Some(window) = self.store.window_of(doc) else {
            return;
        };
        let title = self.tab_title(doc);
        let dirty = self.store.is_dirty(doc);
        if let Some(win) = self.windows.get(&window)
            && let Some((_, id)) = win.tabs.iter().find(|(known, _)| *known == doc)
        {
            let id = *id;
            let changed = win.handle.with_strip(|strip| {
                if let Some(index) = strip.index_of(id) {
                    let tab = &mut strip.tabs_mut()[index];
                    tab.title = title;
                    tab.dirty = dirty;
                }
            });
            if let Err(e) = changed {
                eprintln!("Updating a tab: {e}");
            }
        }
        self.update_window_title(window);
    }

    /// Hands the window from the old document to the store's active one.
    pub fn show_active(&mut self, window: WindowId) {
        let Some(new) = self.store.active_tab(window) else {
            return;
        };
        let Some(win) = self.windows.get(&window) else {
            return;
        };
        let old = win.shown;
        if old == Some(new) {
            return;
        }
        let mut ctx = ViewContext::default();
        self.prepare_doc(new, &mut ctx);
        // Each tab keeps its own Smart rows, in the one window's tools.
        if let (Some(old), Some(win)) = (old, self.windows.get(&window))
            && let Some(ui) = self.docs.get(&old)
        {
            ui.subs.set(win.tools.substitutions());
        }
        let subs = self.docs.get(&new).map(|ui| ui.subs.get());
        let measure = self.measure.clone();
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        if let Some(subs) = subs {
            win.tools.set_substitutions(subs);
        }
        win.shown = Some(new);
        // The store lends one view at a time, so the change has two halves.
        if let Some(old_view) = old.and_then(|old| self.store.view_mut(old)) {
            win.tools.leave_view(old_view, &mut ctx);
        }
        if let Some(view) = self.store.view_mut(new) {
            win.tools.view_switched(None, view, &mut ctx);
        }
        let content = win.content;
        let rest = win.tools.layout(content, measure.as_ref());
        // TSF's focus follows the keyboard, so a tab that lands in a background window or
        // behind an open Find bar leaves it alone.
        let wants_tsf = win.has_keys && win.tools.focus() == qe_tools::ToolsFocus::Editor;
        let rest = self.text_area(new, rest);
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&new), self.store.view_mut(new)) {
            ui.frame.layout(view, rest, measure.as_ref(), &mut ctx);
        }
        ctx.invalidate(content);
        self.apply(window, ctx.requests);
        self.report_widgets(window);
        self.report_input(window);
        if self.ime.started() && wants_tsf {
            self.effects.push(Effect::ImeFocus(window, new));
        }
        self.spell_refresh(new);
    }

    /// Carries out what a view asked for.
    pub fn apply(&mut self, window: WindowId, requests: Vec<ViewRequest>) {
        if requests.is_empty() {
            return;
        }
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        if self.hidden
            && !win.paint_queued
            && requests
                .iter()
                .any(|request| matches!(request, ViewRequest::Invalidate(_)))
        {
            win.paint_queued = true;
            self.effects.push(Effect::PaintHidden(window));
        }
        if let Err(e) = win.handle.apply_requests(requests) {
            eprintln!("A window could not carry out a request: {e}");
        }
    }

    /// Places the find bar and the text in the window's text area.
    pub fn relayout(&mut self, window: WindowId) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        let measure = self.measure.clone();
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        let mut ctx = ViewContext::default();
        match win.handle.content_rect() {
            Ok(rect) => win.content = rect,
            Err(e) => eprintln!("Reading the text area's size: {e}"),
        }
        let rest = win.tools.layout(win.content, measure.as_ref());
        let rest = self.text_area(doc, rest);
        if let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc)) {
            ui.frame.layout(view, rest, measure.as_ref(), &mut ctx);
        }
        self.apply(window, ctx.requests);
    }

    /// Draws the text area.
    pub fn paint(
        &mut self,
        window: WindowId,
        list: &mut qe_api::DisplayList,
        frame: &qe_shell::ContentFrame,
    ) {
        let Some(doc) = self.store.active_tab(window) else {
            return;
        };
        let measure = self.measure.clone();
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        win.paint_queued = false;
        let mut ctx = ViewContext::default();
        if win.content != frame.rect {
            win.content = frame.rect;
        }
        let whole = win.tools.layout(win.content, measure.as_ref());
        let rest = self.text_area(doc, whole);
        let bar_lines = self.notice_lines(doc);
        let Some(win) = self.windows.get_mut(&window) else {
            return;
        };
        let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc)) else {
            return;
        };
        ui.frame.layout(view, rest, measure.as_ref(), &mut ctx);
        let start = list.commands.len();
        ui.frame.draw(view, list, &frame.theme, measure.as_ref());
        crate::notices::draw(list, whole, &bar_lines, &frame.theme);
        win.tools.draw(list, &frame.theme, measure.as_ref());
        if self.channel_on {
            win.painted_text = list.commands[start..]
                .iter()
                .filter_map(|c| match c {
                    qe_api::DrawCommand::TextRun { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect();
        }
        self.last_theme.set(Some(frame.theme));
        let has_text = list.commands[start..]
            .iter()
            .any(|c| matches!(c, qe_api::DrawCommand::GlyphRun { .. }));
        if has_text {
            self.timing.frame_has_text.set(true);
        }
        self.timing
            .frame_is_current
            .set(view.caret_is_laid_out() && view.queued_input_count() == 0);
        self.apply(window, ctx.requests);
    }

    /// Which of the app's windows is at this place on screen, and which tab position there.
    pub fn window_at(&self, at: POINT) -> Option<(WindowId, usize)> {
        // SAFETY: plain queries.
        let root = unsafe { GetAncestor(WindowFromPoint(at), GA_ROOT) };
        let (id, win) = self.windows.iter().find(|(_, win)| win.hwnd == root)?;
        let mut client = at;
        // SAFETY: `client` lives through the call.
        if !unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(win.hwnd, &mut client) }
            .as_bool()
        {
            return Some((*id, usize::MAX));
        }
        let scale = win.dpi.get() as f32 / 96.0;
        let x = client.x as f32 / scale;
        let index = win
            .handle
            .with_strip(|strip| {
                strip
                    .tab_rects()
                    .iter()
                    .position(|rect: &Rect| x < rect.x.0 + rect.width.0 / 2.0)
                    .unwrap_or(strip.tabs().len())
            })
            .unwrap_or(usize::MAX);
        Some((*id, index))
    }

    /// A tab pulled out of its row was let go at this screen place.
    pub fn tab_dropped(&mut self, doc: DocId, at: POINT) {
        let Some(from) = self.store.window_of(doc) else {
            return;
        };
        match self.window_at(at) {
            Some((target, index)) if target != from => self.store.move_tab(doc, target, index),
            Some(_) => {}
            None => {
                let (width, height) = self
                    .store
                    .window_frame(from)
                    .map_or((800, 600), |(frame, _)| (frame.width, frame.height));
                let frame = WindowFrame {
                    x: at.x - width / 4,
                    y: at.y - 16,
                    width,
                    height,
                };
                if self.store.tabs(from).len() > 1 {
                    if let Some(new) = self.store.move_tab_to_new_window(doc) {
                        self.store
                            .set_window_frame(new, frame, ShowState::Normal, true);
                    }
                } else if let Some(win) = self.windows.get(&from)
                    && let Err(e) = place_window(win.hwnd, frame)
                {
                    self.note(format!("The window could not move: {e}"));
                }
            }
        }
    }

    /// A tab's row was dragged out. The pointer now belongs to this drag until it ends.
    pub fn begin_tab_drag(&mut self, window: WindowId, tab: TabId) {
        let Some(win) = self.windows.get(&window) else {
            return;
        };
        let Some((doc, _)) = win.tabs.iter().find(|(_, id)| *id == tab) else {
            return;
        };
        self.tab_drag.set(Some((window, *doc)));
        // SAFETY: a window of this thread.
        unsafe { SetCapture(win.hwnd) };
    }
}
