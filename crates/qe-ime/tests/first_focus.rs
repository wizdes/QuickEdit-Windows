//! What TSF costs, in a fresh process each time. It also holds the proof that nothing of
//! TSF starts before the first focus, by listing the modules a child process has loaded.
#![allow(hidden_glob_reexports)]
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod support;

use qe_api::{ByteOffset, ViewContext};
use qe_editor::EditorView;
use qe_ime::{TextStoreHandle, TsfThread};
use std::{process::Command, time::Instant};
use support::Fixture;
use windows::Win32::{
    Foundation::HMODULE,
    System::{
        ProcessStatus::{EnumProcessModules, GetModuleBaseNameW},
        Threading::GetCurrentProcess,
    },
};

const CHILD: &str = "QE_IME_FIRST_FOCUS_CHILD";

fn loaded_modules() -> Vec<String> {
    let mut modules = vec![HMODULE::default(); 4096];
    let mut needed = 0;
    unsafe {
        let process = GetCurrentProcess();
        EnumProcessModules(
            process,
            modules.as_mut_ptr(),
            std::mem::size_of_val(modules.as_slice()) as u32,
            &mut needed,
        )
        .unwrap();
        let count = needed as usize / size_of::<HMODULE>();
        modules[..count]
            .iter()
            .map(|module| {
                let mut name = [0_u16; 260];
                let length = GetModuleBaseNameW(process, Some(*module), &mut name);
                String::from_utf16_lossy(&name[..length as usize]).to_ascii_lowercase()
            })
            .collect()
    }
}

/// The child's work. Runs as a test, in a process of its own, when the parent asks.
#[test]
fn child_process_body() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    println!(
        "AT_START {}",
        loaded_modules().contains(&"msctf.dll".to_owned())
    );
    // What a launch does: make a view, with no focus yet.
    let fixture = Fixture::new(EditorView::from_text("hello").unwrap());
    let before = loaded_modules();
    println!("BEFORE {}", before.contains(&"msctf.dll".to_owned()));
    if mode == "focus" {
        let started = Instant::now();
        let thread = TsfThread::start().unwrap();
        let after_start = started.elapsed();
        let doc = thread.open_document(fixture.host.clone()).unwrap();
        let after_open = started.elapsed();
        doc.focus().unwrap();
        let after_focus = started.elapsed();
        println!(
            "MICROS start={} open={} focus={} total={}",
            after_start.as_micros(),
            (after_open - after_start).as_micros(),
            (after_focus - after_open).as_micros(),
            after_focus.as_micros()
        );
        // A second tab's document, with TSF already running.
        let started = Instant::now();
        let second = thread.open_document(fixture.host.clone()).unwrap();
        second.focus().unwrap();
        println!("SECOND_TAB_MICROS {}", started.elapsed().as_micros());
        doc.focus().unwrap();

        // What a key costs after TSF is on, with no input method composing.
        let mut ctx = ViewContext::default();
        let mut worst = 0;
        let mut total = 0;
        const KEYS: usize = 2_000;
        for at in 0..KEYS {
            fixture
                .view
                .borrow_mut()
                .replace_range(ByteOffset(at)..ByteOffset(at), "x", &mut ctx);
            ctx.requests.clear();
            let timer = Instant::now();
            doc.sync();
            let spent = timer.elapsed().as_nanos();
            total += spent;
            worst = worst.max(spent);
        }
        println!(
            "SYNC_NANOS mean={} worst={} keys={KEYS}",
            total / KEYS as u128,
            worst
        );
        let timer = Instant::now();
        for _ in 0..KEYS {
            doc.layout_changed();
        }
        println!(
            "LAYOUT_CHANGED_NANOS mean={}",
            timer.elapsed().as_nanos() / KEYS as u128
        );
        println!("SENT {:?}", doc.store().counts());
        let after = loaded_modules();
        println!("AFTER {}", after.contains(&"msctf.dll".to_owned()));
    } else if mode == "store-only" {
        // A store with no thread manager is plain Rust, and must not pull TSF in either.
        let handle = TextStoreHandle::new(fixture.host.clone());
        handle.sync();
        drop(handle);
        let after = loaded_modules();
        println!("AFTER {}", after.contains(&"msctf.dll".to_owned()));
    }
    println!("CHILD_DONE");
}

fn child(mode: &str) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_process_body",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, mode)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && text.contains("CHILD_DONE"),
        "the child failed: {text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    text
}

#[test]
fn tsf_starts_only_on_the_call_and_the_cost_of_the_first_focus_is_measured() {
    let text = child("focus");
    // The check can see the DLL once TSF runs. It may already be there, because Windows
    // can load msctf.dll for any window. The report says what it saw on this machine.
    assert!(text.contains("AFTER true"), "{text}");
    assert!(text.contains("MICROS start="), "{text}");
    // A store with no thread manager is plain Rust.
    let idle = child("store-only");
    assert!(idle.contains("BEFORE") && idle.contains("AFTER"), "{idle}");
    for line in text
        .lines()
        .filter(|l| l.contains("MICROS") || l.contains("NANOS") || l.contains("BEFORE"))
    {
        println!("{line}");
    }
}

#[test]
fn a_key_with_no_input_method_tells_tsf_one_change_and_no_more() {
    let text = child("focus");
    let sent = text
        .lines()
        .find(|l| l.starts_with("SENT"))
        .unwrap_or_else(|| panic!("no counts: {text}"));
    assert!(sent.contains("text_changes_sent: 2000"), "{sent}");
    assert!(
        sent.contains("layout_changes_sent: 0"),
        "typing paid for layout: {sent}"
    );
}
