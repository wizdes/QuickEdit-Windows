//! The real window, always hidden, with real window messages and a renderer that paints
//! nothing. The renderer's own path is in `pictures/`.
mod support;

use qe_api::{
    Bitmap, CommandId, DisplayList, KeyBinding, PixelRect, RenderError, Renderer, SettingKey,
    TextEncoding, TextMeasurer, Theme, ThemeKind,
};
use qe_settings::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::*;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{IsWindowEnabled, SetKeyboardState};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Default)]
struct Counts {
    frames: Cell<usize>,
    presents: Cell<usize>,
    fail_frames: Cell<bool>,
}
struct NoPaint(Rc<Counts>);
impl Renderer for NoPaint {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        if self.0.fail_frames.get() {
            return Err(RenderError::new("no device"));
        }
        Ok(())
    }
    fn draw(&mut self, _: &DisplayList) -> Result<(), RenderError> {
        self.0.frames.set(self.0.frames.get() + 1);
        Ok(())
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
    fn resize(&mut self, _: u32, _: u32) -> Result<(), RenderError> {
        Ok(())
    }
    fn set_dpi(&mut self, _: u32) -> Result<(), RenderError> {
        Ok(())
    }
    fn render_to_bitmap(&mut self, _: &DisplayList, _: u32, _: u32) -> Result<Bitmap, RenderError> {
        Err(RenderError::new("not used"))
    }
    fn present_rect(&mut self, _: *mut std::ffi::c_void, _: PixelRect) -> Result<(), RenderError> {
        self.0.presents.set(self.0.presents.get() + 1);
        Ok(())
    }
}
struct TestHost {
    counts: Rc<Counts>,
    font_answer: RefCell<Result<Option<FontChoice>, String>>,
}
impl TestHost {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            counts: Rc::default(),
            font_answer: RefCell::new(Ok(None)),
        })
    }
}
impl SettingsHost for TestHost {
    fn measurer(&self) -> Arc<dyn TextMeasurer> {
        Arc::new(FixedMeasure)
    }
    fn theme(&self) -> Theme {
        Theme::new(ThemeKind::Light)
    }
    fn make_renderer(&self, _: u32, _: u32, _: u32) -> Result<Box<dyn Renderer>, String> {
        Ok(Box::new(NoPaint(self.counts.clone())))
    }
    fn choose_font(&self, _: Option<HWND>, _: &FontChoice) -> Result<Option<FontChoice>, String> {
        self.font_answer.borrow().clone()
    }
}
fn opened(name: &str) -> (SettingsWindow, SettingsStore, Rc<TestHost>, HWND) {
    let store = SettingsStore::open(settings_path(name));
    let host = TestHost::new();
    let mut window = SettingsWindow::new(store.clone(), host.clone());
    window.open_as(Presentation::Hidden).unwrap();
    let hwnd = window.hwnd().unwrap();
    // SAFETY: a plain query: the test never shows a window.
    assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool(), "hidden");
    (window, store, host, hwnd)
}
fn send(hwnd: HWND, message: u32, w: usize, l: isize) {
    // SAFETY: the window belongs to this thread.
    unsafe { SendMessageW(hwnd, message, Some(WPARAM(w)), Some(LPARAM(l))) };
}
fn pump() {
    let mut message = MSG::default();
    // SAFETY: a normal message loop on this thread.
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
fn pump_until(mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        pump();
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn tap(hwnd: HWND, key: u16) {
    send(hwnd, WM_KEYDOWN, key as usize, 1);
    send(hwnd, WM_KEYUP, key as usize, 1 | (1 << 30) | (1 << 31));
}
/// Tells this thread's key state that these keys are down, as `GetKeyState` will read.
fn hold_keys(keys: &[u16]) {
    let mut state = [0u8; 256];
    for key in keys {
        state[*key as usize] = 0x80;
    }
    // SAFETY: the array has the size the call needs.
    unsafe { SetKeyboardState(&state) }.unwrap();
}

#[test]
fn opening_twice_brings_the_same_window_forward() {
    let (mut window, _, _, hwnd) = opened("twice");
    window.open_as(Presentation::Hidden).unwrap();
    assert_eq!(window.hwnd(), Some(hwnd));
    assert_eq!(window.open_counts(), (1, 1));
    window.close();
    assert!(!window.is_open());
    window.open_as(Presentation::Hidden).unwrap();
    assert_eq!(window.open_counts(), (2, 1));
    assert_ne!(window.hwnd(), None);
}
#[test]
fn new_makes_no_window_and_paints_nothing() {
    let store = SettingsStore::open(settings_path("lazy"));
    let host = TestHost::new();
    let window = SettingsWindow::new(store, host.clone());
    assert!(!window.is_open());
    assert_eq!(window.hwnd(), None);
    assert_eq!(host.counts.frames.get(), 0);
}
#[test]
fn it_has_a_real_title_bar_with_a_close_button_and_a_fixed_size() {
    let (window, _, _, hwnd) = opened("title");
    // SAFETY: plain queries.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    assert_ne!(style & WS_CAPTION.0, 0);
    assert_ne!(style & WS_SYSMENU.0, 0);
    assert_eq!(style & WS_THICKFRAME.0, 0, "fixed size");
    let mut text = [0u16; 32];
    // SAFETY: the buffer is valid.
    let length = unsafe { GetWindowTextW(hwnd, &mut text) } as usize;
    assert_eq!(String::from_utf16_lossy(&text[..length]), "Settings");
    let (width, _) = window.client_size().unwrap();
    // SAFETY: a plain query.
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) } as f32;
    assert_eq!(width, (440.0 * dpi / 96.0).round() as i32);
}
#[test]
fn the_window_grows_for_keyboard_and_keeps_its_top_edge() {
    let (window, _, _, hwnd) = opened("grow");
    let (_, small) = window.client_size().unwrap();
    let top = |hwnd| {
        let mut rect = windows::Win32::Foundation::RECT::default();
        // SAFETY: the out pointer is valid.
        unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
        rect.top
    };
    let before = top(hwnd);
    // Shift+Tab from the Width field reaches the Wrap box, then the tab bar.
    hold_keys(&[0x10]);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    hold_keys(&[]);
    assert_eq!(
        window.with_panel(|p| p.focused_name()).unwrap().as_deref(),
        Some("Settings tabs")
    );
    tap(hwnd, 0x27);
    tap(hwnd, 0x27);
    assert_eq!(window.with_panel(|p| p.current_tab()), Some(TAB_KEYBOARD));
    let (_, tall) = window.client_size().unwrap();
    assert!(tall > small, "{tall} against {small}");
    assert!(
        top(hwnd) <= before,
        "the top stays unless the screen is too short"
    );
}
#[test]
fn real_keys_change_a_setting_that_persists_across_restart() {
    let (window, store, _, hwnd) = opened("persist");
    send(hwnd, WM_SETFOCUS, 0, 0);
    // Type a width in the first field, as WM_CHAR brings it.
    for (key, ch) in [(0x31u16, '1'), (0x32, '2'), (0x33, '3')] {
        send(hwnd, WM_KEYDOWN, key as usize, 1);
        send(hwnd, WM_CHAR, ch as usize, 1);
        send(hwnd, WM_KEYUP, key as usize, 1);
    }
    tap(hwnd, 0x0d);
    assert_eq!(
        store.get_integer(SettingKey::WindowWidthCharacters),
        Some(123)
    );
    store.flush().unwrap();
    drop(window);
    let restarted = SettingsStore::open(store.path());
    assert_eq!(
        restarted.get_integer(SettingKey::WindowWidthCharacters),
        Some(123)
    );
}
#[test]
fn closing_with_the_title_bar_keeps_what_was_typed() {
    let (window, store, _, hwnd) = opened("close-typed");
    send(hwnd, WM_SETFOCUS, 0, 0);
    send(hwnd, WM_KEYDOWN, 0x37, 1);
    send(hwnd, WM_CHAR, '7' as usize, 1);
    send(hwnd, WM_KEYUP, 0x37, 1);
    send(hwnd, WM_CLOSE, 0, 0);
    assert!(!window.is_open());
    assert_eq!(
        store.get_integer(SettingKey::WindowWidthCharacters),
        Some(7)
    );
    assert_eq!(window.open_counts(), (1, 0));
}
#[test]
fn esc_closes_the_window_and_it_can_open_again() {
    let (mut window, _, _, hwnd) = opened("esc-close");
    send(hwnd, WM_SETFOCUS, 0, 0);
    tap(hwnd, 0x1b);
    pump_until(|| !window.is_open());
    window.open_as(Presentation::Hidden).unwrap();
    assert!(window.is_open());
    assert_eq!(window.open_counts(), (2, 0));
}
#[test]
fn alt_f4_closes_the_window_unless_a_key_is_being_recorded() {
    let (window, store, _, hwnd) = opened("alt-f4");
    send(hwnd, WM_SETFOCUS, 0, 0);
    // Go to the keyboard table and start recording.
    hold_keys(&[0x11]);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    hold_keys(&[]);
    assert_eq!(window.with_panel(|p| p.current_tab()), Some(TAB_KEYBOARD));
    for _ in 0..4 {
        if window.with_panel(|p| p.focused_name()).unwrap().as_deref() == Some("Keyboard shortcuts")
        {
            break;
        }
        tap(hwnd, 0x09);
    }
    tap(hwnd, 0x0d);
    assert_eq!(
        window.with_panel(|p| p.table().unwrap().is_recording()),
        Some(true)
    );

    let alt = 1 | (1 << 29);
    send(hwnd, WM_SYSKEYDOWN, 0x12, alt);
    send(hwnd, WM_SYSKEYDOWN, 0x73, alt);
    assert!(window.is_open(), "while recording, Alt+F4 is only refused");
    assert_eq!(
        window.with_panel(|p| p.key_message()).as_deref(),
        Some("Alt+F4 is kept by Windows.")
    );
    send(hwnd, WM_SYSKEYUP, 0x73, alt);
    send(hwnd, WM_SYSKEYUP, 0x12, 1);
    tap(hwnd, 0x1b);
    assert_eq!(
        window.with_panel(|p| p.table().unwrap().is_recording()),
        Some(false)
    );
    assert_eq!(store.write_error(), None);

    // With no recording, the panel leaves the chord (`reserved_keys_are_refused...`) and the
    // window passes it on. `DefWindowProc` then sends this, but not to a hidden window that
    // is not active, so this test sends it itself.
    send(hwnd, WM_SYSKEYDOWN, 0x12, alt);
    send(hwnd, WM_SYSKEYDOWN, 0x73, alt);
    assert!(window.is_open(), "the panel itself closes nothing");
    send(hwnd, WM_SYSCOMMAND, SC_CLOSE as usize, 0);
    pump_until(|| !window.is_open());
}
#[test]
fn a_chord_recorded_through_real_messages_reaches_the_key_table() {
    let (window, store, _, hwnd) = opened("chord");
    send(hwnd, WM_SETFOCUS, 0, 0);
    hold_keys(&[0x11]);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    hold_keys(&[]);
    for _ in 0..4 {
        if window.with_panel(|p| p.focused_name()).unwrap().as_deref() == Some("Keyboard shortcuts")
        {
            break;
        }
        tap(hwnd, 0x09);
    }
    let first = window
        .with_panel(|p| p.table().unwrap().selected_command())
        .flatten()
        .unwrap();
    tap(hwnd, 0x0d);
    hold_keys(&[0x11, 0x10]);
    send(hwnd, WM_KEYDOWN, 0x11, 1);
    send(hwnd, WM_KEYDOWN, 0x10, 1);
    send(hwnd, WM_KEYDOWN, 0x47, 1);
    send(hwnd, WM_KEYUP, 0x47, 1);
    hold_keys(&[]);
    send(hwnd, WM_KEYUP, 0x10, 1);
    send(hwnd, WM_KEYUP, 0x11, 1);
    let want = KeyBinding::new(0x47, true, false, true);
    assert_eq!(store.key_of(first), Some(want));
    assert_eq!(store.command_for_key(want), Some(first));
}
fn press_change_button(window: &SettingsWindow, hwnd: HWND) {
    send(hwnd, WM_SETFOCUS, 0, 0);
    for _ in 0..16 {
        if window.with_panel(|p| p.focused_name()).unwrap().as_deref() == Some("Change\u{2026}") {
            tap(hwnd, 0x20);
            return;
        }
        tap(hwnd, 0x09);
    }
    panic!("Tab never reached the Change button");
}
#[test]
fn the_change_button_takes_a_font_from_the_host_and_shows_it() {
    let (window, store, host, hwnd) = opened("font");
    *host.font_answer.borrow_mut() = Ok(Some(FontChoice {
        family: "Consolas".into(),
        points: 14.0,
    }));
    press_change_button(&window, hwnd);
    assert_eq!(store.font().display_text(), "Consolas 14 pt");
    assert_eq!(
        window.with_panel(|p| p.font_text()).as_deref(),
        Some("Consolas 14 pt")
    );
}
#[test]
fn a_cancelled_font_dialog_changes_nothing_and_a_failed_one_says_so() {
    let (window, store, host, hwnd) = opened("font-fail");
    press_change_button(&window, hwnd);
    assert_eq!(store.font().family, "Cascadia Mono");
    assert_eq!(window.with_panel(|p| p.status_text()).as_deref(), Some(""));
    *host.font_answer.borrow_mut() = Err("The font dialog could not open (code 0x2).".into());
    tap(hwnd, 0x20);
    assert_eq!(
        window.with_panel(|p| p.status_text()).as_deref(),
        Some("The font dialog could not open (code 0x2).")
    );
    assert_eq!(store.font().family, "Cascadia Mono");
}
#[test]
fn a_failed_write_reaches_the_window_with_no_wait_and_no_timer() {
    let path = settings_path("write-fail");
    std::fs::write(&path, "").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    let store = SettingsStore::open(&path);
    let host = TestHost::new();
    let mut window = SettingsWindow::new(store.clone(), host);
    window.open_as(Presentation::Hidden).unwrap();
    let hwnd = window.hwnd().unwrap();
    send(hwnd, WM_SETFOCUS, 0, 0);
    tap(hwnd, 0x09);
    tap(hwnd, 0x09);
    tap(hwnd, 0x09);
    tap(hwnd, 0x09);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    // The writer's own message brings the error: this loop only runs messages.
    pump_until(|| {
        window
            .with_panel(|p| p.status_text())
            .is_some_and(|text| text.contains("read-only"))
    });
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    pump_until(|| window.with_panel(|p| p.status_text()).as_deref() == Some(""));
    assert_eq!(store.write_error(), None);
}
#[test]
fn a_change_from_elsewhere_shows_in_the_open_window() {
    let (window, store, host, _) = opened("from-elsewhere");
    let frames = host.counts.frames.get();
    store.set_bool(SettingKey::ShowLineNumbers, true).unwrap();
    assert!(host.counts.frames.get() > frames, "it repainted");
    let tree = window.widgets().unwrap();
    fn checked(node: &qe_api::Widget, name: &str) -> Option<bool> {
        if node.name == name {
            return Some(node.states.checked);
        }
        node.children.iter().find_map(|child| checked(child, name))
    }
    assert_eq!(checked(&tree, "Show line numbers"), Some(true));
}
#[test]
fn nothing_paints_at_idle() {
    let (_window, _, host, _) = opened("idle");
    pump();
    let frames = host.counts.frames.get();
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        host.counts.frames.get(),
        frames,
        "a hidden idle window paints nothing"
    );
}
#[test]
fn customize_encodings_opens_a_modal_panel_that_changes_the_lists() {
    let (window, store, _, hwnd) = opened("customize");
    send(hwnd, WM_SETFOCUS, 0, 0);
    hold_keys(&[0x11]);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    hold_keys(&[]);
    assert_eq!(
        window.with_panel(|p| p.current_tab()),
        Some(TAB_OPEN_AND_SAVE)
    );
    for _ in 0..6 {
        if window.with_panel(|p| p.focused_name()).unwrap().as_deref()
            == Some("Encoding for opening files")
        {
            break;
        }
        tap(hwnd, 0x09);
    }
    // Open the list, go up to the last row, take it.
    tap(hwnd, 0x20);
    tap(hwnd, 0x26);
    tap(hwnd, 0x0d);
    let panel = window.encoding_panel_hwnd().expect("the panel opened");
    let tree = window.encoding_panel_widgets().expect("its widget tree");
    assert_eq!(tree.name, "Customize Encodings List");
    // SAFETY: plain queries: the panel is hidden too, and its owner is disabled.
    unsafe {
        assert!(!IsWindowVisible(panel).as_bool());
        assert!(!IsWindowEnabled(hwnd).as_bool(), "the panel is modal");
    }
    // Space flips the first row of the list in the panel: UTF-8, then the rest stay.
    send(panel, WM_SETFOCUS, 0, 0);
    let before = store.enabled_encodings();
    assert!(before.contains(&TextEncoding::Utf8));
    tap(panel, 0x20);
    assert!(!store.enabled_encodings().contains(&TextEncoding::Utf8));
    send(panel, WM_CLOSE, 0, 0);
    // SAFETY: a plain query.
    assert!(
        unsafe { IsWindowEnabled(hwnd) }.as_bool(),
        "Settings is back"
    );
    store.flush().unwrap();
    let again = SettingsStore::open(store.path());
    assert!(!again.enabled_encodings().contains(&TextEncoding::Utf8));
    assert_eq!(store.key_of(CommandId::New), CommandId::New.default_key());
}
#[test]
fn announcements_are_there_for_the_app_to_speak() {
    let (window, _, _, hwnd) = opened("speak");
    send(hwnd, WM_SETFOCUS, 0, 0);
    // A size that is refused says why.
    send(hwnd, WM_KEYDOWN, 0x30, 1);
    send(hwnd, WM_CHAR, '0' as usize, 1);
    send(hwnd, WM_KEYUP, 0x30, 1);
    tap(hwnd, 0x0d);
    let said = window.take_announcements();
    assert_eq!(said, ["Width must be a whole number from 1 to 10000."]);
    assert!(window.take_announcements().is_empty());
}

