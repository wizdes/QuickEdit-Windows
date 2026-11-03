//! The whole menu path: a hidden real window, real posted messages and the real message
//! loop (`run_message_loop`). The first menu bar's tests called the bar and the menu
//! apart, and two bugs lived in between them: a key reached both, so a chosen command
//! opened its menu again, and a nested loop in the owner held back `WM_CLOSE`.
//!
//! A test is a script. Each step runs on a timer tick, and `WM_TIMER` comes only when the
//! queue is empty, so a step sees every effect of the step before it. A panic must not
//! cross a window procedure, so that steps note what they find and `finish` reports it.
use qe_api::{
    Bitmap, CommandId, Dip, DisplayList, DrawCommand, InputKind, PixelRect, Rect, RenderError,
    Renderer, View,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_shell::{
    ShellError, ShellEvent, ShellWindow, WindowConfig, quit_message_loop, run_message_loop,
};
use qe_ui::{
    MenuModel, UiTokens, foreground_changed_for_test, ignore_foreground_changes_for_test,
    menu_bar_groups,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, SetKeyboardState};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

mod support;
use support::RealMeasurer;

const SCRIPT_TIMER: u32 = 900;
const OTHER_TIMER: u32 = 901;
const ALT_BIT: isize = 1 << 29;
const REPEAT_BIT: isize = 1 << 30;
const VK_ALT: usize = 0x12;
const VK_ENTER: usize = 0x0d;
const VK_ESC: usize = 0x1b;
const VK_LEFT: usize = 0x25;
const VK_RIGHT: usize = 0x27;
const VK_DOWN: usize = 0x28;
const VK_F10: usize = 0x79;
const VK_F11: usize = 0x7a;
const VK_F4: usize = 0x73;
const VK_SPACE: usize = 0x20;
const SC_KEYMENU_VALUE: u32 = 0xf100;
/// How long one script may take. A hang ends here, with a failed test.
const TIME_LIMIT: Duration = Duration::from_secs(30);

fn pack(x: i32, y: i32) -> isize {
    ((y as i16 as u16 as u32) << 16 | (x as i16 as u16 as u32)) as i32 as isize
}

unsafe extern "system" fn note_popup(hwnd: HWND, list: LPARAM) -> BOOL {
    let mut name = [0u16; 64];
    let length = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
    if String::from_utf16_lossy(&name[..length]) == "QuickEditPopup" {
        let list = unsafe { &mut *(list.0 as *mut Vec<HWND>) };
        list.push(hwnd);
    }
    BOOL(1)
}

type Step = Box<dyn FnOnce(&Script)>;
fn step(f: impl FnOnce(&Script) + 'static) -> Step {
    Box::new(f)
}
fn wait(ms: u64) -> Step {
    step(move |s| {
        *s.wake_at.borrow_mut() = Some(Instant::now() + Duration::from_millis(ms));
    })
}

struct Script {
    window: ShellWindow,
    hwnd: HWND,
    thread: u32,
    steps: RefCell<VecDeque<Step>>,
    wake_at: RefCell<Option<Instant>>,
    seen: RefCell<Vec<ShellEvent>>,
    failures: RefCell<Vec<String>>,
    destroyed: RefCell<bool>,
}
impl Script {
    /// All windows of the thread's popup class, shown or not, alive at this moment.
    fn popups(&self) -> Vec<HWND> {
        let mut list: Vec<HWND> = Vec::new();
        // SAFETY: `list` outlives the call, and the callback runs inside it.
        let _ = unsafe {
            EnumThreadWindows(
                self.thread,
                Some(note_popup),
                LPARAM((&raw mut list) as isize),
            )
        };
        list
    }
    /// The first level: the popup that this window owns.
    fn root_popup(&self) -> Option<HWND> {
        self.popups().into_iter().find(|popup| {
            unsafe { GetWindow(*popup, GW_OWNER) }.is_ok_and(|owner| owner == self.hwnd)
        })
    }
    fn bar_item(&self, field: impl Fn(&qe_api::WidgetStates) -> bool) -> Option<usize> {
        let tree = self.window.widgets().unwrap();
        let bar = tree.children.iter().find(|c| c.name == "Menu bar").unwrap();
        bar.children.iter().position(|item| field(&item.states))
    }
    fn focused_label(&self) -> Option<usize> {
        self.bar_item(|states| states.focused)
    }
    fn open_label(&self) -> Option<usize> {
        self.bar_item(|states| states.expanded)
    }
    fn label(&self, index: usize) -> Rect {
        let tree = self.window.widgets().unwrap();
        let bar = tree.children.iter().find(|c| c.name == "Menu bar").unwrap();
        bar.children[index].bounds
    }
    fn post(&self, msg: u32, w: usize, l: isize) {
        self.post_to(self.hwnd, msg, w, l);
    }
    fn post_to(&self, hwnd: HWND, msg: u32, w: usize, l: isize) {
        // SAFETY: Windows checks the handle.
        unsafe { PostMessageW(Some(hwnd), msg, WPARAM(w), LPARAM(l)) }.unwrap();
    }
    fn key(&self, vk: usize) {
        self.post(WM_KEYDOWN, vk, 1);
    }
    fn key_up(&self, vk: usize) {
        self.post(WM_KEYUP, vk, 1 | (1 << 30) | (1 << 31));
    }
    fn alt_tap(&self) {
        self.post(WM_SYSKEYDOWN, VK_ALT, ALT_BIT | 1);
        self.post(WM_SYSKEYUP, VK_ALT, 1 | (1 << 30) | (1 << 31));
    }
    fn alt_letter(&self, letter: u8) {
        self.post(WM_SYSKEYDOWN, VK_ALT, ALT_BIT | 1);
        self.post(WM_SYSKEYDOWN, usize::from(letter), ALT_BIT | 1);
    }
    /// A typed letter: `TranslateMessage` in the real loop makes its `WM_CHAR`.
    fn type_x(&self) {
        self.key(usize::from(b'X'));
        self.key_up(usize::from(b'X'));
    }
    /// A point in the window's client area, for a message posted to a popup that holds
    /// the mouse: Windows would give it this popup's own client position.
    fn on_popup(&self, popup: HWND, x: f32, y: f32) -> isize {
        let mut at = POINT {
            x: x as i32,
            y: y as i32,
        };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut at);
            let _ = ScreenToClient(popup, &mut at);
        }
        pack(at.x, at.y)
    }
    fn click_label(&self, index: usize) {
        let r = self.label(index);
        let at = pack(r.x.0 as i32 + 4, r.y.0 as i32 + 10);
        self.post(WM_LBUTTONDOWN, 1, at);
        self.post(WM_LBUTTONUP, 0, at);
    }
    /// A click in the text area while a menu is open. The popup holds the mouse.
    fn click_text_through_popup(&self) {
        let popup = self.root_popup().expect("a menu is open");
        let (x, y) = self.text_point();
        let at = self.on_popup(popup, x, y);
        self.post_to(popup, WM_LBUTTONDOWN, 1, at);
        self.post_to(popup, WM_LBUTTONUP, 0, at);
    }
    /// A point in the text area, away from where a menu lies.
    fn text_point(&self) -> (f32, f32) {
        let text = self.window.content_rect().unwrap();
        (
            text.x.0 + text.width.0 - 30.0,
            text.y.0 + text.height.0 - 10.0,
        )
    }
    fn click_text(&self) {
        let (x, y) = self.text_point();
        let at = pack(x as i32, y as i32);
        self.post(WM_LBUTTONDOWN, 1, at);
        self.post(WM_LBUTTONUP, 0, at);
    }
    fn expect(&self, ok: bool, what: impl Into<String>) {
        if !ok {
            self.failures.borrow_mut().push(what.into());
        }
    }
    fn expect_popups(&self, count: usize, when: &str) {
        let now = self.popups().len();
        self.expect(
            now == count,
            format!("{when}: {now} popups, wanted {count}"),
        );
    }
    /// No menu, no bar focus: what a typed key needs to reach the text.
    fn expect_text_has_keys(&self, when: &str) {
        self.expect_popups(0, when);
        self.expect(
            !self.window.menu_is_open().unwrap(),
            format!("{when}: the window says a menu is open"),
        );
        self.expect(
            self.focused_label().is_none(),
            format!(
                "{when}: the bar holds the focus: {:?}",
                self.focused_label()
            ),
        );
        self.expect(
            self.open_label().is_none(),
            format!("{when}: a label says it is open: {:?}", self.open_label()),
        );
    }
    fn typed(&self) -> Vec<char> {
        self.seen
            .borrow()
            .iter()
            .filter_map(|event| match event {
                ShellEvent::ContentInput(input) => match input.kind {
                    InputKind::Char(c) => Some(c),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
    fn commands(&self) -> Vec<CommandId> {
        self.seen
            .borrow()
            .iter()
            .filter_map(|event| match event {
                ShellEvent::RunCommand(command) => Some(*command),
                _ => None,
            })
            .collect()
    }
    fn group_opens(&self) -> Vec<usize> {
        self.seen
            .borrow()
            .iter()
            .filter_map(|event| match event {
                ShellEvent::OpenMenuGroup(index, _) => Some(*index),
                _ => None,
            })
            .collect()
    }
    fn count_timers(&self, id: u32) -> usize {
        let seen = self.seen.borrow();
        seen.iter()
            .filter(|event| matches!(event, ShellEvent::Timer(t) if *t == id))
            .count()
    }
    fn count_wakes(&self) -> usize {
        let seen = self.seen.borrow();
        seen.iter()
            .filter(|event| matches!(event, ShellEvent::Wake))
            .count()
    }
    fn was_destroyed(&self) -> bool {
        *self.destroyed.borrow()
    }
    /// A typed `x` lands in the text as one character, and nothing else came with it.
    fn then_typing_reaches_text(steps: &mut Vec<Step>, when: &'static str) {
        steps.push(step(move |s| {
            s.expect_text_has_keys(when);
            let before = s.typed();
            s.expect(
                before.iter().all(|c| !c.is_control()),
                format!("{when}: a control character reached the text: {before:?}"),
            );
            s.type_x();
            s.seen.borrow_mut().push(ShellEvent::FocusContent);
        }));
        steps.push(step(move |s| {
            let typed = s.typed();
            s.expect(
                typed.last() == Some(&'x'),
                format!("{when}: the typed key did not reach the text: {typed:?}"),
            );
            s.expect_popups(0, when);
        }));
    }
}

/// A hidden window whose owner does just what QuickEdit's owner does.
/// A menu the owner opens by itself, as a context menu is. Format is the shortest group.
fn owner_menu() -> MenuModel {
    menu_bar_groups()[2].submenu.clone().unwrap()
}
fn script(record_system_commands: bool) -> Rc<Script> {
    script_at(record_system_commands, None)
}
fn script_at(record_system_commands: bool, fake_dpi: Option<u32>) -> Rc<Script> {
    ignore_foreground_changes_for_test();
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            record_system_commands,
            fake_dpi,
            ..Default::default()
        },
        Box::new(BitmapRenderer::new(64, 64).unwrap()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    window.take_events().unwrap();
    window
        .set_popup_renderers(|width, height| {
            let mut renderer = BitmapRenderer::new(width, height)?;
            renderer.set_text_settings(TextSettings::reference())?;
            Ok(Box::new(renderer) as Box<dyn Renderer>)
        })
        .unwrap();
    let hwnd = HWND(window.hwnd().unwrap());
    // SAFETY: a plain query.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
    let s = Rc::new(Script {
        window,
        hwnd,
        thread,
        steps: RefCell::default(),
        wake_at: RefCell::default(),
        seen: RefCell::default(),
        failures: RefCell::default(),
        destroyed: RefCell::new(false),
    });
    let weak = Rc::downgrade(&s);
    s.window.set_event_handler(move |handle, event| {
        let Some(s) = weak.upgrade() else { return };
        s.seen.borrow_mut().push(event.clone());
        match event {
            ShellEvent::OpenMenuGroup(index, anchor) => {
                let model = menu_bar_groups()[index].submenu.clone().unwrap();
                if let Err(problem) = handle.open_menu(model, anchor) {
                    s.failures.borrow_mut().push(problem.to_string());
                }
            }
            ShellEvent::Timer(SCRIPT_TIMER) => {
                if let Some(at) = *s.wake_at.borrow()
                    && Instant::now() < at
                {
                    return;
                }
                *s.wake_at.borrow_mut() = None;
                let next = s.steps.borrow_mut().pop_front();
                match next {
                    Some(next) => next(&s),
                    None => quit_message_loop(0),
                }
            }
            ShellEvent::CloseWindowRequested => {
                if let Err(problem) = handle.destroy() {
                    s.failures.borrow_mut().push(problem.to_string());
                }
            }
            ShellEvent::WindowDestroyed => {
                *s.destroyed.borrow_mut() = true;
                quit_message_loop(0);
            }
            _ => {}
        }
    });
    s
}

/// Runs the script through the real message loop, then reports what its steps found.
fn finish(s: &Rc<Script>, steps: Vec<Step>) {
    s.steps.borrow_mut().extend(steps);
    s.window.set_timer(SCRIPT_TIMER, 1).unwrap();
    let thread = s.thread;
    let (done, stop) = channel::<()>();
    let (late_tx, late) = channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if stop.recv_timeout(TIME_LIMIT).is_err() {
            let _ = late_tx.send(());
            // SAFETY: Windows checks the thread id.
            let _ = unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    });
    run_message_loop().unwrap();
    hold(&[]);
    done.send(()).unwrap();
    watchdog.join().unwrap();
    assert!(
        late.try_recv().is_err(),
        "the script hung: {} steps left, failures so far {:?}",
        s.steps.borrow().len(),
        s.failures.borrow()
    );
    let problems: Vec<ShellError> = s
        .seen
        .borrow()
        .iter()
        .filter_map(|event| match event {
            // A desktop with no composition has its own DWM problems.
            ShellEvent::Problem(ShellError::Dwm { .. }) => None,
            ShellEvent::Problem(problem) => Some(problem.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(problems, vec![], "the window reported problems");
    assert_eq!(*s.failures.borrow(), Vec::<String>::new());
    // A script that ended early proves nothing.
    assert!(s.steps.borrow().is_empty() || s.was_destroyed());
    if let Ok(hwnd) = s.window.hwnd() {
        assert!(
            !unsafe { IsWindowVisible(HWND(hwnd)) }.as_bool(),
            "it showed"
        );
    }
    for popup in s.popups() {
        assert!(
            !unsafe { IsWindowVisible(popup) }.as_bool(),
            "a popup showed on screen"
        );
    }
}

/// The first group with a submenu, its row, and the Down presses from the first row.
fn group_with_submenu() -> (usize, usize, usize) {
    for (group, item) in menu_bar_groups().iter().enumerate() {
        let model = item.submenu.as_ref().unwrap();
        for (row, item) in model.items.iter().enumerate() {
            if item.submenu.is_some() {
                let downs = model.items[..row]
                    .iter()
                    .filter(|i| i.enabled && !i.separator)
                    .count();
                return (group, row, downs);
            }
        }
    }
    panic!("no menu has a submenu");
}
fn letter(group: usize) -> u8 {
    b"FEOVW"[group]
}
fn usable_rows(model: &MenuModel) -> Vec<usize> {
    model
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.enabled && !i.separator)
        .map(|(i, _)| i)
        .collect()
}
/// A row's middle, down from the top of its popup, in client dips of that popup.
fn row_middle(model: &MenuModel, row: usize) -> f32 {
    let t = UiTokens::sizes();
    let above: f32 = model.items[..row]
        .iter()
        .map(|i| {
            if i.separator {
                t.separator_row.0
            } else {
                t.menu_row.0
            }
        })
        .sum();
    t.inset.0 + above + t.menu_row.0 / 2.0
}
fn open_submenu_by_keys(steps: &mut Vec<Step>) {
    let (group, _, downs) = group_with_submenu();
    steps.push(step(move |s| {
        s.alt_letter(letter(group));
        for _ in 0..downs {
            s.key(VK_DOWN);
        }
        s.key(VK_RIGHT);
    }));
    steps.push(step(|s| s.expect_popups(2, "a submenu is open")));
}

#[test]
fn a_keyboard_choice_closes_every_menu_and_keys_go_to_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'E')),
        step(|s| {
            s.expect_popups(1, "Alt+E");
            s.expect(s.open_label() == Some(1), "Alt+E opens Edit");
            s.key(VK_DOWN);
        }),
        step(|s| {
            s.expect_popups(1, "Down in the menu");
            s.key(VK_ENTER);
        }),
        step(|s| {
            let edit = menu_bar_groups()[1].submenu.clone().unwrap();
            let second = usable_rows(&edit)[1];
            s.expect(
                s.commands() == vec![edit.items[second].command.unwrap()],
                format!("the second row runs once: {:?}", s.commands()),
            );
            s.expect(
                s.group_opens() == vec![1],
                format!("no menu opens again: {:?}", s.group_opens()),
            );
            s.key_up(VK_ENTER);
            s.post(WM_KEYUP, VK_ALT, 1 | (1 << 30) | (1 << 31));
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a keyboard choice");
    finish(&s, steps);
    assert_eq!(s.typed(), vec!['x']);
    let keys_up = s.seen.borrow().iter().any(|event| {
        matches!(event, ShellEvent::ContentInput(input)
            if matches!(input.kind, InputKind::KeyUp { virtual_key: 0x0d, .. }))
    });
    assert!(!keys_up, "the text got the key-up of the key that chose");
}

#[test]
fn a_held_enter_that_chose_a_command_does_not_go_on_into_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'E')),
        step(|s| s.key(VK_ENTER)),
        step(|s| {
            s.expect(s.commands().len() == 1, "Enter chose the first row");
            s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1);
        }),
        step(|s| s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1)),
        step(|s| s.key_up(VK_ENTER)),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a held Enter");
    finish(&s, steps);
    assert_eq!(s.typed(), vec!['x']);
}

