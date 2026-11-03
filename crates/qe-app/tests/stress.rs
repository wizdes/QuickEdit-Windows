//! Random commands, `focus` among them, on a hidden app. The pump must always end, so the UI
//! thread must always answer. The app's own watchdog ends it with code 99 if it stops, and each
//! command here has a time limit. Either one fails the test, with the last commands in the report.
//!
//! The short run is part of the suite. The long one is for a person to run on purpose:
//! `cargo test -p qe-app --test stress -- --ignored`.
mod support;

use std::time::Duration;
use support::{App, Folder, encode};

/// A small random source with no outside crate, so a seed always gives the same commands.
struct Random(u64);
impl Random {
    fn next(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % below as u64) as usize
    }
}

const COMMANDS: &[&str] = &[
    "focus",
    "focus",
    "focus",
    "blur",
    "type abc",
    "type speling%20",
    "key 8",
    "key 13",
    "key 27",
    "key 35 ctrl",
    "key 90 ctrl",
    "key 37",
    "press 0",
    "press 1",
    "press 2",
    "press 2",
    "alert-x",
    "alert",
    "compose zz",
    "compose zz",
    "commit-now",
    "composing",
    "tsf-focus",
    "run NewTab",
    "run New",
    "run Close",
    "run Close",
    "run Save",
    "run SaveAll",
    "run SaveAs",
    "run Duplicate",
    "run Undo",
    "run Redo",
    "run Find",
    "run FindReplace",
    "run SelectLine",
    "run NextTab",
    "run PreviousTab",
    "run Tab1",
    "run MergeAllWindows",
    "run MoveTabToNewWindow",
    "run CheckSpellingWhileTyping",
    "run CorrectSpellingAutomatically",
    "run ShowDocumentMap",
    "run ShowLineNumbers",
    "run WrapToPage",
    "run PreventEditing",
    "run ExportPdf",
    "panel-cancel",
    "panel-cancel",
    "use-window 0",
    "use-window 1",
    "tab-into 0",
    "tab-drop-new",
    "close-window",
    "close-window-at 1",
    "paint",
    "ping",
    "select 0 0",
    "replace-all a b",
    "settle",
];

fn run_one(seed: u64, steps: usize, limit: Duration) {
    let folder = Folder::new(&format!("stress-{seed}"));
    let file = folder.file("start.txt", "some text\n".repeat(200).as_bytes());
    let other = folder.file("other.txt", b"other text\n");
    let flags: &[&str] = if std::env::var_os("QE_TEST_EXE").is_some() {
        // An older build has no watchdog flag, and the time limit below does the work.
        &[]
    } else {
        &["--watchdog", "10000"]
    };
    let log = folder.0.join("errors.txt");
    let root = folder.root();
    let mut app = App::start_logged(&root, &[&file], flags, Some(&log));
    let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut recent: Vec<String> = Vec::new();
    let mut restarts = 0;
    for step in 0..steps {
        let mut line = COMMANDS[random.next(COMMANDS.len())].to_owned();
        if random.next(25) == 0 {
            line = format!("open {}", encode(&other.display().to_string()));
        }
        recent.push(line.clone());
        if recent.len() > 40 {
            recent.remove(0);
        }
        let answered = app.try_ask(&line, limit);
        if let Err(problem) = answered {
            // The commands can close the last window, and then the app ends with code 0.
            let code = app.wait_code(Duration::from_secs(10));
            if code == Some(0) {
                restarts += 1;
                app = App::start_logged(&root, &[&file], flags, Some(&log));
                continue;
            }
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: String = said.lines().rev().take(8).collect::<Vec<_>>().join("\n");
            panic!(
                "seed {seed} step {step}: {problem}, exit code {code:?}\nlast commands: {recent:?}\nerror stream ends: {tail}"
            );
        }
    }
    // Whatever state the run ended in, the app still answers, and ends when asked to.
    assert_eq!(app.try_ask("ping", limit).as_deref(), Ok("ok"));
    drop(app);
    eprintln!("seed {seed}: {steps} steps, {restarts} restarts");
}

#[test]
fn random_commands_never_hang_the_ui_thread() {
    for seed in [11, 12, 13, 14] {
        run_one(seed, 1000, Duration::from_secs(20));
    }
}

#[test]
#[ignore = "a long run, about two minutes, for a person to start on purpose"]
fn random_commands_never_hang_the_ui_thread_for_a_long_time() {
    for seed in 100..112 {
        run_one(seed, 4000, Duration::from_secs(20));
    }
}