type InDialog = Box<dyn Fn(&SettingsWindow, HWND)>;
/// A host that sees every message and can act inside the font dialog and the announcement.
struct WatchingHost {
    counts: Rc<Counts>,
    messages: RefCell<Vec<(u32, usize)>>,
    theme: Cell<Option<ThemeKind>>,
    window: Cell<*const SettingsWindow>,
    in_dialog: RefCell<Option<InDialog>>,
    dialogs: Cell<usize>,
    trees_during_announce: RefCell<Vec<bool>>,
}
impl WatchingHost {
    fn the_window(&self) -> &SettingsWindow {
        // SAFETY: set in `watched` to a boxed window that the test keeps alive, and only read.
        unsafe { &*self.window.get() }
    }
}
impl SettingsHost for WatchingHost {
    fn measurer(&self) -> Arc<dyn TextMeasurer> {
        Arc::new(FixedMeasure)
    }
    fn theme(&self) -> Theme {
        Theme::new(self.theme.get().unwrap_or(ThemeKind::Light))
    }
    fn make_renderer(&self, _: u32, _: u32, _: u32) -> Result<Box<dyn Renderer>, String> {
        Ok(Box::new(NoPaint(self.counts.clone())))
    }
    fn choose_font(
        &self,
        owner: Option<HWND>,
        _: &FontChoice,
    ) -> Result<Option<FontChoice>, String> {
        self.dialogs.set(self.dialogs.get() + 1);
        if let Some(act) = self.in_dialog.borrow().as_ref() {
            act(self.the_window(), owner.unwrap());
        }
        Ok(None)
    }
    fn window_message(&self, _: HWND, message: u32, w: WPARAM, _: LPARAM) -> Option<LRESULT> {
        self.messages.borrow_mut().push((message, w.0));
        None
    }
    fn announce(&self, _: HWND, _: &str) {
        let tree = self.the_window().widgets();
        self.trees_during_announce.borrow_mut().push(tree.is_some());
    }
}
fn watched(store: SettingsStore) -> (Box<SettingsWindow>, Rc<WatchingHost>, HWND) {
    let host = Rc::new(WatchingHost {
        counts: Rc::default(),
        messages: RefCell::default(),
        theme: Cell::new(None),
        window: Cell::new(std::ptr::null()),
        in_dialog: RefCell::new(None),
        dialogs: Cell::new(0),
        trees_during_announce: RefCell::default(),
    });
    let mut window = Box::new(SettingsWindow::new(store, host.clone()));
    window.open_as(Presentation::Hidden).unwrap();
    host.window.set(&*window);
    let hwnd = window.hwnd().unwrap();
    (window, host, hwnd)
}
fn tab_to_table(window: &SettingsWindow, hwnd: HWND) {
    send(hwnd, WM_SETFOCUS, 0, 0);
    hold_keys(&[0x11]);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    hold_keys(&[]);
    for _ in 0..4 {
        if window.with_panel(|p| p.focused_name()).unwrap().as_deref() == Some("Keyboard shortcuts")
        {
            return;
        }
        tap(hwnd, 0x09);
    }
    panic!("Tab never reached the table");
}
fn start_recording(window: &SettingsWindow, hwnd: HWND) {
    tab_to_table(window, hwnd);
    tap(hwnd, 0x0d);
    assert!(is_recording(window));
}
fn is_recording(window: &SettingsWindow) -> bool {
    window
        .with_panel(|p| p.table().unwrap().is_recording())
        .unwrap()
}
fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: the out pointer is valid.
    unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
    rect
}
const SYS_KEY: isize = 1 | (1 << 29);
#[test]
fn wm_killfocus_ends_a_recording() {
    let (window, _, _, hwnd) = opened("real-killfocus");
    start_recording(&window, hwnd);
    send(hwnd, WM_KILLFOCUS, 0, 0);
    assert!(!is_recording(&window));
}
#[test]
fn wm_killfocus_commits_a_typed_size() {
    let (_window, store, _, hwnd) = opened("real-killfocus-typed");
    send(hwnd, WM_SETFOCUS, 0, 0);
    send(hwnd, WM_CHAR, '7' as usize, 1);
    send(hwnd, WM_KILLFOCUS, 0, 0);
    assert_eq!(
        store.get_integer(SettingKey::WindowWidthCharacters),
        Some(7)
    );
    store.flush().unwrap();
}
#[test]
fn wm_dpichanged_takes_the_rectangle_windows_suggests() {
    let (window, _, _, hwnd) = opened("real-dpi");
    // A place on the work area, away from where the window opened.
    let suggested = {
        let now = window_rect(hwnd);
        let (left, top) = (now.left + 37, now.top + 23);
        RECT {
            left,
            top,
            right: left + 700,
            bottom: top + 700,
        }
    };
    send(
        hwnd,
        WM_DPICHANGED,
        (144usize << 16) | 144,
        &suggested as *const RECT as isize,
    );
    let (width, height) = window.client_size().unwrap();
    assert_eq!(width, 660, "440 at 150%");
    assert_eq!(height, 660, "440 at 150%");
    let after = window_rect(hwnd);
    assert_eq!(
        (after.left, after.top),
        (suggested.left, suggested.top),
        "the place Windows asked for"
    );
}
#[test]
fn a_window_taller_than_the_screen_does_not_move_on_each_paint() {
    let (window, store, _, hwnd) = opened("short-screen");
    // At this size no screen has room, so the height is clamped to the work area.
    let dpi = 96 * 12;
    send(hwnd, WM_DPICHANGED, (dpi << 16) | dpi, 0);
    let (_, clamped) = window.client_size().unwrap();
    assert!(clamped < 440 * 12, "{clamped}");
    // The user drags it. A paint must leave it there.
    let before = window_rect(hwnd);
    // SAFETY: a plain call on this test's own hidden window.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            before.left,
            before.top + 40,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .unwrap();
    let dragged = window_rect(hwnd);
    store.set_bool(SettingKey::ShowLineNumbers, true).unwrap();
    assert_eq!(window_rect(hwnd).top, dragged.top);
    store.flush().unwrap();
}
#[test]
fn an_accepted_alt_shortcut_does_not_beep() {
    let store = SettingsStore::open(settings_path("sys-char"));
    let (window, host, hwnd) = watched(store.clone());
    let menu_key_calls = |host: &WatchingHost| {
        host.messages
            .borrow()
            .iter()
            .filter(|(message, w)| *message == WM_SYSCOMMAND && *w & 0xfff0 == SC_KEYMENU as usize)
            .count()
    };
    // With no recording, this is how Windows answers: it looks for a menu key and beeps.
    send(hwnd, WM_SYSCHAR, 'k' as usize, SYS_KEY);
    assert_eq!(menu_key_calls(&host), 1, "the test can see the beep's path");

    start_recording(&window, hwnd);
    let first = window
        .with_panel(|p| p.table().unwrap().selected_command())
        .flatten()
        .unwrap();
    hold_keys(&[0x12]);
    send(hwnd, WM_SYSKEYDOWN, 0x12, SYS_KEY);
    send(hwnd, WM_SYSKEYDOWN, 0x4b, SYS_KEY);
    assert_eq!(
        store.key_of(first),
        Some(KeyBinding::new(0x4b, false, true, false))
    );
    assert!(!is_recording(&window));
    send(hwnd, WM_SYSCHAR, 'k' as usize, SYS_KEY);
    // A held key repeats, and each repeat brings its own character.
    send(hwnd, WM_SYSKEYDOWN, 0x4b, SYS_KEY | (1 << 30));
    send(hwnd, WM_SYSCHAR, 'k' as usize, SYS_KEY);
    send(hwnd, WM_SYSKEYUP, 0x4b, SYS_KEY);
    send(hwnd, WM_SYSKEYUP, 0x12, 1);
    assert_eq!(menu_key_calls(&host), 1, "no new call: no beep");
    // A later Alt key is normal again.
    send(hwnd, WM_SYSKEYDOWN, 0x12, SYS_KEY);
    send(hwnd, WM_SYSKEYDOWN, 0x51, SYS_KEY);
    send(hwnd, WM_SYSCHAR, 'q' as usize, SYS_KEY);
    hold_keys(&[]);
    assert_eq!(menu_key_calls(&host), 2);
    store.flush().unwrap();
}
#[test]
fn a_click_on_the_title_bar_ends_a_recording() {
    let store = SettingsStore::open(settings_path("title-click"));
    let (window, _host, hwnd) = watched(store);
    start_recording(&window, hwnd);
    // The frame's edge, not the caption: `DefWindowProc` starts no drag loop for it here,
    // and the window treats every press outside the painted area the same.
    send(hwnd, WM_NCLBUTTONDOWN, 18, 0);
    assert!(!is_recording(&window));
}
#[test]
fn a_closed_window_paints_nothing() {
    let (window, store, host, hwnd) = opened("closed-paint");
    send(hwnd, WM_CLOSE, 0, 0);
    assert!(!window.is_open());
    let frames = host.counts.frames.get();
    for i in 0..5 {
        store.set_bool(SettingKey::SmartQuotes, i % 2 == 0).unwrap();
    }
    store.flush().unwrap();
    pump();
    assert_eq!(host.counts.frames.get(), frames);
}
#[test]
fn a_paint_problem_goes_with_the_next_good_paint() {
    let (window, store, host, _) = opened("paint-problem");
    host.counts.fail_frames.set(true);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    let shown = window.with_panel(|p| p.status_text()).unwrap();
    assert!(shown.contains("Painting failed"), "{shown}");
    host.counts.fail_frames.set(false);
    store.set_bool(SettingKey::SmartQuotes, false).unwrap();
    assert_eq!(window.with_panel(|p| p.status_text()).unwrap(), "");
    store.flush().unwrap();
}
/// The review's probe, with every message it said was lost. `ChooseFont` runs its own
/// message loop on this thread; this stands in for it, because no window may show.
#[test]
fn messages_during_the_font_dialog_still_reach_settings() {
    let path = settings_path("nested");
    std::fs::write(&path, "TabWidth=4\r\n").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    let store = SettingsStore::open(&path);
    let (window, host, hwnd) = watched(store.clone());
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    let (inner, log, watcher) = (store.clone(), seen.clone(), Rc::downgrade(&host));
    *host.in_dialog.borrow_mut() = Some(Box::new(move |window, owner| {
        // A write that fails while the dialog is up shows at once.
        inner.set_bool(SettingKey::SmartQuotes, true).unwrap();
        pump_until(|| {
            window
                .with_panel(|p| p.status_text())
                .is_some_and(|text| text.contains("read-only"))
        });
        log.borrow_mut().push("write error".into());
        // Windows tells the window it lost the keyboard: no control keeps its focus mark.
        let marks =
            |window: &SettingsWindow| qe_ui::focused_nodes(&window.widgets().unwrap()).len();
        assert_eq!(marks(window), 1);
        send(owner, WM_KILLFOCUS, 0, 0);
        assert_eq!(marks(window), 0);
        log.borrow_mut().push("focus".into());
        // The DPI and the theme change.
        send(owner, WM_DPICHANGED, (144usize << 16) | 144, 0);
        assert_eq!(window.client_size().unwrap().0, 660);
        log.borrow_mut().push("dpi".into());
        let host = watcher.upgrade().unwrap();
        host.theme.set(Some(ThemeKind::Dark));
        let frames = host.counts.frames.get();
        send(owner, WM_THEMECHANGED, 0, 0);
        assert!(
            host.counts.frames.get() > frames,
            "the theme change painted"
        );
        log.borrow_mut().push("theme".into());
    }));
    send(hwnd, WM_SETFOCUS, 0, 0);
    press_change_button(&window, hwnd);
    assert_eq!(*seen.borrow(), ["write error", "focus", "dpi", "theme"]);
    // After the dialog the window still answers.
    assert!(
        window
            .with_panel(|p| p.status_text())
            .unwrap()
            .contains("read-only")
    );
    *host.in_dialog.borrow_mut() = None;
    send(hwnd, WM_SETFOCUS, 0, 0);
    tap(hwnd, 0x20);
    assert_eq!(host.dialogs.get(), 2);
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
}
#[test]
fn a_second_press_during_the_font_dialog_opens_no_second_one() {
    let store = SettingsStore::open(settings_path("one-dialog"));
    let (window, host, hwnd) = watched(store);
    *host.in_dialog.borrow_mut() = Some(Box::new(|_, owner| tap(owner, 0x20)));
    press_change_button(&window, hwnd);
    assert_eq!(host.dialogs.get(), 1);
}
#[test]
fn the_widget_tree_answers_during_an_announcement() {
    let store = SettingsStore::open(settings_path("announce-tree"));
    let (window, host, hwnd) = watched(store);
    send(hwnd, WM_SETFOCUS, 0, 0);
    send(hwnd, WM_CHAR, '0' as usize, 1);
    tap(hwnd, 0x0d);
    assert_eq!(*host.trees_during_announce.borrow(), [true]);
    assert_eq!(window.take_announcements().len(), 1);
}
fn open_encoding_panel(window: &SettingsWindow, hwnd: HWND) -> HWND {
    send(hwnd, WM_SETFOCUS, 0, 0);
    hold_keys(&[0x11]);
    send(hwnd, WM_KEYDOWN, 0x09, 1);
    send(hwnd, WM_KEYUP, 0x09, 1);
    hold_keys(&[]);
    for _ in 0..6 {
        if window.with_panel(|p| p.focused_name()).unwrap().as_deref()
            == Some("Encoding for opening files")
        {
            break;
        }
        tap(hwnd, 0x09);
    }
    tap(hwnd, 0x20);
    tap(hwnd, 0x26);
    tap(hwnd, 0x0d);
    window.encoding_panel_hwnd().expect("the panel opened")
}
#[test]
fn the_encoding_panel_follows_the_dpi_and_the_theme() {
    let store = SettingsStore::open(settings_path("encoding-dpi"));
    let (window, host, hwnd) = watched(store);
    let panel = open_encoding_panel(&window, hwnd);
    let client = |hwnd| {
        let mut rect = RECT::default();
        // SAFETY: the out pointer is valid.
        unsafe { GetClientRect(hwnd, &mut rect) }.unwrap();
        (rect.right, rect.bottom)
    };
    let (width, height) = client(panel);
    let frames = host.counts.frames.get();
    send(panel, WM_DPICHANGED, (192usize << 16) | 192, 0);
    assert_eq!(client(panel), (width * 2, height * 2));
    assert!(host.counts.frames.get() > frames, "it repainted");
    let frames = host.counts.frames.get();
    host.theme.set(Some(ThemeKind::Dark));
    send(panel, WM_THEMECHANGED, 0, 0);
    assert!(host.counts.frames.get() > frames, "it repainted");
    send(panel, WM_CLOSE, 0, 0);
}
