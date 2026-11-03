//! Another program changes, moves or deletes an open file. The watch is on in these tests,
//! except where the test says.
mod support;

use qe_docs::{DocErrorKind, DocEvent, QuestionKind, StoreConfig};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use support::*;

fn watching(root: &Path) -> Harness {
    let (mut config, wakes): (StoreConfig, Arc<AtomicU64>) = config(root);
    config.watch_folders = true;
    Harness::with_config(config, wakes)
}

fn reloaded(h: &mut Harness, doc: qe_docs::DocId) {
    h.until("the reload ends", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::Reloaded(d) if *d == doc))
    });
}

/// File times move about every 16 ms, and a watch needs a moment to start.
fn pause() {
    std::thread::sleep(Duration::from_millis(40));
}

#[test]
fn a_clean_document_takes_the_new_bytes_and_keeps_its_caret() {
    let folder = test_folder("out-reload");
    let path = folder.join("f.txt");
    write(&path, b"before\n");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.select(doc, 3, 3);
    pause();
    std::fs::write(&path, b"after and more\n").unwrap();
    reloaded(&mut h, doc);
    assert_eq!(h.text(doc), "after and more\n");
    assert!(!h.store.is_dirty(doc));
    assert_eq!(h.store.view(doc).unwrap().caret().0, 3);
    // A later save does not ask, since the stamp moved with the reload.
    h.type_text(doc, "x");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.open_questions().is_empty());
}

#[test]
fn a_caret_past_the_new_end_comes_back_inside() {
    let folder = test_folder("out-clamp");
    let path = folder.join("f.txt");
    write(&path, "long text \u{1f600}\u{1f600} here\n".as_bytes());
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.caret_to_end(doc);
    pause();
    // The new end falls inside the place the old caret was, and is shorter.
    std::fs::write(&path, "\u{1f600}".as_bytes()).unwrap();
    reloaded(&mut h, doc);
    assert_eq!(h.store.view(doc).unwrap().caret().0, 4);
    h.type_text(doc, "z");
    assert_eq!(h.text(doc), "\u{1f600}z", "typing still works");
}

#[test]
fn a_date_only_change_keeps_the_text_the_caret_and_undo() {
    let folder = test_folder("out-date");
    let path = folder.join("f.txt");
    write(&path, b"same bytes");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.undo(doc);
    h.type_text(doc, "y");
    h.store.save(doc).unwrap();
    h.saved(doc);
    h.clear_events();
    let before = std::fs::read(&path).unwrap();
    pause();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(file);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    h.settle(Duration::from_millis(300));
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
    assert_eq!(h.text(doc), "ysame bytes");
    h.undo(doc);
    assert_eq!(h.text(doc), "same bytes", "undo reaches back as before");
    h.type_text(doc, "z");
    h.store.save(doc).unwrap();
    h.clear_events();
    h.until("the save ends with no question", |h| {
        !h.store.is_saving(doc) && !h.store.is_dirty(doc)
    });
    assert!(
        h.store.open_questions().is_empty(),
        "the stamp was taken up"
    );
}

#[test]
fn a_changed_encoding_of_the_same_text_updates_the_format_only() {
    let folder = test_folder("out-format");
    let path = folder.join("f.txt");
    write(&path, b"abc");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "q");
    h.undo(doc);
    pause();
    std::fs::write(&path, b"\xEF\xBB\xBFabc").unwrap();
    h.until("the format follows", |h| {
        h.store.format(doc).unwrap().has_bom
    });
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
    assert_eq!(h.text(doc), "abc");
}

#[test]
fn canonically_equal_but_different_text_does_reload() {
    let folder = test_folder("out-canon");
    let path = folder.join("f.txt");
    write(&path, "\u{e9}".as_bytes());
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    std::fs::write(&path, "e\u{301}".as_bytes()).unwrap();
    reloaded(&mut h, doc);
    assert_eq!(h.text(doc), "e\u{301}");
}

#[test]
fn a_dirty_document_is_not_reloaded_and_its_save_asks() {
    let folder = test_folder("out-dirty");
    let path = folder.join("f.txt");
    write(&path, b"before\n");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "mine ");
    pause();
    std::fs::write(&path, b"theirs\n").unwrap();
    h.settle(Duration::from_millis(300));
    assert_eq!(h.text(doc), "mine before\n");
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
    h.store.save(doc).unwrap();
    let question = h.question("the conflict question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"theirs\n");
}

/// A store whose reloads can be held, as a slow disk would hold them.
fn holding(root: &Path) -> (Harness, Arc<qe_docs::Faults>) {
    let (mut cfg, wakes) = config(root);
    cfg.watch_folders = true;
    let faults = cfg.faults.clone();
    (Harness::with_config(cfg, wakes), faults)
}

fn reload_held(h: &mut Harness, faults: &qe_docs::Faults) {
    h.until("the reload holds", |_| {
        faults.reload_is_held.load(Ordering::Acquire)
    });
}