#[test]
fn a_mouse_choice_closes_every_menu_and_keys_go_to_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.click_label(1)),
        step(|s| {
            s.expect_popups(1, "a click on Edit");
            s.expect(s.open_label() == Some(1), "the click opens Edit");
            let popup = s.root_popup().unwrap();
            let edit = menu_bar_groups()[1].submenu.clone().unwrap();
            let row = usable_rows(&edit)[0];
            let y = row_middle(&edit, row) as i32;
            s.post_to(popup, WM_MOUSEMOVE, 0, pack(20, y));
            s.post_to(popup, WM_LBUTTONDOWN, 1, pack(20, y));
            s.post_to(popup, WM_LBUTTONUP, 0, pack(20, y));
        }),
        step(|s| {
            let edit = menu_bar_groups()[1].submenu.clone().unwrap();
            let row = usable_rows(&edit)[0];
            s.expect(
                s.commands() == vec![edit.items[row].command.unwrap()],
                format!("the clicked row runs once: {:?}", s.commands()),
            );
            s.expect(
                s.group_opens() == vec![1],
                format!("no menu opens again: {:?}", s.group_opens()),
            );
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a mouse choice");
    finish(&s, steps);
    assert_eq!(s.typed(), vec!['x']);
}

#[test]
fn a_choice_from_a_submenu_closes_both_levels() {
    let s = script(false);
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(|s| s.key(VK_ENTER)));
    steps.push(step(|s| {
        s.expect(
            s.commands().len() == 1,
            format!("one command: {:?}", s.commands()),
        );
    }));
    Script::then_typing_reaches_text(&mut steps, "after a submenu choice");
    finish(&s, steps);
}

#[test]
fn wm_close_with_a_menu_open_ends_the_loop_and_destroys_every_popup() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| {
            s.expect_popups(1, "Alt+F");
            s.post(WM_CLOSE, 0, 0);
        }),
        step(|s| s.expect(false, "the window outlived WM_CLOSE")),
    ];
    finish(&s, steps);
    assert!(s.was_destroyed(), "the window was not destroyed");
    assert!(s.window.hwnd().is_err(), "the window still has a handle");
    assert_eq!(s.popups().len(), 0);
}

#[test]
fn wm_close_with_a_submenu_open_destroys_both_levels() {
    let s = script(false);
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(|s| s.post(WM_CLOSE, 0, 0)));
    steps.push(step(|s| s.expect(false, "the window outlived WM_CLOSE")));
    finish(&s, steps);
    assert!(s.was_destroyed());
    assert_eq!(s.popups().len(), 0);
}

/// On an inactive window, Windows' default handler sends no close for Alt+F4, and a hidden
/// window is never active. So this checks that the menu goes and that the key reaches that
/// handler. `WM_CLOSE` with a menu open has its own tests, and root's live run does the rest.
fn alt_f4_reaches_the_default_handler_and_the_text_has_the_keys(open: Step) {
    let s = script(true);
    let steps = vec![
        open,
        step(|s| {
            s.window.default_messages().unwrap();
            s.post(WM_SYSKEYDOWN, VK_F4, ALT_BIT | 1);
        }),
        step(|s| {
            s.expect_text_has_keys("Alt+F4");
            let reached = s.window.default_messages().unwrap();
            s.expect(
                reached.contains(&WM_SYSKEYDOWN),
                format!("Alt+F4 stopped short of Windows: {reached:?}"),
            );
        }),
    ];
    finish(&s, steps);
}

#[test]
fn alt_f4_with_a_menu_open_closes_the_menu_and_reaches_windows() {
    alt_f4_reaches_the_default_handler_and_the_text_has_the_keys(step(|s| s.alt_letter(b'F')));
}

#[test]
fn alt_f4_with_the_bar_focused_lets_go_and_reaches_windows() {
    alt_f4_reaches_the_default_handler_and_the_text_has_the_keys(step(|s| s.alt_tap()));
}

#[test]
fn alt_space_with_a_menu_open_closes_it_and_asks_for_the_system_menu() {
    let s = script(true);
    let steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| {
            s.expect_popups(1, "Alt+F");
            s.post(WM_SYSKEYDOWN, VK_SPACE, ALT_BIT | 1);
        }),
        step(|s| {
            s.expect_text_has_keys("Alt+Space");
            let asked = s.window.system_commands().unwrap();
            s.expect(
                asked.contains(&SC_KEYMENU_VALUE),
                format!("no system menu: {asked:?}"),
            );
        }),
    ];
    finish(&s, steps);
}

