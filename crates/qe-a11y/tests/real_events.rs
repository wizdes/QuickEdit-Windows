#![allow(non_upper_case_globals)]
//! Structure, text, selection and property events, heard by a real client in another process.
//! UIA gives a focus handler only the real keyboard focus, and it refuses a focus event handler
//! on one element, so a hidden window cannot show its focus events to a client. Root checks
//! those in a visible window.
mod support;
use qe_api::{WidgetRange, WidgetRole};
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
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                SAFEARRAY,
            },
            Variant::VARIANT,
        },
        UI::Accessibility::*,
    },
    core::{Ref, Result, implement},
};

// The `implement` macro names `windows_core` directly.
extern crate qe_a11y as windows_core;

const STRUCTURE: usize = 0;
const TEXT: usize = 1;
const SELECTION: usize = 2;
const ELEMENT_SELECTED: usize = 3;
const VALUE: usize = 4;
const TOGGLE: usize = 5;
const EXPAND: usize = 6;
const RANGE: usize = 7;
const KINDS: usize = 8;

type Counts = Arc<[AtomicUsize; KINDS]>;

#[implement(
    IUIAutomationStructureChangedEventHandler,
    IUIAutomationEventHandler,
    IUIAutomationPropertyChangedEventHandler
)]
struct Counter {
    hits: Counts,
}
impl IUIAutomationStructureChangedEventHandler_Impl for Counter_Impl {
    fn HandleStructureChangedEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: StructureChangeType,
        _: *const SAFEARRAY,
    ) -> Result<()> {
        self.hits[STRUCTURE].fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl IUIAutomationEventHandler_Impl for Counter_Impl {
    fn HandleAutomationEvent(&self, _: Ref<IUIAutomationElement>, id: UIA_EVENT_ID) -> Result<()> {
        let kind = match id {
            UIA_Text_TextChangedEventId => TEXT,
            UIA_Text_TextSelectionChangedEventId => SELECTION,
            _ => ELEMENT_SELECTED,
        };
        self.hits[kind].fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl IUIAutomationPropertyChangedEventHandler_Impl for Counter_Impl {
    fn HandlePropertyChangedEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        id: UIA_PROPERTY_ID,
        _: &VARIANT,
    ) -> Result<()> {
        let kind = match id {
            UIA_ValueValuePropertyId => VALUE,
            UIA_ToggleToggleStatePropertyId => TOGGLE,
            UIA_ExpandCollapseExpandCollapseStatePropertyId => EXPAND,
            _ => RANGE,
        };
        self.hits[kind].fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

const VARIABLE: &str = "QE_A11Y_WIDGET_EVENTS_HWND";

/// Runs only when the test below starts this program again.
#[test]
fn child_entry() {
    let Ok(hwnd) = std::env::var(VARIABLE) else {
        return;
    };
    let hwnd: isize = hwnd.parse().expect("a window number");
    let run = || -> Result<()> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        let client: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
        let element = unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }?;
        let hits: Counts = Arc::new(Default::default());
        let structure: IUIAutomationStructureChangedEventHandler =
            Counter { hits: hits.clone() }.into();
        unsafe {
            client.AddStructureChangedEventHandler(&element, TreeScope_Subtree, None, &structure)
        }?;
        let plain: IUIAutomationEventHandler = Counter { hits: hits.clone() }.into();
        for id in [
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId,
            UIA_SelectionItem_ElementSelectedEventId,
        ] {
            unsafe {
                client.AddAutomationEventHandler(id, &element, TreeScope_Subtree, None, &plain)
            }?;
        }
        let property: IUIAutomationPropertyChangedEventHandler =
            Counter { hits: hits.clone() }.into();
        unsafe {
            client.AddPropertyChangedEventHandlerNativeArray(
                &element,
                TreeScope_Subtree,
                None,
                &property,
                &[
                    UIA_ValueValuePropertyId,
                    UIA_ToggleToggleStatePropertyId,
                    UIA_ExpandCollapseExpandCollapseStatePropertyId,
                    UIA_RangeValueValuePropertyId,
                ],
            )
        }?;
        println!("\nHANDLERS-READY");
        std::io::stdout().flush().expect("flush");
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).expect("a line") > 0 {
            let counts: Vec<String> = hits
                .iter()
                .map(|hit| hit.load(Ordering::SeqCst).to_string())
                .collect();
            println!("\nCOUNT {}", counts.join(" "));
            std::io::stdout().flush().expect("flush");
            if line.contains("stop") {
                break;
            }
            line.clear();
        }
        unsafe { client.RemoveAllEventHandlers() }
    };
    if let Err(problem) = run() {
        println!("\nCLIENT-FAILED: {problem}");
        std::io::stdout().flush().expect("flush");
        std::process::exit(3);
    }
}

#[test]
fn a_client_in_another_process_hears_each_widget_change() {
    let mut field = widget(30, WidgetRole::TextField, "Find", 50.0, Vec::new());
    field.value = Some("one".into());
    field.selection = Some(0..1);
    let tab = widget(31, WidgetRole::Tab, "One", 100.0, Vec::new());
    let check = widget(32, WidgetRole::Checkbox, "Wrap", 150.0, Vec::new());
    let mut combo = widget(33, WidgetRole::ComboBox, "Encoding", 200.0, Vec::new());
    combo.value = Some("UTF-8".into());
    let mut scroll = widget(34, WidgetRole::ScrollBar, "Scroll", 250.0, Vec::new());
    scroll.range_value = Some(WidgetRange {
        minimum: 0.0,
        maximum: 9.0,
        value: 1.0,
    });
    let window = TestWindow::with_tree(
        EditorView::from_text("text").expect("a view"),
        root_with(vec![
            widget(101, WidgetRole::Button, "Save", 0.0, Vec::new()),
            field,
            tab,
            check,
            combo,
            scroll,
        ]),
    );
    let mut child = Command::new(std::env::current_exe().expect("this program"))
        .args(["--exact", "child_entry", "--nocapture"])
        .env(VARIABLE, (window.hwnd.0 as isize).to_string())
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
    let mut ask = |word: &str| -> Vec<usize> {
        let until = Instant::now() + Duration::from_millis(400);
        window.pump_until("a quiet moment", 30, || Instant::now() >= until);
        writeln!(stdin, "{word}").expect("ask");
        stdin.flush().expect("flush");
        let line = wait_for("COUNT");
        line.trim()
            .split(' ')
            .skip(1)
            .map(|n| n.parse().expect("a number"))
            .collect()
    };
    let report = || {
        window
            .with_slot(|ui| ui.widgets_changed().expect("widgets_changed"))
            .expect("the provider")
    };
    report();
    let mut last = ask("report");
    let mut step = |what: &str, kinds: &[usize], edit: &dyn Fn(&mut Vec<qe_api::Widget>)| {
        edit(&mut window.ui.tree.borrow_mut().children);
        report();
        let now = ask("report");
        for kind in 0..KINDS {
            let want = last[kind] + usize::from(kinds.contains(&kind));
            assert_eq!(now[kind], want, "{what}: kind {kind}, counts {now:?}");
        }
        last = now;
    };
    step("a new button", &[STRUCTURE], &|c| {
        c.push(widget(200, WidgetRole::Button, "New", 300.0, Vec::new()));
    });
    step("a field's text", &[TEXT, VALUE], &|c| {
        c[1].value = Some("two".into());
    });
    step("a field's selection", &[SELECTION], &|c| {
        c[1].selection = Some(1..2);
    });
    step("a tab selected", &[ELEMENT_SELECTED], &|c| {
        c[2].states.selected = true;
    });
    step("a checkbox", &[TOGGLE], &|c| {
        c[3].states.checked = true;
    });
    step("a list opened", &[EXPAND], &|c| {
        c[4].states.expanded = true;
    });
    step("a scroll bar", &[RANGE], &|c| {
        if let Some(range) = &mut c[5].range_value {
            range.value = 2.0;
        }
    });
    // Focus moving to a widget is no change of shape.
    window.ui.tree.borrow_mut().children[0].states.focused = true;
    assert!(report().focus);
    assert_eq!(ask("stop"), last, "no other event for a focus move");
    child.wait().expect("wait for the client");
    window.close();
}
