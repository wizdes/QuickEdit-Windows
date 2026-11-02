//! The README's wiring, in a real window procedure, with host actions that do what the shell's
//! will do: report input, report widget changes, close the window, and open a modal dialog. A
//! borrow held across any of these would panic, and in release that ends the app.
mod support;
use qe_api::{ByteOffset, ViewContext, WidgetId, WidgetRole};
use qe_editor::EditorView;
use std::{
    cell::Cell,
    ffi::c_void,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use support::*;
use windows::{
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
            Variant::VARIANT,
        },
        UI::{Accessibility::*, WindowsAndMessaging::DestroyWindow},
    },
    core::{Interface, Result},
};

const BUTTON: u64 = 5;

fn window_with_button() -> TestWindow {
    TestWindow::with_tree(
        EditorView::from_text("hello world").expect("a view"),
        root_with(vec![widget(
            BUTTON,
            WidgetRole::Button,
            "Do",
            0.0,
            Vec::new(),
        )]),
    )
}

/// A real client: finds the button by name and presses it.
fn client_presses_button(hwnd: isize) -> Result<IUIAutomationTextRange> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
    let client: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
    let element = unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }?;
    let by_type = |kind: UIA_CONTROLTYPE_ID| unsafe {
        client.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(kind.0))
    };
    let document =
        unsafe { element.FindFirst(TreeScope_Descendants, &by_type(UIA_DocumentControlTypeId)?) }?;
    let pattern: IUIAutomationTextPattern =
        unsafe { document.GetCurrentPatternAs(UIA_TextPatternId) }?;
    let range = unsafe { pattern.DocumentRange() }?;
    let button =
        unsafe { element.FindFirst(TreeScope_Descendants, &by_type(UIA_ButtonControlTypeId)?) }?;
    let invoke: IUIAutomationInvokePattern =
        unsafe { button.GetCurrentPatternAs(UIA_InvokePatternId) }?;
    unsafe { invoke.Invoke() }?;
    Ok(range)
}

fn run_action_through_a_real_client(window: &TestWindow, ran: &Rc<Cell<u32>>) {
    let hwnd = window.hwnd.0 as isize;
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let answer = client_presses_button(hwnd)
            .map(drop)
            .map_err(|e| e.to_string());
        say.send(answer).expect("report");
    });
    let mut result = None;
    window.pump_until("the client and the action", 30, || {
        if let Ok(answer) = heard.try_recv() {
            result = Some(answer);
        }
        result.is_some() && ran.get() > 0
    });
    result.expect("an answer").expect("the client");
}

#[test]
fn an_action_that_reports_input_does_not_panic() {
    let _one_at_a_time = one_ui_thread();
    let window = window_with_button();
    let ran = Rc::new(Cell::new(0));
    let (slot, count) = (window.slot(), ran.clone());
    let ui = window.ui.clone();
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if *action != qe_a11y::WidgetAction::Invoke {
            return;
        }
        // The shell's action edits text, then reports the batch, as README step 4 says.
        ui.view.borrow_mut().replace_range(
            ByteOffset(0)..ByteOffset(0),
            "x",
            &mut ViewContext::default(),
        );
        slot.after_input().expect("after_input inside an action");
        count.set(count.get() + 1);
    })));
    run_action_through_a_real_client(&window, &ran);
    assert_eq!(ran.get(), 1);
    assert_eq!(window.ui.view.borrow().text().unwrap(), "xhello world");
    window.close();
}

#[test]
fn an_action_that_reports_widget_changes_does_not_panic() {
    let _one_at_a_time = one_ui_thread();
    let window = window_with_button();
    let ran = Rc::new(Cell::new(0));
    let (slot, count) = (window.slot(), ran.clone());
    let ui = window.ui.clone();
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if *action != qe_a11y::WidgetAction::Invoke {
            return;
        }
        ui.tree
            .borrow_mut()
            .children
            .push(widget(6, WidgetRole::Button, "New", 60.0, Vec::new()));
        slot.widgets_changed()
            .expect("widgets_changed inside an action");
        count.set(count.get() + 1);
    })));
    run_action_through_a_real_client(&window, &ran);
    assert_eq!(ran.get(), 1);
    window.close();
}