#[test]
fn typing_during_a_reload_read_keeps_the_text_and_the_save_asks() {
    let folder = test_folder("out-typing");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let (mut h, faults) = holding(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    faults.hold_reload.store(true, Ordering::Release);
    std::fs::write(&path, b"theirs").unwrap();
    reload_held(&mut h, &faults);
    h.type_text(doc, "mine ");
    faults.hold_reload.store(false, Ordering::Release);
    h.until("the read ends", |h| {
        !h.store.commands(doc).save || h.store.commands(doc).revert
    });
    h.settle(Duration::from_millis(100));
    assert_eq!(h.text(doc), "mine before", "no edit is lost");
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
    h.store.save(doc).unwrap();
    let question = h.question("the conflict question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
}

#[test]
fn a_change_during_a_reload_is_taken_too() {
    let folder = test_folder("out-twice");
    let path = folder.join("f.txt");
    write(&path, b"first");
    let (mut h, faults) = holding(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    faults.hold_reload.store(true, Ordering::Release);
    std::fs::write(&path, b"second").unwrap();
    reload_held(&mut h, &faults);
    h.settle(Duration::from_millis(50));
    std::fs::write(&path, b"third, the last").unwrap();
    h.settle(Duration::from_millis(100));
    faults.hold_reload.store(false, Ordering::Release);
    h.until("the last text arrives", |h| {
        h.text(doc) == "third, the last"
    });
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn a_close_during_a_reload_ends_it_with_no_trace() {
    let folder = test_folder("out-close");
    let path = folder.join("f.txt");
    write(&path, b"first");
    let (mut h, faults) = holding(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    faults.hold_reload.store(true, Ordering::Release);
    std::fs::write(&path, b"second").unwrap();
    reload_held(&mut h, &faults);
    h.store.request_close(doc);
    assert!(h.store.view(doc).is_none());
    faults.hold_reload.store(false, Ordering::Release);
    h.settle(Duration::from_millis(100));
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
    assert!(h.store.all_documents().is_empty());
}

#[test]
fn file_commands_wait_for_a_reload_and_say_why() {
    let folder = test_folder("out-busy");
    let path = folder.join("f.txt");
    write(&path, b"first");
    let (mut h, faults) = holding(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    faults.hold_reload.store(true, Ordering::Release);
    std::fs::write(&path, b"second").unwrap();
    reload_held(&mut h, &faults);
    let commands = h.store.commands(doc);
    assert!(!commands.save && !commands.save_as && !commands.duplicate);
    assert_eq!(h.store.save(doc).unwrap_err().kind, DocErrorKind::Busy);
    assert_eq!(h.store.duplicate(doc).unwrap_err().kind, DocErrorKind::Busy);
    let other = h
        .store
        .save_as(doc, folder.join("g.txt"), None)
        .unwrap_err();
    assert_eq!(other.kind, DocErrorKind::Busy);
    faults.hold_reload.store(false, Ordering::Release);
    reloaded(&mut h, doc);
    assert!(h.store.commands(doc).save);
}

/// The gap between the read and the moment the watch stands. Nobody can hear a change there.
#[test]
fn a_change_before_the_watch_starts_is_still_seen() {
    let folder = test_folder("out-gap");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let (mut h, faults) = holding(&folder.join("root"));
    faults.hold_watch.store(true, Ordering::Release);
    let doc = h.open(&path);
    h.until("the watch has not started", |_| {
        faults.watch_is_held.load(Ordering::Acquire)
    });
    pause();
    std::fs::write(&path, b"changed in the gap").unwrap();
    h.settle(Duration::from_millis(50));
    assert_eq!(h.text(doc), "before");
    faults.hold_watch.store(false, Ordering::Release);
    reloaded(&mut h, doc);
    assert_eq!(h.text(doc), "changed in the gap");
}

#[test]
fn a_replace_by_rename_is_a_change_and_the_document_stays_on_the_path() {
    let folder = test_folder("out-replace");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    let temp = folder.join("f.tmp");
    std::fs::write(&temp, b"replaced whole").unwrap();
    std::fs::rename(&temp, &path).unwrap();
    reloaded(&mut h, doc);
    assert_eq!(h.text(doc), "replaced whole");
    assert_eq!(h.store.path(doc).unwrap(), path);
}

#[test]
fn delete_then_create_at_once_is_a_reload_and_not_a_deletion() {
    let folder = test_folder("out-safe-save");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"written again").unwrap();
    reloaded(&mut h, doc);
    assert_eq!(h.text(doc), "written again");
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileDeleted { .. })), 0);
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn a_deleted_file_stays_open_counts_as_unsaved_and_a_save_writes_it_again() {
    let folder = test_folder("out-delete");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.watch_folders = true;
    cfg.autosave_delay = Duration::from_millis(5);
    let mut h = Harness::with_config(cfg, wakes);
    let doc = h.open(&path);
    pause();
    std::fs::remove_file(&path).unwrap();
    h.until("the deletion is news", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileDeleted { .. }))
    });
    assert_eq!(h.text(doc), "text");
    assert!(h.store.is_dirty(doc), "disk no longer holds this text");
    assert_eq!(h.store.title_note(doc).as_deref(), Some("Deleted"));
    assert!(!h.store.commands(doc).revert);
    h.until("the text is kept", |_| !autosave_files(&root).is_empty());
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"text");
    assert!(!h.store.is_dirty(doc));
    assert_eq!(h.store.title_note(doc), None);
}

