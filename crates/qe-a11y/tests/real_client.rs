//! A real UIA client, through IUIAutomation (CUIAutomation), against a hidden window. The
//! window answers WM_GETOBJECT, as the shell will, and its thread pumps messages. The client
//! runs on another thread, and again in another process, because UIA must not be called from
//! the window's own thread. The host only opens the view on the window's thread, so if UIA
//! called it from any other, the text reads here would fail.
mod support;
use qe_a11y::WidgetAction;
use qe_api::{WidgetId, WidgetRole};
use qe_editor::EditorView;
use std::{
    ffi::c_void,
    io::{BufRead, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use support::*;
use windows::{
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY},
            Ole::{SafeArrayDestroy, SafeArrayGetElement},
            Variant::VARIANT,
        },
        UI::Accessibility::*,
    },
    core::{BSTR, IUnknown, Interface, Ref, Result as WinResult, implement},
};

// The `implement` macro names `windows_core` directly.
extern crate qe_a11y as windows_core;

const HWND_VARIABLE: &str = "QE_A11Y_CLIENT_HWND";
const WRAPPED: &str = "word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word";

fn body() -> String {
    format!("Hello big world\r\na\u{1f600}e\u{301}x\n{WRAPPED}\nlast")
}

fn make_window() -> TestWindow {
    let mut save = widget(101, WidgetRole::Button, "Save", 0.0, Vec::new());
    save.description = Some("Saves the file".into());
    let mut file = widget(102, WidgetRole::MenuItem, "File", 50.0, Vec::new());
    file.states.has_popup = true;
    let mut find = widget(103, WidgetRole::TextField, "Find", 100.0, Vec::new());
    find.value = Some("needle".into());
    find.selection = Some(0..3);
    let tree = root_with(vec![save, file, find]);
    let mut view = EditorView::from_text(&body()).expect("a view");
    view.set_wrap(true).expect("wrap");
    let window = TestWindow::with_tree(view, tree);
    window.lay_out(220.0, 500.0);
    window
}

macro_rules! ensure {
    ($test:expr, $($message:tt)*) => {
        if !$test {
            return Err(format!($($message)*));
        }
    };
}
fn call<T>(what: &str, answer: WinResult<T>) -> Result<T, String> {
    answer.map_err(|e| format!("{what} failed: {e} (0x{:08x})", e.code().0 as u32))
}
/// A real null: S_OK from the client API with no object.
fn null<T>(what: &str, answer: WinResult<T>) -> Result<(), String> {
    match answer {
        Ok(_) => Err(format!("{what}: expected null, got an object")),
        Err(e) if e.code().0 == 0 => Ok(()),
        Err(e) => Err(format!("{what}: expected null, got error {e} (0x{:08x})", e.code().0 as u32)),
    }
}
fn read(range: &IUIAutomationTextRange) -> Result<String, String> {
    Ok(call("GetText", unsafe { range.GetText(-1) })?.to_string())
}
fn doubles(array: *mut SAFEARRAY) -> Result<Vec<f64>, String> {
    let mut out = Vec::new();
    if array.is_null() {
        return Ok(out);
    }
    let count = unsafe { (*array).rgsabound[0].cElements };
    for index in 0..count as i32 {
        let mut value = 0f64;
        call("read a rectangle", unsafe {
            SafeArrayGetElement(array, &index, (&raw mut value).cast::<c_void>())
        })?;
        out.push(value);
    }
    call("free the rectangles", unsafe { SafeArrayDestroy(array) })?;
    Ok(out)
}
fn automation() -> Result<IUIAutomation, String> {
    call("CoInitializeEx", unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok())?;
    call("CoCreateInstance(CUIAutomation)", unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
    })
}
fn find_by_type(
    client: &IUIAutomation,
    from: &IUIAutomationElement,
    kind: UIA_CONTROLTYPE_ID,
) -> Result<IUIAutomationElement, String> {
    let condition = call("CreatePropertyCondition", unsafe {
        client.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(kind.0))
    })?;
    call("FindFirst", unsafe { from.FindFirst(TreeScope_Descendants, &condition) })
}