#[test]
fn timers_and_wakes_keep_arriving_with_a_menu_open() {
    let s = script(false);
    let waker = s.window.waker().unwrap();
    let steps = vec![
        step(|s| {
            s.window.set_timer(OTHER_TIMER, 5).unwrap();
            s.alt_letter(b'F');
        }),
        step(move |s| {
            s.expect_popups(1, "Alt+F");
            s.seen.borrow_mut().clear();
            // From another thread, as the file loader and the layout workers do it.
            std::thread::spawn(move || waker.wake()).join().unwrap();
        }),
        wait(80),
        step(|s| {
            s.expect_popups(1, "still open");
            s.expect(
                s.count_timers(OTHER_TIMER) >= 2,
                format!("timers: {}", s.count_timers(OTHER_TIMER)),
            );
            s.expect(s.count_wakes() == 1, format!("wakes: {}", s.count_wakes()));
        }),
    ];
    finish(&s, steps);
}

#[test]
fn esc_goes_back_one_level_at_a_time_and_ends_in_the_text() {
    let s = script(false);
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(|s| s.key(VK_ESC)));
    steps.push(step(|s| {
        s.expect_popups(1, "Esc closes the submenu alone");
        s.expect(s.open_label().is_some(), "its menu is still open");
        s.key_up(VK_ESC);
        s.key(VK_ESC);
    }));
    steps.push(step(|s| {
        s.expect_popups(0, "Esc closes the menu");
        s.expect(s.open_label().is_none(), "the label is closed");
        s.expect(s.focused_label().is_some(), "the label keeps the focus");
        s.key_up(VK_ESC);
        s.key(VK_ESC);
    }));
    steps.push(step(|s| s.key_up(VK_ESC)));
    Script::then_typing_reaches_text(&mut steps, "after Esc, Esc, Esc");
    finish(&s, steps);
    assert_eq!(s.typed(), vec!['x']);
}

#[test]
fn esc_with_only_the_bar_focused_returns_to_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "Alt focuses the first label");
            s.expect_popups(0, "Alt alone opens no menu");
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after Esc on the bar");
    finish(&s, steps);
}

#[test]
fn f10_focuses_the_bar_and_f10_again_lets_go() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.post(WM_SYSKEYDOWN, VK_F10, 1)),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "F10 focuses the first label");
            s.post(WM_SYSKEYUP, VK_F10, 1 | (1 << 30) | (1 << 31));
            s.key(VK_ENTER);
        }),
        step(|s| {
            s.expect_popups(1, "Enter on the label opens it");
            s.key_up(VK_ENTER);
            s.post(WM_SYSKEYDOWN, VK_F10, 1);
            s.post(WM_SYSKEYUP, VK_F10, 1 | (1 << 30) | (1 << 31));
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after F10 with a menu open");
    finish(&s, steps);
}

#[test]
fn alt_tapped_with_a_menu_open_closes_it_and_returns_to_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| {
            s.expect_popups(1, "Alt+F");
            // Letting go of the Alt of Alt+F must not close what it opened.
            s.post(WM_KEYUP, usize::from(b'F'), 1 | (1 << 30) | (1 << 31));
            s.post(WM_KEYUP, VK_ALT, 1 | (1 << 30) | (1 << 31));
        }),
        step(|s| {
            s.expect_popups(1, "the Alt of Alt+F let go");
            s.alt_tap();
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after Alt with a menu open");
    finish(&s, steps);
}

#[test]
fn alt_tapped_with_a_submenu_open_closes_both_levels() {
    let s = script(false);
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(|s| s.alt_tap()));
    Script::then_typing_reaches_text(&mut steps, "after Alt with a submenu open");
    finish(&s, steps);
}

#[test]
fn alt_tapped_with_only_the_bar_focused_returns_to_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            s.expect(
                s.focused_label() == Some(0),
                "the first tap focuses the bar",
            );
            s.alt_tap();
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a second Alt");
    finish(&s, steps);
}

#[test]
fn a_click_in_the_text_with_the_bar_focused_lets_go_and_reaches_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(|s| s.click_text()),
        step(|s| {
            let pressed = s.seen.borrow().iter().any(|event| {
                matches!(event, ShellEvent::ContentInput(input)
                    if matches!(input.kind, InputKind::MouseDown { .. }))
            });
            s.expect(pressed, "the press reached the text");
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a click with the bar focused");
    finish(&s, steps);
}

#[test]
fn a_click_in_the_text_with_a_menu_open_closes_it() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| s.click_text_through_popup()),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a click with a menu open");
    finish(&s, steps);
}

#[test]
fn a_click_in_the_text_with_a_submenu_open_closes_both_levels() {
    let s = script(false);
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(|s| s.click_text_through_popup()));
    Script::then_typing_reaches_text(&mut steps, "after a click with a submenu open");
    finish(&s, steps);
}

#[test]
fn a_click_on_the_open_label_closes_its_menu_and_opens_nothing() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.click_label(0)),
        step(|s| {
            s.expect_popups(1, "the click opens File");
            let popup = s.root_popup().unwrap();
            let r = s.label(0);
            let at = s.on_popup(popup, r.x.0 + 4.0, r.y.0 + 10.0);
            s.post_to(popup, WM_LBUTTONDOWN, 1, at);
            s.post_to(popup, WM_LBUTTONUP, 0, at);
        }),
        step(|s| {
            s.expect(
                s.group_opens() == vec![0],
                format!("opens: {:?}", s.group_opens()),
            );
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a second click on the label");
    finish(&s, steps);
}

#[test]
fn another_program_in_front_closes_the_menu() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|_| foreground_changed_for_test(HWND(std::ptr::dangling_mut()))),
    ];
    Script::then_typing_reaches_text(&mut steps, "after another program came to the front");
    finish(&s, steps);
}

#[test]
fn a_letter_that_matches_no_label_does_no_harm() {
    let s = script(false);
    let mut steps = vec![
        // After Alt alone: the bar keeps the focus, and the text gets nothing.
        step(|s| s.alt_tap()),
        step(|s| s.type_x()),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "the bar keeps the focus");
            s.expect_popups(0, "an unknown letter");
            s.expect(s.typed().is_empty(), format!("typed: {:?}", s.typed()));
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
        // Alt+letter: nothing opens, the bar takes no focus and the text gets no letter.
        step(|s| s.post(WM_SYSKEYDOWN, usize::from(b'Z'), ALT_BIT | 1)),
        step(|s| {
            s.expect_text_has_keys("Alt+Z");
            s.expect(s.typed().is_empty(), format!("typed: {:?}", s.typed()));
            s.post(WM_SYSKEYUP, usize::from(b'Z'), 1 | (1 << 30) | (1 << 31));
        }),
        // With a menu open: it stays open, on the same label.
        step(|s| s.alt_letter(b'F')),
        step(|s| s.post(WM_SYSKEYDOWN, usize::from(b'Z'), ALT_BIT | 1)),
        step(|s| {
            s.expect_popups(1, "Alt+Z with a menu open");
            s.expect(s.open_label() == Some(0), "File stays open");
            s.alt_tap();
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after the unknown letters");
    finish(&s, steps);
    assert_eq!(s.typed(), vec!['x']);
}

#[test]
fn left_and_right_move_between_menus_and_wrap() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| s.key(VK_LEFT)),
        step(|s| {
            s.expect_popups(1, "Left from File");
            s.expect(s.open_label() == Some(4), "Left wraps to Window");
            s.key(VK_RIGHT);
        }),
        step(|s| {
            s.expect_popups(1, "Right from Window");
            s.expect(s.open_label() == Some(0), "Right wraps to File");
            s.expect(
                s.group_opens() == vec![0, 4, 0],
                format!("opens: {:?}", s.group_opens()),
            );
        }),
    ];
    finish(&s, steps);
}

#[test]
fn right_opens_a_submenu_left_closes_it_and_right_on_its_plain_row_moves_on() {
    let s = script(false);
    let (group, _, _) = group_with_submenu();
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(move |s| {
        s.expect(s.open_label() == Some(group), "the same menu");
        s.key(VK_LEFT);
    }));
    steps.push(step(move |s| {
        s.expect_popups(1, "Left closes the submenu alone");
        s.expect(s.open_label() == Some(group), "Left stays on this menu");
        s.key(VK_RIGHT);
    }));
    steps.push(step(move |s| {
        s.expect_popups(2, "Right opens it again");
        s.key(VK_RIGHT);
    }));
    steps.push(step(move |s| {
        s.expect_popups(1, "Right on a plain row leaves the submenu");
        s.expect(
            s.open_label() == Some((group + 1) % 5),
            format!("the next menu: {:?}", s.open_label()),
        );
    }));
    finish(&s, steps);
}

#[test]
fn alt_and_another_letter_switch_the_open_menu() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| s.post(WM_SYSKEYDOWN, usize::from(b'V'), ALT_BIT | 1)),
        step(|s| {
            s.expect_popups(1, "Alt+V with File open");
            s.expect(s.open_label() == Some(3), "View opens");
        }),
    ];
    finish(&s, steps);
}

#[test]
fn moving_the_mouse_to_another_label_switches_the_menu() {
    let s = script(false);
    let steps = vec![
        step(|s| s.click_label(0)),
        step(|s| {
            let popup = s.root_popup().unwrap();
            let r = s.label(2);
            s.post_to(
                popup,
                WM_MOUSEMOVE,
                0,
                s.on_popup(popup, r.x.0 + 4.0, r.y.0 + 10.0),
            );
        }),
        step(|s| {
            s.expect_popups(1, "a move to Format");
            s.expect(s.open_label() == Some(2), "Format opens");
        }),
    ];
    finish(&s, steps);
}

#[test]
fn a_hovered_submenu_row_opens_after_its_delay_through_the_timer() {
    let s = script(false);
    let (group, row, _) = group_with_submenu();
    let delay = UiTokens::sizes().submenu_delay_ms;
    let steps = vec![
        step(move |s| s.click_label(group)),
        step(move |s| {
            let popup = s.root_popup().unwrap();
            let model = menu_bar_groups()[group].submenu.clone().unwrap();
            let y = row_middle(&model, row) as i32;
            s.post_to(popup, WM_MOUSEMOVE, 0, pack(20, y));
        }),
        step(|s| s.expect_popups(1, "the delay has not passed")),
        wait(delay + 150),
        step(|s| s.expect_popups(2, "the delay has passed")),
    ];
    finish(&s, steps);
}

#[test]
fn a_menu_opened_by_mouse_selects_no_row_and_runs_nothing_on_a_letter() {
    let s = script(false);
    let steps = vec![
        step(|s| s.click_label(0)),
        step(|s| s.post(WM_CHAR, usize::from(b'n'), 1)),
        step(|s| {
            s.expect_popups(1, "a letter in a menu opened by mouse");
            s.expect(s.commands().is_empty(), "a letter ran a command");
        }),
    ];
    finish(&s, steps);
}

