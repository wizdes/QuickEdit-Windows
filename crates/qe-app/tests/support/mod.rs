//! Starts the real `QuickEdit.exe` as a child with hidden windows, and drives it through its
//! test channel. Each child has its own settings and session folder, and its own pipe name.
#![allow(dead_code)]
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A folder of its own for one test. Removed when it drops.
pub struct Folder(pub PathBuf);
impl Folder {
    pub fn new(name: &str) -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("qe-app")
            .join(format!("{name}-{}-{number}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a test folder");
        Self(path)
    }
    pub fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("a test file");
        path
    }
    pub fn root(&self) -> PathBuf {
        self.0.join("data")
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The app under test. `QE_TEST_EXE` points the stress test at another build.
pub fn exe() -> String {
    std::env::var("QE_TEST_EXE").unwrap_or_else(|_| env!("CARGO_BIN_EXE_QuickEdit").to_owned())
}

pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            out.push(u8::from_str_radix(&text[at + 1..at + 3], 16).expect("hex"));
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_lines(output: ChildStdout) -> Receiver<String> {
    let (send, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else {
                return;
            };
            if send.send(line).is_err() {
                return;
            }
        }
    });
    lines
}

pub struct App {
    pub child: Child,
    input: ChildStdin,
    output: Receiver<String>,
    pub root: PathBuf,
}