/// The whole client script. `close_window` asks for the window to close, and returns when
/// it has.
fn client_script(window: isize, close_window: &dyn Fn()) -> Result<(), String> {
    let client = automation()?;
    let hwnd = HWND(window as *mut c_void);
    let element = call("ElementFromHandle", unsafe { client.ElementFromHandle(hwnd) })?;
    let document = find_by_type(&client, &element, UIA_DocumentControlTypeId)?;
    let name = call("the document's name", unsafe { document.CurrentName() })?;
    ensure!(name == "Untitled", "the document's name was {name}");

    // Text, through the pattern.
    let pattern: IUIAutomationTextPattern = call("cast the text pattern", unsafe {
        document.GetCurrentPatternAs(UIA_TextPatternId)
    })?;
    let doc = call("DocumentRange", unsafe { pattern.DocumentRange() })?;
    let whole = body();
    ensure!(read(&doc)? == whole, "the document text was wrong: {:?}", read(&doc)?);
    let some = call("GetText(5)", unsafe { doc.GetText(5) })?;
    ensure!(some == "Hello", "GetText(5) gave {some}");

    // Selection and caret.
    let selection = call("GetSelection", unsafe { pattern.GetSelection() })?;
    ensure!(call("Length", unsafe { selection.Length() })? == 1, "one selection expected");
    let first = call("GetElement", unsafe { selection.GetElement(0) })?;
    ensure!(read(&first)?.is_empty(), "the selection starts empty");

    // Move by character across the emoji and the combining mark.
    let caret = call("Clone", unsafe { doc.Clone() })?;
    call("collapse", unsafe {
        caret.MoveEndpointByRange(TextPatternRangeEndpoint_End, &doc, TextPatternRangeEndpoint_Start)
    })?;
    let moved = call("Move line", unsafe { caret.Move(TextUnit_Line, 1) })?;
    ensure!(moved == 1, "Move(Line, 1) gave {moved}");
    let mut seen = Vec::new();
    for _ in 0..3 {
        let one = call("Clone", unsafe { caret.Clone() })?;
        call("Expand character", unsafe { one.ExpandToEnclosingUnit(TextUnit_Character) })?;
        seen.push(read(&one)?);
        let moved = call("Move character", unsafe { caret.Move(TextUnit_Character, 1) })?;
        ensure!(moved == 1, "Move(Character, 1) gave {moved}");
    }
    ensure!(
        seen == ["a", "\u{1f600}", "e\u{301}"],
        "characters were {seen:?}"
    );

    // Move by word.
    let word = call("Clone", unsafe { doc.Clone() })?;
    call("Expand word", unsafe { word.ExpandToEnclosingUnit(TextUnit_Word) })?;
    ensure!(read(&word)? == "Hello ", "the first word was {:?}", read(&word)?);
    let moved = call("Move word", unsafe { word.Move(TextUnit_Word, 2) })?;
    ensure!(moved == 2, "Move(Word, 2) gave {moved}");
    ensure!(read(&word)? == "world\r\n", "the third word was {:?}", read(&word)?);

    // Move by line: the wrapped line gives rows, and a paragraph gives the whole line.
    let line = call("Clone", unsafe { doc.Clone() })?;
    call("Expand line", unsafe { line.ExpandToEnclosingUnit(TextUnit_Line) })?;
    ensure!(read(&line)? == "Hello big world\r\n", "the first line was {:?}", read(&line)?);
    let moved = call("Move line", unsafe { line.Move(TextUnit_Line, 2) })?;
    ensure!(moved == 2, "Move(Line, 2) gave {moved}");
    let row = read(&line)?;
    ensure!(
        row.starts_with("word") && row.len() < WRAPPED.len(),
        "a wrapped row was {row:?}"
    );
    let moved = call("Move line", unsafe { line.Move(TextUnit_Line, 1) })?;
    ensure!(moved == 1, "the next row gave {moved}");
    ensure!(read(&line)?.starts_with("word"), "the next row was {:?}", read(&line)?);
    call("Expand paragraph", unsafe { line.ExpandToEnclosingUnit(TextUnit_Paragraph) })?;
    ensure!(read(&line)? == format!("{WRAPPED}\n"), "the paragraph was {:?}", read(&line)?);

    // Rectangles.
    let rectangles = doubles(call("GetBoundingRectangles", unsafe { line.GetBoundingRectangles() })?)?;
    ensure!(
        rectangles.len() >= 8 && rectangles.len() % 4 == 0,
        "a wrapped paragraph gave {} numbers",
        rectangles.len()
    );
    ensure!(rectangles[2] > 0.0 && rectangles[3] > 0.0, "an empty rectangle: {rectangles:?}");

    // Real null answers survive UIA's own layers.
    null("FindText with no match", unsafe { doc.FindText(&BSTR::from("zzzz"), false, false) })?;
    null("a pattern a document lacks", unsafe {
        document.GetCurrentPattern(UIA_InvokePatternId)
    })?;
    let found = call("FindText", unsafe { doc.FindText(&BSTR::from("BIG"), false, true) })?;
    ensure!(read(&found)? == "big", "FindText gave {:?}", read(&found)?);

    // Attributes.
    let reserved = call("the reserved value", unsafe { client.ReservedNotSupportedValue() })?;
    let value = call("an attribute", unsafe { doc.GetAttributeValue(UIA_BackgroundColorAttributeId) })?;
    let unknown = IUnknown::try_from(&value).map_err(|e| format!("not an object: {e}"))?;
    ensure!(unknown.as_raw() == reserved.as_raw(), "not the 'not supported' value");
    let value = call("IsReadOnly", unsafe { doc.GetAttributeValue(UIA_IsReadOnlyAttributeId) })?;
    ensure!(bool::try_from(&value) == Ok(false), "IsReadOnly was not false");
    let value = call("FontName", unsafe { doc.GetAttributeValue(UIA_FontNameAttributeId) })?;
    ensure!(
        BSTR::try_from(&value).is_ok_and(|n| n == "Cascadia Mono"),
        "FontName was wrong"
    );

    // The range and the tree name the same element.
    let enclosing = call("GetEnclosingElement", unsafe { doc.GetEnclosingElement() })?;
    let same = call("CompareElements", unsafe { client.CompareElements(&enclosing, &document) })?;
    ensure!(same.as_bool(), "the enclosing element is not the document");

    // Compare, and a selection made by the client.
    ensure!(
        call("Compare", unsafe { doc.Compare(&doc) })?.as_bool(),
        "a range differs from itself"
    );
    call("Select", unsafe { found.Select() })?;
    let selection = call("GetSelection", unsafe { pattern.GetSelection() })?;
    let first = call("GetElement", unsafe { selection.GetElement(0) })?;
    ensure!(read(&first)? == "big", "the selection was {:?}", read(&first)?);

    // Widgets, through the same tree.
    let button = find_named(&client, &element, UIA_ButtonControlTypeId, "Save")?;
    let invoke: IUIAutomationInvokePattern = call("the invoke pattern", unsafe {
        button.GetCurrentPatternAs(UIA_InvokePatternId)
    })?;
    call("Invoke", unsafe { invoke.Invoke() })?;
    let item = find_named(&client, &element, UIA_MenuItemControlTypeId, "File")?;
    let expand: IUIAutomationExpandCollapsePattern = call("the expand pattern", unsafe {
        item.GetCurrentPatternAs(UIA_ExpandCollapsePatternId)
    })?;
    call("Expand", unsafe { expand.Expand() })?;
    let field = find_named(&client, &element, UIA_EditControlTypeId, "Find")?;
    let field_text: IUIAutomationTextPattern = call("the field's text", unsafe {
        field.GetCurrentPatternAs(UIA_TextPatternId)
    })?;
    let selection = call("the field's selection", unsafe { field_text.GetSelection() })?;
    let first = call("GetElement", unsafe { selection.GetElement(0) })?;
    ensure!(read(&first)? == "nee", "the field's selection was {:?}", read(&first)?);

    // The window closes while this client holds a range and an element. A crash here would
    // end this process, and a wrong answer fails the check.
    close_window();
    let answer = unsafe { doc.GetText(-1) };
    match answer {
        Err(e) if e.code().0 == UIA_E_ELEMENTNOTAVAILABLE as i32 => {}
        other => return Err(format!("GetText after close gave {other:?}")),
    }
    match unsafe { document.CurrentName() } {
        Err(e) if e.code().0 == UIA_E_ELEMENTNOTAVAILABLE as i32 => {}
        other => return Err(format!("CurrentName after close gave {other:?}")),
    }
    Ok(())
}

