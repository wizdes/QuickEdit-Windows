//! The final text-safety review: a person's text is never lost with no question.
mod support;

use qe_api::{EncodingChoice, InputEvent, InputKind, Modifiers, TextEncoding, View, ViewContext};
use qe_docs::{Answer, DocErrorKind, DocEvent, DocId, QuestionKind};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;
use support::{Harness, autosave_files, config, set_read_only, test_folder, write};

fn quick(root: &Path) -> Harness {
    let (mut cfg, wakes) = config(root);
    cfg.autosave_delay = Duration::from_millis(5);
    Harness::with_config(cfg, wakes)
}

fn cp1252(h: &mut Harness, path: &Path) -> DocId {
    let doc = h.open_with(path, EncodingChoice::Specific(TextEncoding::Windows1252));
    h.caret_to_end(doc);
    doc
}

/// The look at the mark has reached the store, so the next edit asks.
fn mark_read_only(h: &mut Harness, path: &Path) {
    set_read_only(path, true);
    h.store.app_activated();
    h.settle(Duration::from_millis(100));
}

/// No other opener may read or write the file while this lives, so a Save fails for real.
fn hold_exclusively(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .unwrap()
}

fn backspace(h: &mut Harness, doc: DocId) {
    let mut ctx = ViewContext::default();
    h.store.view_mut(doc).unwrap().handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: 0x08,
                repeat: false,
                modifiers: Modifiers::default(),
            },
        },
        &mut ctx,
    );
    h.store.note_view_changed(doc);
}

// ---- Finding 1: Cancel on a first-edit question.

#[test]
fn f1_cancel_on_the_encoding_question_removes_only_the_asking_key() {
    let folder = test_folder("final-1-fit");
    let path = folder.join("f.txt");
    write(&path, b"caf\xe9\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = cp1252(&mut h, &path);
    h.type_text(doc, "first second third ");
    let before = h.text(doc);
    h.type_text(doc, "\u{2603}");
    let question = h.question("the encoding question");
    assert_eq!(
        question.kind,
        QuestionKind::EncodingNoLongerFits { on_edit: true }
    );
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), before, "the earlier typing stays");
    assert!(h.store.is_dirty(doc), "and it is still unsaved");
    assert!(h.store.open_questions().is_empty());
    // A plain Undo then takes the typing back as one step, to the file's own text.
    h.undo(doc);
    assert_eq!(h.text(doc), "caf\u{e9}\n");
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn f1_a_first_edit_with_no_earlier_typing_goes_back_to_clean() {
    let folder = test_folder("final-1-clean");
    let path = folder.join("f.txt");
    write(&path, b"caf\xe9\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = cp1252(&mut h, &path);
    h.type_text(doc, "\u{2603}");
    let question = h.question("the encoding question");
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), "caf\u{e9}\n");
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn f1_cancel_in_the_encoding_dialog_removes_only_the_asking_key() {
    let folder = test_folder("final-1-panel");
    let path = folder.join("f.txt");
    write(&path, b"caf\xe9\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = cp1252(&mut h, &path);
    h.type_text(doc, "first second ");
    let before = h.text(doc);
    h.type_text(doc, "\u{2603}");
    let question = h.question("the encoding question");
    h.store.answer(question.id, Answer::ChooseEncoding).unwrap();
    h.pump();
    h.store.panel_cancelled(doc);
    assert_eq!(h.text(doc), before);
    assert!(h.store.is_dirty(doc));
}

#[test]
fn f1_cancel_on_the_locked_question_removes_only_the_asking_key() {
    let folder = test_folder("final-1-locked");
    let path = folder.join("f.txt");
    write(&path, b"base\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.caret_to_end(doc);
    h.type_text(doc, "long typed sentence ");
    let before = h.text(doc);
    mark_read_only(&mut h, &path);
    h.type_text(doc, "x");
    let question = h.question("the locked question");
    assert_eq!(question.kind, QuestionKind::LockedFile { on_edit: true });
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), before);
    assert!(h.store.is_dirty(doc));
    // The next key asks again, and is taken back on its own too.
    h.type_text(doc, "y");
    let question = h.question("the locked question again");
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), before);
    set_read_only(&path, false);
}

