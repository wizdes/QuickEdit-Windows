//! The file handoff between launches. Each test has its own pipe name, so that it never meets
//! a real QuickEdit or another test.
mod support;

use qe_docs::handoff::{
    HandoffError, HandoffEvent, HandoffServer, IntegrityLevel, LaunchRole, MAX_PATHS, PipeNames,
    hand_off, hand_off_checked, listen, server_is_this_user_and_level, start, start_checked,
    start_with,
};
use qe_docs::{DocEvent, OpenTarget};
use std::io::{BufRead, BufReader, Write as _};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use support::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CHILD_BASE: &str = "QE_DOCS_HANDOFF_BASE";
const CHILD_PATH: &str = "QE_DOCS_HANDOFF_PATH";

fn names(test: &str) -> PipeNames {
    PipeNames {
        base: format!("QuickEdit-test-{}-{test}", std::process::id()),
        level: IntegrityLevel::Medium,
    }
}

fn server(names: &PipeNames) -> (HandoffServer, Arc<AtomicU64>) {
    let wakes = Arc::new(AtomicU64::new(0));
    let counter = wakes.clone();
    let server = listen(
        names,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .unwrap()
    .expect("the pipe's name is free");
    (server, wakes)
}

fn no_wake() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

#[test]
fn with_no_instance_a_launch_is_told_so() {
    assert_eq!(hand_off(&names("none"), &[]), Ok(false));
}

#[test]
fn a_second_launch_hands_its_files_over_exactly_and_wakes_the_instance() {
    let names = names("basic");
    let (server, wakes) = server(&names);
    let paths = vec![
        PathBuf::from(r"C:\some folder\\u{4e2d}\u{1f600}.txt"),
        PathBuf::from(r"D:\b.txt"),
    ];
    assert_eq!(hand_off(&names, &paths), Ok(true));
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].paths, paths);
    assert!(server.take_requests().is_empty(), "each request comes once");
}

#[test]
fn a_launch_with_no_files_still_reaches_the_instance() {
    let names = names("empty");
    let (server, _) = server(&names);
    assert_eq!(hand_off(&names, &[]), Ok(true));
    assert_eq!(server.take_requests()[0].paths, Vec::<PathBuf>::new());
}

#[test]
fn fifteen_launches_at_once_all_reach_it() {
    let names = names("fifteen");
    let (server, _) = server(&names);
    let mut launches = Vec::new();
    for i in 0..15 {
        let names = names.clone();
        launches.push(std::thread::spawn(move || {
            hand_off(&names, &[PathBuf::from(format!(r"C:\f{i}.txt"))])
        }));
    }
    for launch in launches {
        assert_eq!(launch.join().unwrap(), Ok(true));
    }
    let requests = server.take_requests();
    assert_eq!(requests.len(), 15);
    let mut seen: Vec<String> = requests
        .iter()
        .map(|r| r.paths[0].to_string_lossy().into_owned())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 15);
}

#[test]
fn a_launch_that_sends_nothing_does_not_stop_the_next_one() {
    let names = names("stalled-client");
    let (server, _) = server(&names);
    let pipe = format!(r"\\.\pipe\{}-medium", names.base);
    // A launch that connects and hangs.
    let stalled = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pipe)
        .unwrap();
    assert_eq!(hand_off(&names, &[PathBuf::from(r"C:\a.txt")]), Ok(true));
    drop(stalled);
    assert_eq!(server.take_requests().len(), 1);
}

#[test]
fn a_bad_message_is_dropped_and_the_instance_goes_on() {
    let names = names("bad-message");
    let (server, _) = server(&names);
    let pipe = format!(r"\\.\pipe\{}-medium", names.base);
    let mut bad = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pipe)
        .unwrap();
    bad.write_all(&[0xAB; 40]).unwrap();
    drop(bad);
    assert_eq!(hand_off(&names, &[PathBuf::from(r"C:\a.txt")]), Ok(true));
    assert_eq!(server.take_requests().len(), 1, "only the good message");
}

