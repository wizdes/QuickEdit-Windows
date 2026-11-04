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
        popup.begin_hidden().unwrap();
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
            pdf: None,
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

/// Replaces the user's clipboard, so it runs only on request, like the test above.
#[test]
fn real_clipboard_keeps_text_after_a_zero_when_asked() {
    if std::env::var_os("QE_UI_TEST_REAL_CLIPBOARD").is_none() {
        println!("not run: set QE_UI_TEST_REAL_CLIPBOARD=1 to let it replace the clipboard");
        return;
    }
    let board = SystemClipboard;
    board.write_text("before\0after").unwrap();
    assert_eq!(board.read_text().unwrap().as_deref(), Some("before\0after"));
    // A later plain write leaves no old full text behind.
    board.write_text("plain").unwrap();
    assert_eq!(board.read_text().unwrap().as_deref(), Some("plain"));
}

// These look at the memory a clipboard write would use. None of them opens the clipboard.
#[test]
fn prepared_text_holds_all_of_the_text_and_its_end_zero() {
    let prepared = prepare_clipboard_text("héllo 😀").unwrap();
    let (text, full) = prepared.test_read();
    assert_eq!(text.unwrap(), "héllo 😀");
    assert!(full.is_none(), "text with no zero needs no second format");
    let empty = prepare_clipboard_text("").unwrap();
    assert_eq!(empty.test_read().0.unwrap(), "");
}
#[test]
fn prepared_text_with_a_zero_keeps_a_full_copy_beside_the_normal_one() {
    for value in ["before\0after 😀", "\0", "a\0", "\0\0b", "\0é"] {
        let prepared = prepare_clipboard_text(value).unwrap();
        let (text, full) = prepared.test_read();
        let cut_short = value.split('\0').next().unwrap();
        assert_eq!(text.unwrap(), cut_short, "other programs stop at the zero");
        assert_eq!(full.unwrap().unwrap(), value, "this editor gets it all");
    }
}
#[test]
fn prepared_text_can_be_made_on_another_thread_and_dropped_there() {
    let prepared = std::thread::spawn(|| prepare_clipboard_text("a\0b").unwrap())
        .join()
        .unwrap();
    assert_eq!(prepared.test_read().1.unwrap().unwrap(), "a\0b");
    std::thread::spawn(move || drop(prepared)).join().unwrap();
}
#[test]
fn a_cancelled_prepare_stops_and_makes_nothing() {
    let cancel = std::sync::atomic::AtomicBool::new(true);
    assert!(prepare_clipboard_text_unless(&"x".repeat(1 << 22), &cancel).is_err());
    assert!(prepare_clipboard_text_unless("a", &cancel).is_err());
}
#[test]
fn a_failed_format_name_still_reads_plain_text() {
    let plain = 13;
    assert_eq!(clipboard_read_format(None, plain, |_| true), Some(plain));
    assert_eq!(clipboard_read_format(None, plain, |_| false), None);
    assert_eq!(clipboard_read_format(Some(99), plain, |_| true), Some(99));
    assert_eq!(
        clipboard_read_format(Some(99), plain, |format| format == plain),
        Some(plain)
    );
}
#[test]
fn a_full_text_block_whose_count_does_not_fit_is_refused() {
    use windows::Win32::Foundation::GlobalFree;
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    // SAFETY: blocks this test owns and fills.
    unsafe {
        let small = GlobalAlloc(GMEM_MOVEABLE, 4).unwrap();
        let error = read_full_text_block(small).unwrap_err();
        assert!(error.contains("damaged"), "{error}");
        let _ = GlobalFree(Some(small));
        let memory = GlobalAlloc(GMEM_MOVEABLE, 16).unwrap();
        (GlobalLock(memory) as *mut u64).write_unaligned(1_000);
        let _ = GlobalUnlock(memory);
        let error = read_full_text_block(memory).unwrap_err();
        assert!(error.contains("damaged"), "{error}");
        let _ = GlobalFree(Some(memory));
    }
}
#[test]
fn clipboard_memory_failures_use_plain_words() {
    let error = reserve_text(usize::MAX).unwrap_err();
    assert!(error.contains("not enough memory"), "{error}");
    let error = decode_units(&[0xd800]).unwrap_err();
    assert!(error.contains("not valid Unicode"), "{error}");
}
/// Prints what a big clipboard write and read cost, with no clipboard. Run it by hand:
/// `cargo test -p qe-ui --release --locked --test native clipboard_costs -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn clipboard_costs_in_memory() {
    for mib in [1usize, 100, 512] {
        let text = "x".repeat(mib << 20);
        let start = std::time::Instant::now();
        let prepared = prepare_clipboard_text(&text).unwrap();
        let prepare = start.elapsed();
        let start = std::time::Instant::now();
        let back = prepared.test_read().0.unwrap();
        let read = start.elapsed();
        assert_eq!(back.len(), text.len());
        println!(
            "{mib} MiB: prepare off the UI thread {prepare:?}, decode on the UI thread at Paste {read:?}"
        );
    }
}