#[test]
fn a_menu_of_the_owner_runs_the_same_way_and_leaves_the_bar_alone() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.open_owner_menu()),
        step(|s| {
            s.expect_popups(1, "the owner's menu");
            s.expect(s.focused_label().is_none(), "the bar takes no focus");
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after Esc in the owner's menu");
    finish(&s, steps);
}

#[test]
fn a_label_clicked_in_an_inactive_window_activates_it_and_opens_the_menu() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            let asked = (WM_LBUTTONDOWN as isize) << 16 | HTCLIENT as isize;
            let answer = unsafe {
                SendMessageW(
                    s.hwnd,
                    WM_MOUSEACTIVATE,
                    Some(WPARAM(s.hwnd.0 as usize)),
                    Some(LPARAM(asked)),
                )
            };
            s.expect(
                answer.0 == MA_ACTIVATE as isize,
                format!("the click does not activate: {}", answer.0),
            );
            s.click_label(1);
        }),
        step(|s| {
            s.expect_popups(1, "the same click");
            s.expect(s.open_label() == Some(1), "it opens Edit");
        }),
    ];
    finish(&s, steps);
}

#[test]
fn the_bar_makes_no_drag_or_resize_zone_and_snap_still_finds_the_button() {
    let s = script(false);
    let hit = |x: i32, y: i32| {
        let mut at = POINT { x, y };
        unsafe {
            let _ = ClientToScreen(s.hwnd, &mut at);
            SendMessageW(
                s.hwnd,
                WM_NCHITTEST,
                Some(WPARAM(0)),
                Some(LPARAM(pack(at.x, at.y))),
            )
        }
        .0 as u32
    };
    let mut client = RECT::default();
    unsafe { GetClientRect(s.hwnd, &mut client) }.unwrap();
    let bar = s.label(0);
    let (top, bottom) = (bar.y.0 as i32, (bar.y.0 + bar.height.0) as i32);
    assert_eq!(bar.height, Dip(32.0));
    // Snap opens from HTMAXBUTTON. The caption's own row keeps it, with the bar below.
    assert_eq!(hit(client.right - 69, 20), HTMAXBUTTON);
    assert_eq!(hit(client.right - 200, 2), HTTOP, "the top resize edge");
    assert_eq!(hit(-2, top + 10), HTLEFT, "the left edge beside the bar");
    assert_eq!(
        hit(client.right + 2, top + 10),
        HTRIGHT,
        "the right edge beside the bar"
    );
    // Across the whole bar, from its first pixel to its last, nothing but client area.
    for x in [0, 1, 50, client.right / 2, client.right - 1] {
        for y in [top, top + 1, top + 10, bottom - 1] {
            assert_eq!(hit(x, y), HTCLIENT, "the bar at {x}, {y}");
        }
    }
}