#[test]
fn a_dropped_instance_lets_the_next_launch_take_over() {
    let names = names("dropped");
    let (server, _) = server(&names);
    assert_eq!(hand_off(&names, &[]), Ok(true));
    drop(server);
    let end = Instant::now() + WAIT;
    loop {
        match start(&names, &[], no_wake()) {
            LaunchRole::Primary(_) => break,
            // The old thread's last pipe can stay for a moment.
            LaunchRole::HandedOff | LaunchRole::Alone { .. } if Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => panic!("no new instance could start"),
        }
    }
}

/// A process that holds the pipe's name and never answers.
fn hung_instance(names: &PipeNames, level: &str) -> impl Drop + use<> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::PCWSTR;
    let name: Vec<u16> = format!(r"\\.\pipe\{}-{level}", names.base)
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: the name ends in a zero and outlives the call.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(3),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64,
            64,
            0,
            None,
        )
    };
    assert!(!handle.is_invalid());
    struct Guard(windows::Win32::Foundation::HANDLE);
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: this holds the only copy of the handle.
            unsafe { CloseHandle(self.0) }.unwrap();
        }
    }
    Guard(handle)
}

#[test]
fn a_hung_instance_is_named_within_a_bound_and_the_launch_runs_alone() {
    let names = names("hung");
    let _hung = hung_instance(&names, "medium");
    let started = Instant::now();
    let result = hand_off(&names, &[PathBuf::from(r"C:\a.txt")]);
    assert!(
        matches!(result, Err(HandoffError::StaleInstance { .. })),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(
        start(&names, &[], no_wake()),
        LaunchRole::Alone {
            error: HandoffError::StaleInstance { .. }
        }
    ));
}

#[test]
fn an_instance_at_another_integrity_level_gets_a_named_error() {
    let names = names("other-level");
    let _elevated = hung_instance(&names, "high");
    let result = hand_off(&names, &[PathBuf::from(r"C:\a.txt")]);
    let Err(HandoffError::DifferentIntegrity { ours, theirs }) = &result else {
        panic!("{result:?}");
    };
    assert_eq!(
        (ours.as_str(), theirs.as_str()),
        ("normal", "administrator")
    );
    let shown = result.unwrap_err().to_string();
    assert!(shown.contains("administrator"), "{shown}");
    assert!(matches!(
        start(&names, &[], no_wake()),
        LaunchRole::Alone { .. }
    ));
}

#[test]
fn a_real_instance_killed_leaves_no_stale_name() {
    let names = names("killed");
    let mut child = spawn_role_child(&names.base, r"C:\x.txt", true);
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("ROLE ") {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "the child ended");
    }
    assert!(line.contains("ROLE primary"), "{line}");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        hand_off(&names, &[]),
        Ok(false),
        "Windows removed the pipe with the process"
    );
    assert!(matches!(
        start(&names, &[], no_wake()),
        LaunchRole::Primary(_)
    ));
}

fn spawn_role_child(base: &str, path: &str, stay: bool) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["child_launch", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_BASE, base)
        .env(CHILD_PATH, path)
        .env("QE_DOCS_HANDOFF_STAY", if stay { "1" } else { "0" })
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// The child: one launch. A primary lists what it received, and stays until told.
#[test]
fn child_launch() {
    let Some(base) = std::env::var_os(CHILD_BASE) else {
        return;
    };
    let names = PipeNames {
        base: base.to_string_lossy().into_owned(),
        level: IntegrityLevel::Medium,
    };
    let path = PathBuf::from(std::env::var_os(CHILD_PATH).unwrap());
    let stay = std::env::var_os("QE_DOCS_HANDOFF_STAY").is_some_and(|v| v == "1");
    match start(&names, &[path], Arc::new(|| {})) {
        LaunchRole::Primary(server) => {
            println!("ROLE primary");
            let end = Instant::now() + Duration::from_secs(if stay { 3600 } else { 8 });
            let mut total = 0;
            while Instant::now() < end {
                for request in server.take_requests() {
                    total += request.paths.len();
                    for path in request.paths {
                        println!("GOT {}", path.display());
                    }
                }
                if !stay && total >= 14 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        LaunchRole::HandedOff => println!("ROLE handed-off"),
        LaunchRole::Alone { error } => println!("ROLE alone {error}"),
    }
}

/// The plan's done-test: 15 files from Explorer. Explorer starts one process for each, at once,
/// with nothing running. Exactly one becomes the instance, and it hears about the other 14.
#[test]
fn fifteen_real_processes_at_once_make_one_instance_with_every_file() {
    let names = names("fifteen-processes");
    let children: Vec<_> = (0..15)
        .map(|i| spawn_role_child(&names.base, &format!(r"C:\file{i}.txt"), false))
        .collect();
    let mut primaries = 0;
    let mut handed = 0;
    let mut got = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains("ROLE primary") {
                primaries += 1;
            }
            if line.contains("ROLE handed-off") {
                handed += 1;
            }
            assert!(!line.contains("ROLE alone"), "{line}");
            if let Some(at) = line.find("GOT ") {
                got.push(line[at + 4..].trim().to_owned());
            }
        }
    }
    assert_eq!(primaries, 1);
    assert_eq!(handed, 14);
    got.sort();
    got.dedup();
    assert_eq!(got.len(), 14, "{got:?}");
}

