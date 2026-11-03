//! Real kills: a child process dies in the middle of an autosave or a save, and a new store
//! restores what it left. The child is this test binary, with one test name and a folder.
mod support;

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CHILD_ROOT: &str = "QE_DOCS_CHILD_ROOT";
const CHILD_FILE: &str = "QE_DOCS_CHILD_FILE";

fn start_child(test: &str, root: &Path, file: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, root)
        .env(CHILD_FILE, file)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn child_paths() -> Option<(PathBuf, PathBuf)> {
    Some((
        PathBuf::from(std::env::var_os(CHILD_ROOT)?),
        PathBuf::from(std::env::var_os(CHILD_FILE)?),
    ))
}

/// Reads the child's lines until one has this prefix, and gives what follows it.
fn wait_for_line(reader: &mut impl BufRead, prefix: &str) -> String {
    let mut line = String::new();
    loop {
        line.clear();
        let got = reader.read_line(&mut line).unwrap();
        assert!(got > 0, "the child ended before it said {prefix}");
        if let Some(at) = line.find(prefix) {
            return line[at + prefix.len()..].trim().to_owned();
        }
    }
}

fn all_loaded(h: &mut Harness) {
    h.until("every tab has its text", |h| {
        h.store
            .all_documents()
            .iter()
            .all(|d| !h.store.is_loading(*d))
    });
}

/// The child: one good copy, then a copy that stops part way, for good.
#[test]
fn child_stalls_in_the_middle_of_an_autosave() {
    let Some((root, file)) = child_paths() else {
        return;
    };
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(1);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let doc = h.open(&file);
    h.select(doc, 1, 1);
    h.type_text(doc, "GOOD");
    h.until("the first copy", |h| {
        autosave_files(&h.root).iter().any(|n| n.ends_with(".qea"))
    });
    h.quiet();
    println!("FIRST-COPY-DONE");
    faults.autosave_stall_after.store(1, Ordering::Relaxed);
    let mut ctx = qe_api::ViewContext::default();
    h.store.view_mut(doc).unwrap().replace_range(
        qe_api::ByteOffset(0)..qe_api::ByteOffset(0),
        &"NOT YET KEPT ".repeat(50_000),
        &mut ctx,
    );
    h.store.note_view_changed(doc);
    h.until("the stall", |_| {
        faults.autosave_is_stalled.load(Ordering::Acquire)
    });
    println!("STALLED");
    loop {
        h.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_kill_in_the_middle_of_an_autosave_keeps_the_last_good_copy() {
    let folder = test_folder("kill-autosave");
    let root = folder.join("root");
    let file = folder.join("f.txt");
    write(&file, b"disk\r\ntext");
    let mut child = start_child("child_stalls_in_the_middle_of_an_autosave", &root, &file);
    let mut out = BufReader::new(child.stdout.take().unwrap());
    wait_for_line(&mut out, "FIRST-COPY-DONE");
    wait_for_line(&mut out, "STALLED");
    let names = autosave_files(&root);
    assert!(
        names.iter().any(|n| n.ends_with(".tmp")),
        "the kill comes with a partial copy on disk: {names:?}"
    );
    child.kill().unwrap();
    child.wait().unwrap();

    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = h.store.all_documents()[0];
    assert_eq!(h.text(doc), "dGOODisk\r\ntext", "the last good copy, whole");
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.store.path(doc).unwrap(), file);
    assert_eq!(std::fs::read(&file).unwrap(), b"disk\r\ntext");
    assert!(
        !autosave_files(&root).iter().any(|n| n.ends_with(".tmp")),
        "the partial copy went"
    );
    // The restored document goes on as usual.
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&file).unwrap(), b"dGOODisk\r\ntext");
}

/// The child: a copy after every key, as fast as the disk allows. Each line says how long the
/// text in the newest finished copy is.
#[test]
fn child_autosaves_until_killed() {
    let Some((root, _)) = child_paths() else {
        return;
    };
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(1);
    let mut h = Harness::with_config(cfg, wakes);
    let existing = h.store.all_documents();
    let doc = match existing.first() {
        Some(doc) => {
            let doc = *doc;
            h.until("the restored text", |h| !h.store.is_loading(doc));
            h.caret_to_end(doc);
            doc
        }
        None => h.store.new_window(false).unwrap().1,
    };
    // Enough text that a write takes real time, so kills fall inside writes.
    if h.text(doc).is_empty() {
        let mut ctx = qe_api::ViewContext::default();
        h.store.view_mut(doc).unwrap().replace_range(
            qe_api::ByteOffset(0)..qe_api::ByteOffset(0),
            &"0123456789abcde\n".repeat(100_000),
            &mut ctx,
        );
        h.store.note_view_changed(doc);
        h.caret_to_end(doc);
    }
    let mut seen = 0usize;
    loop {
        h.type_text(doc, "k");
        h.quiet();
        // The newest copy on disk now has this text. This line is the promise the test holds.
        let length = h.store.view(doc).unwrap().store_status().known_bytes.0;
        if length != seen {
            seen = length;
            println!("KEPT {length}");
        }
    }
}