// Item 3: one popup for each level, on hidden windows, with the real capture path.
fn capture_holder() -> HWND {
    // SAFETY: a plain query.
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() }
}
fn all_menus_view(x: f32, y: f32) -> MenuView {
    let mut view = MenuView::new(
        qe_api::WidgetId(1),
        "All menus",
        MenuModel {
            items: menu_bar_groups(),
        },
    );
    view.bounds = qe_api::Rect {
        x: qe_api::Dip(x),
        y: qe_api::Dip(y),
        width: qe_api::Dip(200.0),
        height: qe_api::Dip(300.0),
    };
    view.open_keyboard();
    view
}
fn key(v: u16) -> qe_api::InputEvent {
    qe_ui::key(v, qe_api::Modifiers::default())
}
fn lparam_at(x: i32, y: i32) -> LPARAM {
    LPARAM(((y as u16 as isize) << 16) | (x as u16 as isize))
}
#[test]
fn a_submenu_window_does_not_close_the_list() {
    on_thread(|| {
        use qe_api::View;
        let owner = Owner::new();
        let mut view = all_menus_view(100.0, 100.0);
        let mut windows = MenuWindows::new_hidden(owner.0, 1.0);
        windows.sync(&view).unwrap();
        assert_eq!(windows.levels().len(), 1);
        let root = windows.levels()[0].hwnd();
        assert_eq!(capture_holder(), root, "the first window holds the mouse");

        view.handle(&key(0x27), &mut qe_api::ViewContext::default());
        windows.sync(&view).unwrap();
        assert_eq!(windows.levels().len(), 2);
        assert_eq!(
            windows.dismissed(),
            None,
            "a second window must not end the list"
        );
        assert_eq!(
            capture_holder(),
            root,
            "the capture stayed on the first window"
        );
        assert_eq!(
            // SAFETY: a plain query.
            unsafe { GetWindow(windows.levels()[1].hwnd(), GW_OWNER) }.unwrap(),
            root
        );
        // Each window is the size of its level.
        let child = view.levels()[1];
        let b = windows.levels()[1].bounds().unwrap();
        assert_eq!(b.left, child.x.0 as i32);
        assert_eq!(b.right - b.left, child.width.0 as i32);
        assert_ne!(b.right - b.left, 520);

        // A third level, then back out one at a time.
        view.handle(&key(0x1b), &mut qe_api::ViewContext::default());
        windows.sync(&view).unwrap();
        assert_eq!(windows.levels().len(), 1);
        assert_eq!(windows.dismissed(), None);
        assert_eq!(capture_holder(), root);
        assert_eq!(view.levels().len(), 1);
    });
}
#[test]
fn a_press_on_any_level_is_inside_and_elsewhere_ends_them_all() {
    on_thread(|| {
        use qe_api::View;
        let owner = Owner::new();
        let mut view = all_menus_view(100.0, 100.0);
        let mut windows = MenuWindows::new_hidden(owner.0, 1.0);
        windows.sync(&view).unwrap();
        view.handle(&key(0x27), &mut qe_api::ViewContext::default());
        windows.sync(&view).unwrap();
        let root = windows.levels()[0].hwnd();
        // A press in the second window, as the first window hears it with the capture.
        let child = windows.levels()[1].bounds().unwrap();
        let inside = lparam_at(child.left + 10 - 100, child.top + 10 - 100);
        // SAFETY: a live window on this thread.
        unsafe { SendMessageW(root, WM_LBUTTONDOWN, Some(WPARAM(0)), Some(inside)) };
        assert_eq!(windows.dismissed(), None, "a press on a submenu is inside");
        assert_eq!(
            windows.pointer(inside).unwrap().x.0,
            (child.left + 10) as f32
        );
        // SAFETY: a live window on this thread.
        unsafe {
            SendMessageW(
                root,
                WM_LBUTTONDOWN,
                Some(WPARAM(0)),
                Some(lparam_at(-90, -90)),
            )
        };
        assert_eq!(windows.dismissed(), Some(PopupDismissed::OutsideClick));
        assert_ne!(capture_holder(), root, "the capture is let go");
        let mut msg = MSG::default();
        let mut notices = 0;
        // SAFETY: `msg` is a valid out pointer.
        while unsafe {
            PeekMessageW(
                &mut msg,
                Some(owner.0),
                POPUP_DISMISSED_MESSAGE,
                POPUP_DISMISSED_MESSAGE,
                PM_REMOVE,
            )
        }
        .as_bool()
        {
            notices += 1;
        }
        assert_eq!(notices, 1, "the owner hears once for the whole group");
    });
}
#[test]
fn something_else_taking_the_mouse_ends_the_group() {
    on_thread(|| {
        let owner = Owner::new();
        let root = PopupWindow::create(owner.0, 10, 10, 100, 100).unwrap();
        let level = root.create_level(110, 10, 100, 100).unwrap();
        root.begin_hidden().unwrap();
        level.begin_hidden().unwrap();
        assert_eq!(capture_holder(), root.hwnd());
        // SAFETY: a live window on this thread.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(owner.0) };
        assert_eq!(root.dismissed(), Some(PopupDismissed::CaptureLost));
        assert_eq!(level.dismissed(), Some(PopupDismissed::CaptureLost));
    });
}
#[test]
fn another_program_becoming_active_ends_the_group() {
    on_thread(|| {
        let owner = Owner::new();
        let other = Owner::new();
        let root = PopupWindow::create(owner.0, 10, 10, 100, 100).unwrap();
        let level = root.create_level(110, 10, 100, 100).unwrap();
        assert!(!foreground_watch_active());
        root.begin_hidden().unwrap();
        assert!(
            foreground_watch_active(),
            "the watch is on while a list shows"
        );
        foreground_changed_for_test(owner.0);
        assert_eq!(
            root.dismissed(),
            None,
            "its own owner in front changes nothing"
        );
        foreground_changed_for_test(level.hwnd());
        assert_eq!(root.dismissed(), None, "nor does one of its levels");
        foreground_changed_for_test(other.0);
        assert_eq!(root.dismissed(), Some(PopupDismissed::Deactivated));
        assert!(!foreground_watch_active(), "the watch ends with the list");
    });
}
#[test]
fn focus_comes_back_when_the_list_goes() {
    on_thread(|| {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
        let owner = Owner::new();
        let other = Owner::new();
        // SAFETY: live windows on this thread.
        unsafe { SetFocus(Some(owner.0)).ok() };
        // SAFETY: a plain query.
        assert_eq!(unsafe { GetFocus() }, owner.0, "this thread can hold focus");
        let root = PopupWindow::create(owner.0, 10, 10, 100, 100).unwrap();
        root.begin_hidden().unwrap();
        // SAFETY: live windows on this thread.
        unsafe { SetFocus(Some(other.0)).ok() };
        // SAFETY: a plain query.
        assert_eq!(unsafe { GetFocus() }, other.0);
        root.dismiss();
        // SAFETY: a plain query.
        assert_eq!(unsafe { GetFocus() }, owner.0);
    });
}
#[test]
fn destroying_the_first_window_takes_the_levels_safely() {
    on_thread(|| {
        let owner = Owner::new();
        let root = PopupWindow::create(owner.0, 10, 10, 100, 100).unwrap();
        let level = root.create_level(110, 10, 100, 100).unwrap();
        root.begin_hidden().unwrap();
        let hwnd = level.hwnd();
        drop(root);
        assert!(!level.is_alive());
        assert!(!alive(hwnd));
        drop(level);
        assert!(!foreground_watch_active());
    });
}