#[test]
fn f1_cancel_on_the_locked_question_gives_back_a_backspaced_character() {
    let folder = test_folder("final-1-back");
    let path = folder.join("f.txt");
    write(&path, b"base\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.caret_to_end(doc);
    h.type_text(doc, "typed");
    let before = h.text(doc);
    mark_read_only(&mut h, &path);
    backspace(&mut h, doc);
    assert_eq!(h.text(doc), "base\ntype");
    let question = h.question("the locked question");
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), before);
    assert!(h.store.is_dirty(doc));
    set_read_only(&path, false);
}

#[test]
fn f1_duplicate_on_a_first_edit_question_keeps_the_earlier_typing_here() {
    let folder = test_folder("final-1-dup");
    let path = folder.join("f.txt");
    write(&path, b"base\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.caret_to_end(doc);
    h.type_text(doc, "typed ");
    let before = h.text(doc);
    mark_read_only(&mut h, &path);
    h.type_text(doc, "x");
    let question = h.question("the locked question");
    h.store.answer(question.id, Answer::Duplicate).unwrap();
    assert_eq!(h.text(doc), before);
    assert!(h.store.is_dirty(doc));
    set_read_only(&path, false);
}

// ---- Finding 3: a new file at a deleted file's name.

fn deleted(h: &mut Harness, doc: DocId, path: &Path) {
    std::fs::remove_file(path).unwrap();
    h.store.app_activated();
    h.until("the deletion is news", |h| {
        h.store.title_note(doc).as_deref() == Some("Deleted")
    });
    h.until("the copy is written", |h| {
        !autosave_files(&h.root).is_empty() && !h.store.is_autosaving(doc)
    });
}

#[test]
fn f3_a_different_file_at_a_deleted_name_keeps_the_text_and_the_copy_and_asks() {
    let folder = test_folder("final-3-differs");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"only here\n");
    let mut h = quick(&root);
    let doc = h.open(&path);
    deleted(&mut h, doc, &path);
    write(&path, b"another program's bytes\n");
    h.store.app_activated();
    let question = h.question("the changed-on-disk question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(h.text(doc), "only here\n");
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.store.title_note(doc), None, "the file is there again");
    assert!(!autosave_files(&root).is_empty(), "the copy stays");
    // Cancel keeps it all, and a close still asks.
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.settle(Duration::from_millis(100));
    assert_eq!(h.text(doc), "only here\n");
    assert!(!autosave_files(&root).is_empty());
    h.store.request_close(doc);
    let question = h.question("the close question");
    assert_eq!(question.kind, QuestionKind::SaveBeforeClose);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    // A Save asks too, and Save Anyway writes the kept text.
    h.store.save(doc).unwrap();
    let question = h.question("the save question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    h.store.answer(question.id, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"only here\n");
}

#[test]
fn f3_revert_on_that_question_takes_the_new_file() {
    let folder = test_folder("final-3-revert");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"only here\n");
    let mut h = quick(&root);
    let doc = h.open(&path);
    deleted(&mut h, doc, &path);
    write(&path, b"another\n");
    h.store.app_activated();
    let question = h.question("the changed-on-disk question");
    h.store.answer(question.id, Answer::Revert).unwrap();
    h.until("the new text", |h| h.text(doc) == "another\n");
    assert!(!h.store.is_dirty(doc));
    h.until("the copy goes", |h| autosave_files(&h.root).is_empty());
}

#[test]
fn f3_the_same_bytes_at_a_deleted_name_come_back_quietly() {
    let folder = test_folder("final-3-same");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"same text\n");
    let mut h = quick(&root);
    let doc = h.open(&path);
    deleted(&mut h, doc, &path);
    write(&path, b"same text\n");
    h.store.app_activated();
    h.until("the file is back and clean", |h| {
        h.store.title_note(doc).is_none() && !h.store.is_dirty(doc)
    });
    assert!(h.store.open_questions().is_empty());
    assert_eq!(h.text(doc), "same text\n");
    h.until("the copy goes", |h| autosave_files(&h.root).is_empty());
}