#[test]
fn a_close_item_that_destroys_the_window_during_its_action_does_not_panic() {
    let _one_at_a_time = one_ui_thread();
    let window = window_with_button();
    let ran = Rc::new(Cell::new(0));
    let (hwnd, count) = (window.hwnd, ran.clone());
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if *action != qe_a11y::WidgetAction::Invoke {
            return;
        }
        // WM_DESTROY arrives inside this action, and the procedure shuts down UIA.
        unsafe { DestroyWindow(hwnd) }.expect("DestroyWindow");
        count.set(count.get() + 1);
    })));
    let raw = window.hwnd.0 as isize;
    let (say, heard) = mpsc::channel();
    let (say_read, read) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let run = || -> std::result::Result<(), String> {
            let range = client_presses_button(raw).map_err(|e| e.to_string())?;
            read.recv().map_err(|e| e.to_string())?;
            match unsafe { range.GetText(-1) } {
                Err(e) if e.code().0 == GONE => Ok(()),
                other => Err(format!("a held range after close said {other:?}")),
            }
        };
        say.send(run()).expect("report");
    });
    window.pump_until("the close action", 30, || ran.get() > 0);
    assert!(
        window.slot().current().is_none(),
        "WM_DESTROY shut down UIA"
    );
    say_read.send(()).expect("tell the client");
    let mut result = None;
    window.pump_until("the client", 30, || {
        if let Ok(answer) = heard.try_recv() {
            result = Some(answer);
        }
        result.is_some()
    });
    result.expect("an answer").expect("the client");
}

#[test]
fn a_host_read_that_calls_back_into_the_slot_does_not_panic() {
    // The slot's own call is on the stack while a host call runs. These must all still work.
    let window = window_with_button();
    let slot = window.slot();
    let accessibility = window.accessibility();
    let document = accessibility.document().unwrap();
    let doc = document_range(&document);
    let ran = Rc::new(Cell::new(0));
    let count = ran.clone();
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, _| {
        slot.after_input().expect("after_input");
        slot.widgets_changed().expect("widgets_changed");
        slot.shutdown().expect("shutdown");
        slot.after_input().expect("after_input after shutdown");
        count.set(count.get() + 1);
    })));
    let root: IRawElementProviderFragment = accessibility.root().cast().unwrap();
    let button: IRawElementProviderSimple = unsafe { root.Navigate(NavigateDirection_FirstChild) }
        .unwrap()
        .cast()
        .unwrap();
    let invoke: IInvokeProvider = unsafe { button.GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { invoke.Invoke() }.unwrap();
    window.pump_until("the action", 10, || ran.get() > 0);
    assert_eq!(unsafe { doc.GetText(-1) }.unwrap(), "hello world");
}

/// Shutdown with the window still open, as a window that stops offering its tree would. UIA
/// lets go of what it held, a client's held objects say "gone", and a new client finds no tree.
#[test]
fn shutdown_ends_the_tree_for_a_client_that_holds_it_and_for_a_new_one() {
    let _one_at_a_time = one_ui_thread();
    let window = window_with_button();
    let hwnd = window.hwnd.0 as isize;
    let (say_ready, ready) = mpsc::channel();
    let (say_go, go) = mpsc::channel::<()>();
    let (say_done, done) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> std::result::Result<(), String> {
            let text = |e: windows::core::Error| e.to_string();
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(text)?;
            let client: IUIAutomation =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                    .map_err(text)?;
            let element =
                unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }.map_err(text)?;
            let condition = unsafe {
                client.CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_DocumentControlTypeId.0),
                )
            }
            .map_err(text)?;
            let document =
                unsafe { element.FindFirst(TreeScope_Descendants, &condition) }.map_err(text)?;
            say_ready.send(()).map_err(|e| e.to_string())?;
            go.recv().map_err(|e| e.to_string())?;
            match unsafe { document.CurrentName() } {
                Err(e) if e.code().0 == GONE => {}
                other => return Err(format!("the held document said {other:?}")),
            }
            let fresh =
                unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }.map_err(text)?;
            match unsafe { fresh.FindFirst(TreeScope_Descendants, &condition) } {
                Err(e) if e.code().0 == 0 => Ok(()),
                Err(e) => Err(format!("a new client got an error: {e}")),
                Ok(_) => Err("a new client found a document after shutdown".into()),
            }
        };
        say_done.send(run()).expect("report");
    });
    window.pump_until("the client", 30, || ready.try_recv().is_ok());
    let ui = window
        .slot()
        .current()
        .expect("the client made the provider");
    let before = ui.live_objects();
    window.slot().shutdown().expect("shutdown");
    let after = ui.live_objects();
    assert!(
        after < before,
        "UIA should let go of the document: {before} objects, then {after}"
    );
    drop(ui);
    say_go.send(()).expect("tell the client");
    let mut result = None;
    window.pump_until("the client to finish", 30, || {
        if let Ok(answer) = done.try_recv() {
            result = Some(answer);
        }
        result.is_some()
    });
    result.expect("an answer").expect("the client");
    window.close();
}