// Item 7
#[test]
fn a_handler_may_drop_its_own_popup() {
    on_thread(|| {
        use std::cell::RefCell;
        use std::rc::Rc;
        let owner = Owner::new();
        let slot: Rc<RefCell<Option<PopupWindow>>> = Rc::new(RefCell::new(None));
        let popup = PopupWindow::create(owner.0, 10, 10, 100, 100).unwrap();
        let hwnd = popup.hwnd();
        let inner = slot.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        popup.set_message_handler(Box::new(move |_, msg, _, _| {
            if msg == WM_USER {
                seen.fetch_add(1, Ordering::SeqCst);
                let gone = inner.borrow_mut().take();
                drop(gone);
            }
            None
        }));
        *slot.borrow_mut() = Some(popup);
        // SAFETY: a live window on this thread.
        unsafe { SendMessageW(hwnd, WM_USER, None, None) };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!alive(hwnd));
        assert!(slot.borrow().is_none());
    });
}
#[test]
fn a_handler_may_drop_its_own_dialog() {
    on_thread(|| {
        use std::cell::RefCell;
        use std::rc::Rc;
        let slot: Rc<RefCell<Option<DialogWindow>>> = Rc::new(RefCell::new(None));
        let dialog = DialogWindow::create("Test", 100, 100, false, false).unwrap();
        let hwnd = dialog.hwnd();
        let inner = slot.clone();
        dialog.set_paint_handler(Box::new(move |_, _| {
            let gone = inner.borrow_mut().take();
            drop(gone);
        }));
        *slot.borrow_mut() = Some(dialog);
        // SAFETY: a live window on this thread.
        unsafe { SendMessageW(hwnd, WM_PAINT, None, None) };
        assert!(!alive(hwnd));
    });
}
#[test]
fn a_handler_may_replace_itself() {
    on_thread(|| {
        use std::cell::RefCell;
        use std::rc::Rc;
        let owner = Owner::new();
        let popup = Rc::new(RefCell::new(None::<PopupWindow>));
        let made = PopupWindow::create(owner.0, 10, 10, 100, 100).unwrap();
        let hwnd = made.hwnd();
        *popup.borrow_mut() = Some(made);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let again = popup.clone();
        popup
            .borrow()
            .as_ref()
            .unwrap()
            .set_message_handler(Box::new(move |_, msg, _, _| {
                if msg == WM_USER {
                    let count = count.clone();
                    again
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .set_message_handler(Box::new(move |_, msg, _, _| {
                            if msg == WM_USER {
                                count.fetch_add(1, Ordering::SeqCst);
                            }
                            None
                        }));
                }
                None
            }));
        // SAFETY: a live window on this thread.
        unsafe {
            SendMessageW(hwnd, WM_USER, None, None);
            SendMessageW(hwnd, WM_USER, None, None);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the new handler stays");
        popup.borrow_mut().take();
    });
}

// Item 8
#[test]
fn a_modal_opened_from_wm_enable_does_not_abort() {
    on_thread(|| {
        use std::cell::RefCell;
        use std::rc::Rc;
        let owner = DialogWindow::create("Owner", 100, 100, false, false).unwrap();
        let owner_hwnd = owner.hwnd();
        let nested = Rc::new(RefCell::new(Vec::<DialogWindow>::new()));
        let held = nested.clone();
        owner.set_message_handler(Box::new(move |_, msg, w, _| {
            if msg == WM_ENABLE && w.0 == 0 && held.borrow().is_empty() {
                let made = DialogWindow::create_with_owner(
                    "Nested",
                    100,
                    100,
                    true,
                    false,
                    Some(owner_hwnd),
                )
                .unwrap();
                held.borrow_mut().push(made);
            }
            None
        }));
        let first =
            DialogWindow::create_with_owner("First", 100, 100, true, false, Some(owner_hwnd))
                .unwrap();
        assert_eq!(nested.borrow().len(), 1, "the handler ran");
        // SAFETY: a plain query.
        assert!(!unsafe { IsWindowEnabled(owner_hwnd) }.as_bool());
        drop(first);
        // SAFETY: a plain query.
        assert!(
            !unsafe { IsWindowEnabled(owner_hwnd) }.as_bool(),
            "one still holds it"
        );
        nested.borrow_mut().clear();
        // SAFETY: a plain query.
        assert!(unsafe { IsWindowEnabled(owner_hwnd) }.as_bool());
    });
}
#[test]
fn a_window_opened_from_wm_enable_on_release_does_not_abort() {
    on_thread(|| {
        use std::cell::RefCell;
        use std::rc::Rc;
        let owner = DialogWindow::create("Owner", 100, 100, false, false).unwrap();
        let owner_hwnd = owner.hwnd();
        let dialog =
            DialogWindow::create_with_owner("First", 100, 100, true, false, Some(owner_hwnd))
                .unwrap();
        let nested = Rc::new(RefCell::new(Vec::<DialogWindow>::new()));
        let held = nested.clone();
        owner.set_message_handler(Box::new(move |_, msg, w, _| {
            if msg == WM_ENABLE && w.0 != 0 && held.borrow().is_empty() {
                let made = DialogWindow::create_with_owner(
                    "Nested",
                    100,
                    100,
                    true,
                    false,
                    Some(owner_hwnd),
                )
                .unwrap();
                held.borrow_mut().push(made);
            }
            None
        }));
        drop(dialog);
        assert_eq!(nested.borrow().len(), 1);
    });
}

// Item 15
#[test]
fn a_dialog_size_is_its_painted_area() {
    on_thread(|| {
        let owner = Owner::new();
        let dialog = DialogWindow::create_with_owner(
            "Customize Encodings List",
            ENCODING_PANEL_WIDTH as i32,
            ENCODING_PANEL_HEIGHT as i32,
            false,
            true,
            Some(owner.0),
        )
        .unwrap();
        assert_eq!(dialog.client_size().unwrap(), (336, 187), "row I39");
    });
}
#[test]
fn a_dialog_is_centered_on_its_owner() {
    on_thread(|| {
        let owner = Owner::new();
        // SAFETY: a live window on this thread.
        unsafe {
            SetWindowPos(
                owner.0,
                None,
                300,
                200,
                800,
                600,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .unwrap();
        }
        let dialog =
            DialogWindow::create_with_owner("Test", 300, 200, false, false, Some(owner.0)).unwrap();
        let mut b = windows::Win32::Foundation::RECT::default();
        // SAFETY: `b` is a valid out pointer.
        unsafe { GetWindowRect(dialog.hwnd(), &mut b).unwrap() };
        assert!(((b.left + b.right) / 2 - 700).abs() <= 1, "{b:?}");
        assert!(((b.top + b.bottom) / 2 - 500).abs() <= 1, "{b:?}");
    });
}
#[test]
fn a_dialog_with_an_owner_off_screen_stays_on_the_work_area() {
    on_thread(|| {
        let owner = Owner::new();
        // SAFETY: a live window on this thread.
        unsafe {
            SetWindowPos(
                owner.0,
                None,
                100_000,
                100_000,
                100,
                100,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .unwrap();
        }
        let dialog =
            DialogWindow::create_with_owner("Test", 300, 200, false, false, Some(owner.0)).unwrap();
        let mut b = windows::Win32::Foundation::RECT::default();
        // SAFETY: `b` is a valid out pointer.
        unsafe { GetWindowRect(dialog.hwnd(), &mut b).unwrap() };
        assert!(b.right < 100_000, "{b:?}");
    });
}

// Item 17
#[test]
fn file_dialogs_ask_for_real_file_system_items() {
    on_com_thread(|| {
        const FORCE_FILE_SYSTEM: u32 = 0x40;
        let save = FileDialogSetup::save(&FileDialogOptions::default()).unwrap();
        assert_ne!(save.options().unwrap() & FORCE_FILE_SYSTEM, 0);
        let open = FileDialogSetup::open(&FileDialogOptions::default()).unwrap();
        assert_ne!(open.options().unwrap() & FORCE_FILE_SYSTEM, 0);
    });
}
#[test]
fn the_default_extension_follows_the_txt_checkbox() {
    on_com_thread(|| {
        let on = FileDialogOptions {
            add_txt: Some(true),
            pdf: None,
            ..Default::default()
        };
        let dialog = FileDialogSetup::save(&on).unwrap();
        assert!(
            dialog.default_extension_is_txt(),
            "starts from the checkbox"
        );
        dialog.send_txt_toggle(false).unwrap();
        assert!(!dialog.default_extension_is_txt(), "the user turned it off");
        dialog.send_txt_toggle(true).unwrap();
        assert!(dialog.default_extension_is_txt(), "and on again");
        drop(dialog);

        let off = FileDialogOptions {
            add_txt: Some(false),
            pdf: None,
            ..Default::default()
        };
        assert!(
            !FileDialogSetup::save(&off)
                .unwrap()
                .default_extension_is_txt()
        );
    });
}
#[test]
fn with_every_row_disabled_the_choice_keeps_its_path_and_says_why() {
    on_com_thread(|| {
        let options = FileDialogOptions {
            encodings: vec![EncodingRow {
                name: "Windows-1252".into(),
                enabled: false,
            }],
            ..Default::default()
        };
        let dialog = FileDialogSetup::save(&options).unwrap();
        let (row, note) = dialog.checked_encoding();
        assert_eq!(row, None);
        assert!(note.is_some());
    });
}

// Item 18, with no use of the real clipboard.
#[test]
fn the_clipboard_opens_with_a_real_owner_window() {
    on_thread(|| {
        let owner = clipboard_owner_window().unwrap();
        assert!(alive(owner));
        assert!(!visible(owner));
        assert_eq!(
            clipboard_owner_window().unwrap(),
            owner,
            "one for each thread"
        );
    });
}
fn block_of(units: &[u16]) -> windows::Win32::Foundation::HGLOBAL {
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    // SAFETY: the block is large enough for the copy.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2).unwrap();
        let pointer = GlobalLock(memory) as *mut u16;
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
        let _ = GlobalUnlock(memory);
        memory
    }
}
#[test]
fn clipboard_text_is_read_inside_its_block() {
    use windows::Win32::Foundation::GlobalFree;
    let text: Vec<u16> = "abc".encode_utf16().collect();
    let no_zero = block_of(&text);
    // SAFETY: a live block that only this thread uses.
    unsafe {
        assert_eq!(read_text_block(no_zero).unwrap(), "abc", "no final zero");
        let _ = GlobalFree(Some(no_zero));
    }
    let with_zero = block_of(&[0x61, 0, 0x62]);
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            read_text_block(with_zero).unwrap(),
            "a",
            "it stops at the zero"
        );
        let _ = GlobalFree(Some(with_zero));
    }
}
#[test]
fn clipboard_text_that_is_not_unicode_is_reported() {
    use windows::Win32::Foundation::GlobalFree;
    let bad = block_of(&[0x61, 0xd800, 0x62, 0]);
    // SAFETY: a live block that only this thread uses.
    unsafe {
        let error = read_text_block(bad).unwrap_err();
        assert!(error.contains("not valid Unicode"), "{error}");
        let _ = GlobalFree(Some(bad));
    }
}

