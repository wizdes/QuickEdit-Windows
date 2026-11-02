//! Native window behavior, with hidden windows only. Nothing here shows a window.
//! Each test runs its windows on its own thread, because windows belong to a thread.
use qe_ui::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// A plain hidden top-level window for owners.
struct Owner(HWND);
impl Owner {
    fn new() -> Self {
        // SAFETY: a built-in class, a hidden window and a null parent.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("owner"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                None,
                None,
                None,
                None,
            )
        }
        .expect("owner window");
        Self(hwnd)
    }
    fn enabled(&self) -> bool {
        // SAFETY: a plain query.
        unsafe { IsWindowEnabled(self.0) }.as_bool()
    }
    fn set_enabled(&self, on: bool) {
        // SAFETY: the window is alive.
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(self.0, on);
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: the window is alive, or the call fails harmlessly.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
fn alive(hwnd: HWND) -> bool {
    // SAFETY: a plain query.
    unsafe { IsWindow(Some(hwnd)) }.as_bool()
}
fn visible(hwnd: HWND) -> bool {
    // SAFETY: a plain query.
    unsafe { IsWindowVisible(hwnd) }.as_bool()
}
fn send_close(hwnd: HWND) {
    // SAFETY: a live window on this thread.
    unsafe {
        SendMessageW(hwnd, WM_CLOSE, None, None);
    }
}
fn on_thread(test: impl FnOnce() + Send + 'static) {
    std::thread::spawn(test).join().expect("the test thread");
}

// Item 4
#[test]
fn native_close_gives_the_owner_back_and_does_not_destroy_twice() {
    on_thread(|| {
        let owner = Owner::new();
        let dialog =
            DialogWindow::create_with_owner("Test", 300, 200, true, false, Some(owner.0)).unwrap();
        let hwnd = dialog.hwnd();
        assert!(!owner.enabled(), "a modal dialog disables its owner");
        assert!(!visible(hwnd), "windows start hidden");
        send_close(hwnd);
        assert!(owner.enabled(), "the owner is free after the close button");
        assert!(dialog.closed_by_user());
        assert!(!dialog.is_alive());
        assert!(!alive(hwnd));
        drop(dialog);
        assert!(owner.enabled());
        // The close notice reached the owner's queue.
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out pointer.
        let found = unsafe {
            PeekMessageW(
                &mut msg,
                Some(owner.0),
                DIALOG_CLOSED_MESSAGE,
                DIALOG_CLOSED_MESSAGE,
                PM_REMOVE,
            )
        };
        assert!(found.as_bool(), "the owner got DIALOG_CLOSED_MESSAGE");
        assert_eq!(msg.wParam.0, hwnd.0 as usize);
    });
}
#[test]
fn owner_is_enabled_before_the_dialog_is_destroyed() {
    on_thread(|| {
        let owner = Owner::new();
        let dialog =
            DialogWindow::create_with_owner("Test", 300, 200, true, false, Some(owner.0)).unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let check = seen.clone();
        let owner_hwnd = owner.0.0 as usize;
        dialog.set_message_handler(Box::new(move |_, msg, _, _| {
            if msg == WM_DESTROY {
                // SAFETY: a plain query.
                let on = unsafe { IsWindowEnabled(HWND(owner_hwnd as *mut _)) }.as_bool();
                check.store(if on { 1 } else { 2 }, Ordering::SeqCst);
            }
            None
        }));
        drop(dialog);
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "activation must find the owner enabled when the dialog goes away"
        );
    });
}
#[test]
fn an_owner_disabled_before_the_dialog_stays_disabled() {
    on_thread(|| {
        let owner = Owner::new();
        owner.set_enabled(false);
        let dialog =
            DialogWindow::create_with_owner("Test", 300, 200, true, false, Some(owner.0)).unwrap();
        drop(dialog);
        assert!(!owner.enabled());
    });
}
#[test]
fn two_modal_dialogs_release_the_owner_only_at_the_last() {
    on_thread(|| {
        let owner = Owner::new();
        let one =
            DialogWindow::create_with_owner("A", 300, 200, true, false, Some(owner.0)).unwrap();
        let two =
            DialogWindow::create_with_owner("B", 300, 200, true, false, Some(owner.0)).unwrap();
        drop(one);
        assert!(!owner.enabled(), "the other dialog still holds the owner");
        send_close(two.hwnd());
        assert!(owner.enabled());
    });
}
#[test]
fn modal_with_no_owner_is_an_error() {
    on_thread(|| {
        let error = DialogWindow::create("Test", 300, 200, true, false)
            .err()
            .expect("modal with no owner must fail");
        assert!(error.message().contains("owner"), "{error}");
        let plain = DialogWindow::create("Test", 300, 200, false, true).unwrap();
        assert!(!plain.modal());
        assert!(plain.resizable());
    });
}
#[test]
fn destroying_the_owner_first_leaves_a_safe_dialog() {
    on_thread(|| {
        let owner = Owner::new();
        let dialog =
            DialogWindow::create_with_owner("Test", 300, 200, true, false, Some(owner.0)).unwrap();
        let popup = PopupWindow::create(owner.0, 0, 0, 50, 50).unwrap();
        drop(owner);
        assert!(
            !dialog.is_alive(),
            "Windows destroys owned windows with their owner"
        );
        assert!(!popup.is_alive());
    });
}
#[test]
fn a_dialog_paints_through_its_handler_on_wm_paint() {
    on_thread(|| {
        let dialog = DialogWindow::create("Test", 300, 200, false, false).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        dialog.set_paint_handler(Box::new(move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        // SAFETY: a live window on this thread.
        unsafe {
            SendMessageW(dialog.hwnd(), WM_PAINT, None, None);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

// Item 5
#[test]
fn popup_has_an_owner_and_takes_no_activation() {
    on_thread(|| {
        let owner = Owner::new();
        let popup = PopupWindow::create(owner.0, 10, 10, 200, 100).unwrap();
        let hwnd = popup.hwnd();
        assert!(!visible(hwnd));
        // SAFETY: plain queries and a message sent to a live window on this thread.
        unsafe {
            assert_eq!(GetWindow(hwnd, GW_OWNER).unwrap(), owner.0);
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(ex & WS_EX_TOOLWINDOW.0, 0, "no taskbar entry");
            assert_ne!(ex & WS_EX_NOACTIVATE.0, 0, "no activation");
            let answer = SendMessageW(hwnd, WM_MOUSEACTIVATE, None, None);
            assert_eq!(answer.0 as u32, MA_NOACTIVATE);
            // A press lands on the popup and does not pass through it.
            let hit = SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM((50 << 16) | 50)));
            assert_eq!(hit.0 as u32, HTCLIENT, "the popup takes mouse input");
        }
    });
}
#[test]
fn outside_press_dismisses_and_notifies_the_owner() {
    on_thread(|| {
        let owner = Owner::new();
        let popup = PopupWindow::create(owner.0, 10, 10, 200, 100).unwrap();
        let hwnd = popup.hwnd();
        popup.show_for_test();
        assert_eq!(popup.dismissed(), None);
        // A press inside does nothing.
        // SAFETY: a live window on this thread.
        unsafe {
            SendMessageW(
                hwnd,
                WM_LBUTTONDOWN,
                Some(WPARAM(0)),
                Some(LPARAM((10 << 16) | 10)),
            );
        }
        assert_eq!(popup.dismissed(), None);
        // Client coordinates of a press outside are past the window's size.
        // SAFETY: a live window on this thread.
        unsafe {
            SendMessageW(
                hwnd,
                WM_LBUTTONDOWN,
                Some(WPARAM(0)),
                Some(LPARAM((500 << 16) | 500)),
            );
        }
        assert_eq!(popup.dismissed(), Some(PopupDismissed::OutsideClick));
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out pointer.
        let found = unsafe {
            PeekMessageW(
                &mut msg,
                Some(owner.0),
                POPUP_DISMISSED_MESSAGE,
                POPUP_DISMISSED_MESSAGE,
                PM_REMOVE,
            )
        };
        assert!(found.as_bool());
        assert_eq!(msg.lParam.0, PopupDismissed::OutsideClick as isize);
    });
}
#[test]
fn popup_paints_with_its_own_window() {
    on_thread(|| {
        let owner = Owner::new();
        let popup = PopupWindow::create(owner.0, 10, 10, 200, 100).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        popup.set_paint_handler(Box::new(move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        // SAFETY: a live window on this thread.
        unsafe {
            SendMessageW(popup.hwnd(), WM_PAINT, None, None);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}
#[test]
fn popup_reports_or_has_no_decoration_error() {
    on_thread(|| {
        let owner = Owner::new();
        let popup = PopupWindow::create(owner.0, 10, 10, 200, 100).unwrap();
        // On Windows 11 the call works. The getter is the route for a failure.
        assert!(
            popup.decoration_error().is_none(),
            "{:?}",
            popup.decoration_error()
        );
    });
}
#[test]
fn anchored_popup_stays_on_the_monitor() {
    on_thread(|| {
        let owner = Owner::new();
        let anchor = windows::Win32::Foundation::RECT {
            left: 100_000,
            top: 100_000,
            right: 100_020,
            bottom: 100_020,
        };
        let popup = PopupWindow::create_anchored(owner.0, anchor, 200, 100, false).unwrap();
        let b = popup.bounds().unwrap();
        assert!(b.right < 100_000, "{b:?}");
    });
}
#[test]
fn a_missing_theme_value_does_not_block_a_dialog() {
    // The registry value cannot be removed from a test, so this checks that the
    // result of reading it can no longer fail creation: the note is only a note.
    on_thread(|| {
        let dialog = DialogWindow::create("Test", 300, 200, false, false).unwrap();
        assert!(dialog.theme_note().is_none(), "{:?}", dialog.theme_note());
    });
}
#[test]
fn system_colors_read_without_error() {
    let palette = system_palette();
    assert_ne!(palette.window, palette.window_text);
    let active = high_contrast_active().unwrap();
    println!("high contrast on this PC: {active}");
}

// Item 25: native dialogs, built but never shown.
fn on_com_thread(test: impl FnOnce() + Send + 'static) {
    on_thread(|| {
        // SAFETY: starts COM on this new thread, ended below.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .unwrap();
        test();
        // SAFETY: pairs with the call above.
        unsafe { CoUninitialize() };
    });
}
#[test]
fn save_dialog_has_encodings_and_the_txt_checkbox() {
    on_com_thread(|| {
        let options = FileDialogOptions {
            title: Some("Save".into()),
            file_name: Some("Untitled".into()),
            encodings: vec![
                EncodingRow::new("UTF-8"),
                EncodingRow {
                    name: "Windows-1252".into(),
                    enabled: false,
                },
                EncodingRow::new("UTF-16"),
            ],
            selected: 2,
            add_txt: Some(true),
        };
        let dialog = FileDialogSetup::save(&options).unwrap();
        assert_eq!(dialog.selected_encoding().unwrap(), Some(2));
        assert_eq!(dialog.add_txt().unwrap(), Some(true));
        assert!(dialog.encoding_enabled(0).unwrap());
        assert!(
            !dialog.encoding_enabled(1).unwrap(),
            "P23: a row that cannot hold the text"
        );
    });
}
#[test]
fn save_dialog_does_not_start_on_a_disabled_row() {
    on_com_thread(|| {
        let options = FileDialogOptions {
            encodings: vec![
                EncodingRow {
                    name: "Windows-1252".into(),
                    enabled: false,
                },
                EncodingRow::new("UTF-8"),
            ],
            selected: 0,
            ..Default::default()
        };
        let dialog = FileDialogSetup::save(&options).unwrap();
        assert_eq!(dialog.selected_encoding().unwrap(), Some(1));
        assert_eq!(dialog.add_txt().unwrap(), None);
    });
}
#[test]
fn open_dialog_offers_automatic_and_named_encodings() {
    on_com_thread(|| {
        let options = FileDialogOptions {
            encodings: vec![EncodingRow::new("Automatic"), EncodingRow::new("UTF-8")],
            ..Default::default()
        };
        let dialog = FileDialogSetup::open(&options).unwrap();
        assert_eq!(dialog.selected_encoding().unwrap(), Some(0));
    });
}

/// This replaces the user's clipboard, so it runs only on request:
/// `QE_UI_TEST_REAL_CLIPBOARD=1 cargo test -p qe-ui --locked --test native real_clipboard`.
#[test]
fn real_clipboard_round_trip_when_asked() {
    if std::env::var_os("QE_UI_TEST_REAL_CLIPBOARD").is_none() {
        println!("not run: set QE_UI_TEST_REAL_CLIPBOARD=1 to let it replace the clipboard");
        return;
    }
    let board = SystemClipboard;
    board.write_text("héllo 😀").unwrap();
    assert_eq!(board.read_text().unwrap().as_deref(), Some("héllo 😀"));
}