/// The action pumps messages for a while, as a modal dialog's loop does.
fn modal_action(window: &TestWindow, ran: &Rc<Cell<u32>>, length: Duration) {
    let count = ran.clone();
    *window.ui.on_action.borrow_mut() = Some(Rc::new(Box::new(move |_, action| {
        if *action != qe_a11y::WidgetAction::Invoke {
            return;
        }
        count.set(1);
        let end = Instant::now() + length;
        while Instant::now() < end {
            pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        count.set(2);
    })));
}

#[test]
fn invoke_returns_at_once_and_a_client_still_reads_during_a_modal_dialog() {
    let _one_at_a_time = one_ui_thread();
    let window = window_with_button();
    let ran = Rc::new(Cell::new(0));
    modal_action(&window, &ran, Duration::from_secs(4));
    let hwnd = window.hwnd.0 as isize;
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> std::result::Result<(u128, String, u128), String> {
            let text = |e: windows::core::Error| e.to_string();
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(text)?;
            let client: IUIAutomation =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                    .map_err(text)?;
            let element =
                unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }.map_err(text)?;
            let condition = unsafe {
                client.CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_ButtonControlTypeId.0),
                )
            }
            .map_err(text)?;
            let button =
                unsafe { element.FindFirst(TreeScope_Descendants, &condition) }.map_err(text)?;
            let invoke: IUIAutomationInvokePattern =
                unsafe { button.GetCurrentPatternAs(UIA_InvokePatternId) }.map_err(text)?;
            let began = Instant::now();
            unsafe { invoke.Invoke() }.map_err(text)?;
            let invoke_ms = began.elapsed().as_millis();
            // The dialog is up by now. A fresh WM_GETOBJECT and a read both go through.
            std::thread::sleep(Duration::from_millis(300));
            let began = Instant::now();
            let fresh =
                unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }.map_err(text)?;
            let condition = unsafe {
                client.CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_DocumentControlTypeId.0),
                )
            }
            .map_err(text)?;
            let document =
                unsafe { fresh.FindFirst(TreeScope_Descendants, &condition) }.map_err(text)?;
            let pattern: IUIAutomationTextPattern =
                unsafe { document.GetCurrentPatternAs(UIA_TextPatternId) }.map_err(text)?;
            let range = unsafe { pattern.DocumentRange() }.map_err(text)?;
            let read = unsafe { range.GetText(-1) }.map_err(text)?.to_string();
            Ok((invoke_ms, read, began.elapsed().as_millis()))
        };
        say.send(run()).expect("report");
    });
    let mut result = None;
    window.pump_until("the client and the dialog", 40, || {
        if let Ok(answer) = heard.try_recv() {
            result = Some(answer);
        }
        result.is_some() && ran.get() == 2
    });
    let (invoke_ms, read, read_ms) = result.expect("an answer").expect("the client");
    assert_eq!(read, "hello world");
    assert!(
        invoke_ms < 2000,
        "Invoke took {invoke_ms} ms with a dialog open"
    );
    assert!(
        read_ms < 3000,
        "reading took {read_ms} ms with a dialog open"
    );
    window.close();
}

