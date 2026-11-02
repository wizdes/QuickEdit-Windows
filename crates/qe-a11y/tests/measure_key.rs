//! What the screen reader path costs a key, and what a real client call costs. Run by name, in
//! release:
//! `cargo test -p qe-a11y --release --locked --test measure_key -- --ignored --nocapture --test-threads=1`
mod support;
use qe_api::{InputEvent, InputKind, View, ViewContext};
use std::{
    ffi::c_void,
    sync::{atomic::Ordering, mpsc},
    time::Instant,
};
use support::*;
use windows::{
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
            Variant::VARIANT,
        },
        UI::Accessibility::*,
    },
    core::BSTR,
};

fn median(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[test]
#[ignore = "A timing. Run by name, in release."]
fn a_key_with_no_client_costs_no_more_than_the_check() {
    let listening_now = unsafe { UiaClientsAreListening() }.as_bool();
    println!("\nUiaClientsAreListening on this PC right now: {listening_now}");
    let window = TestWindow::from_text(&"line of text\n".repeat(2000));
    window.lay_out(800.0, 600.0);
    let ui = window.accessibility();
    let mut ctx = ViewContext::default();
    let key = InputEvent {
        time_us: 0,
        kind: InputKind::Char('x'),
    };
    // Keys take turns, so the growing text does not favor one kind. A line break now and
    // then keeps the line short.
    let mut plain = Vec::new();
    let mut no_client = Vec::new();
    let rounds = 6000;
    for round in 0..rounds {
        let mut view = window.ui.view.borrow_mut();
        if round % 80 == 0 {
            let caret = view.caret();
            view.replace_range(caret..caret, "\n", &mut ctx);
        }
        let began = Instant::now();
        view.handle(&key, &mut ctx);
        drop(view);
        if round % 2 == 0 {
            plain.push(began.elapsed().as_secs_f64() * 1e6);
        } else {
            ui.after_input_when(false).expect("after_input");
            no_client.push(began.elapsed().as_secs_f64() * 1e6);
        }
        ctx.requests.clear();
    }
    assert_eq!(
        window.counts.editor_calls.load(Ordering::SeqCst),
        0,
        "with no client, the check must not open the view"
    );
    assert_eq!(window.counts.widget_calls.load(Ordering::SeqCst), 0);
    println!(
        "  a key, no check:                 median {:.2} microseconds",
        median(&mut plain)
    );
    println!(
        "  a key, with the no-client check: median {:.2} microseconds",
        median(&mut no_client)
    );

    let runs = 1_000_000;
    let began = Instant::now();
    for _ in 0..runs {
        ui.after_input_when(false).expect("after_input");
    }
    println!(
        "  the check with no client:        {:.1} nanoseconds",
        began.elapsed().as_secs_f64() * 1e9 / f64::from(runs)
    );
    let began = Instant::now();
    let mut said_yes = 0u32;
    for _ in 0..runs {
        said_yes += u32::from(unsafe { UiaClientsAreListening() }.as_bool());
    }
    println!(
        "  UiaClientsAreListening itself:   {:.1} nanoseconds, said yes {said_yes} times",
        began.elapsed().as_secs_f64() * 1e9 / f64::from(runs)
    );
    drop(ui);
    window.close();
}

/// The review's worst case: a query half as long as the text, nearly matching everywhere.
#[test]
#[ignore = "A timing. Run by name, in release."]
fn a_long_query_on_a_short_range() {
    let text = qe_api::testing::StringStore::new("a".repeat(60_000));
    let query = format!("{}b", "a".repeat(30_000));
    for ignore_case in [true, false] {
        let request = qe_a11y::search::FindRequest {
            range: qe_a11y::TextRange::new(0, 60_000),
            query: query.clone(),
            backward: false,
            ignore_case,
        };
        let began = Instant::now();
        let answer = qe_a11y::search::find_on_window(&text, &request);
        let took = began.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(answer, Ok(None));
        println!("\n  60,000 units, 30,001-unit query, ignore_case {ignore_case}: {took:.3} ms");
        assert!(took < 16.0, "{took} ms is over a frame");
    }
}

/// The second review's input, through the COM call on the window's thread: an all-a document
/// and a half-length run of a followed by b, ignoring case.
#[test]
#[ignore = "A timing. Run by name, in release."]
fn a_repetitive_ignore_case_search_on_the_window_thread() {
    println!();
    for size in [4096usize, 8192, 16384, 65_536] {
        let window = TestWindow::from_text(&"a".repeat(size));
        let ui = window.accessibility();
        let doc = document_range(&ui.document().unwrap());
        let query = BSTR::from(format!("{}b", "a".repeat(size / 2)).as_str());
        let mut samples = Vec::new();
        for _ in 0..50 {
            let began = Instant::now();
            let answer = unsafe { doc.FindText(&query, false, true) };
            samples.push(began.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(answer.err().map(|e| e.code().0), Some(0), "no match");
        }
        let worst = samples.iter().copied().fold(0.0, f64::max);
        let middle = median(&mut samples);
        println!("  {size} units: median {middle:.3} ms, worst {worst:.3} ms");
        assert!(middle < 16.0, "{middle} ms is over a frame");
    }
}

/// Through UIA, with its thread hop and the move to the window's thread.
#[test]
#[ignore = "A timing. Run by name, in release."]
fn a_real_client_call_round_trip() {
    let window = TestWindow::from_text(&"The quick brown fox\n".repeat(50_000));
    window.lay_out(800.0, 600.0);
    let hwnd = window.hwnd.0 as isize;
    let (say, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> windows::core::Result<Vec<(&'static str, f64)>> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
            let client: IUIAutomation =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
            let element = unsafe { client.ElementFromHandle(HWND(hwnd as *mut c_void)) }?;
            let condition = unsafe {
                client.CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_DocumentControlTypeId.0),
                )
            }?;
            let document = unsafe { element.FindFirst(TreeScope_Descendants, &condition) }?;
            let pattern: IUIAutomationTextPattern =
                unsafe { document.GetCurrentPatternAs(UIA_TextPatternId) }?;
            let doc = unsafe { pattern.DocumentRange() }?;
            let time = |name, body: &mut dyn FnMut() -> windows::core::Result<()>| {
                let mut samples = Vec::new();
                for _ in 0..300 {
                    let began = Instant::now();
                    body()?;
                    samples.push(began.elapsed().as_secs_f64() * 1000.0);
                }
                windows::core::Result::Ok((name, median(&mut samples)))
            };
            let moving = unsafe { doc.Clone() }?;
            let mut way = 1;
            Ok(vec![
                time("GetText(200)", &mut || {
                    unsafe { doc.GetText(200) }.map(drop)
                })?,
                time("Move(Line, 1 or -1)", &mut || {
                    way = -way;
                    unsafe { moving.Move(TextUnit_Line, -way) }.map(drop)
                })?,
                time("FindText, short range, no match", &mut || {
                    let word = unsafe { moving.Clone() }?;
                    unsafe { word.ExpandToEnclosingUnit(TextUnit_Line) }?;
                    match unsafe { word.FindText(&BSTR::from("zz"), false, false) } {
                        Err(error) if error.code().0 == 0 => Ok(()),
                        Err(error) => Err(error),
                        Ok(_) => Ok(()),
                    }
                })?,
            ])
        };
        say.send(run().map_err(|e| e.to_string())).expect("report");
    });
    let mut result = None;
    window.pump_until("the client", 60, || {
        if let Ok(answer) = heard.try_recv() {
            result = Some(answer);
        }
        result.is_some()
    });
    println!();
    for (name, ms) in result.expect("an answer").expect("the client") {
        println!("  real client {name:36} median {ms:.3} ms");
        assert!(ms < 16.0, "{name} took {ms} ms");
    }
    window.close();
}