fn check_after_script(window: &TestWindow) {
    // UIA does not wait for Invoke or Expand to finish, so they may arrive a little later. It
    // also sets focus on an element before it invokes one, so those actions are left out here.
    let actions = || -> Vec<(WidgetId, WidgetAction)> {
        let all = window.counts.actions.lock().expect("actions");
        all.iter().filter(|a| a.1 != WidgetAction::Focus).cloned().collect()
    };
    window.pump_until("the widget actions", 30, || actions().len() >= 2);
    assert_eq!(
        actions(),
        [
            (WidgetId(101), WidgetAction::Invoke),
            (WidgetId(102), WidgetAction::Expand)
        ]
    );
}

#[test]
fn a_real_client_on_another_thread_reads_everything() {
    let _one_at_a_time = one_ui_thread();
    let window = make_window();
    let hwnd = window.hwnd.0 as isize;
    let (ask_close, close_asked) = mpsc::channel::<()>();
    let (say_closed, closed) = mpsc::channel::<()>();
    let (say_done, done) = mpsc::channel();
    std::thread::spawn(move || {
        let answer = client_script(hwnd, &|| {
            ask_close.send(()).expect("ask to close");
            closed.recv().expect("hear it closed");
        });
        say_done.send(answer).expect("report");
    });
    let mut result = None;
    let mut closed_window = false;
    window.pump_until("the client script", 60, || {
        if !closed_window && close_asked.try_recv().is_ok() {
            closed_window = true;
            check_after_script(&window);
            let selection = window.ui.view.borrow().selection();
            assert_eq!(selection.start.0..selection.end.0, 6..9, "the client's Select");
            window.close();
            say_closed.send(()).expect("say closed");
        }
        if let Ok(answer) = done.try_recv() {
            result = Some(answer);
        }
        result.is_some()
    });
    result.expect("an answer").expect("the client script");
    assert!(closed_window, "the script reached the close step");
    assert!(
        qe_a11y::marshal::crossed_calls() > 0,
        "UIA calls a provider on its own thread, so these must cross to the window"
    );
}

