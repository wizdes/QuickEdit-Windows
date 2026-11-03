//! Measures the second launch: from starting a process until its file is in a tab of the
//! running instance. Windows run hidden; none opens here, since this crate owns no window.
//!
//! `cargo run -p qe-docs --release --locked --example handoff_bench`
//!
//! The child is this small program, not the real app, so the real app's own start-up time
//! (its size, its DLLs) adds to these numbers. The main thread of that app must call
//! `handoff::start` before it loads anything else.
use qe_docs::handoff::{IntegrityLevel, PipeNames, hand_off, listen};
use qe_docs::{DocumentStore, OpenTarget, StoreConfig};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ROUNDS: usize = 40;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--client") {
        let names = PipeNames {
            base: args[2].clone(),
            level: IntegrityLevel::Medium,
        };
        let sent = hand_off(&names, &[PathBuf::from(&args[3])]);
        std::process::exit(if sent == Ok(true) { 0 } else { 1 });
    }
    let folder = std::env::temp_dir().join(format!("qe-docs-bench-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("make the bench folder");
    let names = PipeNames {
        base: format!("QuickEdit-bench-{}", std::process::id()),
        level: IntegrityLevel::Medium,
    };
    let woke = Arc::new(AtomicBool::new(false));
    let flag = woke.clone();
    let server = listen(
        &names,
        Arc::new(move || flag.store(true, Ordering::Release)),
    )
    .expect("listen")
    .expect("the name is free");
    let mut config = StoreConfig::new(folder.join("root"), Arc::new(|| {}));
    config.watch_folders = false;
    let mut store = DocumentStore::new(config);
    store.begin_restore();
    let end = Instant::now() + Duration::from_secs(10);
    while store.windows().is_empty() && Instant::now() < end {
        store.poll(Instant::now());
        if store
            .take_events()
            .iter()
            .any(|e| matches!(e, qe_docs::DocEvent::RestoreFinished { .. }))
        {
            break;
        }
    }
    store.new_window(false).expect("a window");
    let exe = std::env::current_exe().expect("this program's path");
    let (mut to_queue, mut to_tab, mut to_exit) = (Vec::new(), Vec::new(), Vec::new());
    for round in 0..ROUNDS {
        let path = folder.join(format!("f{round}.txt"));
        std::fs::write(&path, b"some text").expect("write a file");
        let started = Instant::now();
        let mut child = std::process::Command::new(&exe)
            .args(["--client", &names.base, &path.to_string_lossy()])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start the second launch");
        while !woke.swap(false, Ordering::AcqRel) {
            std::hint::spin_loop();
        }
        to_queue.push(started.elapsed());
        let requests = server.take_requests();
        let opened = store
            .open_paths(&requests[0].paths, OpenTarget::LastUsedWindow, None)
            .expect("open");
        while store.is_loading(opened[0]) {
            store.poll(Instant::now());
        }
        to_tab.push(started.elapsed());
        store.take_events();
        let status = child.wait().expect("the launch ends");
        assert!(status.success(), "round {round}: the launch failed");
        to_exit.push(started.elapsed());
        store.request_close(opened[0]);
    }
    let report = |name: &str, list: &mut Vec<Duration>| {
        list.sort();
        println!(
            "{name:<34} p50={:>8.2?} p90={:>8.2?} worst={:>8.2?}",
            list[list.len() / 2],
            list[list.len() * 9 / 10],
            list[list.len() - 1]
        );
    };
    report("process start to the queue", &mut to_queue);
    report("process start to the text in a tab", &mut to_tab);
    report("process start to its exit", &mut to_exit);
    let median = to_tab[to_tab.len() / 2];
    drop(store);
    drop(server);
    if let Err(error) = std::fs::remove_dir_all(&folder) {
        eprintln!("could not remove {}: {error}", folder.display());
    }
    assert!(
        median < Duration::from_millis(50),
        "the plan's goal is under 50 ms; the median is {median:?}"
    );
}