#[test]
fn f3_typing_during_the_look_at_the_new_file_stays_and_a_save_asks() {
    let folder = test_folder("final-3-typing");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"only here\n");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(5);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let doc = h.open(&path);
    deleted(&mut h, doc, &path);
    write(&path, b"another\n");
    faults.hold_reload.store(true, Ordering::Release);
    h.store.app_activated();
    h.until("the look is under way", |_| {
        faults.reload_is_held.load(Ordering::Acquire)
    });
    h.type_text(doc, "z");
    faults.hold_reload.store(false, Ordering::Release);
    h.until("the title follows", |h| h.store.title_note(doc).is_none());
    assert_eq!(h.text(doc), "zonly here\n");
    assert!(h.store.is_dirty(doc));
    h.settle(Duration::from_millis(50));
    for question in h.store.open_questions() {
        h.store.answer(question.id, Answer::Cancel).unwrap();
    }
    h.store.save(doc).unwrap();
    let question = h.question("the save question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    h.store.answer(question.id, Answer::Cancel).unwrap();
}

// ---- Finding 4: Rename and Move To take only names that stay as written.

#[test]
fn f4_rename_refuses_devices_and_names_windows_would_change() {
    let folder = test_folder("final-4-names");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    for bad in [
        "CON", "AUX", "NUL", "COM1.txt", "a. ", "sub..", "b.", "c ", ".", "..", "x<y", "x?", "a:b",
    ] {
        let error = h.store.rename(doc, bad).expect_err(bad);
        assert_eq!(error.kind, DocErrorKind::PathRefused, "{bad}: {error}");
    }
    h.settle(Duration::from_millis(50));
    assert_eq!(h.store.path(doc).unwrap(), path);
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
}

#[test]
fn f4_move_to_refuses_the_same_names() {
    let folder = test_folder("final-4-move");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    for bad in ["CON", "COM1.txt", "a. ", "sub..", "y?"] {
        let target = folder.join(bad);
        let error = h.store.move_to(doc, target, false).unwrap_err();
        assert_eq!(error.kind, DocErrorKind::PathRefused, "{bad}: {error}");
    }
    assert_eq!(h.store.path(doc).unwrap(), path);
}

#[test]
fn f4_an_ordinary_rename_still_works_and_the_name_is_the_files_name() {
    let folder = test_folder("final-4-good");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.store.rename(doc, "con2.notes.txt").unwrap();
    h.until("the move ends", |h| {
        h.count(|e| matches!(e, DocEvent::FileMoved { .. })) == 1
    });
    let real = folder.join("con2.notes.txt");
    assert_eq!(h.store.path(doc).unwrap(), real);
    assert_eq!(h.store.display_name(doc), "con2.notes.txt");
    assert!(real.exists());
}

// ---- Finding 5: a failed Save during a quit.

#[test]
fn f5_a_failed_save_stops_a_quit_and_the_error_shows() {
    let folder = test_folder("final-5-quit");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"base");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(5);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "typed ");
    faults.hold_save.store(true, Ordering::Release);
    h.store.save(doc).unwrap();
    h.until("the save waits", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    let held = hold_exclusively(&path);
    h.store.begin_quit();
    h.settle(Duration::from_millis(30));
    faults.hold_save.store(false, Ordering::Release);
    h.until("the failure", |h| {
        h.count(|e| matches!(e, DocEvent::SaveFailed { .. })) == 1
    });
    h.settle(Duration::from_millis(200));
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::ReadyToQuit)),
        0,
        "the person must see the failure first"
    );
    // The text is safe, and a new quit works, because a copy holds it.
    assert!(h.store.is_dirty(doc));
    drop(held);
    h.quit();
}