/// Runs only when the test below starts this program again.
#[test]
fn child_process_entry() {
    let Ok(hwnd) = std::env::var(HWND_VARIABLE) else {
        return;
    };
    let hwnd: isize = hwnd.parse().expect("a window number");
    let answer = client_script(hwnd, &|| {
        println!("\nREADY-TO-CLOSE");
        std::io::stdout().flush().expect("flush");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).expect("hear it closed");
    });
    if let Err(problem) = answer {
        println!("\nCLIENT-FAILED: {problem}");
        std::io::stdout().flush().expect("flush");
        std::process::exit(3);
    }
    println!("\nCLIENT-PASSED");
    std::io::stdout().flush().expect("flush");
}

#[test]
fn a_real_client_in_another_process_reads_everything() {
    let _one_at_a_time = one_ui_thread();
    let window = make_window();
    let mut child = Command::new(std::env::current_exe().expect("this program"))
        .args(["--exact", "child_process_entry", "--nocapture"])
        .env(HWND_VARIABLE, (window.hwnd.0 as isize).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the client");
    let stdout = child.stdout.take().expect("its output");
    let mut stdin = child.stdin.take().expect("its input");
    let (lines, heard) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut passed = false;
    let mut failure = None;
    let mut closed = false;
    window.pump_until("the client process", 90, || {
        while let Ok(line) = heard.try_recv() {
            if line.contains("READY-TO-CLOSE") && !closed {
                closed = true;
                check_after_script(&window);
                window.close();
                writeln!(stdin, "closed").expect("tell the client");
                stdin.flush().expect("flush");
            } else if line.contains("CLIENT-PASSED") {
                passed = true;
            } else if line.contains("CLIENT-FAILED") {
                failure = Some(line);
            }
        }
        passed || failure.is_some()
    });
    assert_eq!(failure, None);
    let status = child.wait().expect("wait for the client");
    assert!(status.success(), "the client ended with {status}");
    assert!(closed);
}

#[implement(IUIAutomationEventHandler)]
struct Counter {
    hits: Arc<AtomicUsize>,
}
impl IUIAutomationEventHandler_Impl for Counter_Impl {
    fn HandleAutomationEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: UIA_EVENT_ID,
    ) -> WinResult<()> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn a_real_client_hears_one_event_for_a_batch_of_typing() {
    let _one_at_a_time = one_ui_thread();
    let window = make_window();
    let hwnd = window.hwnd.0 as isize;
    let text_events = Arc::new(AtomicUsize::new(0));
    let selection_events = Arc::new(AtomicUsize::new(0));
    let (texts, selections) = (text_events.clone(), selection_events.clone());
    let (say_ready, ready) = mpsc::channel();
    let (say_stop, stop) = mpsc::channel::<()>();
    let (say_done, done) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> Result<(), String> {
            let client = automation()?;
            let element = call("ElementFromHandle", unsafe {
                client.ElementFromHandle(HWND(hwnd as *mut c_void))
            })?;
            let document = find_by_type(&client, &element, UIA_DocumentControlTypeId)?;
            let text: IUIAutomationEventHandler = Counter { hits: texts }.into();
            let selection: IUIAutomationEventHandler = Counter { hits: selections }.into();
            call("add a handler", unsafe {
                client.AddAutomationEventHandler(
                    UIA_Text_TextChangedEventId,
                    &document,
                    TreeScope_Element,
                    None,
                    &text,
                )
            })?;
            call("add a handler", unsafe {
                client.AddAutomationEventHandler(
                    UIA_Text_TextSelectionChangedEventId,
                    &document,
                    TreeScope_Element,
                    None,
                    &selection,
                )
            })?;
            say_ready.send(()).map_err(|e| e.to_string())?;
            stop.recv().map_err(|e| e.to_string())?;
            call("remove the handlers", unsafe { client.RemoveAllEventHandlers() })
        };
        say_done.send(run()).expect("report");
    });
    window.pump_until("the client's handlers", 30, || {
        if let Ok(answer) = done.try_recv() {
            panic!("the client stopped early: {answer:?}");
        }
        ready.try_recv().is_ok()
    });
    window.pump_until("UIA to report a listener", 30, || unsafe {
        UiaClientsAreListening().as_bool()
    });
    let ui = window.with_slot(|_| ()).is_some();
    assert!(ui, "the client's request made the provider");

    // One batch to set the baseline.
    window.with_slot(|ui| ui.after_input().expect("after_input"));
    window.pump_until("the first events", 30, || text_events.load(Ordering::SeqCst) >= 1);
    let settle = |window: &TestWindow| {
        let until = Instant::now() + Duration::from_millis(400);
        window.pump_until("a quiet moment", 30, || Instant::now() >= until);
    };
    settle(&window);
    let texts_before = text_events.load(Ordering::SeqCst);
    let selections_before = selection_events.load(Ordering::SeqCst);

    // 500 edits in one batch make one event of each kind.
    {
        let mut view = window.ui.view.borrow_mut();
        let mut ctx = qe_api::ViewContext::default();
        for i in 0..500 {
            view.replace_range(qe_api::ByteOffset(i)..qe_api::ByteOffset(i), "x", &mut ctx);
        }
        view.select(qe_api::ByteOffset(1)..qe_api::ByteOffset(2), &mut ctx);
    }
    let flags = window.with_slot(|ui| ui.after_input().expect("after_input")).expect("provider");
    assert!(flags.text_changed && flags.selection_changed);
    window.pump_until("the text event", 30, || {
        text_events.load(Ordering::SeqCst) > texts_before
            && selection_events.load(Ordering::SeqCst) > selections_before
    });
    settle(&window);
    assert_eq!(text_events.load(Ordering::SeqCst), texts_before * 2, "the same deliveries as one batch");
    assert_eq!(
        selection_events.load(Ordering::SeqCst),
        selections_before * 2,
        "the same deliveries as one batch"
    );

    say_stop.send(()).expect("stop the client");
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