#[test]
fn handed_over_files_land_as_tabs_of_the_last_used_window() {
    let folder = test_folder("handoff-tabs");
    let names = names("store");
    let (server, _) = server(&names);
    let mut h = Harness::new(&folder.join("root"));
    let (first, _) = h.store.new_window(false).unwrap();
    let (second, keep) = h.store.new_window(false).unwrap();
    h.store.window_activated(second);
    let mut paths = Vec::new();
    for i in 0..15 {
        let path = folder.join(format!("h{i:02}.txt"));
        write(&path, b"x");
        paths.push(path);
    }
    assert_eq!(hand_off(&names, &paths), Ok(true));
    let requests = server.take_requests();
    let opened = h
        .store
        .open_paths(&requests[0].paths, OpenTarget::LastUsedWindow, None)
        .unwrap();
    h.until("all load", |h| {
        opened.iter().all(|d| !h.store.is_loading(*d))
    });
    let mut expected = vec![keep];
    expected.extend(&opened);
    assert_eq!(h.store.tabs(second), expected);
    assert_eq!(h.store.tabs(first).len(), 1);
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::OpenFailed { .. })),
        0,
        "{:?}",
        h.events
    );
}

/// Two launches with nothing running. The other one takes the name in the moment after this
/// one found no instance. This one must hand off to it and not run alone.
#[test]
fn a_launch_that_loses_the_race_hands_off_to_the_winner() {
    let names = names("race");
    let winner: std::sync::Mutex<Option<HandoffServer>> = std::sync::Mutex::new(None);
    let path = PathBuf::from(r"C:\raced.txt");
    let role = start_with(&names, std::slice::from_ref(&path), no_wake(), &|| {
        let mut slot = winner.lock().unwrap();
        if slot.is_none() {
            *slot = listen(&names, no_wake()).unwrap();
        }
    });
    assert!(matches!(role, LaunchRole::HandedOff));
    let winner = winner.lock().unwrap().take().expect("the other launch won");
    assert_eq!(winner.take_requests()[0].paths, vec![path]);
}