#[test]
fn pdf_dialog_has_its_boxes_and_wrap_to_page_has_no_rewrap_box() {
    on_com_thread(|| {
        let with = |rewrap| FileDialogOptions {
            title: Some("Export as PDF".into()),
            pdf: Some(PdfChecks {
                header: true,
                rewrap,
            }),
            ..Default::default()
        };
        let dialog = FileDialogSetup::save(&with(Some(false))).unwrap();
        assert_eq!(
            dialog.pdf_checks().unwrap(),
            Some(PdfChecks {
                header: true,
                rewrap: Some(false),
            })
        );
        let dialog = FileDialogSetup::save(&with(None)).unwrap();
        assert_eq!(dialog.pdf_checks().unwrap().unwrap().rewrap, None);
        let plain = FileDialogSetup::save(&FileDialogOptions::default()).unwrap();
        assert_eq!(plain.pdf_checks().unwrap(), None);
    });
}

#[test]
fn closing_a_menu_does_not_show_it_again_or_take_the_mouse_again() {
    on_thread(|| {
        let owner = Owner::new();
        let mut view = all_menus_view(100.0, 100.0);
        let mut windows = MenuWindows::new_hidden(owner.0, 1.0);
        windows.sync(&view).unwrap();
        let root = windows.levels()[0].hwnd();
        assert_eq!(capture_holder(), root);
        view.closed = true;
        windows.sync(&view).unwrap();
        assert_eq!(windows.dismissed(), Some(PopupDismissed::Caller));
        assert_ne!(
            capture_holder(),
            root,
            "the closed menu took the mouse again"
        );
        assert!(!windows.levels()[0].has_begun());
        // A menu closed before it first showed: there is no dismissal yet to stop it.
        let mut view = all_menus_view(100.0, 100.0);
        let mut windows = MenuWindows::new_hidden(owner.0, 1.0);
        windows.sync_unshown(&view).unwrap();
        view.closed = true;
        windows.sync(&view).unwrap();
        assert!(
            !windows.levels()[0].has_begun(),
            "it showed after its close"
        );
    });
}
#[test]
fn a_dismissed_menu_has_nothing_left_to_show() {
    on_thread(|| {
        let owner = Owner::new();
        let view = all_menus_view(100.0, 100.0);
        let mut windows = MenuWindows::new_hidden(owner.0, 1.0);
        windows.sync_unshown(&view).unwrap();
        assert!(windows.has_unshown_level());
        windows.show_level(0).unwrap();
        assert!(!windows.has_unshown_level());
        let root = windows.levels()[0].hwnd();
        assert_eq!(capture_holder(), root);
        windows.levels()[0].dismiss();
        assert!(windows.dismissed().is_some());
        assert!(!windows.has_unshown_level(), "it would show again");
        windows.show_level(0).unwrap();
        assert!(windows.dismissed().is_some(), "it came back");
        assert_ne!(capture_holder(), root);
        windows.sync(&view).unwrap();
        assert!(windows.dismissed().is_some(), "it came back by sync");
        assert_ne!(capture_holder(), root);
    });
}