/// By name as well as type, because a window can have others of the same type.
fn find_named(
    client: &IUIAutomation,
    from: &IUIAutomationElement,
    kind: UIA_CONTROLTYPE_ID,
    name: &str,
) -> Result<IUIAutomationElement, String> {
    let of_type = call("CreatePropertyCondition", unsafe {
        client.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(kind.0))
    })?;
    let named = call("CreatePropertyCondition", unsafe {
        client.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(name))
    })?;
    let both = call("CreateAndCondition", unsafe { client.CreateAndCondition(&of_type, &named) })?;
    call("FindFirst", unsafe { from.FindFirst(TreeScope_Descendants, &both) })
}

const EVENTS_VARIABLE: &str = "QE_A11Y_EVENTS_HWND";

/// Runs only when the test below starts this program again.
#[test]
fn events_child_entry() {
    let Ok(hwnd) = std::env::var(EVENTS_VARIABLE) else {
        return;
    };
    let hwnd: isize = hwnd.parse().expect("a window number");
    let run = || -> Result<(), String> {
        let client = automation()?;
        let element = call("ElementFromHandle", unsafe {
            client.ElementFromHandle(HWND(hwnd as *mut c_void))
        })?;
        let document = find_by_type(&client, &element, UIA_DocumentControlTypeId)?;
        let texts = Arc::new(AtomicUsize::new(0));
        let text: IUIAutomationEventHandler = Counter { hits: texts.clone() }.into();
        call("add a handler", unsafe {
            client.AddAutomationEventHandler(
                UIA_Text_TextChangedEventId,
                &document,
                TreeScope_Element,
                None,
                &text,
            )
        })?;
        println!("\nHANDLERS-READY");
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? > 0 {
            println!("\nCOUNT {}", texts.load(Ordering::SeqCst));
            std::io::stdout().flush().map_err(|e| e.to_string())?;
            if line.contains("stop") {
                break;
            }
            line.clear();
        }
        call("remove the handlers", unsafe { client.RemoveAllEventHandlers() })
    };
    if let Err(problem) = run() {
        println!("\nCLIENT-FAILED: {problem}");
        std::io::stdout().flush().expect("flush");
        std::process::exit(3);
    }
}