/// The bytes of a whole message, as a launch sends them.
fn raw_message(paths: &[&str]) -> Vec<u8> {
    let mut units: Vec<u16> = Vec::new();
    for path in paths {
        units.extend(path.encode_utf16());
        units.push(0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x3145_5551u32.to_le_bytes());
    out.extend_from_slice(&(paths.len() as u32).to_le_bytes());
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn open_client(names: &PipeNames) -> std::fs::File {
    let end = Instant::now() + WAIT;
    loop {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(
                r"\\.\pipe\{}-{}",
                names.base,
                match names.level {
                    IntegrityLevel::Low => "low",
                    IntegrityLevel::Medium => "medium",
                    IntegrityLevel::High => "high",
                    IntegrityLevel::System => "system",
                }
            ));
        match opened {
            Ok(file) => return file,
            // The server makes its next pipe a moment after each connect.
            Err(error) if error.raw_os_error() == Some(231) && Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

#[test]
fn f14_a_client_that_never_reads_its_answer_does_not_block_the_next_launch() {
    let names = names("never-reads");
    let (server, _) = server(&names);
    let mut stalled = open_client(&names);
    stalled
        .write_all(&raw_message(&[r"C:\stalled.txt"]))
        .unwrap();
    for round in 0..3 {
        let began = Instant::now();
        assert_eq!(
            hand_off(&names, &[PathBuf::from(r"C:\next.txt")]),
            Ok(true),
            "round {round}"
        );
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "round {round}: {:?}",
            began.elapsed()
        );
    }
    drop(stalled);
    let total: usize = server.take_requests().iter().map(|r| r.paths.len()).sum();
    assert_eq!(total, 4);
}

#[test]
fn f14_a_client_that_trickles_is_dropped_by_a_limit_on_the_whole_message() {
    let names = names("trickle");
    let (server, _) = server(&names);
    let mut slow = open_client(&names);
    let message = raw_message(&[r"C:\slow.txt"]);
    let began = Instant::now();
    let trickle = std::thread::spawn(move || {
        for byte in &message {
            if slow.write_all(&[*byte]).is_err() {
                return Instant::now();
            }
            std::thread::sleep(Duration::from_millis(800));
        }
        Instant::now()
    });
    // A later launch is served while the slow one is still going.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(hand_off(&names, &[PathBuf::from(r"C:\next.txt")]), Ok(true));
    let refused_at = trickle.join().unwrap();
    assert!(
        refused_at.duration_since(began) < Duration::from_secs(4),
        "one byte in 800 ms must not hold it open: {:?}",
        refused_at.duration_since(began)
    );
    let all: Vec<PathBuf> = server
        .take_requests()
        .into_iter()
        .flat_map(|r| r.paths)
        .collect();
    assert_eq!(all, vec![PathBuf::from(r"C:\next.txt")]);
}

#[test]
fn f14_many_stalled_clients_at_once_do_not_make_a_later_launch_run_alone() {
    let names = names("many-stalled");
    let (_server, _) = server(&names);
    let stalled: Vec<_> = (0..20).map(|_| open_client(&names)).collect();
    assert_eq!(hand_off(&names, &[PathBuf::from(r"C:\next.txt")]), Ok(true));
    drop(stalled);
}

#[test]
fn f16_a_failed_server_says_so_restarts_once_and_then_says_it_stopped() {
    let names = names("restart");
    let (server, wakes) = server(&names);
    assert_eq!(hand_off(&names, &[]), Ok(true));
    let before = wakes.load(Ordering::Relaxed);
    server.break_for_test();
    let end = Instant::now() + WAIT;
    let mut events = Vec::new();
    while events.is_empty() && Instant::now() < end {
        events = server.take_events();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        matches!(events.as_slice(), [HandoffEvent::ServerRestarted { .. }]),
        "{events:?}"
    );
    assert!(wakes.load(Ordering::Relaxed) > before, "the UI is told");
    // It works again, and a launch is not left to run alone.
    assert_eq!(hand_off(&names, &[PathBuf::from(r"C:\a.txt")]), Ok(true));
    assert_eq!(server.take_requests().len(), 2);
    server.break_for_test();
    let mut events = Vec::new();
    while events.is_empty() && Instant::now() < end {
        events = server.take_events();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        matches!(events.as_slice(), [HandoffEvent::ServerStopped { .. }]),
        "{events:?}"
    );
}

#[test]
fn f15_a_message_past_the_limit_is_refused_and_the_instance_goes_on() {
    let names = names("too-many");
    let (server, _) = server(&names);
    let many: Vec<PathBuf> = (0..MAX_PATHS + 1)
        .map(|i| PathBuf::from(format!(r"C:\{i}.txt")))
        .collect();
    assert!(matches!(
        hand_off(&names, &many),
        Err(HandoffError::Failed { .. })
    ));
    // A launch that ignores the limit is dropped by the server.
    let words: Vec<String> = many.iter().map(|p| p.display().to_string()).collect();
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let mut raw = open_client(&names);
    raw.write_all(&raw_message(&refs)).unwrap();
    drop(raw);
    assert_eq!(hand_off(&names, &[PathBuf::from(r"C:\a.txt")]), Ok(true));
    let all: Vec<PathBuf> = server
        .take_requests()
        .into_iter()
        .flat_map(|r| r.paths)
        .collect();
    assert_eq!(all, vec![PathBuf::from(r"C:\a.txt")]);
}

#[test]
fn f15_odd_names_in_a_message_are_named_and_never_reach_the_paths() {
    let names = names("odd-names");
    let (server, _) = server(&names);
    let good = PathBuf::from(r"\\host\share\a.txt");
    let list = [
        good.clone(),
        PathBuf::from(r"\\.\pipe\x"),
        PathBuf::from(r"\\.\NUL"),
        PathBuf::from(r"\\?\GLOBALROOT\Device\x"),
        PathBuf::from(r"C:\a.txt:stream"),
        PathBuf::from(r"C:\folder\NUL.txt"),
    ];
    assert_eq!(hand_off(&names, &list), Ok(true));
    let requests = server.take_requests();
    assert_eq!(requests[0].paths, vec![good]);
    assert_eq!(requests[0].refused.len(), 5, "{:?}", requests[0].refused);
    assert!(
        requests[0]
            .refused
            .iter()
            .all(|e| e.kind == qe_docs::DocErrorKind::PathRefused)
    );
    // One with no folder of its own would mean this server's folder, so it is refused too.
    let mut raw = open_client(&names);
    raw.write_all(&raw_message(&["relative.txt"])).unwrap();
    let mut answer = [0u8; 1];
    std::io::Read::read_exact(&mut raw, &mut answer).unwrap();
    drop(raw);
    let requests = server.take_requests();
    assert!(requests[0].paths.is_empty());
    assert_eq!(requests[0].refused.len(), 1);
}

#[test]
fn f15_the_queue_for_the_ui_has_a_limit_and_a_launch_past_it_is_told() {
    let names = names("queue-limit");
    let (server, _) = server(&names);
    let batch: Vec<PathBuf> = (0..MAX_PATHS)
        .map(|i| PathBuf::from(format!(r"C:\{i}.txt")))
        .collect();
    for _ in 0..4 {
        assert_eq!(hand_off(&names, &batch), Ok(true));
    }
    assert!(matches!(
        hand_off(&names, &[PathBuf::from(r"C:\late.txt")]),
        Err(HandoffError::Failed { .. })
    ));
    let total: usize = server.take_requests().iter().map(|r| r.paths.len()).sum();
    assert_eq!(total, 4 * MAX_PATHS);
    assert_eq!(hand_off(&names, &batch), Ok(true), "room again");
}

#[test]
fn stopping_the_server_gives_back_what_came_and_the_next_launch_becomes_the_instance() {
    let names = names("stop-and-take");
    let (server, _) = server(&names);
    let path = std::env::current_exe().unwrap();
    assert_eq!(hand_off(&names, std::slice::from_ref(&path)), Ok(true));
    let requests = server.stop_and_take(Duration::from_secs(5));
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].paths, [path]);
    assert!(matches!(
        start(&names, &[], no_wake()),
        LaunchRole::Primary(_)
    ));
}

// ---- Who is on the other end of the pipe ----

/// A pipe held by a test, as a program that took the name first would hold it. `serve` runs on
/// a thread with the connected handle, and its result comes back at `join`.
fn fake_server<T: Send + 'static>(
    names: &PipeNames,
    serve: impl FnOnce(windows::Win32::Foundation::HANDLE) -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::PCWSTR;
    let name: Vec<u16> = format!(r"\\.\pipe\{}-medium", names.base)
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: the name ends in a zero and outlives the call.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(3),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        )
    };
    assert!(
        !handle.is_invalid(),
        "{}",
        windows::core::Error::from_thread()
    );
    let raw = handle.0 as usize;
    std::thread::spawn(move || {
        let handle = HANDLE(raw as *mut _);
        // SAFETY: the pipe is open and blocking, so this waits for the client.
        // A client that came first reports "connected", which is success for this test.
        let _ = unsafe { ConnectNamedPipe(handle, None) };
        let out = serve(handle);
        // SAFETY: this thread holds the only copy of the handle.
        unsafe { CloseHandle(handle) }.unwrap();
        out
    })
}

