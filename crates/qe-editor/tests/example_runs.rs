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