#[test]
fn every_posted_action_returns_before_it_runs() {
    let mut tree_item = widget(7, WidgetRole::MenuItem, "Sub", 0.0, Vec::new());
    tree_item.states.has_popup = true;
    let toggle = widget(8, WidgetRole::Checkbox, "Wrap", 50.0, Vec::new());
    let tab = widget(9, WidgetRole::Tab, "One", 100.0, Vec::new());
    let button = widget(BUTTON, WidgetRole::Button, "Do", 150.0, Vec::new());
    let window = TestWindow::with_tree(
        EditorView::from_text("x").expect("a view"),
        root_with(vec![tree_item, toggle, tab, button]),
    );
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let mut node = unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap();
    let simple =
        |node: &IRawElementProviderFragment| -> IRawElementProviderSimple { node.cast().unwrap() };

    let expand: IExpandCollapseProvider =
        unsafe { simple(&node).GetPatternProvider(UIA_ExpandCollapsePatternId) }
            .unwrap()
            .cast()
            .unwrap();
    unsafe { expand.Expand() }.unwrap();
    unsafe { expand.Collapse() }.unwrap();
    node = unsafe { node.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let toggle: IToggleProvider = unsafe { simple(&node).GetPatternProvider(UIA_TogglePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { toggle.Toggle() }.unwrap();
    node = unsafe { node.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let select: ISelectionItemProvider =
        unsafe { simple(&node).GetPatternProvider(UIA_SelectionItemPatternId) }
            .unwrap()
            .cast()
            .unwrap();
    unsafe { select.Select() }.unwrap();
    node = unsafe { node.Navigate(NavigateDirection_NextSibling) }.unwrap();
    let invoke: IInvokeProvider = unsafe { simple(&node).GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { invoke.Invoke() }.unwrap();

    // Nothing has run: the calls returned, and no message has been pumped.
    assert!(window.counts.actions.lock().unwrap().is_empty());
    use qe_a11y::WidgetAction as A;
    assert_eq!(
        window.actions_after_pump(),
        vec![
            (WidgetId(7), A::Expand),
            (WidgetId(7), A::Collapse),
            (WidgetId(8), A::Toggle),
            (WidgetId(9), A::Select),
            (WidgetId(BUTTON), A::Invoke),
        ]
    );
}

#[test]
fn a_posted_action_on_a_control_that_is_off_says_so_at_once() {
    let mut off = widget(BUTTON, WidgetRole::Button, "Off", 0.0, Vec::new());
    off.states.enabled = false;
    let window = TestWindow::with_tree(
        EditorView::from_text("x").expect("a view"),
        root_with(vec![off]),
    );
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let node: IRawElementProviderSimple = unsafe { root.Navigate(NavigateDirection_FirstChild) }
        .unwrap()
        .cast()
        .unwrap();
    let invoke: IInvokeProvider = unsafe { node.GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    assert_code(
        unsafe { invoke.Invoke() },
        UIA_E_ELEMENTNOTENABLED as i32,
        "a control that is off",
    );
    assert!(window.actions_after_pump().is_empty());
}

#[test]
fn a_real_client_with_calls_in_flight_does_not_stall_close_and_the_host_ends_on_the_window_thread()
{
    let _one_at_a_time = one_ui_thread();
    let window = TestWindow::from_text(&"some text here\n".repeat(2000));
    window.lay_out(400.0, 200.0);
    let hwnd = window.hwnd.0 as isize;
    let (say_ready, ready) = mpsc::channel();
    let (say_done, done) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> std::result::Result<u32, String> {
            let text = |e: windows::core::Error| e.to_string();
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(text)?;
            let client: IUIAutomation =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                    .map_err(text)?;
            let element =
                unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }.map_err(text)?;
            let condition = unsafe {
                client.CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_DocumentControlTypeId.0),
                )
            }
            .map_err(text)?;
            let document =
                unsafe { element.FindFirst(TreeScope_Descendants, &condition) }.map_err(text)?;
            let pattern: IUIAutomationTextPattern =
                unsafe { document.GetCurrentPatternAs(UIA_TextPatternId) }.map_err(text)?;
            let range = unsafe { pattern.DocumentRange() }.map_err(text)?;
            say_ready.send(()).map_err(|e| e.to_string())?;
            // Call without a pause until the window has gone.
            let mut calls = 0;
            loop {
                match unsafe { range.GetText(1000) } {
                    Ok(_) => calls += 1,
                    Err(e) if e.code().0 == GONE => return Ok(calls),
                    Err(e) => return Err(format!("a call during close said {e}")),
                }
            }
        };
        say_done.send(run()).expect("report");
    });
    window.pump_until("the client", 30, || ready.try_recv().is_ok());
    // Let calls run for a moment.
    let until = Instant::now() + Duration::from_millis(200);
    window.pump_until("calls in flight", 10, || Instant::now() >= until);

    let began = Instant::now();
    window.close();
    let close_ms = began.elapsed().as_millis();
    let mut result = None;
    window.pump_until("the client to hear", 30, || {
        if let Ok(answer) = done.try_recv() {
            result = Some(answer);
        }
        result.is_some()
    });
    let calls = result.expect("an answer").expect("the client");
    assert!(calls > 0, "the client made calls before the close");
    assert!(
        close_ms < 1000,
        "closing took {close_ms} ms with calls in flight"
    );

    // Only this test's references are left. When they go, the state ends here, on this thread.
    let TestWindow {
        ui, host, counts, ..
    } = window;
    drop(host);
    drop(ui);
    let ended = counts.ended_on.lock().unwrap().clone();
    assert_eq!(
        ended,
        vec![std::thread::current().id()],
        "ended once, on this thread"
    );
}