#[test]
fn f5_a_failed_save_at_session_end_keeps_the_copy_and_shows_at_the_next_launch() {
    let folder = test_folder("final-5-session");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"base");
    {
        let (mut cfg, wakes) = config(&root);
        cfg.autosave_delay = Duration::from_millis(5);
        let faults = cfg.faults.clone();
        let mut h = Harness::with_config(cfg, wakes);
        let doc = h.open(&path);
        h.type_text(doc, "typed ");
        faults.hold_save.store(true, Ordering::Release);
        h.store.save(doc).unwrap();
        h.until("the save waits", |_| {
            faults.save_is_held.load(Ordering::Acquire)
        });
        let held = hold_exclusively(&path);
        let release = faults.clone();
        let opener = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            release.hold_save.store(false, Ordering::Release);
        });
        assert!(h.store.wait_for_quit(Duration::from_secs(20)));
        opener.join().unwrap();
        drop(held);
    }
    let mut h = Harness::new(&root);
    h.until("the document is back", |h| {
        h.store
            .all_documents()
            .first()
            .is_some_and(|doc| !h.store.is_loading(*doc))
    });
    let doc = h.store.all_documents()[0];
    assert_eq!(h.text(doc), "typed base");
    let told: Vec<String> = h
        .events
        .iter()
        .filter_map(|e| match e {
            DocEvent::RestoreProblem(error) => Some(error.message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        told.iter()
            .any(|m| m.contains("f.txt") && m.contains("kept")),
        "{told:?}"
    );
    drop(h);
    // It shows once.
    let mut h = Harness::new(&root);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoreProblem(_))),
        0,
        "{:?}",
        h.events
    );
}

// ---- Finding 6: Move To and a file open in another tab.

#[test]
fn f6_move_to_refuses_a_target_open_in_another_tab() {
    let folder = test_folder("final-6");
    let a = folder.join("a.txt");
    let b = folder.join("sub").join("a.txt");
    write(&a, b"aaa");
    write(&b, b"bbb");
    let mut h = Harness::new(&folder.join("root"));
    let doc_a = h.open(&a);
    let doc_b = h.open(&b);
    let error = h.store.move_to(doc_b, a.clone(), true).unwrap_err();
    assert_eq!(error.kind, DocErrorKind::OpenInAnotherTab, "{error}");
    h.settle(Duration::from_millis(100));
    assert_eq!(
        std::fs::read(&a).unwrap(),
        b"aaa",
        "the target is untouched"
    );
    assert_eq!(std::fs::read(&b).unwrap(), b"bbb");
    assert_eq!(h.store.path(doc_b).unwrap(), b);
    assert_eq!(h.store.path(doc_a).unwrap(), a);
}

#[test]
fn f6_move_to_refuses_another_name_of_a_file_open_in_another_tab() {
    let folder = test_folder("final-6-link");
    let a = folder.join("a.txt");
    let link = folder.join("link.txt");
    let b = folder.join("b.txt");
    write(&a, b"aaa");
    std::fs::hard_link(&a, &link).unwrap();
    write(&b, b"bbb");
    let mut h = Harness::new(&folder.join("root"));
    h.open(&a);
    let doc_b = h.open(&b);
    h.store.move_to(doc_b, link.clone(), true).unwrap();
    h.until("the refusal", |h| {
        h.events.iter().any(|e| {
            matches!(e, DocEvent::MoveFailed { error, .. }
                if error.kind == DocErrorKind::OpenInAnotherTab)
        })
    });
    assert_eq!(std::fs::read(&a).unwrap(), b"aaa");
    assert_eq!(std::fs::read(&b).unwrap(), b"bbb");
    assert_eq!(h.store.path(doc_b).unwrap(), b);
}

// ---- Finding 8: a close question whose document became clean.

#[test]
fn f8_a_close_question_ends_itself_when_the_save_it_outlived_makes_the_text_safe() {
    let folder = test_folder("final-8");
    let path = folder.join("f.txt");
    write(&path, b"base");
    let (cfg, wakes) = config(&folder.join("root"));
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "typed ");
    faults.hold_save.store(true, Ordering::Release);
    h.store.save(doc).unwrap();
    h.until("the save waits", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.store.request_close(doc);
    assert_eq!(
        h.question("the close question").kind,
        QuestionKind::SaveBeforeClose
    );
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc);
    h.until("the tab closes", |h| h.store.view(doc).is_none());
    assert!(h.store.open_questions().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"typed base");
}