#[test]
fn twelve_kills_at_random_moments_of_autosaves_lose_nothing_promised() {
    let folder = test_folder("kill-loop");
    let root = folder.join("root");
    let file = folder.join("unused.txt");
    let mut promised = 0usize;
    let base = "0123456789abcde\n".repeat(100_000);
    let mut clock = 7u64;
    let mut kills_in_a_write = 0;
    for round in 0..12 {
        let mut child = start_child("child_autosaves_until_killed", &root, &file);
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let first: usize = wait_for_line(&mut out, "KEPT ").parse().unwrap();
        promised = promised.max(first);
        // A varying wait, so that kills fall in different parts of a write.
        clock = clock
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        std::thread::sleep(Duration::from_millis(5 + (clock >> 58)));
        child.kill().unwrap();
        // Lines already written are promises too.
        for line in out.lines().map_while(Result::ok) {
            if let Some(at) = line.find("KEPT ")
                && let Ok(length) = line[at + 5..].trim().parse::<usize>()
            {
                promised = promised.max(length);
            }
        }
        child.wait().unwrap();
        if autosave_files(&root).iter().any(|n| n.ends_with(".tmp")) {
            kills_in_a_write += 1;
        }
        let mut h = Harness::new(&root);
        all_loaded(&mut h);
        let docs = h.store.all_documents();
        assert_eq!(docs.len(), 1, "round {round}: {:?}", h.events);
        let text = h.text(docs[0]);
        assert!(
            text.len() >= promised,
            "round {round}: {} bytes came back and {promised} were promised",
            text.len()
        );
        assert!(text.starts_with(&base), "round {round}: its start is whole");
        assert!(
            text[base.len()..].bytes().all(|b| b == b'k'),
            "round {round}: its end is whole"
        );
        assert_eq!(
            h.count(|e| matches!(e, qe_docs::DocEvent::RestoreProblem(_))),
            0,
            "round {round}: {:?}",
            h.events
        );
    }
    println!("{kills_in_a_write} of 12 kills fell inside a write");
    assert!(kills_in_a_write > 0, "no kill fell inside a write");
}

/// The child: saves the file over and over, each time whole lines of one letter.
#[test]
fn child_saves_until_killed() {
    let Some((root, file)) = child_paths() else {
        return;
    };
    let mut h = Harness::new(&root);
    let doc = match h.store.all_documents().first() {
        Some(doc) => {
            let doc = *doc;
            h.until("the text", |h| !h.store.is_loading(doc));
            doc
        }
        None => h.open(&file),
    };
    println!("READY");
    let mut letter = b'a';
    loop {
        letter = if letter == b'z' { b'a' } else { letter + 1 };
        let text = (letter as char).to_string().repeat(1 << 20);
        let mut ctx = qe_api::ViewContext::default();
        let view = h.store.view_mut(doc).unwrap();
        let end = view.store_status().known_bytes;
        view.replace_range(qe_api::ByteOffset(0)..end, &text, &mut ctx);
        h.store.note_view_changed(doc);
        h.clear_events();
        h.store.save(doc).unwrap();
        h.saved(doc);
    }
}

#[test]
fn ten_kills_in_the_middle_of_saves_leave_a_whole_file() {
    let folder = test_folder("kill-save");
    let root = folder.join("root");
    let file = folder.join("f.txt");
    write(&file, &[b'a'; 1 << 20]);
    let mut clock = 3u64;
    let mut gone_rounds = 0;
    for round in 0..10 {
        let mut child = start_child("child_saves_until_killed", &root, &file);
        let mut out = BufReader::new(child.stdout.take().unwrap());
        wait_for_line(&mut out, "READY");
        clock = clock
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        std::thread::sleep(Duration::from_millis(10 + (clock >> 58)));
        child.kill().unwrap();
        child.wait().unwrap();
        // A kill between the two renames inside ReplaceFileW leaves no file at the name for a
        // moment. The old text is whole in the backup, and the restore offers it.
        if !file.exists() {
            gone_rounds += 1;
            let mut h = Harness::new(&root);
            all_loaded(&mut h);
            let leftover = h.events.iter().find_map(|e| match e {
                qe_docs::DocEvent::LeftoverFound { leftover, .. }
                    if leftover.extension().is_some_and(|x| x == "bak") =>
                {
                    Some(leftover.clone())
                }
                _ => None,
            });
            let leftover = leftover.unwrap_or_else(|| {
                panic!(
                    "round {round}: the file is gone and nothing offers it: {:?}",
                    h.events
                )
            });
            h.store.recover_leftover(leftover, file.clone());
            h.until("the old text is back", |h| {
                h.events
                    .iter()
                    .any(|e| matches!(e, qe_docs::DocEvent::LeftoverRecovered { .. }))
            });
        }
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(bytes.len(), 1 << 20, "round {round}");
        assert!(
            bytes.iter().all(|b| *b == bytes[0]),
            "round {round}: one save's bytes, not a mix"
        );
        // Each kill leaves at most a temp file, which `find_leftovers` lists and never offers
        // while the document is whole.
        let leftovers = qe_codec::find_leftovers(&file).unwrap();
        assert!(
            leftovers
                .iter()
                .all(|l| !l.offer_recovery || l.owner_running),
            "round {round}: {leftovers:?}"
        );
        for leftover in leftovers {
            qe_codec::delete_leftover(&leftover, qe_codec::LeftoverDelete::IncludeRecoverable)
                .unwrap();
        }
    }
    println!("{gone_rounds} of 10 kills fell inside the swap");
    let started = Instant::now();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    assert!(started.elapsed() < WAIT);
    h.settle(Duration::from_millis(20));
    let temps: Vec<_> = std::fs::read_dir(root.join("session"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("session.tmp-") && !n.ends_with(&std::process::id().to_string()))
        .collect();
    assert_eq!(
        temps,
        Vec::<String>::new(),
        "the temp files of killed runs went"
    );
}
