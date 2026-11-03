//! The file handoff between launches. Each test has its own pipe name, so that it never meets
//! a real QuickEdit or another test.
mod support;

use qe_docs::handoff::{
    HandoffError, HandoffEvent, HandoffServer, IntegrityLevel, LaunchRole, MAX_PATHS, PipeNames,
    hand_off, listen, start, start_with,
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
            .open(format!(r"\\.\pipe\{}-medium", names.base));
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