#[test]
fn f8_a_window_close_question_ends_the_same_way_and_other_tabs_still_ask() {
    let folder = test_folder("final-8-window");
    let a = folder.join("a.txt");
    let b = folder.join("b.txt");
    write(&a, b"aaa");
    write(&b, b"bbb");
    let (cfg, wakes) = config(&folder.join("root"));
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let doc_a = h.open(&a);
    let doc_b = h.open(&b);
    h.type_text(doc_a, "typed a ");
    h.type_text(doc_b, "typed b ");
    faults.hold_save.store(true, Ordering::Release);
    h.store.save(doc_a).unwrap();
    h.until("the save waits", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    let window = h.store.window_of(doc_a).unwrap();
    h.store.request_close_window(window);
    let question = h.question("the first close question");
    assert_eq!(question.doc, doc_a);
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc_a);
    h.until("the next tab asks", |h| {
        h.store
            .open_questions()
            .first()
            .is_some_and(|q| q.doc == doc_b)
    });
    assert!(
        h.store.view(doc_a).is_some(),
        "no tab closes before the last answer"
    );
    let question = h.store.open_questions().remove(0);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert!(h.store.view(doc_b).is_some());
    assert_eq!(h.text(doc_b), "typed b bbb");
}

// ---- Uncertain concerns.

#[test]
fn moving_a_file_stops_a_save_and_save_all_says_so() {
    let folder = test_folder("final-moving");
    let path = folder.join("f.txt");
    write(&path, b"base");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "typed ");
    h.store.move_to(doc, folder.join("g.txt"), false).unwrap();
    let error = h.store.save(doc).unwrap_err();
    assert_eq!(error.kind, DocErrorKind::Busy, "{error}");
    assert!(error.message.contains("moved"), "{}", error.message);
    let failed = h.store.save_all();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].0, doc);
    assert!(
        failed[0].1.message.contains("moved"),
        "{}",
        failed[0].1.message
    );
    h.until("the move ends", |h| {
        h.count(|e| matches!(e, DocEvent::FileMoved { .. })) == 1
    });
    assert_eq!(h.count(|e| matches!(e, DocEvent::SaveFinished { .. })), 0);
    assert!(h.store.is_dirty(doc), "the text still waits for a Save");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(folder.join("g.txt")).unwrap(), b"typed base");
}

#[test]
fn a_cancelled_copy_with_text_still_unsaved_leaves_the_timer_set() {
    let folder = test_folder("final-timer");
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(5);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let path = folder.join("f.txt");
    write(&path, b"base");
    let doc = h.open(&path);
    h.type_text(doc, "one");
    h.until("the first copy", |h| !autosave_files(&h.root).is_empty());
    h.until("it is written", |h| !h.store.is_autosaving(doc));
    // A second copy holds in its worker while the text goes clean, which cancels it.
    faults.hold_autosave.store(true, Ordering::Release);
    h.type_text(doc, "two");
    h.until("the second copy waits", |_| {
        faults.autosave_is_held.load(Ordering::Acquire)
    });
    h.undo(doc);
    assert!(!h.store.is_dirty(doc));
    h.settle(Duration::from_millis(30));
    // New text comes while that copy still runs. Its due time passes during the run.
    h.type_text(doc, "three");
    assert!(h.store.is_dirty(doc));
    h.settle(Duration::from_millis(30));
    faults.hold_autosave.store(false, Ordering::Release);
    // The cancelled copy ends. The new text must still reach a copy with no new edit.
    h.until("a copy of the new text", |h| {
        !h.store.is_autosaving(doc)
            && autosave_files(&h.root).iter().any(|name| {
                std::fs::read(h.root.join("session").join("docs").join(name))
                    .is_ok_and(|bytes| bytes.windows(5).any(|w| w == b"three"))
            })
    });
}