impl App {
    pub fn start(root: &Path, files: &[&Path]) -> Self {
        Self::start_with(root, files, &[])
    }
    pub fn start_with(root: &Path, files: &[&Path], flags: &[&str]) -> Self {
        Self::start_logged(root, files, flags, None)
    }
    /// As `start_with`, with the app's error stream in this file.
    pub fn start_logged(root: &Path, files: &[&Path], flags: &[&str], log: Option<&Path>) -> Self {
        let mut app = Self::launch(root, files, flags, log);
        app.wait_for("the first window", |app| {
            app.ask("restored") == "true" && !app.ask("windows").is_empty()
        });
        app
    }
    /// Starts the app, and does not wait for a window.
    pub fn launch(root: &Path, files: &[&Path], flags: &[&str], log: Option<&Path>) -> Self {
        let errors = match log {
            Some(path) => Stdio::from(std::fs::File::create(path).expect("a log file")),
            None => Stdio::inherit(),
        };
        let mut child = Command::new(exe())
            .arg("--test-channel")
            .arg("--stub-spell")
            .args(flags)
            .arg("--data-root")
            .arg(root)
            .args(files)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(errors)
            .spawn()
            .expect("the app starts");
        let input = child.stdin.take().expect("stdin");
        let output = read_lines(child.stdout.take().expect("stdout"));
        Self {
            child,
            input,
            output,
            root: root.to_path_buf(),
        }
    }
    /// One command and its answer, or the reason there was none in this time. A write to a
    /// process that has ended counts as no answer.
    pub fn try_ask(&mut self, line: &str, limit: Duration) -> Result<String, String> {
        if writeln!(self.input, "{line}")
            .and_then(|()| self.input.flush())
            .is_err()
        {
            return Err(format!("the app has ended, before {line}"));
        }
        self.output
            .recv_timeout(limit)
            .map(|answer| answer.trim_end().to_owned())
            .map_err(|e| format!("no answer to {line} in {limit:?}: {e}"))
    }
    /// The exit code, once the process has ended.
    pub fn wait_code(&mut self, limit: Duration) -> Option<i32> {
        let end = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status.code();
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// One command and its one line of answer.
    pub fn ask(&mut self, line: &str) -> String {
        writeln!(self.input, "{line}").expect("the app reads its channel");
        self.input.flush().expect("the channel flushes");
        // A hung app fails the test. It does not hang the test.
        let answer = self
            .output
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("no answer to {line}: {e}"));
        answer.trim_end().to_owned()
    }
    /// A command that must answer `ok`.
    pub fn ok(&mut self, line: &str) {
        let answer = self.ask(line);
        assert_eq!(answer, "ok", "the command: {line}");
    }
    pub fn run(&mut self, command: &str) {
        self.ok(&format!("run {command}"));
    }
    pub fn type_text(&mut self, text: &str) {
        self.ok(&format!("type {}", encode(text)));
    }
    pub fn text(&mut self) -> String {
        decode(&self.ask("text"))
    }
    pub fn notes(&mut self) -> String {
        decode(&self.ask("notes"))
    }
    /// `(windows, tabs in each)`.
    pub fn windows(&mut self) -> Vec<Vec<String>> {
        self.ask("windows")
            .split(' ')
            .filter(|w| !w.is_empty())
            .map(|w| w.split(',').map(decode).collect())
            .collect()
    }
    pub fn tab_count(&mut self) -> usize {
        self.windows().iter().map(Vec::len).sum()
    }
    pub fn doc(&mut self, field: &str) -> String {
        let doc = self.ask("doc");
        for part in doc.split(' ') {
            if let Some(value) = part.strip_prefix(&format!("{field}=")) {
                return decode(value);
            }
        }
        panic!("no {field} in {doc}");
    }
    pub fn state(&mut self, command: &str) -> (bool, bool) {
        let answer = self.ask(&format!("state {command}"));
        let mut parts = answer.split(' ');
        (parts.next() == Some("true"), parts.next() == Some("true"))
    }
    /// The open question: its title, message and buttons.
    pub fn alert(&mut self) -> Option<(String, String, Vec<String>)> {
        let answer = self.ask("alert");
        if answer == "none" {
            return None;
        }
        let mut parts = answer.split(' ');
        let title = decode(parts.next().unwrap_or(""));
        let message = decode(parts.next().unwrap_or(""));
        let buttons = decode(parts.next().unwrap_or(""))
            .split('|')
            .map(str::to_owned)
            .collect();
        Some((title, message, buttons))
    }
    /// Presses the alert's button with this text.
    pub fn press(&mut self, label: &str) {
        let (title, _, buttons) = self.wait_value("an alert", App::alert);
        let index = buttons
            .iter()
            .position(|button| button == label)
            .unwrap_or_else(|| panic!("no {label} in {title}: {buttons:?}"));
        self.ok(&format!("press {index}"));
    }
    pub fn wait_for(&mut self, what: &str, mut check: impl FnMut(&mut Self) -> bool) {
        let limit = Instant::now() + Duration::from_secs(30);
        loop {
            if check(self) {
                return;
            }
            assert!(Instant::now() < limit, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn wait_value<T>(&mut self, what: &str, mut read: impl FnMut(&mut Self) -> Option<T>) -> T {
        let limit = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(value) = read(self) {
                return value;
            }
            assert!(Instant::now() < limit, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn wait_panel(&mut self) -> String {
        self.wait_for("a file dialog", |app| app.ask("panels") != "0");
        decode(&self.ask("panel-title"))
    }
    pub fn settled(&mut self) {
        self.wait_for("loads and saves to end", |app| app.ask("settle") == "true");
    }
    pub fn wait_text(&mut self, want: &str) {
        let limit = Instant::now() + Duration::from_secs(30);
        loop {
            let text = self.text();
            if text == want {
                return;
            }
            assert!(
                Instant::now() < limit,
                "timed out: the text is {text:?}, and the test wants {want:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// Asks the app to end, and waits. The code must be 0.
    pub fn quit(mut self) {
        let answer = self.ask("quit");
        assert_eq!(answer, "ok");
        self.wait_exit();
    }
    pub fn wait_exit(&mut self) {
        let limit = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                assert!(status.success(), "the app ended with {status}");
                return;
            }
            assert!(Instant::now() < limit, "the app did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// A real kill, as a crash would be.
    pub fn kill(mut self) {
        self.child.kill().expect("kill");
        self.child.wait().expect("wait after kill");
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