/// All that the client sends before it closes, or stops sending for this server's one read.
fn fake_read(pipe: windows::Win32::Foundation::HANDLE) -> Vec<u8> {
    let mut bytes = vec![0u8; 4096];
    let mut got = 0;
    // SAFETY: the buffer and the count outlive this blocking call. A closed pipe reads zero.
    let _ = unsafe {
        windows::Win32::Storage::FileSystem::ReadFile(pipe, Some(&mut bytes), Some(&mut got), None)
    };
    bytes.truncate(got as usize);
    bytes
}

/// The first bug's proof: a server that takes the name can read how far it may act as the
/// client. It must see the identification level, which lets it read who, and do nothing as them.
#[test]
fn a_launch_lets_the_server_learn_who_it_is_and_never_act_as_it() {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL, SecurityIdentification,
        TOKEN_QUERY, TokenImpersonationLevel,
    };
    use windows::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
    let names = names("identification-level");
    let fake = fake_server(&names, |pipe| {
        drop(fake_read(pipe));
        // SAFETY: the client has written, so its identity is there to take.
        unsafe { ImpersonateNamedPipeClient(pipe) }.unwrap();
        let mut token = HANDLE::default();
        // SAFETY: the out value outlives the call.
        let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) };
        let mut level = SECURITY_IMPERSONATION_LEVEL::default();
        let mut size = 0;
        opened.unwrap();
        // SAFETY: the buffer is the right size for this class and outlives the call.
        unsafe {
            GetTokenInformation(
                token,
                TokenImpersonationLevel,
                Some((&raw mut level).cast()),
                size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                &mut size,
            )
        }
        .unwrap();
        // SAFETY: this thread took the identity above.
        unsafe { RevertToSelf() }.unwrap();
        // SAFETY: the token is open, and this is its only close.
        unsafe { windows::Win32::Foundation::CloseHandle(token) }.unwrap();
        level
    });
    // The fake never answers, so the launch fails after it sends. That is not this test's point.
    let launch = hand_off(&names, &[PathBuf::from(r"C:\a.txt")]);
    assert!(launch.is_err(), "{launch:?}");
    assert_eq!(fake.join().unwrap(), SecurityIdentification);
}