#[test]
fn full_screen_keeps_the_bar_and_its_menus_work() {
    let s = script(false);
    let bar_before = s.label(0).height;
    let mut steps = vec![
        step(|s| {
            s.key(VK_F11);
            s.key_up(VK_F11);
        }),
        step(move |s| {
            s.expect(s.label(0).height == bar_before, "the bar keeps its height");
            let text = s.window.content_rect().unwrap();
            let r = s.label(0);
            s.expect(
                text.y.0 == r.y.0 + r.height.0,
                "the text starts under the bar",
            );
            s.alt_letter(b'F');
        }),
        step(|s| {
            s.expect_popups(1, "Alt+F in full screen");
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
        step(|s| {
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after full screen");
    finish(&s, steps);
}

/// Counts each level's frames and keeps the text they draw.
#[derive(Clone, Default)]
struct PaintLog {
    frames: Rc<RefCell<Vec<(u32, u32)>>>,
    text: Rc<RefCell<Vec<String>>>,
    presents: Rc<RefCell<usize>>,
    dpi: Rc<RefCell<Vec<u32>>>,
}
struct LoggedPopup {
    inner: BitmapRenderer,
    size: (u32, u32),
    log: PaintLog,
}
impl Renderer for LoggedPopup {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.inner.begin_frame()
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        self.log.frames.borrow_mut().push(self.size);
        for command in &list.commands {
            if let DrawCommand::TextRun { text, .. } = command {
                self.log.text.borrow_mut().push(text.to_string());
            }
        }
        Renderer::draw(&mut self.inner, list)
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        self.inner.end_frame()
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.size = (width, height);
        self.inner.resize(width, height)
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.log.dpi.borrow_mut().push(dpi);
        self.inner.set_dpi(dpi)
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        self.inner.render_to_bitmap(list, width, height)
    }
    fn invalidate(&mut self, rect: Rect) {
        Renderer::invalidate(&mut self.inner, rect);
    }
    fn invalidate_all(&mut self) {
        Renderer::invalidate_all(&mut self.inner);
    }
    fn present(&mut self, window: *mut std::ffi::c_void) -> Result<(), RenderError> {
        *self.log.presents.borrow_mut() += 1;
        Renderer::present(&mut self.inner, window)
    }
    fn present_rect(
        &mut self,
        dc: *mut std::ffi::c_void,
        rect: PixelRect,
    ) -> Result<(), RenderError> {
        Renderer::present_rect(&mut self.inner, dc, rect)
    }
}

#[test]
fn every_level_paints_when_it_opens_and_when_its_selection_moves() {
    let s = script(false);
    let log = PaintLog::default();
    let made = log.clone();
    s.window
        .set_popup_renderers(move |width, height| {
            Ok(Box::new(LoggedPopup {
                inner: BitmapRenderer::new(width, height)?,
                size: (width, height),
                log: made.clone(),
            }) as Box<dyn Renderer>)
        })
        .unwrap();
    let (group, _, downs) = group_with_submenu();
    let first_row = menu_bar_groups()[1].submenu.clone().unwrap().items[0]
        .label
        .clone();
    let seen = log.clone();
    let after_open = log.clone();
    let steps = vec![
        step(|s| s.alt_letter(b'E')),
        step(move |s| {
            s.expect(seen.frames.borrow().len() == 1, "one frame for one level");
            s.expect(
                seen.text.borrow().contains(&first_row),
                format!("the frame has no {first_row}: {:?}", seen.text.borrow()),
            );
            s.expect(
                *seen.presents.borrow() == 1,
                "that frame reached its window",
            );
            s.key(VK_DOWN);
        }),
        step(move |s| {
            s.expect(after_open.frames.borrow().len() == 2, "Down paints again");
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
        step(move |s| {
            s.alt_letter(letter(group));
            for _ in 0..downs {
                s.key(VK_DOWN);
            }
            s.key(VK_RIGHT);
        }),
        step(|s| s.expect_popups(2, "a submenu is open")),
    ];
    finish(&s, steps);
    assert_eq!(
        log.dpi.borrow().first(),
        Some(&96),
        "the window sets the DPI"
    );
    assert!(
        log.frames
            .borrow()
            .windows(2)
            .any(|pair| pair[0] != pair[1]),
        "the submenu's level has its own size: {:?}",
        log.frames.borrow()
    );
}

#[test]
fn opening_a_menu_with_no_renderers_says_so_and_leaves_the_bar_closed() {
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            ..Default::default()
        },
        Box::new(BitmapRenderer::new(64, 64).unwrap()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    let anchor = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(10.0),
        height: Dip(10.0),
    };
    let failed = window.open_menu(owner_menu(), anchor);
    assert!(
        matches!(&failed, Err(ShellError::Menu(message)) if message.contains("set_popup_renderers")),
        "{failed:?}"
    );
    assert!(!window.menu_is_open().unwrap());
}

// The review's findings, each through the real window and message loop. Keys really held
// use `hold`, which sets this thread's key state, as `GetKeyState` reads it.
const VK_CTRL: u16 = 0x11;
const VK_MENU_KEY: u16 = 0x12;
const VK_SHIFT: u16 = 0x10;
const REPEAT_UP: isize = 1 | (1 << 30) | (1 << 31);

fn hold(keys: &[u16]) {
    let mut state = [0u8; 256];
    for key in keys {
        state[*key as usize] = 0x80;
    }
    // SAFETY: the array lives for the call.
    unsafe { SetKeyboardState(&state) }.unwrap();
}
fn count_focused(widget: &qe_api::Widget) -> usize {
    usize::from(widget.states.focused) + widget.children.iter().map(count_focused).sum::<usize>()
}
fn find_widget<'a>(widget: &'a qe_api::Widget, name: &str) -> Option<&'a qe_api::Widget> {
    if widget.name == name {
        return Some(widget);
    }
    widget.children.iter().find_map(|c| find_widget(c, name))
}
fn focused_name(widget: &qe_api::Widget) -> Option<String> {
    if widget.states.focused {
        return Some(widget.name.clone());
    }
    widget.children.iter().find_map(focused_name)
}
fn center(r: Rect) -> isize {
    pack(
        (r.x.0 + r.width.0 / 2.0) as i32,
        (r.y.0 + r.height.0 / 2.0) as i32,
    )
}
impl Script {
    fn content_key_downs(&self, vk: u16) -> usize {
        self.seen
            .borrow()
            .iter()
            .filter(|event| {
                matches!(event, ShellEvent::ContentInput(input)
                    if matches!(input.kind, InputKind::KeyDown { virtual_key, .. } if virtual_key == vk))
            })
            .count()
    }
    fn content_has(&self, wanted: impl Fn(&InputKind) -> bool) -> usize {
        self.seen
            .borrow()
            .iter()
            .filter(|event| matches!(event, ShellEvent::ContentInput(input) if wanted(&input.kind)))
            .count()
    }
    fn tree(&self) -> qe_api::Widget {
        self.window.widgets().unwrap()
    }
    fn tear_offs(&self) -> usize {
        self.seen
            .borrow()
            .iter()
            .filter(|event| matches!(event, ShellEvent::TearOffTab { .. }))
            .count()
    }
    fn plus(&self) -> Rect {
        self.window.with_strip(|strip| strip.plus_rect()).unwrap()
    }
    fn tab(&self) -> Rect {
        self.window
            .with_strip(|strip| strip.tab_rects()[0])
            .unwrap()
    }
    /// A menu that is not a menu bar group, such as a context menu.
    fn open_owner_menu(&self) {
        let anchor = self.plus();
        self.window.open_menu(owner_menu(), anchor).unwrap();
    }
    fn alt_down(&self) {
        self.post(WM_SYSKEYDOWN, VK_ALT, ALT_BIT | 1);
    }
    fn alt_up(&self) {
        self.post(WM_KEYUP, VK_ALT, REPEAT_UP);
    }
    /// How the strip paints now. With no press, this shows only its hover.
    fn strip_paint(&self) -> DisplayList {
        let theme = self.window.theme().unwrap();
        self.window
            .with_strip(|strip| {
                let mut list = DisplayList::default();
                strip.draw(&mut list, &theme, &RealMeasurer);
                list
            })
            .unwrap()
    }
}

fn a_press_released_on_the_bar_ends_there(part: fn(&Script) -> Rect) {
    let s = script(false);
    let steps = vec![
        step(move |s| {
            let at = center(part(s));
            s.post(WM_MOUSEMOVE, 0, at);
            s.post(WM_LBUTTONDOWN, 1, at);
        }),
        step(|s| {
            s.expect(
                unsafe { GetCapture() } == s.hwnd,
                "the press took the mouse",
            );
            let on_bar = center(Rect {
                width: Dip(4.0),
                ..s.label(4)
            });
            s.post(WM_MOUSEMOVE, 1, on_bar);
            s.post(WM_LBUTTONUP, 0, on_bar);
        }),
        step(|s| {
            s.expect(
                unsafe { GetCapture() }.is_invalid(),
                "the release on the bar left the mouse held",
            );
            s.seen.borrow_mut().clear();
            let (x, y) = s.text_point();
            s.post(WM_MOUSEMOVE, 0, pack(x as i32, y as i32));
        }),
        step(|s| {
            s.expect(s.tear_offs() == 0, "a tab tore off with no button held");
            s.expect(
                s.content_has(|kind| matches!(kind, InputKind::MouseMove { .. })) == 1,
                "the next move did not reach the text",
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_tab_pressed_and_released_on_the_bar_ends_the_press() {
    a_press_released_on_the_bar_ends_there(Script::tab);
}
#[test]
fn a_plus_pressed_and_released_on_the_bar_ends_the_press() {
    a_press_released_on_the_bar_ends_there(Script::plus);
}

#[test]
fn a_tab_dragged_across_the_bar_still_tears_off_once() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            let at = center(s.tab());
            s.post(WM_MOUSEMOVE, 0, at);
            s.post(WM_LBUTTONDOWN, 1, at);
        }),
        step(|s| {
            let tab = s.tab();
            let bar = s.label(0);
            let x = tab.x.0 as i32 + 10;
            s.post(
                WM_MOUSEMOVE,
                1,
                pack(x, (bar.y.0 + bar.height.0) as i32 - 1),
            );
        }),
        step(|s| {
            s.expect(s.tear_offs() == 1, format!("tear-offs: {}", s.tear_offs()));
            s.expect(
                unsafe { GetCapture() }.is_invalid(),
                "it let go of the mouse",
            );
        }),
    ];
    finish(&s, steps);
}

#[test]
fn moving_from_a_top_row_button_onto_the_bar_clears_its_hover() {
    let s = script(false);
    let steps = vec![
        step(|s| s.post(WM_SETFOCUS, 0, 0)),
        // Sent, not posted, and looked at at once. Windows tells a hidden window right away
        // that the pointer has left, and that notice would clear the hover by itself.
        step(|s| {
            let rest = s.strip_paint();
            let send = |at: isize| unsafe {
                SendMessageW(s.hwnd, WM_MOUSEMOVE, Some(WPARAM(0)), Some(LPARAM(at)));
            };
            send(center(s.plus()));
            s.expect(s.strip_paint() != rest, "the hover never showed");
            send(center(s.label(0)));
            s.expect(s.strip_paint() == rest, "the top row still shows its hover");
        }),
    ];
    finish(&s, steps);
}

fn alt_is_no_tap_after(between: fn(&Script)) {
    let s = script(true);
    let mut steps = vec![
        step(|s| s.alt_down()),
        step(between),
        step(|s| {
            hold(&[]);
            s.alt_up();
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after an Alt that was not a tap");
    finish(&s, steps);
}
#[test]
fn alt_with_a_left_click_in_the_text_is_no_tap() {
    alt_is_no_tap_after(|s| s.click_text());
}
#[test]
fn alt_with_a_right_click_in_the_text_is_no_tap() {
    alt_is_no_tap_after(|s| {
        let (x, y) = s.text_point();
        s.post(WM_RBUTTONDOWN, 2, pack(x as i32, y as i32));
        s.post(WM_RBUTTONUP, 0, pack(x as i32, y as i32));
    });
}
#[test]
fn alt_with_the_wheel_is_no_tap() {
    alt_is_no_tap_after(|s| {
        let mut at = POINT { x: 50, y: 300 };
        let _ = unsafe { ClientToScreen(s.hwnd, &mut at) };
        s.post(WM_MOUSEWHEEL, 120 << 16, pack(at.x, at.y));
    });
}
#[test]
fn alt_with_a_ctrl_chord_is_no_tap() {
    alt_is_no_tap_after(|s| {
        hold(&[VK_CTRL, VK_MENU_KEY]);
        s.post(WM_SYSKEYDOWN, VK_CTRL as usize, ALT_BIT | 1);
        s.post(WM_SYSKEYDOWN, usize::from(b'K'), ALT_BIT | 1);
        s.post(WM_SYSKEYUP, usize::from(b'K'), ALT_BIT | REPEAT_UP);
        s.post(WM_SYSKEYUP, VK_CTRL as usize, ALT_BIT | REPEAT_UP);
    });
}
#[test]
fn alt_with_a_chord_from_the_key_table_is_no_tap() {
    alt_is_no_tap_after(|s| {
        hold(&[VK_MENU_KEY]);
        s.window
            .set_key_table(qe_shell::KeyTable {
                bindings: vec![qe_shell::KeyBinding {
                    virtual_key: u16::from(b'K'),
                    modifiers: qe_api::Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                    action: qe_shell::ShellAction::NewTab,
                }],
            })
            .unwrap();
        s.post(WM_SYSKEYDOWN, usize::from(b'K'), ALT_BIT | 1);
        s.post(WM_SYSKEYUP, usize::from(b'K'), ALT_BIT | REPEAT_UP);
    });
}
#[test]
fn alt_with_a_press_on_the_title_area_is_no_tap() {
    alt_is_no_tap_after(|s| {
        let mut at = POINT { x: 300, y: 4 };
        let _ = unsafe { ClientToScreen(s.hwnd, &mut at) };
        s.post(WM_NCLBUTTONDOWN, HTCAPTION as usize, pack(at.x, at.y));
    });
}

fn no_ime_message_reaches_the_text(open: fn(&Script)) {
    let s = script(false);
    let steps = vec![
        step(open),
        step(|s| {
            s.seen.borrow_mut().clear();
            s.post(WM_IME_STARTCOMPOSITION, 0, 0);
            s.post(WM_IME_COMPOSITION, 0, 0x800);
            s.post(WM_IME_ENDCOMPOSITION, 0, 0);
        }),
        step(|s| {
            let ime = s.content_has(|kind| {
                matches!(
                    kind,
                    InputKind::ImeStart | InputKind::ImeUpdate { .. } | InputKind::ImeEnd { .. }
                )
            });
            s.expect(
                ime == 0,
                format!("{ime} input method events reached the text"),
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn an_input_method_stays_out_of_the_text_behind_an_open_menu() {
    no_ime_message_reaches_the_text(|s| s.alt_letter(b'F'));
}
#[test]
fn an_input_method_stays_out_of_the_text_while_the_bar_has_focus() {
    no_ime_message_reaches_the_text(|s| s.alt_tap());
}
#[test]
fn a_composition_that_began_before_the_menu_still_ends_in_the_text() {
    let s = script(false);
    let steps = vec![
        step(|s| s.post(WM_IME_STARTCOMPOSITION, 0, 0)),
        step(|s| s.alt_letter(b'F')),
        step(|s| s.post(WM_IME_ENDCOMPOSITION, 0, 0)),
        step(|s| {
            let (start, end) = (
                s.content_has(|kind| matches!(kind, InputKind::ImeStart)),
                s.content_has(|kind| matches!(kind, InputKind::ImeEnd { .. })),
            );
            s.expect((start, end) == (1, 1), format!("start {start}, end {end}"));
        }),
    ];
    finish(&s, steps);
}

#[test]
fn shift_f10_goes_to_the_text_and_the_bar_takes_no_focus() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            hold(&[VK_SHIFT]);
            s.post(WM_SYSKEYDOWN, VK_F10, 1);
        }),
        step(|s| {
            s.expect(s.focused_label().is_none(), "Shift+F10 focused the bar");
            s.expect(
                s.content_key_downs(VK_F10 as u16) == 1,
                "it reached the text",
            );
            s.alt_letter(b'F');
        }),
        step(|s| s.post(WM_SYSKEYDOWN, VK_F10, 1)),
        step(|s| s.expect_popups(1, "Shift+F10 with a menu open")),
    ];
    finish(&s, steps);
}

fn a_click_of_this_button_lets_go_of_the_bar(down: u32, up: u32, flag: usize) {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(move |s| {
            let (x, y) = s.text_point();
            s.post(down, flag, pack(x as i32, y as i32));
            s.post(up, 0, pack(x as i32, y as i32));
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a click with the bar focused");
    finish(&s, steps);
}
#[test]
fn a_right_click_in_the_text_lets_go_of_the_bar() {
    a_click_of_this_button_lets_go_of_the_bar(WM_RBUTTONDOWN, WM_RBUTTONUP, 2);
}
#[test]
fn a_middle_click_in_the_text_lets_go_of_the_bar() {
    a_click_of_this_button_lets_go_of_the_bar(WM_MBUTTONDOWN, WM_MBUTTONUP, 16);
}

#[test]
fn a_ctrl_chord_on_the_focused_bar_returns_to_the_text_and_reaches_it() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            hold(&[VK_CTRL]);
            s.key(usize::from(b'X'));
        }),
        step(|s| {
            s.expect(s.focused_label().is_none(), "the bar kept the focus");
            s.expect(
                s.content_key_downs(u16::from(b'X')) == 1,
                "Ctrl+X reached the text",
            );
            hold(&[]);
            s.key_up(usize::from(b'X'));
            // Its control character reaches the text as usual, and the owner ignores it.
            s.seen.borrow_mut().clear();
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after Ctrl+X on the bar");
    finish(&s, steps);
}
#[test]
fn an_altgr_character_on_the_focused_bar_returns_to_the_text_and_reaches_it() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            hold(&[VK_CTRL, VK_MENU_KEY]);
            s.post(WM_KEYDOWN, usize::from(b'E'), ALT_BIT | 1);
            s.post(WM_CHAR, 0x20ac, ALT_BIT | 1);
        }),
        step(|s| {
            s.expect(s.focused_label().is_none(), "the bar kept the focus");
            s.expect(
                s.typed().contains(&'\u{20ac}'),
                format!("typed {:?}", s.typed()),
            );
            hold(&[]);
            s.key_up(usize::from(b'E'));
            s.seen.borrow_mut().clear();
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after AltGr on the bar");
    finish(&s, steps);
}
#[test]
fn ctrl_alone_and_plain_keys_stay_with_the_bar() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            hold(&[VK_CTRL]);
            s.key(VK_CTRL as usize);
        }),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "Ctrl alone is no chord");
            s.expect(s.content_key_downs(VK_CTRL) == 0, "the text saw Ctrl");
            hold(&[]);
            s.key_up(VK_CTRL as usize);
            s.key(usize::from(b'X'));
            s.key_up(usize::from(b'X'));
        }),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "a plain key stays");
            s.expect(s.typed().is_empty(), format!("typed {:?}", s.typed()));
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_ctrl_chord_from_the_key_table_also_returns_focus_to_the_text() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            hold(&[VK_CTRL]);
            s.key(usize::from(b'T'));
        }),
        step(|s| {
            let tabs = s
                .seen
                .borrow()
                .iter()
                .filter(|e| **e == ShellEvent::NewTab)
                .count();
            s.expect(tabs == 1, format!("new tabs: {tabs}"));
            s.expect(s.focused_label().is_none(), "the bar kept the focus");
            hold(&[]);
            s.key_up(usize::from(b'T'));
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after Ctrl+T on the bar");
    finish(&s, steps);
}

#[test]
fn a_repeated_enter_on_a_label_opens_nothing() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_tap()),
        step(|s| s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1)),
        step(|s| {
            s.expect_popups(0, "a repeated Enter");
            s.expect(s.focused_label() == Some(0), "the label keeps the focus");
        }),
    ];
    finish(&s, steps);
}
fn a_repeat_runs_no_row(vk: usize) {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'E')),
        step(move |s| s.post(WM_KEYDOWN, vk, REPEAT_BIT | 1)),
        step(|s| {
            s.expect_popups(1, "a repeat in the menu");
            s.expect(s.commands().is_empty(), format!("ran {:?}", s.commands()));
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_repeated_enter_in_a_menu_runs_no_row() {
    a_repeat_runs_no_row(VK_ENTER);
}
#[test]
fn a_repeated_space_in_a_menu_runs_no_row() {
    a_repeat_runs_no_row(VK_SPACE);
}
#[test]
fn a_held_enter_opens_a_label_and_then_does_nothing_more() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_tap()),
        step(|s| s.key(VK_RIGHT)),
        step(|s| s.key(VK_ENTER)),
        step(|s| {
            s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1);
            s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1);
        }),
        step(|s| {
            s.expect_popups(1, "held Enter");
            s.expect(s.commands().is_empty(), format!("ran {:?}", s.commands()));
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_held_letter_that_opens_a_menu_runs_none_of_its_rows() {
    let s = script(false);
    // A letter that opens a menu and is also a row's mnemonic in it, when one exists.
    let group = (0..5)
        .find(|&group| {
            let wanted = char::from(letter(group)).to_ascii_lowercase();
            menu_bar_groups()[group]
                .submenu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|item| {
                    item.mnemonic
                        .is_some_and(|m| m.to_ascii_lowercase() == wanted)
                })
        })
        .unwrap_or(0);
    let vk = usize::from(letter(group));
    let steps = vec![
        step(|s| s.alt_tap()),
        step(move |s| s.key(vk)),
        step(move |s| {
            s.expect_popups(1, "the letter opens its menu");
            s.post(WM_KEYDOWN, vk, REPEAT_BIT | 1);
            s.post(WM_KEYDOWN, vk, REPEAT_BIT | 1);
        }),
        step(move |s| {
            s.expect(s.commands().is_empty(), format!("ran {:?}", s.commands()));
            s.expect_popups(1, "the held letter");
        }),
    ];
    finish(&s, steps);
}

#[test]
fn a_key_up_that_never_came_does_not_eat_the_next_held_key() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'E')),
        // A command can open a dialog, which then gets this key's release.
        step(|s| s.key(VK_ENTER)),
        step(|s| {
            s.expect(s.commands().len() == 1, "Enter chose the row");
            s.post(WM_KEYUP, VK_ALT, REPEAT_UP);
            s.seen.borrow_mut().clear();
            s.key(VK_ENTER);
            s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1);
            s.post(WM_KEYDOWN, VK_ENTER, REPEAT_BIT | 1);
            s.key_up(VK_ENTER);
        }),
        step(|s| {
            let downs = s.content_key_downs(VK_ENTER as u16);
            s.expect(
                downs == 3,
                format!("the text got {downs} key downs, wanted 3"),
            );
        }),
    ];
    finish(&s, steps);
}

