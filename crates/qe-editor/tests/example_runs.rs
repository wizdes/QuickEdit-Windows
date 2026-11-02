//! Runs the example as a process, always with a hidden flag, so no window appears.
//! `cargo test -p qe-editor` builds the example first.
use std::process::{Command, Output};

fn example() -> std::path::PathBuf {
    let test = std::env::current_exe().unwrap();
    let deps = test.parent().unwrap();
    let path = deps.parent().unwrap().join("examples/editor.exe");
    assert!(
        path.exists(),
        "{} is missing. Run `cargo test -p qe-editor` with no --test filter.",
        path.display()
    );
    path
}
fn run(arguments: &[&str]) -> Output {
    assert!(
        arguments.iter().any(|a| matches!(
            *a,
            "--hidden-smoke" | "--hidden-loop" | "--bench-keys" | "--help"
        )),
        "this test must never open a window"
    );
    Command::new(example()).args(arguments).output().unwrap()
}
struct TempFile(std::path::PathBuf);
impl TempFile {
    fn new(name: &str, text: &str) -> Self {
        let path = std::env::temp_dir().join(format!("qe-example-{}-{name}", std::process::id()));
        std::fs::write(&path, text).unwrap();
        Self(path)
    }
    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) {
            eprintln!("could not remove {}: {e}", self.0.display());
        }
    }
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn l1_a_loading_file_wakes_the_window_at_once_whatever_its_timers_do() {
    // The window takes the focus before the load's wake is served, and focus asks for a
    // 530 ms blink timer. Timers replace each other, so the wake must not be a timer.
    let file = TempFile::new("wake.txt", &"hello world\n".repeat(1000));
    let mut times = Vec::new();
    for _ in 0..3 {
        let output = run(&["--hidden-loop", file.path()]);
        assert!(output.status.success(), "{}", stderr(&output));
        let text = stdout(&output);
        let ms: u64 = text
            .rsplit("after ")
            .next()
            .and_then(|tail| tail.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no time in {text:?}"));
        times.push(ms);
    }
    let best = times.iter().min().unwrap();
    assert!(*best < 300, "the file took {times:?} ms to reach the view");
}

#[test]
fn l2_closing_the_window_acts_on_no_destroyed_window() {
    let file = TempFile::new("close.txt", "first\nsecond\n");
    for arguments in [
        vec!["--hidden-loop", file.path()],
        vec!["--hidden-loop", "--force-mapped", file.path()],
        vec!["--hidden-smoke", file.path()],
        vec!["--bench-keys", "4", file.path()],
    ] {
        let output = run(&arguments);
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            stderr(&output)
        );
        assert!(
            !stderr(&output).contains("no longer exists"),
            "{arguments:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn l3_key_timers_count_frames_apart_from_keys_with_no_frame() {
    let file = TempFile::new("keys.txt", &"hello world\n".repeat(50));
    let output = run(&["--bench-keys", "20", file.path()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(
        text.contains("20 with a frame, 0 with no frame, 0 still waiting"),
        "{text}"
    );
    assert!(text.contains("input to present"), "{text}");
    assert!(text.contains("first text frame"), "{text}");
    assert!(text.contains("full first screen"), "{text}");
}

#[test]
fn r10_a_key_on_a_shaped_line_is_timed_to_the_frame_with_its_character() {
    let file = TempFile::new("shaped.txt", &"\u{4e2d}\u{6587}\u{5b57}\n".repeat(50));
    let output = run(&["--bench-keys", "20", file.path()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(
        text.contains("20 with a frame, 0 with no frame, 0 still waiting"),
        "{text}"
    );
}

#[test]
fn l3_an_empty_document_prints_first_frame_and_not_first_text_frame() {
    let output = run(&["--hidden-smoke", "--timings"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("first frame:"), "{text}");
    assert!(!text.contains("first text frame"), "{text}");
}

#[test]
fn l5_an_empty_window_prints_only_its_first_frame_even_after_typing() {
    let output = run(&["--bench-keys", "5"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("first frame:"), "{text}");
    assert!(!text.contains("first text frame"), "{text}");
    assert!(!text.contains("full first screen"), "{text}");
    assert!(!text.contains("load start"), "{text}");
}

#[test]
fn p9_the_start_label_says_what_it_measures_and_covers_program_loading() {
    let file = TempFile::new("label.txt", "hello\n");
    let output = run(&["--hidden-smoke", "--timings", file.path()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("from process creation"), "{text}");
    assert!(!text.contains("from process start"), "{text}");
    // Windows made the process before main ran, so that number is the larger one.
    let line = text
        .lines()
        .find(|l| l.starts_with("first text frame"))
        .unwrap();
    let numbers: Vec<f64> = line
        .split_whitespace()
        .filter_map(|word| word.parse().ok())
        .collect();
    assert!(numbers[0] > numbers[1], "{line}");
}

#[test]
fn s7_keys_that_wait_count_in_input_to_present() {
    // The last line has no end of line, and Home on its last row needs its layout.
    let line = "\u{4e2d}\u{6587}\u{5b57}".repeat(200);
    let file = TempFile::new(
        "waits.txt",
        &format!("{}{line}", format!("{line}\n").repeat(500)),
    );
    let output = run(&[
        "--bench-keys",
        "30",
        "--bench-wrap",
        "--bench-no-pause",
        "--bench-end-first",
        file.path(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    let waited: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix("keys that waited, typing and Backspace, editable file: "))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{text}"));
    assert!(waited > 0, "{text}");
    assert!(text.contains("0 still waiting"), "{text}");
    assert!(text.contains("0 with no frame"), "{text}");
}

#[test]
fn bench_keys_can_go_to_the_middle_of_the_text() {
    let line = "a\tb\tc\td\n";
    let file = TempFile::new("middle.txt", &line.repeat(100));
    let output = run(&["--bench-keys", "6", "--bench-middle", file.path()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("the keys go to byte 400"), "{text}");
}

#[test]
fn bench_options_alone_open_nothing() {
    for option in [
        "--bench-wrap",
        "--bench-middle",
        "--bench-no-pause",
        "--bench-end-first",
    ] {
        // `--help` keeps this test safe if the refusal ever goes: help opens no window.
        let output = run(&[option, "--help"]);
        assert!(!output.status.success(), "{option}");
        assert!(stderr(&output).contains("needs --bench-keys"), "{option}");
    }
}

#[test]
fn a_failed_load_says_why_and_the_loop_still_ends() {
    let missing = std::env::temp_dir().join(format!("qe-example-{}-none.txt", std::process::id()));
    let output = run(&["--hidden-loop", missing.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Could not open"),
        "{}",
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("hidden loop"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn help_and_unknown_flags_open_nothing() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("--hidden-smoke"));
    let output = Command::new(example())
        .args(["--no-such-flag", "--help"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unknown option"));
}