#[test]
fn a_rename_is_followed_within_a_folder_with_dirty_text_kept() {
    let folder = test_folder("out-rename");
    let path = folder.join("old.txt");
    write(&path, b"text");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    pause();
    let moved = folder.join("new name.txt");
    std::fs::rename(&path, &moved).unwrap();
    h.until("the rename is news", |h| {
        h.store.path(doc).as_deref() == Some(moved.as_path())
    });
    assert_eq!(h.store.display_name(doc), "new name.txt");
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.text(doc), "xtext");
    assert_eq!(h.store.recent()[0], moved);
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&moved).unwrap(), b"xtext");
    assert!(!path.exists());
}

#[test]
fn a_move_to_another_folder_is_followed_by_the_file_number() {
    let folder = test_folder("out-move");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let elsewhere = folder.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    let moved = elsewhere.join("f.txt");
    std::fs::rename(&path, &moved).unwrap();
    h.until("the move is news", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileMoved { .. }))
    });
    let now = h.store.path(doc).unwrap();
    assert!(
        now.to_string_lossy()
            .to_lowercase()
            .ends_with(r"elsewhere\f.txt"),
        "{now:?}"
    );
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileDeleted { .. })), 0);
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn bytes_that_no_longer_fit_report_once_and_keep_the_text() {
    let folder = test_folder("out-undecodable");
    let path = folder.join("f.txt");
    write(&path, b"good");
    let mut h = watching(&folder.join("root"));
    let doc = h.open_with(
        &path,
        qe_api::EncodingChoice::Specific(qe_api::TextEncoding::Utf8),
    );
    pause();
    std::fs::write(&path, [0xFFu8, 0xFE, 0xFD]).unwrap();
    h.until("the failure is named", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::ReloadFailed { .. }))
    });
    h.settle(Duration::from_millis(100));
    assert_eq!(h.count(|e| matches!(e, DocEvent::ReloadFailed { .. })), 1);
    let kind = h.events.iter().find_map(|e| match e {
        DocEvent::ReloadFailed { error, .. } => Some(error.kind),
        _ => None,
    });
    assert_eq!(kind, Some(DocErrorKind::EncodingDoesNotFit));
    assert_eq!(h.text(doc), "good");
}

#[test]
fn activation_finds_a_change_with_the_watch_off() {
    let folder = test_folder("out-activate");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    std::fs::write(&path, b"after").unwrap();
    h.settle(Duration::from_millis(100));
    assert_eq!(h.text(doc), "before", "no watch, no news");
    h.store.app_activated();
    reloaded(&mut h, doc);
    assert_eq!(h.text(doc), "after");
}

#[test]
fn nothing_wakes_at_idle_and_no_deadline_stands() {
    let folder = test_folder("out-idle");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = watching(&folder.join("root"));
    h.open(&path);
    // The list's write and the views' first work end first, however busy the PC is.
    h.quiet();
    h.settle(Duration::from_millis(400));
    let before = h.wakes.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(h.wakes.load(Ordering::Relaxed), before, "no wake in 500 ms");
    assert_eq!(h.store.next_deadline(), None);
}

#[test]
fn our_own_save_is_not_seen_as_an_outside_change() {
    let folder = test_folder("out-own");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    for round in 0..5 {
        h.type_text(doc, "x");
        h.store.save(doc).unwrap();
        h.clear_events();
        h.until("the save ends", |h| !h.store.is_saving(doc));
        h.settle(Duration::from_millis(60));
        assert_eq!(
            h.count(|e| matches!(e, DocEvent::Reloaded(_))),
            0,
            "round {round}"
        );
        assert!(h.store.open_questions().is_empty(), "round {round}");
    }
    assert_eq!(h.text(doc), "xxxxxtext");
}

#[test]
fn another_file_in_the_folder_changes_nothing() {
    let folder = test_folder("out-other-file");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.undo(doc);
    pause();
    std::fs::write(folder.join("other.txt"), b"noise").unwrap();
    h.settle(Duration::from_millis(100));
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
    assert_eq!(h.text(doc), "text");
}

#[test]
fn the_watch_stops_with_the_last_document_in_a_folder() {
    let folder = test_folder("out-watch-stops");
    let middle = folder.join("middle");
    let path = middle.join("deep").join("f.txt");
    write(&path, b"text");
    let moved = folder.join("moved");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    // The watch may not have started yet. Then the rename works, and it goes back.
    h.until("the watch holds its folder", |_| {
        match std::fs::rename(&middle, &moved) {
            Err(_) => true,
            Ok(()) => {
                std::fs::rename(&moved, &middle).unwrap();
                false
            }
        }
    });
    h.store.request_close(doc);
    // Windows refuses to rename a folder above an open handle, so this shows the handle went.
    h.until("the watch lets the folder go", |_| {
        std::fs::rename(&middle, &moved).is_ok()
    });
    assert!(moved.join("deep").join("f.txt").exists());
}