/// The rights Windows holds for the real server's pipe, read back through a client's handle.
#[test]
fn the_pipe_allows_only_this_user() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    use windows::core::PWSTR;
    let real = PipeNames::for_this_process().unwrap();
    let names = names("rights");
    let (_server, _) = server(&names);
    let client = open_client(&names);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the handle is open, and `descriptor` is freed below.
    let read = unsafe {
        GetSecurityInfo(
            HANDLE(client.as_raw_handle()),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    assert!(read.is_ok(), "{read:?}");
    let mut text = PWSTR::null();
    // SAFETY: the descriptor came from the call above, and `text` is freed below.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
    }
    .unwrap();
    // SAFETY: Windows gave a string that ends in a zero.
    let rights = unsafe { text.to_string() }.unwrap();
    // SAFETY: both came from Windows, and each is freed once.
    unsafe {
        LocalFree(Some(HLOCAL(text.0.cast())));
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
    // The base is `QuickEdit-<the user's id>-<session>`.
    let user = real.base.split('-').skip(1).collect::<Vec<_>>();
    let user = user[..user.len() - 1].join("-");
    println!("the pipe's rights: {rights}");
    assert!(rights.starts_with("D:P"), "not protected: {rights}");
    assert_eq!(rights.matches('(').count(), 1, "one rule only: {rights}");
    assert!(rights.ends_with(&format!(";;;{user})")), "{rights}");
    for other in ["WD", "AN", "SY", "BA"] {
        assert!(!rights.contains(other), "{other} in {rights}");
    }
}

/// The fake holds the name and answers with a wrong byte, as another program would. The launch
/// names the problem and runs alone, so its file still opens.
#[test]
fn a_fake_server_with_the_wrong_answer_is_named_and_the_launch_runs_alone() {
    let names = names("wrong-protocol");
    let fake = fake_server(&names, |pipe| {
        let got = fake_read(pipe);
        // SAFETY: the pipe is open, and the bytes outlive this blocking call.
        unsafe { windows::Win32::Storage::FileSystem::WriteFile(pipe, Some(b"X"), None, None) }
            .unwrap();
        // The launch reads its answer, then closes. This read ends then.
        drop(fake_read(pipe));
        got
    });
    let role = start(&names, &[PathBuf::from(r"C:\some folder\a.txt")], no_wake());
    let LaunchRole::Alone { error } = role else {
        panic!("the launch trusted a fake server");
    };
    assert_eq!(error, HandoffError::WrongAnswer { byte: b'X' });
    assert!(
        error.to_string().contains("opens its own window"),
        "{error}"
    );
    assert!(
        !fake.join().unwrap().is_empty(),
        "it passed the same-user check"
    );
}

/// A server that fails the identity check is sent nothing at all, and the launch runs alone.
#[test]
fn a_server_that_fails_the_identity_check_gets_no_files() {
    let names = names("bad-identity");
    let fake = fake_server(&names, fake_read);
    let refuse = |_server: u32| {
        Err(HandoffError::UntrustedServer {
            why: "a test says so".to_owned(),
        })
    };
    let role = start_checked(
        &names,
        &[PathBuf::from(r"C:\secret folder\my taxes.txt")],
        no_wake(),
        &refuse,
    );
    let LaunchRole::Alone { error } = role else {
        panic!("the launch did not run alone");
    };
    assert!(
        matches!(error, HandoffError::UntrustedServer { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("a test says so"), "{error}");
    assert_eq!(fake.join().unwrap(), Vec::<u8>::new(), "bytes were sent");
}

/// The check gets the process that really holds the pipe.
#[test]
fn the_identity_check_is_given_the_process_that_holds_the_pipe() {
    let names = names("check-pid");
    let (_server, _) = server(&names);
    let seen = std::sync::Mutex::new(0);
    let check = |server: u32| {
        *seen.lock().unwrap() = server;
        Ok(())
    };
    assert_eq!(hand_off_checked(&names, &[], &check), Ok(true));
    assert_eq!(*seen.lock().unwrap(), std::process::id());
}

/// The real check, on real processes: this one passes. The System process is another user, and
/// Windows also may not let this process read it. Either way the launch must refuse it.
#[test]
fn the_real_identity_check_passes_this_process_and_refuses_system() {
    assert_eq!(server_is_this_user_and_level(std::process::id()), Ok(()));
    let system = server_is_this_user_and_level(4);
    assert!(
        matches!(system, Err(HandoffError::UntrustedServer { .. })),
        "{system:?}"
    );
    // A process that is gone is refused too, by a name that says what happened.
    let gone = server_is_this_user_and_level(0xFFFF_FFF0);
    assert_eq!(gone, Err(HandoffError::ServerGone));
}

/// A launch's added cost is small: the check on a real server, 50 times.
#[test]
fn the_identity_check_takes_little_time() {
    let mut samples: Vec<Duration> = (0..50)
        .map(|_| {
            let started = Instant::now();
            server_is_this_user_and_level(std::process::id()).unwrap();
            started.elapsed()
        })
        .collect();
    samples.sort();
    println!("the identity check: median {:?}", samples[25]);
    assert!(samples[25] < Duration::from_millis(5), "{:?}", samples[25]);
}

// ---- An instance that ends between the connect and the check.

/// The real order of events: this launch has the pipe open, and the instance ends before the
/// check on it. No other program holds the name, so the launch must not say one does.
#[test]
fn an_instance_that_ends_during_the_check_is_not_called_another_program() {
    let base = format!("QuickEdit-test-{}-gone", std::process::id());
    let names = PipeNames {
        base: base.clone(),
        level: IntegrityLevel::Medium,
    };
    let mut child = spawn_role_child(&base, r"C:\gone.txt", true);
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("ROLE") {
        line.clear();
        assert_ne!(output.read_line(&mut line).unwrap(), 0, "the child ended");
    }
    assert!(line.contains("ROLE primary"), "{line}");
    let child = std::sync::Mutex::new(Some(child));
    let tries = AtomicU64::new(0);
    let check = |server: u32| {
        tries.fetch_add(1, Ordering::Relaxed);
        // The last handle on the process goes too, so Windows forgets the id.
        if let Some(mut child) = child.lock().unwrap().take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        server_is_this_user_and_level(server)
    };
    assert_eq!(hand_off_checked(&names, &[], &check), Ok(false));
    assert_eq!(tries.load(Ordering::Relaxed), 1, "the pipe went with it");
}

#[test]
fn a_process_id_that_nothing_holds_counts_as_gone() {
    let mut child = Command::new("cmd")
        .args(["/c", "exit"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    let id = child.id();
    child.wait().unwrap();
    // This handle is one more thing that keeps the process's record. Either way it has ended.
    assert_eq!(
        server_is_this_user_and_level(id),
        Err(HandoffError::ServerGone)
    );
    drop(child);
    assert_eq!(
        server_is_this_user_and_level(id),
        Err(HandoffError::ServerGone)
    );
}

#[test]
fn a_connect_is_tried_again_when_the_instance_goes_and_nothing_is_lost() {
    let names = names("gone-once");
    let (server, _) = server(&names);
    let tries = AtomicU64::new(0);
    let check = |_: u32| {
        if tries.fetch_add(1, Ordering::Relaxed) == 0 {
            Err(HandoffError::ServerGone)
        } else {
            Ok(())
        }
    };
    let paths = [PathBuf::from(r"D:\kept.txt")];
    assert_eq!(hand_off_checked(&names, &paths, &check), Ok(true));
    assert_eq!(tries.load(Ordering::Relaxed), 2);
    let got: Vec<PathBuf> = server
        .take_requests()
        .into_iter()
        .flat_map(|request| request.paths)
        .collect();
    assert_eq!(got, paths, "sent once, after the check that passed");
}

#[test]
fn a_connect_stops_trying_and_says_the_instance_closed() {
    let names = names("gone-always");
    let (_server, _) = server(&names);
    let tries = AtomicU64::new(0);
    let check = |_: u32| {
        tries.fetch_add(1, Ordering::Relaxed);
        Err(HandoffError::ServerGone)
    };
    let error = hand_off_checked(&names, &[], &check).unwrap_err();
    assert_eq!(error, HandoffError::ServerGone);
    assert_eq!(tries.load(Ordering::Relaxed), 3);
    let shown = error.to_string();
    assert!(shown.contains("closed"), "{shown}");
    assert!(!shown.contains("Another program"), "{shown}");
}

/// A real pipe at this process's level, with the level mark read back through a client's handle.
/// At normal level there must be no mark. An elevated run sees the High mark; that branch needs
/// a person to run these tests from an elevated terminal.
#[test]
fn the_pipe_carries_a_level_mark_exactly_when_elevated() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::{LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    use windows::core::PWSTR;
    let real = PipeNames::for_this_process().unwrap();
    let names = PipeNames {
        base: format!("QuickEdit-test-{}-label", std::process::id()),
        level: real.level,
    };
    let (_server, _) = server(&names);
    let client = open_client(&names);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the handle is open, and `descriptor` is freed below.
    let read = unsafe {
        GetSecurityInfo(
            HANDLE(client.as_raw_handle()),
            SE_KERNEL_OBJECT,
            LABEL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    assert!(read.is_ok(), "{read:?}");
    let mut text = PWSTR::null();
    // SAFETY: the descriptor came from the call above, and `text` is freed below.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            LABEL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
    }
    .unwrap();
    // SAFETY: Windows gave a string that ends in a zero.
    let label = unsafe { text.to_string() }.unwrap();
    // SAFETY: both came from Windows, and each is freed once.
    unsafe {
        LocalFree(Some(HLOCAL(text.0.cast())));
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
    println!("level {:?}, the pipe's mark: {label:?}", real.level);
    match real.level {
        IntegrityLevel::High => assert_eq!(label, "S:(ML;;NW;;;HI)"),
        IntegrityLevel::System => assert_eq!(label, "S:(ML;;NW;;;SI)"),
        IntegrityLevel::Low | IntegrityLevel::Medium => {
            assert!(!label.contains("ML;"), "{label}");
        }
    }
}
