//! Proves in a release build that no UI-thread call waits on disk or grows with the document.
//! `cargo test -p qe-docs --release --locked --test release_budget -- --nocapture`
//! It holds a large editable document, and it times each call while real disk work runs.
#![cfg(not(debug_assertions))]
mod support;

use qe_docs::OpenTarget;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::*;

const MIB: usize = 1024 * 1024;

#[derive(Default)]
struct Times(Vec<(&'static str, Vec<Duration>)>);
impl Times {
    fn record<T>(&mut self, name: &'static str, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let out = work();
        let took = started.elapsed();
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some((_, list)) => list.push(took),
            None => self.0.push((name, vec![took])),
        }
        out
    }
    /// Prints each call's numbers and fails if a p99 passes its limit.
    fn check(&mut self, limit_for: impl Fn(&str) -> Duration) {
        let mut over = Vec::new();
        for (name, list) in &mut self.0 {
            list.sort();
            let p99 = list[(list.len() * 99 / 100).min(list.len() - 1)];
            let worst = list[list.len() - 1];
            println!(
                "{name:<28} n={:<5} p50={:>9.3?} p99={:>9.3?} worst={:>9.3?}",
                list.len(),
                list[list.len() / 2],
                p99,
                worst
            );
            if p99 > limit_for(name) {
                over.push(format!("{name}: p99 {p99:?}"));
            }
        }
        assert!(over.is_empty(), "over the limit: {over:?}");
    }
}

struct Remove(std::path::PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

fn big_file(path: &std::path::Path, bytes: usize) {
    let line = "the quick brown fox jumps over the lazy dog, again and again\r\n";
    let mut block = String::with_capacity(MIB + line.len());
    while block.len() < MIB {
        block.push_str(line);
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut written = 0;
    while written < bytes {
        file.write_all(block.as_bytes()).unwrap();
        written += block.len();
    }
    file.flush().unwrap();
}

#[test]
fn release_budget_no_ui_call_waits_on_disk_with_a_large_editable_document() {
    let folder = test_folder("budget");
    let _remove = Remove(folder.clone());
    let path = folder.join("large.txt");
    let size = 500 * MIB;
    big_file(&path, size);
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(1);
    cfg.session_delay = Duration::from_millis(1);
    cfg.watch_folders = true;
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let mut t = Times::default();

    let started = Instant::now();
    let opened = t.record("open_paths", || {
        h.store
            .open_paths(std::slice::from_ref(&path), OpenTarget::NewWindow, None)
            .unwrap()
    });
    let doc = opened[0];
    while h.store.is_loading(doc) {
        t.record("poll during the load", || h.store.poll(Instant::now()));
        h.store.take_events();
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "the load of {} MiB took {:?}",
        size / MIB,
        started.elapsed()
    );
    assert!(h.store.commands(doc).save, "this document must be editable");
    assert!(h.store.is_large(doc));

    // Typing, each key with its note, while autosaves of the whole text run.
    let mut copies = 0;
    let end = Instant::now() + Duration::from_secs(60);
    let mut was_running = false;
    let mut copy_started = Instant::now();
    while copies < 2 && Instant::now() < end {
        let mut ctx = qe_api::ViewContext::default();
        t.record("a key in the view", || {
            let view = h.store.view_mut(doc).unwrap();
            qe_api::View::handle(
                view,
                &qe_api::InputEvent {
                    time_us: 0,
                    kind: qe_api::InputKind::Char('x'),
                },
                &mut ctx,
            );
        });
        t.record("note_view_changed", || h.store.note_view_changed(doc));
        t.record("poll during an autosave", || h.store.poll(Instant::now()));
        t.record("next_deadline", || h.store.next_deadline());
        t.record("is_dirty", || h.store.is_dirty(doc));
        t.record("is_blank_untitled", || h.store.is_blank_untitled(doc));
        t.record("commands", || h.store.commands(doc));
        t.record("title_note", || h.store.title_note(doc));
        let running = h.store.is_autosaving(doc);
        if running && !was_running {
            copy_started = Instant::now();
        }
        if was_running && !running {
            copies += 1;
            println!("a copy of the whole text took {:?}", copy_started.elapsed());
        }
        was_running = running;
        h.store.take_events();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(copies >= 2, "{copies} whole copies were written");
    while h.store.is_autosaving(doc) {
        h.store.poll(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!autosave_files(&root).is_empty());

    // Saves, with typing during each write.
    faults.autosave_stall_after.store(0, Ordering::Relaxed);
    for _ in 0..3 {
        h.type_text(doc, "s");
        h.store.take_events();
        h.events.clear();
        let started = Instant::now();
        t.record("save", || h.store.save(doc).unwrap());
        let mut typed = false;
        while h.store.is_saving(doc) {
            t.record("poll during a save", || h.store.poll(Instant::now()));
            if !typed {
                t.record("note during a save", || h.store.note_view_changed(doc));
                typed = true;
            }
            h.events.extend(h.store.take_events());
            std::thread::sleep(Duration::from_millis(1));
        }
        println!("a save of the whole text took {:?}", started.elapsed());
        assert!(
            h.count(|e| matches!(e, qe_docs::DocEvent::SaveFailed { .. })) == 0,
            "{:?}",
            h.events
        );
    }
    h.quiet();

    // The other calls a menu runs, with this large document open.
    let window = h.store.window_of(doc).unwrap();
    for _ in 0..20 {
        let tab = t.record("new_tab", || h.store.new_tab(window).unwrap());
        t.record("close of an empty tab", || h.store.request_close(tab));
        h.store.take_events();
    }
    let small = folder.join("small.txt");
    write(&small, b"x");
    for _ in 0..20 {
        let opened = t.record("open_paths, small file", || {
            h.store
                .open_paths(
                    std::slice::from_ref(&small),
                    OpenTarget::Window(window),
                    None,
                )
                .unwrap()
        });
        h.loaded(opened[0]);
        h.store.request_close(opened[0]);
    }
    t.record("duplicate", || h.store.duplicate(doc).unwrap());
    t.record("app_activated", || h.store.app_activated());
    // Close of the large document: its text goes to a worker to free.
    h.store.save(doc).unwrap();
    h.until("the save ends", |h| {
        !h.store.is_saving(doc) && !h.store.is_dirty(doc)
    });
    t.record("close of the large tab", || h.store.request_close(doc));

    t.check(|name| match name {
        // One small edit's work, as the editor's own budget has.
        "note_view_changed" | "next_deadline" | "is_dirty" | "is_blank_untitled" | "commands"
        | "title_note" | "note during a save" => Duration::from_millis(2),
        // A new tab starts a view with its workers and its font, on any size of document.
        "new_tab" | "open_paths" | "open_paths, small file" | "duplicate" => {
            Duration::from_millis(40)
        }
        _ => Duration::from_millis(16),
    });
    assert_eq!(h.store.windows().len(), 1, "the copy's tab stays");
}