#[test]
fn a_client_in_another_process_hears_one_event_for_each_raise() {
    let _one_at_a_time = one_ui_thread();
    let window = make_window();
    let mut child = Command::new(std::env::current_exe().expect("this program"))
        .args(["--exact", "events_child_entry", "--nocapture"])
        .env(EVENTS_VARIABLE, (window.hwnd.0 as isize).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the client");
    let stdout = child.stdout.take().expect("its output");
    let mut stdin = child.stdin.take().expect("its input");
    let (lines, heard) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let wait_for = |prefix: &str| -> String {
        let mut found = None;
        window.pump_until(prefix, 30, || {
            while let Ok(line) = heard.try_recv() {
                assert!(!line.contains("CLIENT-FAILED"), "{line}");
                if line.contains(prefix) {
                    found = Some(line);
                }
            }
            found.is_some()
        });
        found.expect("a line")
    };
    wait_for("HANDLERS-READY");
    window.pump_until("UIA to report a listener", 30, || unsafe {
        UiaClientsAreListening().as_bool()
    });
    let mut ask = |window: &TestWindow, word: &str| -> usize {
        // Give any event on its way time to arrive.
        let until = Instant::now() + Duration::from_millis(500);
        window.pump_until("a quiet moment", 30, || Instant::now() >= until);
        writeln!(stdin, "{word}").expect("ask");
        stdin.flush().expect("flush");
        let line = wait_for("COUNT");
        line.trim().trim_start_matches("COUNT ").parse().expect("a count")
    };
    window.with_slot(|ui| ui.after_input().expect("after_input")).expect("provider");
    let first = ask(&window, "report");
    assert_eq!(first, 1, "the first batch raises one text event");
    {
        let mut view = window.ui.view.borrow_mut();
        let mut ctx = qe_api::ViewContext::default();
        for i in 0..500 {
            view.replace_range(qe_api::ByteOffset(i)..qe_api::ByteOffset(i), "x", &mut ctx);
        }
    }
    window.with_slot(|ui| ui.after_input().expect("after_input")).expect("provider");
    window.with_slot(|ui| ui.after_input().expect("after_input")).expect("provider");
    assert_eq!(ask(&window, "stop"), 2, "500 edits in a batch raise one text event");
    child.wait().expect("wait for the client");
    window.close();
}

/// One process, one UI thread, two windows, as the app runs them. Both show the same widget
/// IDs, so this also proves that UIA keeps them apart.
#[test]
fn a_real_client_keeps_two_windows_apart_and_one_closing_leaves_the_other() {
    let _one_at_a_time = one_ui_thread();
    let first = TestWindow::with_tree(
        EditorView::from_text("first window").expect("a view"),
        root_with(Vec::new()),
    );
    let second = TestWindow::with_tree(
        EditorView::from_text("second window").expect("a view"),
        root_with(Vec::new()),
    );
    let (a, b) = (first.hwnd.0 as isize, second.hwnd.0 as isize);
    let (ask_close, close_asked) = mpsc::channel::<()>();
    let (say_closed, closed) = mpsc::channel::<()>();
    let (say_done, done) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> Result<(), String> {
            let client = automation()?;
            let range_of = |window: isize| -> Result<IUIAutomationTextRange, String> {
                let element = call("ElementFromHandle", unsafe {
                    client.ElementFromHandle(HWND(window as *mut c_void))
                })?;
                let document = find_by_type(&client, &element, UIA_DocumentControlTypeId)?;
                let pattern: IUIAutomationTextPattern = call("the text pattern", unsafe {
                    document.GetCurrentPatternAs(UIA_TextPatternId)
                })?;
                call("DocumentRange", unsafe { pattern.DocumentRange() })
            };
            let one = range_of(a)?;
            let two = range_of(b)?;
            ensure!(read(&one)? == "first window", "the first window said {:?}", read(&one)?);
            ensure!(read(&two)? == "second window", "the second window said {:?}", read(&two)?);
            ask_close.send(()).map_err(|e| e.to_string())?;
            closed.recv().map_err(|e| e.to_string())?;
            match unsafe { one.GetText(-1) } {
                Err(e) if e.code().0 == UIA_E_ELEMENTNOTAVAILABLE as i32 => {}
                other => return Err(format!("the closed window said {other:?}")),
            }
            ensure!(read(&two)? == "second window", "the open window said {:?}", read(&two)?);
            Ok(())
        };
        say_done.send(run()).expect("report");
    });
    let mut result = None;
    let mut closing = false;
    second.pump_until("the client", 60, || {
        if !closing && close_asked.try_recv().is_ok() {
            closing = true;
            first.close();
            say_closed.send(()).expect("say closed");
        }
        if let Ok(answer) = done.try_recv() {
            result = Some(answer);
        }
        result.is_some()
    });
    result.expect("an answer").expect("the client");
    second.close();
}

/// The app has one UI thread. Several test threads with UIA windows at once in one process
/// make UIA fail with E_FAIL or an empty tree, so these tests take turns.
fn one_ui_thread() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