#[test]
fn a_held_f10_does_nothing_after_the_first_press() {
    let s = script(false);
    let steps = vec![
        step(|s| s.post(WM_SYSKEYDOWN, VK_F10, 1)),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "F10 focuses the bar");
            s.post(WM_SYSKEYDOWN, VK_F10, REPEAT_BIT | 1);
        }),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "the first repeat");
            s.post(WM_SYSKEYDOWN, VK_F10, REPEAT_BIT | 1);
        }),
        step(|s| s.expect(s.focused_label() == Some(0), "the second repeat")),
    ];
    finish(&s, steps);
}
#[test]
fn f10_with_a_menu_of_the_owner_open_closes_it_and_focuses_the_bar() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.open_owner_menu()),
        step(|s| {
            s.expect_popups(1, "the owner's menu");
            s.post(WM_SYSKEYDOWN, VK_F10, 1);
        }),
        step(|s| {
            s.expect_popups(0, "F10 closes the menu");
            s.expect(s.focused_label() == Some(0), "F10 focuses the bar");
            s.post(WM_SYSKEYDOWN, VK_F10, REPEAT_BIT | 1);
            s.post(WM_SYSKEYDOWN, VK_F10, REPEAT_BIT | 1);
        }),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "the held F10 does nothing");
            s.expect(s.content_key_downs(VK_F10 as u16) == 0, "the text saw F10");
            s.post(WM_SYSKEYUP, VK_F10, REPEAT_UP);
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after F10 from the owner's menu");
    finish(&s, steps);
}
#[test]
fn a_held_f10_that_closed_a_bar_menu_reaches_nothing_more() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| s.post(WM_SYSKEYDOWN, VK_F10, 1)),
        step(|s| {
            s.expect_text_has_keys("F10 with File open");
            s.post(WM_SYSKEYDOWN, VK_F10, REPEAT_BIT | 1);
        }),
        step(|s| {
            s.expect_text_has_keys("a repeat of that F10");
            s.expect(s.content_key_downs(VK_F10 as u16) == 0, "the text saw F10");
            s.post(WM_SYSKEYUP, VK_F10, REPEAT_UP);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a held F10");
    finish(&s, steps);
}

#[test]
fn a_hover_that_a_key_overtook_opens_no_submenu_later() {
    let s = script(false);
    let (group, row, _) = group_with_submenu();
    let delay = UiTokens::sizes().submenu_delay_ms;
    let steps = vec![
        step(move |s| s.click_label(group)),
        step(move |s| {
            let popup = s.root_popup().unwrap();
            let model = menu_bar_groups()[group].submenu.clone().unwrap();
            s.post_to(
                popup,
                WM_MOUSEMOVE,
                0,
                pack(20, row_middle(&model, row) as i32),
            );
            s.key(VK_DOWN);
        }),
        wait(delay + 100),
        step(|s| {
            s.expect_popups(1, "the key moved off the row");
            s.key(VK_UP_KEY);
        }),
        wait(delay + 100),
        step(|s| s.expect_popups(1, "the key came back to the row")),
    ];
    finish(&s, steps);
}
const VK_UP_KEY: usize = 0x26;

#[test]
fn the_tree_has_a_menu_bar_and_one_focused_widget() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            let tree = s.tree();
            let bar = find_widget(&tree, "Menu bar").unwrap();
            s.expect(
                bar.role == qe_api::WidgetRole::MenuBar,
                format!("{:?}", bar.role),
            );
            s.expect(count_focused(&tree) == 0, "nothing is focused yet");
            s.alt_tap();
        }),
        step(|s| {
            let tree = s.tree();
            s.expect(
                count_focused(&tree) == 1,
                format!("focused: {}", count_focused(&tree)),
            );
            s.expect(
                focused_name(&tree).as_deref() == Some("File"),
                "File has the focus",
            );
            s.key(VK_RIGHT);
            s.key(VK_DOWN);
        }),
        step(|s| {
            let tree = s.tree();
            s.expect(
                count_focused(&tree) == 1,
                format!("focused: {}", count_focused(&tree)),
            );
            let edit = menu_bar_groups()[1].submenu.clone().unwrap();
            let first = usable_rows(&edit)[0];
            s.expect(
                focused_name(&tree).as_deref() == Some(edit.items[first].label.as_str()),
                format!("focused {:?}", focused_name(&tree)),
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn the_open_menu_is_in_the_tree_with_rows_in_client_places() {
    let s = script(false);
    let (group, row, downs) = group_with_submenu();
    let model = menu_bar_groups()[group].submenu.clone().unwrap();
    let row_name = model.items[row].label.clone();
    let closed_name = row_name.clone();
    let steps = vec![
        step(move |s| {
            s.alt_letter(letter(group));
            for _ in 0..downs {
                s.key(VK_DOWN);
            }
        }),
        step(move |s| {
            let tree = s.tree();
            let label = &find_widget(&tree, "Menu bar").unwrap().children[group];
            s.expect(label.states.expanded, "the label says it is open");
            s.expect(label.children.len() == 1, "the label holds the open menu");
            let menu = &label.children[0];
            s.expect(menu.role == qe_api::WidgetRole::Menu, "it is a menu");
            let row = find_widget(menu, &closed_name).unwrap();
            s.expect(row.states.has_popup, "the row says it has a submenu");
            s.expect(row.states.focused, "the selected row has the focus");
            // Rows are in the window's dips, just under the label, whatever the screen.
            let under = label.bounds.y.0 + label.bounds.height.0;
            s.expect(
                (menu.bounds.y.0 - under).abs() <= 1.0,
                format!(
                    "the menu at {} and the label's bottom at {under}",
                    menu.bounds.y.0
                ),
            );
            s.expect(
                (menu.bounds.x.0 - label.bounds.x.0).abs() <= 1.0,
                format!(
                    "the menu's left {} and the label's {}",
                    menu.bounds.x.0, label.bounds.x.0
                ),
            );
            s.expect(
                row.bounds.y.0 >= menu.bounds.y.0,
                "the row is inside the menu",
            );
            s.key(VK_RIGHT);
        }),
        step(move |s| {
            let tree = s.tree();
            let row = find_widget(&tree, &row_name).unwrap();
            s.expect(row.children.len() == 1, "the open submenu is in the tree");
            s.expect(
                count_focused(&tree) == 1,
                format!("focused: {}", count_focused(&tree)),
            );
            s.key(VK_ESC);
            s.key_up(VK_ESC);
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
        step(|s| {
            let tree = s.tree();
            let bar = find_widget(&tree, "Menu bar").unwrap();
            s.expect(
                bar.children.iter().all(|label| label.children.is_empty()),
                "a closed menu stays out of the tree",
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_menu_of_the_owner_is_in_the_tree_and_the_strip_gives_up_its_focus() {
    let s = script(false);
    let steps = vec![
        step(|s| s.open_owner_menu()),
        step(|s| {
            s.window.with_strip(|strip| strip.focus_strip()).unwrap();
            let tree = s.tree();
            let first = owner_menu()
                .items
                .iter()
                .find(|i| i.command.is_some())
                .unwrap()
                .label
                .clone();
            s.expect(
                find_widget(&tree, &first).is_some(),
                "the menu's rows are in the tree",
            );
            s.expect(
                count_focused(&tree) == 1,
                format!("focused: {}", count_focused(&tree)),
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn alt_on_a_focused_strip_leaves_one_focus_owner() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            s.window.with_strip(|strip| strip.focus_strip()).unwrap();
            s.alt_tap();
        }),
        step(|s| {
            let tree = s.tree();
            s.expect(
                count_focused(&tree) == 1,
                format!("focused: {}", count_focused(&tree)),
            );
            s.expect(
                focused_name(&tree).as_deref() == Some("File"),
                "File has the focus",
            );
        }),
    ];
    finish(&s, steps);
}

fn press_label_and_release_on_row(group: usize) -> Vec<Step> {
    vec![
        step(move |s| {
            let r = s.label(group);
            let at = pack(r.x.0 as i32 + 4, r.y.0 as i32 + 10);
            s.post(WM_MOUSEMOVE, 0, at);
            s.post(WM_LBUTTONDOWN, 1, at);
        }),
        step(move |s| {
            s.expect_popups(1, "the press opens the menu");
            let popup = s.root_popup().unwrap();
            let model = menu_bar_groups()[group].submenu.clone().unwrap();
            let row = usable_rows(&model)[0];
            let y = row_middle(&model, row) as i32;
            s.post_to(popup, WM_MOUSEMOVE, 1, pack(20, y));
            s.post_to(popup, WM_LBUTTONUP, 0, pack(20, y));
        }),
        step(move |s| {
            let model = menu_bar_groups()[group].submenu.clone().unwrap();
            let row = usable_rows(&model)[0];
            s.expect(
                s.commands() == vec![model.items[row].command.unwrap()],
                format!("the row under the release runs once: {:?}", s.commands()),
            );
        }),
    ]
}
#[test]
fn a_press_on_a_label_and_a_release_on_a_row_chooses_that_row() {
    let s = script(false);
    let mut steps = press_label_and_release_on_row(1);
    Script::then_typing_reaches_text(&mut steps, "after a press and a release");
    finish(&s, steps);
}
#[test]
fn a_press_dragged_to_another_label_then_a_release_on_its_row_chooses_that_row() {
    let s = script(false);
    let mut steps = vec![
        step(|s| {
            let r = s.label(0);
            let at = pack(r.x.0 as i32 + 4, r.y.0 as i32 + 10);
            s.post(WM_MOUSEMOVE, 0, at);
            s.post(WM_LBUTTONDOWN, 1, at);
        }),
        step(|s| {
            let popup = s.root_popup().unwrap();
            let r = s.label(1);
            s.post_to(
                popup,
                WM_MOUSEMOVE,
                1,
                s.on_popup(popup, r.x.0 + 4.0, r.y.0 + 10.0),
            );
        }),
        step(|s| {
            s.expect(s.open_label() == Some(1), "the drag switched to Edit");
            let popup = s.root_popup().unwrap();
            let model = menu_bar_groups()[1].submenu.clone().unwrap();
            let y = row_middle(&model, usable_rows(&model)[0]) as i32;
            s.post_to(popup, WM_MOUSEMOVE, 1, pack(20, y));
            s.post_to(popup, WM_LBUTTONUP, 0, pack(20, y));
        }),
        step(|s| s.expect(s.commands().len() == 1, format!("ran {:?}", s.commands()))),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a drag across labels");
    finish(&s, steps);
}
#[test]
fn a_press_on_a_label_released_on_a_submenu_row_chooses_that_row() {
    let s = script(false);
    let (group, row, _) = group_with_submenu();
    let delay = UiTokens::sizes().submenu_delay_ms;
    let mut steps = vec![
        step(move |s| {
            let r = s.label(group);
            let at = pack(r.x.0 as i32 + 4, r.y.0 as i32 + 10);
            s.post(WM_MOUSEMOVE, 0, at);
            s.post(WM_LBUTTONDOWN, 1, at);
        }),
        step(move |s| {
            let popup = s.root_popup().unwrap();
            let model = menu_bar_groups()[group].submenu.clone().unwrap();
            s.post_to(
                popup,
                WM_MOUSEMOVE,
                1,
                pack(20, row_middle(&model, row) as i32),
            );
        }),
        wait(delay + 150),
        step(move |s| {
            s.expect_popups(2, "the submenu opened under the held press");
            let root = s.root_popup().unwrap();
            let child = s.popups().into_iter().find(|p| *p != root).unwrap();
            let model = menu_bar_groups()[group].submenu.clone().unwrap();
            let sub = model.items[row].submenu.clone().unwrap();
            let first = usable_rows(&sub)
                .into_iter()
                .find(|i| sub.items[*i].command.is_some())
                .expect("a submenu row with a command");
            let mut at = POINT {
                x: 20,
                y: row_middle(&sub, first) as i32,
            };
            unsafe {
                let _ = ClientToScreen(child, &mut at);
                let _ = ScreenToClient(root, &mut at);
            }
            s.post_to(root, WM_MOUSEMOVE, 1, pack(at.x, at.y));
            s.post_to(root, WM_LBUTTONUP, 0, pack(at.x, at.y));
        }),
        step(|s| s.expect(s.commands().len() == 1, format!("ran {:?}", s.commands()))),
    ];
    Script::then_typing_reaches_text(&mut steps, "after a release on a submenu row");
    finish(&s, steps);
}
#[test]
fn a_press_on_a_label_released_on_the_label_keeps_the_menu_open_and_runs_nothing() {
    let s = script(false);
    let steps = vec![
        step(|s| s.click_label(1)),
        step(|s| {
            s.expect_popups(1, "the click opens Edit");
            s.expect(s.commands().is_empty(), "nothing ran");
        }),
    ];
    finish(&s, steps);
}

#[test]
fn keys_in_one_batch_each_go_where_their_own_moment_says() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            s.key(usize::from(b'X'));
            s.key(usize::from(b'Y'));
        }),
        step(|s| {
            s.expect(s.focused_label() == Some(0), "the bar keeps the focus");
            s.expect(s.typed().is_empty(), format!("typed {:?}", s.typed()));
            s.key_up(usize::from(b'X'));
            s.key_up(usize::from(b'Y'));
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
        step(|s| s.alt_letter(b'E')),
        step(|s| {
            s.key(VK_ENTER);
            s.key(usize::from(b'X'));
        }),
        step(|s| {
            s.expect(s.commands().len() == 1, format!("ran {:?}", s.commands()));
            s.expect(s.typed() == vec!['x'], format!("typed {:?}", s.typed()));
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_dismissed_menu_runs_nothing_for_a_key_already_queued() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'E')),
        step(|s| {
            let popup = s.root_popup().unwrap();
            let (x, y) = s.text_point();
            s.post_to(popup, WM_LBUTTONDOWN, 1, s.on_popup(popup, x, y));
            s.key(VK_ENTER);
        }),
        step(|s| {
            s.expect(
                s.commands().is_empty(),
                format!("closed menu ran {:?}", s.commands()),
            );
            s.expect_popups(0, "after the click away");
            s.expect(s.focused_label().is_none(), "the bar let go");
            s.expect(s.open_label().is_none(), "no label is open");
        }),
    ];
    finish(&s, steps);
}
/// The `rects[depth]` question: a view with fewer levels than its windows.
#[test]
fn a_key_queued_behind_a_dismissal_cannot_paint_a_level_that_went_away() {
    let s = script(false);
    let mut steps = Vec::new();
    open_submenu_by_keys(&mut steps);
    steps.push(step(|s| {
        let popup = s.root_popup().unwrap();
        let (x, y) = s.text_point();
        s.post_to(popup, WM_LBUTTONDOWN, 1, s.on_popup(popup, x, y));
        s.key(VK_LEFT);
    }));
    steps.push(step(|s| s.expect_popups(0, "after the click away")));
    finish(&s, steps);
}
fn alt_chord_keeps_the_menu_open(release_alt_first: bool) {
    let s = script(false);
    let steps = vec![
        step(|s| s.click_label(0)),
        step(move |s| {
            s.alt_down();
            s.post(WM_SYSKEYDOWN, usize::from(b'Z'), ALT_BIT | 1);
            if release_alt_first {
                s.alt_up();
                s.post(WM_KEYUP, usize::from(b'Z'), REPEAT_UP);
            } else {
                s.post(WM_SYSKEYUP, usize::from(b'Z'), ALT_BIT | REPEAT_UP);
                s.alt_up();
            }
        }),
        step(|s| s.expect_popups(1, "an Alt chord is no tap")),
    ];
    finish(&s, steps);
}
#[test]
fn an_alt_chord_released_alt_first_keeps_the_menu_open() {
    alt_chord_keeps_the_menu_open(true);
}
#[test]
fn an_alt_chord_released_key_first_keeps_the_menu_open() {
    alt_chord_keeps_the_menu_open(false);
}
#[test]
fn opening_a_menu_of_the_owner_over_a_bar_menu_closes_the_label() {
    let s = script(false);
    let mut steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| {
            s.expect(s.open_label() == Some(0), "File is open");
            s.open_owner_menu();
            s.expect_popups(1, "the owner's menu replaced the bar's");
            s.expect(s.open_label().is_none(), "File still says it is open");
            s.expect(s.focused_label().is_none(), "File still has the focus");
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after the owner's menu replaced a bar menu");
    finish(&s, steps);
}

fn this_change_ends_the_menu(change: fn(&Script), submenu: bool) {
    let s = script(false);
    let mut steps = Vec::new();
    if submenu {
        open_submenu_by_keys(&mut steps);
    } else {
        steps.push(step(|s| s.alt_letter(b'F')));
        steps.push(step(|s| s.expect_popups(1, "Alt+F")));
    }
    steps.push(step(change));
    Script::then_typing_reaches_text(&mut steps, "after the window changed");
    finish(&s, steps);
}
fn dpi_change(s: &Script) {
    let mut outer = RECT::default();
    unsafe { GetWindowRect(s.hwnd, &mut outer) }.unwrap();
    // SAFETY: `outer` lives for the call, as this message's documentation asks.
    unsafe {
        SendMessageW(
            s.hwnd,
            WM_DPICHANGED,
            Some(WPARAM(96 | (96 << 16))),
            Some(LPARAM(&raw const outer as isize)),
        );
    }
}
#[test]
fn a_lost_focus_ends_the_menu() {
    this_change_ends_the_menu(|s| s.post(WM_KILLFOCUS, 0, 0), false);
}
#[test]
fn a_lost_focus_ends_a_submenu_too() {
    this_change_ends_the_menu(|s| s.post(WM_KILLFOCUS, 0, 0), true);
}
#[test]
fn a_dpi_change_ends_the_menu() {
    this_change_ends_the_menu(dpi_change, false);
}
#[test]
fn a_size_change_ends_the_menu() {
    this_change_ends_the_menu(|s| s.post(WM_SIZE, 0, pack(900, 600)), false);
}
#[test]
fn a_move_ends_the_menu() {
    this_change_ends_the_menu(|s| s.post(WM_MOVE, 0, pack(30, 30)), false);
}
#[test]
fn a_display_change_ends_the_menu() {
    this_change_ends_the_menu(|s| s.post(WM_DISPLAYCHANGE, 32, pack(1920, 1080)), false);
}
#[test]
fn destroying_the_window_ends_the_menu_with_no_event_after() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| {
            s.expect_popups(1, "Alt+F");
            s.window.destroy().unwrap();
            s.expect_popups(0, "the window went, and the popups with it");
        }),
    ];
    finish(&s, steps);
    assert!(s.was_destroyed());
    assert_eq!(s.popups().len(), 0);
}

#[test]
fn a_lost_focus_lets_go_of_the_bar() {
    let s = script(false);
    let mut steps = vec![step(|s| s.alt_tap()), step(|s| s.post(WM_KILLFOCUS, 0, 0))];
    Script::then_typing_reaches_text(&mut steps, "after a lost focus");
    finish(&s, steps);
}
#[test]
fn a_mouse_opened_menu_selects_no_row_and_a_mnemonic_runs_nothing() {
    let s = script(false);
    let model = menu_bar_groups()[0].submenu.clone().unwrap();
    let letter = model
        .items
        .iter()
        .find_map(|item| item.mnemonic.filter(|_| item.enabled))
        .expect("a row with a mnemonic");
    let steps = vec![
        step(|s| s.click_label(0)),
        step(move |s| {
            let tree = s.tree();
            s.expect(
                focused_name(&tree).as_deref() == Some("File"),
                format!(
                    "no row is selected, but {:?} has the focus",
                    focused_name(&tree)
                ),
            );
            s.post(WM_CHAR, letter.to_ascii_lowercase() as usize, 1);
        }),
        step(|s| {
            s.expect_popups(1, "a mnemonic in a menu opened by mouse");
            s.expect(s.commands().is_empty(), format!("ran {:?}", s.commands()));
        }),
    ];
    finish(&s, steps);
}
#[test]
fn hovering_the_open_label_does_not_open_it_again() {
    let s = script(false);
    let steps = vec![
        step(|s| s.click_label(0)),
        step(|s| {
            let popup = s.root_popup().unwrap();
            let r = s.label(0);
            let at = s.on_popup(popup, r.x.0 + 6.0, r.y.0 + 10.0);
            s.post_to(popup, WM_MOUSEMOVE, 0, at);
            s.post_to(popup, WM_MOUSEMOVE, 0, at);
        }),
        step(|s| {
            s.expect(
                s.group_opens() == vec![0],
                format!("opens: {:?}", s.group_opens()),
            );
            s.expect_popups(1, "File stays");
        }),
    ];
    finish(&s, steps);
}
#[test]
fn hovering_a_label_with_no_menu_open_opens_nothing() {
    let s = script(false);
    let steps = vec![
        step(|s| s.post(WM_MOUSEMOVE, 0, center(s.label(1)))),
        step(|s| {
            s.expect(
                s.group_opens().is_empty(),
                format!("opens: {:?}", s.group_opens()),
            );
            s.expect_popups(0, "a hover");
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_dismissal_notice_from_another_menu_changes_nothing() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_letter(b'F')),
        step(|s| {
            s.post(
                qe_ui::POPUP_DISMISSED_MESSAGE,
                0x1234,
                qe_ui::PopupDismissed::OutsideClick as isize,
            );
        }),
        step(|s| {
            s.expect_popups(1, "a stale notice");
            s.expect(s.open_label() == Some(0), "File stays open");
        }),
    ];
    finish(&s, steps);
}
#[test]
fn altgr_with_a_letter_does_not_switch_the_open_menu_or_open_one() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            hold(&[VK_CTRL, VK_MENU_KEY]);
            s.post(WM_KEYDOWN, usize::from(b'V'), ALT_BIT | 1);
        }),
        step(|s| {
            s.expect_popups(0, "AltGr+V with no menu");
            s.expect(s.focused_label().is_none(), "the bar takes no focus");
            hold(&[]);
            s.alt_letter(b'F');
        }),
        step(|s| {
            hold(&[VK_CTRL, VK_MENU_KEY]);
            s.post(WM_KEYDOWN, usize::from(b'V'), ALT_BIT | 1);
        }),
        step(|s| {
            s.expect(s.open_label() == Some(0), "AltGr+V switched the menu");
            s.expect(
                s.group_opens() == vec![0],
                format!("opens: {:?}", s.group_opens()),
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn alt_tapped_with_ctrl_or_shift_held_does_not_focus_the_bar() {
    let s = script(false);
    let steps = vec![
        step(|s| {
            hold(&[VK_CTRL, VK_MENU_KEY]);
            s.alt_down();
        }),
        step(|s| {
            hold(&[VK_CTRL]);
            s.alt_up();
        }),
        step(|s| {
            s.expect(s.focused_label().is_none(), "Ctrl+Alt is no tap");
            hold(&[VK_SHIFT, VK_MENU_KEY]);
            s.alt_down();
        }),
        step(|s| {
            hold(&[VK_SHIFT]);
            s.alt_up();
        }),
        step(|s| s.expect(s.focused_label().is_none(), "Shift+Alt is no tap")),
    ];
    finish(&s, steps);
}
#[test]
fn leaving_the_bar_tells_the_owner_once_to_focus_the_text() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_tap()),
        step(|s| {
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
        step(|s| {
            let told = s
                .seen
                .borrow()
                .iter()
                .filter(|event| **event == ShellEvent::FocusContent)
                .count();
            s.expect(told == 1, format!("told {told} times"));
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_menu_at_150_percent_opens_under_its_label() {
    let s = script_at(false, Some(144));
    let steps = vec![
        step(|s| {
            // High on the screen, so that the menu has room to open downward.
            unsafe {
                SetWindowPos(
                    s.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            }
            .unwrap();
            s.alt_letter(b'W');
        }),
        step(|s| {
            s.expect_popups(1, "Alt+W");
            let r = s.label(4);
            let mut at = POINT {
                x: (r.x.0 * 1.5).round() as i32,
                y: ((r.y.0 + r.height.0) * 1.5).round() as i32,
            };
            let _ = unsafe { ClientToScreen(s.hwnd, &mut at) };
            let mut popup = RECT::default();
            unsafe { GetWindowRect(s.root_popup().unwrap(), &mut popup) }.unwrap();
            s.expect(
                (popup.left - at.x).abs() <= 1 && (popup.top - at.y).abs() <= 1,
                format!(
                    "popup at {},{}, wanted {},{}",
                    popup.left, popup.top, at.x, at.y
                ),
            );
        }),
    ];
    finish(&s, steps);
}

#[test]
fn alt_with_a_lost_focus_is_no_tap() {
    alt_is_no_tap_after(|s| s.post(WM_KILLFOCUS, 0, 0));
}

/// A key that opens a menu, held: its repeats must not reach a row by that row's letter.
#[test]
fn a_repeat_of_a_letter_runs_no_row_by_its_mnemonic() {
    let s = script(false);
    let steps = vec![
        step(|s| s.key(VK_DOWN)),
        step(|s| {
            let mut row = qe_ui::MenuItem::command("Xylophone", CommandId::Print);
            row.mnemonic = Some('x');
            let anchor = s.label(0);
            s.window
                .open_menu(MenuModel { items: vec![row] }, anchor)
                .unwrap();
            s.post(WM_KEYDOWN, usize::from(b'X'), REPEAT_BIT | 1);
        }),
        step(|s| {
            s.expect(s.commands().is_empty(), format!("ran {:?}", s.commands()));
            s.key(usize::from(b'X'));
        }),
        step(|s| {
            s.expect(
                s.commands().len() == 1,
                "a fresh press of the letter does run it",
            );
        }),
    ];
    finish(&s, steps);
}
#[test]
fn a_stray_character_stays_out_of_the_text_while_the_bar_has_focus() {
    let s = script(false);
    let steps = vec![
        step(|s| s.alt_tap()),
        step(|s| s.post(WM_CHAR, usize::from(b'q'), 1)),
        step(|s| {
            s.expect(s.typed().is_empty(), format!("typed {:?}", s.typed()));
            s.expect(s.focused_label() == Some(0), "the bar keeps the focus");
        }),
    ];
    finish(&s, steps);
}

// ---- Alt+F4 with no command behind it: the whole way through the default handler.

/// Makes the hidden window this thread's active window, which is what `DefWindowProcW` looks
/// for before it sends `SC_CLOSE`. This does not touch the foreground, and nothing shows.
fn make_active(s: &Script) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetActiveWindow, SetActiveWindow};
    // SAFETY: plain calls on a window of this thread.
    let active = unsafe {
        let _ = SetActiveWindow(s.hwnd);
        GetActiveWindow()
    };
    s.expect(active == s.hwnd, format!("the active window is {active:?}"));
}

/// The real order of a press: Alt, then F4, each as `WM_SYSKEYDOWN` with bit 29, both held in
/// the thread's key state. Nothing here is a test double. Windows of other tests in this
/// process can take the active state away from a hidden window, so each case runs by itself
/// in a child process, and the parent asks for that child's result.
fn alt_f4_closes_the_window_through_windows(name: &str, open: fn() -> Vec<Step>) {
    const CHILD: &str = "QE_SHELL_ALT_F4_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let exe = std::env::current_exe().unwrap();
        let output = std::process::Command::new(exe)
            .args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("1 passed"),
            "the child ran nothing: {stdout}"
        );
        return;
    }
    let s = script(false);
    let mut steps = open();
    steps.push(step(|s| {
        make_active(s);
        hold(&[VK_MENU_KEY, 0x73]);
        s.post(WM_SYSKEYDOWN, VK_ALT, ALT_BIT | 1);
        s.post(WM_SYSKEYDOWN, VK_F4, ALT_BIT | 1);
    }));
    steps.push(step(|s| s.expect(false, "the window outlived Alt+F4")));
    finish(&s, steps);
    assert!(s.was_destroyed(), "the window was not destroyed");
    assert_eq!(s.popups().len(), 0);
    let close_requests = s
        .seen
        .borrow()
        .iter()
        .filter(|event| **event == ShellEvent::CloseWindowRequested)
        .count();
    assert_eq!(close_requests, 1);
    assert!(s.commands().is_empty(), "ran {:?}", s.commands());
}

#[test]
fn alt_f4_closes_the_window_from_the_text() {
    alt_f4_closes_the_window_through_windows("alt_f4_closes_the_window_from_the_text", Vec::new);
}

#[test]
fn alt_f4_closes_the_window_from_a_focused_menu_bar() {
    alt_f4_closes_the_window_through_windows(
        "alt_f4_closes_the_window_from_a_focused_menu_bar",
        || {
            vec![
                step(|s| s.alt_tap()),
                step(|s| s.expect(s.focused_label() == Some(0), "Alt focuses the bar")),
            ]
        },
    );
}

#[test]
fn alt_f4_closes_the_window_with_a_menu_open() {
    alt_f4_closes_the_window_through_windows("alt_f4_closes_the_window_with_a_menu_open", || {
        vec![
            step(|s| s.alt_letter(b'F')),
            step(|s| s.expect_popups(1, "Alt+F")),
        ]
    });
}

#[test]
fn alt_f4_closes_the_window_with_a_submenu_open() {
    alt_f4_closes_the_window_through_windows(
        "alt_f4_closes_the_window_with_a_submenu_open",
        || {
            let mut steps = Vec::new();
            open_submenu_by_keys(&mut steps);
            steps
        },
    );
}

/// The menu a right-click on the title bar opens is the one Windows owns, and it keeps Close.
#[test]
fn the_system_menu_still_has_an_enabled_close() {
    let s = script(false);
    // SAFETY: plain queries on a window of this thread. No menu opens.
    unsafe {
        let menu = GetSystemMenu(s.hwnd, false);
        assert!(!menu.is_invalid(), "the window has no system menu");
        let state = GetMenuState(menu, SC_CLOSE, MF_BYCOMMAND);
        assert_ne!(state, u32::MAX, "no Close row");
        assert_eq!(
            state & (MF_GRAYED.0 | MF_DISABLED.0),
            0,
            "Close is disabled"
        );
    }
}
