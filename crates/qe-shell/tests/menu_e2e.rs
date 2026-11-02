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
    Renderer,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_shell::{
    ShellError, ShellEvent, ShellWindow, WindowConfig, quit_message_loop, run_message_loop,
};
use qe_ui::{
    MenuModel, UiTokens, caret_menu, foreground_changed_for_test,
    ignore_foreground_changes_for_test, menu_bar_groups,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
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
fn script(record_system_commands: bool) -> Rc<Script> {
    ignore_foreground_changes_for_test();
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            record_system_commands,
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
            ShellEvent::OpenCaretList(anchor) => {
                if let Err(problem) = handle.open_menu(caret_menu(), anchor) {
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
fn the_caret_list_runs_the_same_way_and_leaves_the_bar_alone() {
    let s = script(false);
    let caret = |s: &Script| {
        let r = s.window.with_strip(|strip| strip.split_rect()).unwrap();
        // The caret is the right half of the split button, in the window's client area.
        pack(
            (r.x.0 + r.width.0 - 6.0) as i32,
            (r.y.0 + r.height.0 / 2.0) as i32,
        )
    };
    let mut steps = vec![
        step(move |s| {
            s.post(WM_MOUSEMOVE, 0, caret(s));
            s.post(WM_LBUTTONDOWN, 1, caret(s));
            s.post(WM_LBUTTONUP, 0, caret(s));
        }),
        step(|s| {
            s.expect_popups(1, "a click on the caret");
            s.expect(s.focused_label().is_none(), "the bar takes no focus");
            s.key(VK_ESC);
            s.key_up(VK_ESC);
        }),
    ];
    Script::then_typing_reaches_text(&mut steps, "after Esc in the caret list");
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
    let failed = window.open_menu(caret_menu(), anchor);
    assert!(
        matches!(&failed, Err(ShellError::Menu(message)) if message.contains("set_popup_renderers")),
        "{failed:?}"
    );
    assert!(!window.menu_is_open().unwrap());
}
