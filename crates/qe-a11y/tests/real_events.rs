//! Structure events, heard by a real client in another process. UIA gives a focus handler only
//! the real keyboard focus, and it refuses a focus event handler on one element, so a hidden
//! window cannot show its focus events to a client. Root checks those in a visible window.
mod support;
use qe_api::WidgetRole;
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
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY,
        },
        UI::Accessibility::*,
    },
    core::{Ref, Result, implement},
};

// The `implement` macro names `windows_core` directly.
extern crate qe_a11y as windows_core;

#[implement(IUIAutomationStructureChangedEventHandler)]
struct StructureCounter {
    hits: Arc<AtomicUsize>,
}
impl IUIAutomationStructureChangedEventHandler_Impl for StructureCounter_Impl {
    fn HandleStructureChangedEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: StructureChangeType,
        _: *const SAFEARRAY,
    ) -> Result<()> {
        self.hits.fetch_add(1, Ordering::SeqCst);
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
        let structures = Arc::new(AtomicUsize::new(0));
        let structure: IUIAutomationStructureChangedEventHandler = StructureCounter {
            hits: structures.clone(),
        }
        .into();
        unsafe {
            client.AddStructureChangedEventHandler(&element, TreeScope_Subtree, None, &structure)
        }?;
        println!("\nHANDLERS-READY");
        std::io::stdout().flush().expect("flush");
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).expect("a line") > 0 {
            println!("\nCOUNT {}", structures.load(Ordering::SeqCst));
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
fn a_client_in_another_process_hears_one_structure_event_for_a_shape_change() {
    let window = TestWindow::with_tree(
        EditorView::from_text("text").expect("a view"),
        root_with(vec![widget(
            101,
            WidgetRole::Button,
            "Save",
            0.0,
            Vec::new(),
        )]),
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
    let mut ask = |window: &TestWindow, word: &str| -> usize {
        let until = Instant::now() + Duration::from_millis(500);
        window.pump_until("a quiet moment", 30, || Instant::now() >= until);
        writeln!(stdin, "{word}").expect("ask");
        stdin.flush().expect("flush");
        let line = wait_for("COUNT");
        let mut parts = line.trim().split(" ").skip(1);
        parts.next().expect("a count").parse().expect("a number")
    };
    let report = |window: &TestWindow| {
        window
            .with_slot(|ui| ui.widgets_changed().expect("widgets_changed"))
            .expect("the provider")
    };
    report(&window);
    let structures = ask(&window, "report");

    // The tree changes shape.
    window.ui.tree.borrow_mut().children.push(widget(
        200,
        WidgetRole::Button,
        "New",
        300.0,
        Vec::new(),
    ));
    assert!(report(&window).structure);
    let after_shape = ask(&window, "report");
    assert_eq!(after_shape, structures + 1, "one structure event");

    // Focus moving to it is no change of shape.
    window.ui.tree.borrow_mut().children[1].states.focused = true;
    assert!(report(&window).focus);
    assert_eq!(
        ask(&window, "stop"),
        after_shape,
        "no structure event for a focus move"
    );
    child.wait().expect("wait for the client");
    window.close();
}
