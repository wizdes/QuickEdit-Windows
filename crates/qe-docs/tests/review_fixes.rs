//! One test or more for each finding of the review of this crate at 5f4c2ad. Each name says
//! what must hold. They run in real event order, with real files and real sharing rules.
mod support;

use qe_api::{EncodingChoice, TextEncoding};
use qe_docs::{
    Answer, DocErrorKind, DocEvent, DocId, DocumentStore, Faults, OpenTarget, QuestionKind,
};
use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};
use support::*;

const SHARE_READ: u32 = 1;

fn quick(root: &Path) -> Harness {
    let (mut cfg, wakes) = config(root);
    cfg.autosave_delay = Duration::from_millis(5);
    Harness::with_config(cfg, wakes)
}

fn with_faults(root: &Path, delay: Duration) -> (Harness, Arc<Faults>) {
    let (mut cfg, wakes) = config(root);
    cfg.autosave_delay = delay;
    let faults = cfg.faults.clone();
    (Harness::with_config(cfg, wakes), faults)
}

fn watching(root: &Path) -> Harness {
    let (mut cfg, wakes) = config(root);
    cfg.watch_folders = true;
    Harness::with_config(cfg, wakes)
}

fn all_loaded(h: &mut Harness) {
    h.until("every tab has its text", |h| {
        h.store
            .all_documents()
            .iter()
            .all(|d| !h.store.is_loading(*d))
    });
}

fn only_doc(h: &Harness) -> DocId {
    let all = h.store.all_documents();
    assert_eq!(all.len(), 1, "{all:?}");
    all[0]
}

fn pause() {
    std::thread::sleep(Duration::from_millis(40));
}

fn wait_for_copy(h: &mut Harness) {
    let root = h.root.clone();
    h.until("an autosaved copy appears", |_| {
        autosave_files(&root).iter().any(|n| n.ends_with(".qea"))
    });
}

fn qea_names(root: &Path) -> Vec<String> {
    autosave_files(root)
        .into_iter()
        .filter(|n| n.ends_with(".qea"))
        .collect()
}

fn ready(h: &Harness) -> usize {
    h.count(|e| matches!(e, DocEvent::ReadyToQuit))
}

/// A file another program holds open with no delete and no write sharing.
fn hold(path: &Path) -> std::fs::File {
    OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .open(path)
        .unwrap()
}

// ---- 1: a file coming back.

#[test]
fn f01_a_file_that_comes_back_keeps_the_restored_text_and_its_copy() {
    let folder = test_folder("rf-01");
    let root = folder.join("root");
    let path = folder.join("x.txt");
    write(&path, b"on disk\n");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "unsaved edits ");
        h.quit();
    }
    std::fs::remove_file(&path).unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = only_doc(&h);
    assert_eq!(h.store.title_note(doc).as_deref(), Some("Deleted"));
    assert!(h.store.is_dirty(doc));
    write(&path, b"on disk\n");
    h.store.app_activated();
    h.until("the title follows", |h| h.store.title_note(doc).is_none());
    h.settle(Duration::from_millis(300));
    assert_eq!(h.text(doc), "unsaved edits on disk\n");
    assert!(h.store.is_dirty(doc), "disk does not hold this text");
    assert_eq!(qea_names(&root).len(), 1, "the copy stays");
    h.store.save(doc).unwrap();
    let question = h.question("a Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"on disk\n");
}

#[test]
fn f01_a_clean_document_whose_file_comes_back_is_clean_and_keeps_no_copy() {
    let folder = test_folder("rf-01b");
    let root = folder.join("root");
    let path = folder.join("x.txt");
    write(&path, b"text");
    let mut h = quick(&root);
    let doc = h.open(&path);
    std::fs::remove_file(&path).unwrap();
    h.store.app_activated();
    h.until("the deletion is news", |h| {
        h.store.title_note(doc).as_deref() == Some("Deleted")
    });
    wait_for_copy(&mut h);
    write(&path, b"text");
    h.store.app_activated();
    h.until("the file is back and clean", |h| {
        h.store.title_note(doc).is_none() && !h.store.is_dirty(doc)
    });
    h.until("the copy goes", |h| qea_names(&h.root).is_empty());
}

// ---- 2: a close during a restore.

fn left_one_copy(root: &Path, text: &str) {
    let mut h = quick(root);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, text);
    h.quit();
}

#[test]
fn f02_closing_a_tab_while_it_restores_asks_and_the_copy_stays() {
    let folder = test_folder("rf-02");
    let root = folder.join("root");
    left_one_copy(&root, "precious");
    let (cfg, wakes) = config(&root);
    cfg.faults.hold_restore_load.store(true, Ordering::Release);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    h.until("the read holds", |_| {
        faults.restore_load_is_held.load(Ordering::Acquire)
    });
    let doc = only_doc(&h);
    assert!(h.store.is_loading(doc));
    assert!(h.store.is_dirty(doc), "the copy holds text not on disk");
    let window = h.store.window_of(doc).unwrap();
    h.store.request_close_window(window);
    let question = h.question("the close question");
    assert_eq!(question.kind, QuestionKind::SaveBeforeClose);
    assert!(h.store.view(doc).is_some());
    assert_eq!(qea_names(&root).len(), 1);
    // A Save cannot run yet. The question stays, and nothing is lost.
    let busy = h.store.answer(question.id, Answer::Save).unwrap_err();
    assert_eq!(busy.kind, DocErrorKind::Busy);
    assert_eq!(h.store.open_questions().len(), 1);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    faults.hold_restore_load.store(false, Ordering::Release);
    h.loaded(doc);
    assert_eq!(h.text(doc), "precious");
    assert!(h.store.is_dirty(doc));
    // Don't Save is the person's choice, and only then does the copy go.
    h.store.request_close(doc);
    let question = h.question("the close question again");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    h.until("the copy goes", |_| qea_names(&root).is_empty());
}

// ---- 3: the quit review.

fn failing_quit(root: &Path) -> (Harness, Arc<Faults>) {
    let (h, faults) = with_faults(root, Duration::from_secs(600));
    faults.autosave_error_code.store(112, Ordering::Relaxed);
    (h, faults)
}

fn answer_save_at_quit(h: &mut Harness) {
    h.store.begin_quit();
    h.until("the quit question", |h| {
        h.store
            .open_questions()
            .iter()
            .any(|q| q.kind == QuestionKind::AutosaveFailedAtQuit)
    });
    let question = h
        .store
        .open_questions()
        .into_iter()
        .find(|q| q.kind == QuestionKind::AutosaveFailedAtQuit)
        .unwrap();
    h.store.answer(question.id, Answer::Save).unwrap();
}

fn until_ready(h: &mut Harness) {
    h.until("the quit ends", |h| ready(h) > 0);
}

#[test]
fn f03_an_untitled_document_answered_save_waits_for_the_dialog_and_the_write() {
    let folder = test_folder("rf-03a");
    let (mut h, faults) = failing_quit(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "words");
    answer_save_at_quit(&mut h);
    h.settle(Duration::from_millis(50));
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::NeedsSavePanel(_))),
        1,
        "{:?}",
        h.events
    );
    assert_eq!(ready(&h), 0, "the dialog is still up");
    faults.hold_save.store(true, Ordering::Release);
    let target = folder.join("named.txt");
    h.store.save_as(doc, target.clone(), None).unwrap();
    h.until("the write holds", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.settle(Duration::from_millis(30));
    assert_eq!(ready(&h), 0, "the Save has not finished");
    faults.hold_save.store(false, Ordering::Release);
    until_ready(&mut h);
    assert_eq!(std::fs::read(&target).unwrap(), b"words");
}

#[test]
fn f03_a_cancelled_dialog_cancels_the_quit_with_the_text_in_place() {
    let folder = test_folder("rf-03a2");
    let (mut h, _faults) = failing_quit(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "words");
    answer_save_at_quit(&mut h);
    h.until("the dialog", |h| {
        h.count(|e| matches!(e, DocEvent::NeedsSavePanel(_))) == 1
    });
    h.store.panel_cancelled(doc);
    h.settle(Duration::from_millis(30));
    assert_eq!(ready(&h), 0);
    assert_eq!(h.text(doc), "words");
    assert!(h.store.open_questions().is_empty());
}

#[test]
fn f03_a_changed_file_asks_and_the_quit_waits_for_the_answer() {
    let folder = test_folder("rf-03b");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let (mut h, _faults) = failing_quit(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "mine ");
    pause();
    std::fs::write(&path, b"theirs").unwrap();
    answer_save_at_quit(&mut h);
    let question = h.question("the conflict question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    h.settle(Duration::from_millis(50));
    assert_eq!(ready(&h), 0, "{:?}", h.events);
    assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
    h.store.answer(question.id, Answer::SaveAnyway).unwrap();
    until_ready(&mut h);
    assert_eq!(std::fs::read(&path).unwrap(), b"mine before");
}

#[test]
fn f03_a_locked_file_asks_and_the_quit_waits_through_the_unlock() {
    let folder = test_folder("rf-03c");
    let path = folder.join("f.txt");
    write(&path, b"keep");
    let (mut h, _faults) = failing_quit(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    set_read_only(&path, true);
    answer_save_at_quit(&mut h);
    let question = h.question("the locked question");
    assert_eq!(question.kind, QuestionKind::LockedFile { on_edit: false });
    h.settle(Duration::from_millis(50));
    assert_eq!(ready(&h), 0);
    h.store.answer(question.id, Answer::Unlock).unwrap();
    until_ready(&mut h);
    assert_eq!(std::fs::read(&path).unwrap(), b"xkeep");
}

#[test]
fn f03_a_lossy_file_asks_and_the_quit_waits_for_the_answer() {
    let folder = test_folder("rf-03d");
    let path = folder.join("f.txt");
    let lossy = vec![0xFF, 0xFE, b'h', 0, b'i', 0, 0x41];
    write(&path, &lossy);
    let (mut h, _faults) = failing_quit(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    // The first-edit question is a plain one, and it is still open at the quit.
    let first = h.question("the first-edit question");
    assert_eq!(first.kind, QuestionKind::LossyFirstEdit);
    answer_save_at_quit(&mut h);
    h.until("the lossy question", |h| {
        h.store
            .open_questions()
            .iter()
            .any(|q| q.kind == QuestionKind::LossyOverwrite)
    });
    h.settle(Duration::from_millis(50));
    assert_eq!(ready(&h), 0);
    assert_eq!(std::fs::read(&path).unwrap(), lossy);
    let question = h
        .store
        .open_questions()
        .into_iter()
        .find(|q| q.kind == QuestionKind::LossyOverwrite)
        .unwrap();
    h.store.answer(question.id, Answer::Overwrite).unwrap();
    until_ready(&mut h);
    assert_ne!(std::fs::read(&path).unwrap(), lossy);
}

#[test]
fn f03_a_failed_save_brings_the_question_back_and_dont_save_ends_it() {
    let folder = test_folder("rf-03e");
    let path = folder.join("f.txt");
    write(&path, b"before");
    let (mut h, faults) = failing_quit(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "mine ");
    faults.save_error_code.store(112, Ordering::Relaxed);
    answer_save_at_quit(&mut h);
    h.until("the failure is named", |h| {
        h.count(|e| matches!(e, DocEvent::SaveFailed { .. })) == 1
    });
    let question = h.question("the question comes back");
    assert_eq!(question.kind, QuestionKind::AutosaveFailedAtQuit);
    h.settle(Duration::from_millis(30));
    assert_eq!(ready(&h), 0);
    assert_eq!(h.store.open_questions().len(), 1, "asked once");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    until_ready(&mut h);
    assert_eq!(std::fs::read(&path).unwrap(), b"before");
}

#[test]
fn f03_a_cancelled_quit_leaves_no_skip_behind_for_the_next_quit() {
    let folder = test_folder("rf-03f");
    let (mut h, _faults) = failing_quit(&folder.join("root"));
    let (_, a) = h.store.new_window(false).unwrap();
    h.type_text(a, "first");
    let (_, b) = h.store.new_window(false).unwrap();
    h.type_text(b, "second");
    h.store.begin_quit();
    let question = h.question("the first review");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    h.until("the second review", |h| h.store.open_questions().len() == 1);
    let question = h.question("the second review");
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.settle(Duration::from_millis(30));
    assert_eq!(ready(&h), 0);
    assert!(h.store.open_questions().is_empty());
    h.store.begin_quit();
    let question = h.question("the first document is asked again");
    assert_eq!(question.kind, QuestionKind::AutosaveFailedAtQuit);
    h.settle(Duration::from_millis(20));
    assert_eq!(ready(&h), 0);
}

// ---- 4: the warning about a newer file.

#[test]
fn f04_the_newer_file_warning_lasts_through_more_restores_and_save_still_asks() {
    let folder = test_folder("rf-04");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"old\n");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "mine ");
        h.quit();
    }
    pause();
    std::fs::write(&path, b"outside text\n").unwrap();
    for run in 2..=4 {
        let mut h = quick(&root);
        all_loaded(&mut h);
        h.pump();
        assert_eq!(
            h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
            1,
            "run {run}"
        );
        h.settle(Duration::from_millis(30));
        h.quit();
    }
    // An edit writes a new copy with the new base, and the warning still stands.
    {
        let mut h = quick(&root);
        all_loaded(&mut h);
        let doc = only_doc(&h);
        h.type_text(doc, "more ");
        h.settle(Duration::from_millis(30));
        h.quit();
    }
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
        1
    );
    let doc = only_doc(&h);
    h.store.save(doc).unwrap();
    let question = h.question("Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"outside text\n");
    h.store.answer(question.id, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    h.settle(Duration::from_millis(30));
    h.quit();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
        0,
        "the answer ended it"
    );
}

#[test]
fn f04_a_change_seen_while_dirty_is_still_known_after_a_restart() {
    let folder = test_folder("rf-04b");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"old\n");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "a");
        wait_for_copy(&mut h);
        pause();
        std::fs::write(&path, b"outside\n").unwrap();
        h.store.app_activated();
        h.settle(Duration::from_millis(50));
        // This copy is written after the change was seen.
        h.type_text(doc, "b");
        h.settle(Duration::from_millis(50));
        h.quit();
    }
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = only_doc(&h);
    h.store.save(doc).unwrap();
    let question = h.question("Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"outside\n");
}

// ---- 5: a stale copy.

/// Saves with the copy held, so its removal fails. Gives the session list from before the Save.
fn save_with_copy_held(root: &Path, path: &Path) -> Vec<u8> {
    let mut h = quick(root);
    let doc = h.open(path);
    h.type_text(doc, "A");
    wait_for_copy(&mut h);
    h.quiet();
    h.settle(Duration::from_millis(20));
    let list = std::fs::read(root.join("session").join("session.txt")).unwrap();
    let copy = root.join("session").join("docs").join(&qea_names(root)[0]);
    let held = hold(&copy);
    h.store.save(doc).unwrap();
    h.saved(doc);
    h.until("the failed removal is named", |h| {
        h.count(|e| matches!(e, DocEvent::AutosaveFailed { .. })) == 1
    });
    assert!(copy.exists());
    h.quit();
    drop(held);
    list
}

#[test]
fn f05_a_copy_whose_removal_failed_never_comes_back_over_saved_text() {
    let folder = test_folder("rf-05");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"base\n");
    save_with_copy_held(&root, &path);
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = only_doc(&h);
    assert_eq!(h.text(doc), "Abase\n");
    assert!(!h.store.is_dirty(doc), "the list says the copy is dead");
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
        0
    );
    assert!(
        qea_names(&root).is_empty(),
        "the next launch removes it: {:?}",
        qea_names(&root)
    );
}

#[test]
fn f05_a_stale_copy_with_an_old_list_warns_and_save_asks() {
    let folder = test_folder("rf-05b");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"base\n");
    let old_list = save_with_copy_held(&root, &path);
    // The list write failed, or a kill came first.
    std::fs::write(root.join("session").join("session.txt"), old_list).unwrap();
    pause();
    std::fs::write(&path, b"ABbase\n").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
        1,
        "{:?}",
        h.events
    );
    let doc = only_doc(&h);
    h.store.save(doc).unwrap();
    let question = h.question("Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"ABbase\n");
}

#[test]
fn f05_a_list_with_the_new_base_and_no_mark_still_warns_by_the_base_in_the_copy() {
    let folder = test_folder("rf-05d");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"base\n");
    save_with_copy_held(&root, &path);
    // The row has the base from after the Save. Only the copy knows it is older than that.
    let list = root.join("session").join("session.txt");
    let text = std::fs::read_to_string(&list).unwrap();
    assert!(text.contains("dead key="), "{text}");
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("dead ")).collect();
    std::fs::write(&list, kept.join("\n") + "\n").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
        1,
        "{:?}",
        h.events
    );
    let doc = only_doc(&h);
    h.store.save(doc).unwrap();
    let question = h.question("Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
}

#[test]
fn f04_a_list_written_during_the_load_keeps_the_warning() {
    let folder = test_folder("rf-04c");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"old\n");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "mine ");
        h.quit();
    }
    pause();
    std::fs::write(&path, b"outside\n").unwrap();
    {
        let mut h = quick(&root);
        all_loaded(&mut h);
        h.settle(Duration::from_millis(30));
        h.quit();
    }
    let list = root.join("session").join("session.txt");
    let (cfg, wakes) = config(&root);
    cfg.faults.hold_restore_load.store(true, Ordering::Release);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    h.until("the read holds", |_| {
        faults.restore_load_is_held.load(Ordering::Acquire)
    });
    h.settle(Duration::from_millis(50));
    let text = std::fs::read_to_string(&list).unwrap();
    assert!(text.contains("away=1"), "{text}");
    faults.hold_restore_load.store(false, Ordering::Release);
    all_loaded(&mut h);
}

#[test]
fn f05_an_unreadable_copy_at_restore_stays_live_for_the_next_launch() {
    let folder = test_folder("rf-05c");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "kept ");
        h.quit();
    }
    let copy = root.join("session").join("docs").join(&qea_names(&root)[0]);
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&copy)
        .unwrap();
    {
        let mut h = quick(&root);
        all_loaded(&mut h);
        h.pump();
        assert!(h.count(|e| matches!(e, DocEvent::RestoreProblem(_))) >= 1);
        let doc = only_doc(&h);
        assert_eq!(h.text(doc), "disk", "the file is the best that is left");
        assert!(!h.store.is_dirty(doc));
        h.type_text(doc, "z");
        // A new copy of its own, so that the Save below has copies of its own to remove.
        h.until("this document's own copy", |h| {
            qea_names(&h.root).len() == 2
        });
        h.store.save(doc).unwrap();
        h.saved(doc);
        h.settle(Duration::from_millis(30));
        assert!(copy.exists(), "this Save says nothing about that copy");
        h.quit();
    }
    drop(locked);
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let texts: Vec<String> = h.store.all_documents().iter().map(|d| h.text(*d)).collect();
    assert!(texts.iter().any(|t| t == "kept disk"), "{texts:?}");
}

// ---- 6: a merge of sessions.

/// Two copies ended, each with an untitled document. Gives the folder of the second.
fn two_sessions(root: &Path) -> PathBuf {
    let mut first = Harness::new(root);
    let (_, a) = first.store.new_window(false).unwrap();
    first.type_text(a, "from the first");
    let mut second = Harness::new(root);
    let (_, b) = second.store.new_window(false).unwrap();
    second.type_text(b, "from the second");
    first.quit();
    second.quit();
    root.join("session-2")
}

fn files_in(folder: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect()
}

#[test]
fn f06_a_held_file_stays_in_its_folder_and_the_next_launch_merges_it() {
    let folder = test_folder("rf-06");
    let root = folder.join("root");
    let other = two_sessions(&root);
    let copy = files_in(&other.join("docs"))
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "qea"))
        .unwrap();
    let held = hold(&copy);
    {
        let mut h = Harness::new(&root);
        all_loaded(&mut h);
        h.pump();
        assert!(copy.exists(), "the copy is not removed");
        let problems: Vec<_> = h
            .events
            .iter()
            .filter_map(|e| match e {
                DocEvent::RestoreProblem(error) => Some(error.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].kind, DocErrorKind::FileInUse);
        assert!(
            !problems[0].message.contains("missing"),
            "{}",
            problems[0].message
        );
        assert_eq!(problems[0].left_on_disk, vec![copy.clone()]);
        let texts: Vec<String> = h.store.all_documents().iter().map(|d| h.text(*d)).collect();
        assert_eq!(texts, ["from the first"]);
        h.quit();
    }
    drop(held);
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let mut texts: Vec<String> = h.store.all_documents().iter().map(|d| h.text(*d)).collect();
    texts.sort();
    assert_eq!(texts, ["from the first", "from the second"]);
    assert!(!other.exists(), "now the folder goes");
}

#[test]
fn f06_damaged_files_and_a_broken_list_in_a_merged_folder_stay_on_disk() {
    let folder = test_folder("rf-06b");
    let root = folder.join("root");
    let other = two_sessions(&root);
    let damaged = other.join("docs").join("abc.3.damaged");
    std::fs::write(&damaged, b"evidence").unwrap();
    std::fs::write(other.join("session.txt"), b"not a list").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert!(!other.exists());
    let moved = root.join("session").join("docs").join("abc.3.damaged");
    assert_eq!(std::fs::read(&moved).unwrap(), b"evidence");
    let told: Vec<PathBuf> = h
        .events
        .iter()
        .filter_map(|e| match e {
            DocEvent::RestoreProblem(error) if error.kind == DocErrorKind::SessionDamaged => {
                Some(error.left_on_disk.clone())
            }
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(told.len(), 1, "{:?}", h.events);
    assert_eq!(
        std::fs::read(&told[0]).unwrap(),
        b"not a list",
        "still true"
    );
}

#[test]
fn concern_a_kill_between_the_list_and_the_removal_merges_nothing_twice() {
    let folder = test_folder("rf-06c");
    let root = folder.join("root");
    let other = two_sessions(&root);
    let their_list = std::fs::read(other.join("session.txt")).unwrap();
    {
        let mut h = Harness::new(&root);
        all_loaded(&mut h);
        assert_eq!(h.store.all_documents().len(), 2);
        h.quit();
    }
    // As a kill would leave it: the list has its rows, and the old folder still has them too.
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("session.lock"), b"").unwrap();
    std::fs::write(other.join("session.txt"), their_list).unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(h.store.all_documents().len(), 2, "{:?}", h.events);
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoreProblem(_))),
        0,
        "{:?}",
        h.events
    );
    assert!(!other.exists());
}

// ---- 7: a failed reload.

#[test]
fn f07_a_read_that_fails_leaves_the_stamp_so_a_later_save_asks() {
    let folder = test_folder("rf-07");
    let path = folder.join("f.txt");
    write(&path, b"old\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    pause();
    let mut writer = OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut writer, b"outside program text").unwrap();
    writer
        .set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    h.store.app_activated();
    h.until("the failure is named", |h| {
        h.count(|e| matches!(e, DocEvent::ReloadFailed { .. })) == 1
    });
    drop(writer);
    assert_eq!(h.text(doc), "old\n");
    assert!(!h.store.is_dirty(doc));
    h.type_text(doc, "z");
    h.store.save(doc).unwrap();
    let question = h.question("Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"outside program text");
}

// ---- 8: the end of a Windows session.

#[test]
fn f08_wait_for_quit_waits_for_running_writes_with_an_ordinary_question_open() {
    let folder = test_folder("rf-08");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    let (mut h, faults) = with_faults(&root, Duration::from_secs(600));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.store.request_revert(doc).unwrap();
    h.question("an ordinary question");
    faults.hold_autosave.store(true, Ordering::Release);
    let release = faults.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        release.hold_autosave.store(false, Ordering::Release);
    });
    let began = Instant::now();
    assert!(h.store.wait_for_quit(WAIT), "the copy is on disk");
    assert!(began.elapsed() >= Duration::from_millis(250));
    releaser.join().unwrap();
    assert_eq!(qea_names(&root).len(), 1);
}

#[test]
fn f08_wait_for_quit_stops_for_a_review_question_but_only_after_the_writes() {
    let folder = test_folder("rf-08b");
    let root = folder.join("root");
    let (mut h, faults) = with_faults(&root, Duration::from_secs(600));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "cannot be kept");
    faults.autosave_error_code.store(112, Ordering::Relaxed);
    let began = Instant::now();
    assert!(!h.store.wait_for_quit(WAIT), "nobody can answer now");
    assert!(began.elapsed() < WAIT / 2, "it does not sit out the limit");
    assert_eq!(h.store.open_questions().len(), 1);
}

// ---- 9: typing during a revert.

#[test]
fn f09_edits_are_refused_during_a_reverts_read() {
    let folder = test_folder("rf-09");
    let path = folder.join("f.txt");
    write(&path, b"disk\n");
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_secs(600));
    let doc = h.open(&path);
    h.type_text(doc, "first ");
    h.store.request_revert(doc).unwrap();
    let question = h.question("the revert question");
    faults.hold_reload.store(true, Ordering::Release);
    h.store.answer(question.id, Answer::Revert).unwrap();
    h.until("the read holds", |_| {
        faults.reload_is_held.load(Ordering::Acquire)
    });
    h.type_text(doc, "typed after the answer ");
    assert_eq!(
        h.text(doc),
        "first disk\n",
        "the view refuses, as the Mac refuses during a reload"
    );
    faults.hold_reload.store(false, Ordering::Release);
    h.until("the revert ends", |h| !h.store.is_dirty(doc));
    assert_eq!(h.text(doc), "disk\n");
    h.type_text(doc, "z");
    assert_eq!(h.text(doc), "zdisk\n", "the view takes edits again");
}

#[test]
fn concern_a_reload_that_waited_for_a_cut_does_not_replace_later_typing() {
    let folder = test_folder("rf-09b");
    let path = folder.join("f.txt");
    let body = "line of text\n".repeat(200_000);
    write(&path, body.as_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    let mut ctx = qe_api::ViewContext::default();
    let view = h.store.view_mut(doc).unwrap();
    view.run(qe_editor::EditorCommand::SelectAll, &mut ctx);
    view.run(qe_editor::EditorCommand::Cut, &mut ctx);
    assert!(view.cut_is_running());
    h.store.note_view_changed(doc);
    h.undo(doc);
    assert!(!h.store.is_dirty(doc));
    pause();
    std::fs::write(&path, b"outside").unwrap();
    h.store.app_activated();
    // The read ends, and the text waits for the Cut.
    h.settle(Duration::from_millis(400));
    assert_eq!(h.text(doc), body);
    h.type_text(doc, "x");
    assert!(h.store.is_dirty(doc));
    h.until("the Cut ends", |h| {
        let view = h.store.view_mut(doc).unwrap();
        view.poll_layout(&mut qe_api::ViewContext::default());
        !view.cut_is_running()
    });
    h.settle(Duration::from_millis(100));
    assert!(h.text(doc).len() == body.len() + 1, "the typing stays");
    assert!(h.store.is_dirty(doc));
}

// ---- 10: a deleted file too large to edit.

fn low_limit(root: &Path) -> Harness {
    let (mut cfg, wakes) = config(root);
    cfg.read_only_bytes = 1_000;
    Harness::with_config(cfg, wakes)
}

fn deleted_while_open(h: &mut Harness, doc: DocId, path: &Path) {
    h.until("the index ends", |h| {
        h.store.view(doc).unwrap().store_status().complete
    });
    std::fs::remove_file(path).unwrap();
    h.store.app_activated();
    h.until("the deletion is news", |h| {
        h.store.title_note(doc).as_deref() == Some("Deleted")
    });
    assert!(h.store.is_dirty(doc));
}

/// The map shows the file's own bytes, and it refuses them once the file has gone. So the text
/// cannot be written. What matters is that it says so and does not close as if it had.
#[test]
fn f10_save_before_close_on_a_deleted_mapped_file_says_it_cannot_and_stays_open() {
    let folder = test_folder("rf-10");
    let path = folder.join("huge.txt");
    write(&path, "line of text\n".repeat(300).as_bytes());
    let mut h = low_limit(&folder.join("root"));
    let doc = h.open(&path);
    deleted_while_open(&mut h, doc, &path);
    assert!(
        !h.store.commands(doc).save,
        "the row is off, as it cannot work"
    );
    h.store.request_close(doc);
    let question = h.question("the close question");
    assert_eq!(question.kind, QuestionKind::SaveBeforeClose);
    h.store.answer(question.id, Answer::Save).unwrap();
    h.until("the failure is named", |h| {
        h.count(|e| {
            matches!(
                e,
                DocEvent::SaveFailed { error, .. } if error.kind == DocErrorKind::FileNotFound
            )
        }) == 1
    });
    h.settle(Duration::from_millis(30));
    assert!(
        h.store.view(doc).is_some(),
        "it does not close on a failure"
    );
    assert!(!path.exists());
    assert!(h.store.is_dirty(doc));
    // Don't Save stays open to the person.
    h.store.request_close(doc);
    let question = h.question("the close question again");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    assert!(h.store.view(doc).is_none());
}

#[test]
fn f10_a_deleted_large_file_read_through_a_copy_is_written_again_by_a_save() {
    let folder = test_folder("rf-10b");
    let path = folder.join("wide.txt");
    let bytes = utf16_large(&path);
    let mut h = low_limit(&folder.join("root"));
    let doc = h.open(&path);
    deleted_while_open(&mut h, doc, &path);
    assert!(h.store.commands(doc).save, "a Save can write it again");
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::Save).unwrap();
    h.until("the document closes", |h| h.store.view(doc).is_none());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn f10_a_plain_save_of_such_a_file_reads_the_new_file_after_it() {
    let folder = test_folder("rf-10c");
    let path = folder.join("wide.txt");
    let bytes = utf16_large(&path);
    let mut h = low_limit(&folder.join("root"));
    let doc = h.open(&path);
    deleted_while_open(&mut h, doc, &path);
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!h.store.is_dirty(doc));
    h.until("the view reads the new file", |h| {
        h.store.commands(doc).save_as
    });
    assert_eq!(
        h.store.title_note(doc).as_deref(),
        Some("Read only: file too large to edit (0 MB)")
    );
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
}

// ---- 11: a save by backup rename.

fn replaced(backup: &str, rename_first: bool, dirty: bool) {
    replaced_after(backup, rename_first, dirty, Duration::ZERO);
}

fn replaced_after(backup: &str, rename_first: bool, dirty: bool, gap: Duration) {
    let name = format!(
        "rf-11-{}-{rename_first}-{dirty}-{}",
        backup.replace('~', "tilde"),
        gap.as_millis()
    );
    let folder = test_folder(&name);
    let path = folder.join("x.txt");
    write(&path, b"old\n");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    if dirty {
        h.type_text(doc, "mine ");
    }
    pause();
    let saved_aside = folder.join(backup);
    if rename_first {
        std::fs::rename(&path, &saved_aside).unwrap();
        // The editor takes a moment to write, and the check runs in that moment.
        h.settle(gap);
        std::fs::write(&path, b"new text\n").unwrap();
    } else {
        let temp = folder.join("x.tmp");
        std::fs::write(&temp, b"new text\n").unwrap();
        std::fs::rename(&path, &saved_aside).unwrap();
        std::fs::rename(&temp, &path).unwrap();
    }
    if dirty {
        h.settle(Duration::from_millis(800));
        assert_eq!(h.text(doc), "mine old\n");
    } else {
        h.until("the new text arrives", |h| {
            h.count(|e| matches!(e, DocEvent::Reloaded(d) if *d == doc)) > 0
        });
        assert_eq!(h.text(doc), "new text\n");
        assert!(!h.store.is_dirty(doc));
    }
    h.settle(Duration::from_millis(500));
    assert_eq!(h.store.path(doc).unwrap(), path, "it stays at its path");
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileMoved { .. })), 0);
    // The backup goes. That is no news about this document.
    std::fs::remove_file(&saved_aside).unwrap();
    h.settle(Duration::from_millis(500));
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileDeleted { .. })), 0);
    assert_eq!(h.store.title_note(doc), None);
    if dirty {
        h.store.save(doc).unwrap();
        let question = h.question("the Save asks");
        assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
        assert_eq!(std::fs::read(&path).unwrap(), b"new text\n");
    }
}

#[test]
fn f11_rename_then_write_tilde_clean() {
    replaced("x.txt~", true, false);
}
#[test]
fn f11_rename_then_write_bak_clean() {
    replaced("x.bak", true, false);
}
#[test]
fn f11_write_then_renames_tilde_clean() {
    replaced("x.txt~", false, false);
}
#[test]
fn f11_write_then_renames_bak_clean() {
    replaced("x.bak", false, false);
}
#[test]
fn f11_rename_then_write_tilde_dirty() {
    replaced("x.txt~", true, true);
}
#[test]
fn f11_write_then_renames_bak_dirty() {
    replaced("x.bak", false, true);
}

#[test]
fn f11_a_new_file_a_moment_after_the_rename_tilde_clean() {
    replaced_after("x.txt~", true, false, Duration::from_millis(100));
}
#[test]
fn f11_a_new_file_a_moment_after_the_rename_bak_clean() {
    replaced_after("x.bak", true, false, Duration::from_millis(100));
}
#[test]
fn f11_a_new_file_a_moment_after_the_rename_tilde_dirty() {
    replaced_after("x.txt~", true, true, Duration::from_millis(100));
}

#[test]
fn f11_a_real_rename_to_a_backup_name_still_follows() {
    let folder = test_folder("rf-11-real");
    let path = folder.join("x.txt");
    write(&path, b"text");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "m");
    pause();
    let moved = folder.join("x.bak");
    std::fs::rename(&path, &moved).unwrap();
    h.until("it follows", |h| {
        h.store.path(doc).as_deref() == Some(moved.as_path())
    });
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.text(doc), "mtext");
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileDeleted { .. })), 0);
}

// ---- 12: hard links.

fn link_deleted(link_first: bool) {
    let folder = test_folder(&format!("rf-12-{link_first}"));
    let (x, y) = (folder.join("x.txt"), folder.join("y.txt"));
    write(&x, b"text\n");
    if link_first {
        std::fs::hard_link(&x, &y).unwrap();
    }
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&x);
    if !link_first {
        std::fs::hard_link(&x, &y).unwrap();
    }
    // The folder's news about the new name refreshes what the document knows.
    h.settle(Duration::from_millis(150));
    std::fs::remove_file(&x).unwrap();
    h.until("the deletion is news", |h| {
        h.count(|e| matches!(e, DocEvent::FileDeleted { .. })) == 1
            || h.count(|e| matches!(e, DocEvent::FileMoved { .. })) == 1
    });
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileMoved { .. })), 0);
    assert_eq!(
        h.store.path(doc).unwrap(),
        x,
        "it does not move to the link"
    );
    assert_eq!(h.store.title_note(doc).as_deref(), Some("Deleted"));
}

#[test]
fn f12_deleting_one_name_of_a_file_is_a_deletion() {
    link_deleted(true);
}
#[test]
fn f12_deleting_one_name_made_after_the_open_is_a_deletion_too() {
    link_deleted(false);
}

#[test]
fn f12_a_real_rename_of_a_file_with_two_names_still_follows() {
    let folder = test_folder("rf-12c");
    let (x, y) = (folder.join("x.txt"), folder.join("y.txt"));
    write(&x, b"text\n");
    std::fs::hard_link(&x, &y).unwrap();
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&x);
    pause();
    let moved = folder.join("moved.txt");
    std::fs::rename(&x, &moved).unwrap();
    h.until("the move is news", |h| {
        h.store.path(doc).as_deref() == Some(moved.as_path())
    });
    assert_eq!(h.count(|e| matches!(e, DocEvent::FileDeleted { .. })), 0);
}

// ---- 13: the stamp after a Save.

#[test]
fn f13_a_write_in_the_gap_after_the_swap_is_not_taken_as_ours() {
    let folder = test_folder("rf-13");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_secs(600));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::fs::write(&target, b"another program, in the gap").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    h.until("the outside text shows", |h| {
        h.text(doc) == "another program, in the gap"
    });
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn f13_a_write_in_the_gap_with_typing_in_the_view_asks_at_the_next_save() {
    let folder = test_folder("rf-13b");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_secs(600));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::fs::write(&target, b"another program, in the gap").unwrap();
    }));
    faults.hold_save.store(true, Ordering::Release);
    h.store.save(doc).unwrap();
    h.until("the write holds", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.type_text(doc, "y");
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc);
    assert!(h.store.is_dirty(doc));
    h.store.save(doc).unwrap();
    let question = h.question("the Save asks");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"another program, in the gap"
    );
}

// ---- 17: no session.

#[test]
fn f17_next_deadline_stops_when_no_session_folder_exists_and_the_user_is_told() {
    let folder = test_folder("rf-17");
    // A file where the root folder should be, so no session folder can exist.
    let root = folder.join("root");
    std::fs::write(&root, b"a file").unwrap();
    let mut h = quick(&root);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "x");
    std::thread::sleep(Duration::from_millis(20));
    h.pump();
    for _ in 0..50 {
        h.pump();
        assert!(
            h.store.next_deadline().is_none_or(|at| at > Instant::now()),
            "a deadline already past would wake the app forever"
        );
    }
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::AutosaveFailed { .. })),
        1,
        "{:?}",
        h.events
    );
}

// ---- 18: a Busy answer.

#[test]
fn f18_a_busy_answer_keeps_the_question_and_leaves_no_close_behind() {
    let folder = test_folder("rf-18");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_secs(600));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.store.request_close(doc);
    let question = h.question("the close question");
    // A Revert read is under way from another door: a second question.
    h.store.request_revert(doc).unwrap();
    let revert = h
        .store
        .open_questions()
        .into_iter()
        .find(|q| q.kind == QuestionKind::ConfirmRevert)
        .unwrap();
    faults.hold_reload.store(true, Ordering::Release);
    h.store.answer(revert.id, Answer::Revert).unwrap();
    h.until("the read holds", |_| {
        faults.reload_is_held.load(Ordering::Acquire)
    });
    let busy = h.store.answer(question.id, Answer::Save).unwrap_err();
    assert_eq!(busy.kind, DocErrorKind::Busy);
    assert!(
        h.store.open_questions().iter().any(|q| q.id == question.id),
        "the question stays"
    );
    h.store.answer(question.id, Answer::Cancel).unwrap();
    faults.hold_reload.store(false, Ordering::Release);
    h.until("the revert ends", |h| !h.store.is_dirty(doc));
    h.type_text(doc, "y");
    h.store.save(doc).unwrap();
    h.saved(doc);
    h.settle(Duration::from_millis(30));
    assert!(h.store.view(doc).is_some(), "a later Save closes nothing");
}

// ---- 19: Duplicate of a large file that is not UTF-8.

fn utf16_large(path: &Path) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in "line \u{e9}\n".repeat(400).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    write(path, &bytes);
    bytes
}

#[test]
fn f19_duplicate_and_restore_of_a_large_file_in_another_encoding_work() {
    let folder = test_folder("rf-19");
    let root = folder.join("root");
    let path = folder.join("wide.txt");
    let bytes = utf16_large(&path);
    {
        let mut h = low_limit(&root);
        let doc = h.open(&path);
        let copy = h.store.duplicate(doc).unwrap();
        h.loaded(copy);
        h.pump();
        assert_eq!(
            h.count(|e| matches!(e, DocEvent::OpenFailed { .. })),
            0,
            "{:?}",
            h.events
        );
        assert!(h.store.is_too_large_to_edit(copy));
        assert!(h.store.is_dirty(copy));
        assert_eq!(h.store.path(copy), None);
        // Closing the source must leave the copy's own text working.
        h.store.request_close(doc);
        h.settle(Duration::from_millis(30));
        h.quit();
    }
    let mut h = low_limit(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::OpenFailed { .. })),
        0,
        "{:?}",
        h.events
    );
    let copy = only_doc(&h);
    assert!(h.store.is_too_large_to_edit(copy));
    let target = folder.join("kept.txt");
    h.store.save_as(copy, target.clone(), None).unwrap();
    h.saved(copy);
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
}

// ---- 20: smaller faults.

#[test]
fn f20_a_failed_save_as_from_the_edit_question_leaves_no_stale_undo() {
    let folder = test_folder("rf-20a");
    let path = folder.join("f.txt");
    write(&path, b"abc");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open_with(&path, EncodingChoice::Specific(TextEncoding::Windows1252));
    h.type_text(doc, "\u{4e2d}");
    let question = h.question("the encoding question");
    h.store.answer(question.id, Answer::ChooseEncoding).unwrap();
    // The dialog's choice is still one that cannot hold the text.
    h.store
        .save_as(doc, folder.join("g.txt"), Some(TextEncoding::Windows1252))
        .unwrap();
    h.until("the failure", |h| {
        h.count(|e| matches!(e, DocEvent::SaveFailed { .. })) == 1
    });
    h.type_text(doc, "more");
    h.store.request_save_as(doc);
    h.store.panel_cancelled(doc);
    assert!(h.text(doc).contains("more"), "{}", h.text(doc));
}

#[test]
fn f20_cancel_on_the_lossy_edit_question_asks_again_at_the_next_edit() {
    let folder = test_folder("rf-20b");
    let path = folder.join("f.txt");
    write(&path, &[0xFF, 0xFE, b'h', 0, b'i', 0, 0x41]);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let question = h.question("the lossy question");
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.type_text(doc, "y");
    let question = h.question("the lossy question again");
    assert_eq!(question.kind, QuestionKind::LossyFirstEdit);
}

#[test]
fn f20_a_move_with_no_thread_to_run_it_fails_by_name_and_is_not_stuck() {
    let folder = test_folder("rf-20c");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_secs(600));
    let doc = h.open(&path);
    h.quiet();
    faults.refuse_threads.store(true, Ordering::Relaxed);
    assert!(h.store.rename(doc, "g.txt").is_err());
    faults.refuse_threads.store(false, Ordering::Relaxed);
    assert!(h.store.commands(doc).rename, "not stuck in a move");
}

#[test]
fn f20_every_other_refused_thread_reaches_the_user() {
    let folder = test_folder("rf-20d");
    let path = folder.join("f.txt");
    write(&path, b"text");
    set_read_only(&path, true);
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_millis(1));
    let doc = h.open(&path);
    h.quiet();
    h.type_text(doc, "x");
    let question = h.question("the locked question");
    faults.refuse_threads.store(true, Ordering::Relaxed);
    h.store.answer(question.id, Answer::Unlock).unwrap();
    h.pump();
    assert_eq!(h.count(|e| matches!(e, DocEvent::UnlockFailed { .. })), 1);
    h.store.recover_leftover(folder.join("a"), folder.join("b"));
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::LeftoverNotRecovered { .. })),
        1
    );
    h.store
        .check_encodings(doc, vec![TextEncoding::Windows1252]);
    h.pump();
    assert_eq!(h.count(|e| matches!(e, DocEvent::EncodingFit { .. })), 1);
    // The autosave cannot start: it says so, and it does not wake the app forever.
    std::thread::sleep(Duration::from_millis(5));
    h.pump();
    h.pump();
    assert!(
        h.count(|e| matches!(e, DocEvent::AutosaveFailed { .. })) >= 1,
        "{:?}",
        h.events
    );
    assert!(
        h.store.next_deadline().is_none_or(|at| at > Instant::now()),
        "no past deadline"
    );
    faults.refuse_threads.store(false, Ordering::Relaxed);
}

#[test]
fn f20_an_unreadable_head_is_kept_under_another_name_and_named_once() {
    let folder = test_folder("rf-20e");
    let root = folder.join("root");
    let docs = root.join("session").join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let junk = docs.join("abcd.5.qea");
    std::fs::write(&junk, b"no head here").unwrap();
    {
        let mut h = Harness::new(&root);
        h.pump();
        let shown = h.count(|e| {
            matches!(e, DocEvent::RestoreProblem(error) if error.kind == DocErrorKind::AutosaveDamaged)
        });
        assert_eq!(shown, 1, "{:?}", h.events);
        h.quit();
    }
    assert!(!junk.exists());
    assert!(junk.with_extension("damaged").exists(), "the file stays");
    let h = Harness::new(&root);
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoreProblem(_))),
        0,
        "{:?}",
        h.events
    );
}

#[test]
fn f20_removing_a_documents_copies_leaves_its_damaged_files() {
    let folder = test_folder("rf-20f");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = quick(&root);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    wait_for_copy(&mut h);
    let copy = qea_names(&root).remove(0);
    let key = copy.split('.').next().unwrap();
    let docs = root.join("session").join("docs");
    let kept = docs.join(format!("{key}.1.damaged"));
    std::fs::write(&kept, b"evidence").unwrap();
    h.store.save(doc).unwrap();
    h.saved(doc);
    h.until("the copy goes", |h| qea_names(&h.root).is_empty());
    assert!(kept.exists(), "a path a message named must stay true");
}

#[test]
fn f20_a_folder_made_again_gets_a_new_watch() {
    let folder = test_folder("rf-20g");
    let inner = folder.join("inner");
    let path = inner.join("f.txt");
    write(&path, b"one");
    let mut h = watching(&folder.join("root"));
    let doc = h.open(&path);
    h.quiet();
    pause();
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&inner).unwrap();
    h.until("the watch ends", |h| {
        h.count(|e| matches!(e, DocEvent::WatchUnavailable { .. })) > 0
    });
    h.store.app_activated();
    h.until("the deletion is news", |h| {
        h.store.title_note(doc).as_deref() == Some("Deleted")
    });
    write(&path, b"one");
    h.store.app_activated();
    h.until("the file is back", |h| h.store.title_note(doc).is_none());
    h.quiet();
    h.settle(Duration::from_millis(200));
    std::fs::write(&path, b"two, heard by the new watch").unwrap();
    h.until("the watch hears it, with no activation", |h| {
        h.text(doc) == "two, heard by the new watch"
    });
}

// ---- 22: requests around a restore.

fn unstarted(root: &Path) -> Harness {
    let (cfg, wakes) = config(root);
    Harness {
        root: cfg.root.clone(),
        store: DocumentStore::new(cfg),
        events: Vec::new(),
        wakes,
    }
}

#[test]
fn f22_requests_before_and_after_a_restore_land_in_one_window() {
    let folder = test_folder("rf-22");
    let mut paths = Vec::new();
    for i in 0..15 {
        let path = folder.join(format!("f{i:02}.txt"));
        write(&path, b"x");
        paths.push(path);
    }
    let mut h = unstarted(&folder.join("root"));
    h.store.begin_restore();
    let held = h
        .store
        .open_paths(&paths[..7], OpenTarget::LastUsedWindow, None)
        .unwrap();
    assert!(held.is_empty(), "held until the restore ends: {held:?}");
    assert!(h.store.windows().is_empty());
    h.until("the restore ends", |h| {
        h.count(|e| matches!(e, DocEvent::RestoreFinished { .. })) == 1
    });
    h.store
        .open_paths(&paths[7..], OpenTarget::LastUsedWindow, None)
        .unwrap();
    all_loaded(&mut h);
    let windows = h.store.windows();
    assert_eq!(windows.len(), 1, "{windows:?}");
    let names: Vec<String> = h
        .store
        .tabs(windows[0])
        .iter()
        .map(|d| h.store.display_name(*d))
        .collect();
    let wanted: Vec<String> = (0..15).map(|i| format!("f{i:02}.txt")).collect();
    assert_eq!(names, wanted);
    assert_eq!(
        h.events.iter().find_map(|e| match e {
            DocEvent::RestoreFinished { windows } => Some(*windows),
            _ => None,
        }),
        Some(1),
        "the host must not make a launch window too"
    );
}

#[test]
fn f22_a_request_before_the_end_of_a_restore_with_windows_joins_the_last_used() {
    let folder = test_folder("rf-22b");
    let root = folder.join("root");
    left_one_copy(&root, "restored");
    let path = folder.join("f.txt");
    write(&path, b"x");
    let mut h = unstarted(&root);
    h.store.begin_restore();
    h.store
        .open_paths(&[path], OpenTarget::LastUsedWindow, None)
        .unwrap();
    h.until("the restore ends", |h| {
        h.count(|e| matches!(e, DocEvent::RestoreFinished { .. })) == 1
    });
    all_loaded(&mut h);
    let windows = h.store.windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(h.store.tabs(windows[0]).len(), 2);
}

// ---- 23: event order.

#[test]
fn f23_no_event_names_a_document_after_its_close() {
    let folder = test_folder("rf-23a");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::Save).unwrap();
    h.until("the document closes", |h| h.store.view(doc).is_none());
    h.settle(Duration::from_millis(30));
    let closed = h
        .events
        .iter()
        .position(|e| matches!(e, DocEvent::DocClosed(d) if *d == doc))
        .unwrap();
    for event in &h.events[closed + 1..] {
        assert!(
            !matches!(event, DocEvent::DocChanged(d) | DocEvent::TextReplaced(d) if *d == doc),
            "{event:?}"
        );
    }
}

#[test]
fn f23_a_question_for_a_closed_document_is_taken_out_of_the_events() {
    let folder = test_folder("rf-23b");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "x");
    // A Question event waits in the queue, and the document closes before the host looks.
    h.store.request_close(doc);
    let id = h.store.open_questions()[0].id;
    h.store.answer(id, Answer::DontSave).unwrap();
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::Question(_))),
        0,
        "{:?}",
        h.events
    );
}

#[test]
fn f23_a_retry_of_a_large_file_is_not_called_an_outside_change() {
    let folder = test_folder("rf-23c");
    let path = folder.join("deep.txt");
    let mut bytes = vec![b'a'; 200 * 1024];
    bytes.push(0xE9);
    bytes.extend_from_slice(&[b'z'; 1024]);
    write(&path, &bytes);
    let mut h = low_limit(&folder.join("root"));
    let doc = h.open(&path);
    h.until("the UTF-8 copy takes over", |h| {
        let status = h.store.view(doc).unwrap().store_status();
        status.failure.is_none() && status.complete && status.known_bytes.0 > 200 * 1024
    });
    assert_eq!(h.count(|e| matches!(e, DocEvent::Reloaded(_))), 0);
}

// ---- Non-ASCII letter case.

#[test]
fn concern_path_text_ignores_case_past_ascii() {
    let folder = test_folder("rf-case");
    let path = folder.join("\u{c9}t\u{c9}.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    let other_case = folder.join("\u{e9}T\u{e9}.TXT");
    let opened = h
        .store
        .open_paths(&[other_case], OpenTarget::LastUsedWindow, None)
        .unwrap();
    assert!(opened.is_empty(), "found at once, with no second read");
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::AlreadyOpen { doc: d, .. } if *d == doc)),
        1
    );
}

// ---- 15: odd paths and long lists.

#[test]
fn f15_a_device_a_stream_and_a_pipe_get_a_named_error_and_no_tab() {
    let folder = test_folder("rf-15");
    let good = folder.join("good.txt");
    write(&good, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let list: Vec<PathBuf> = [
        r"\\.\pipe\anything",
        r"\\.\NUL",
        r"\\?\GLOBALROOT\Device\x",
        r"C:\Windows\win.ini:stream",
        r"C:\folder\CON.txt",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    let opened = h
        .store
        .open_paths(&list, OpenTarget::NewWindow, None)
        .unwrap();
    assert!(opened.is_empty());
    h.pump();
    assert_eq!(
        h.count(|e| matches!(
            e,
            DocEvent::OpenFailed { error, .. } if error.kind == DocErrorKind::PathRefused
        )),
        5,
        "{:?}",
        h.events
    );
    assert!(h.store.windows().is_empty(), "nothing was made for them");
    assert!(h.store.recent().is_empty());
    h.open(&good);
}

#[test]
fn f15_a_long_list_opens_a_group_at_a_time_in_order_with_a_short_time_on_the_ui_thread() {
    let folder = test_folder("rf-15b");
    let dir = folder.join("files");
    std::fs::create_dir_all(&dir).unwrap();
    let list: Vec<PathBuf> = (0..200)
        .map(|i| {
            let path = dir.join(format!("f{i:03}.txt"));
            std::fs::write(&path, b"x").unwrap();
            path
        })
        .collect();
    let mut h = Harness::new(&folder.join("root"));
    let first = h
        .store
        .open_paths(&list, OpenTarget::NewWindow, None)
        .unwrap();
    assert_eq!(first.len(), 32, "one group now");
    assert_eq!(h.store.windows().len(), 1);
    assert!(
        h.store.next_deadline().is_some(),
        "the rest have a time, for a host with no other wake"
    );
    let mut most = 0;
    let mut before = h.store.all_documents().len();
    h.until("every tab exists", |h| {
        let now = h.store.all_documents().len();
        most = most.max(now - before);
        before = now;
        now == 200
    });
    assert!(most <= 32, "{most} in one poll");
    all_loaded(&mut h);
    let windows = h.store.windows();
    assert_eq!(windows.len(), 1, "one window for all of it");
    let names: Vec<String> = h
        .store
        .tabs(windows[0])
        .iter()
        .map(|d| h.store.display_name(*d))
        .collect();
    let wanted: Vec<String> = (0..200).map(|i| format!("f{i:03}.txt")).collect();
    assert_eq!(names, wanted);
    h.quiet();
    assert_eq!(h.store.next_deadline(), None, "no timer once it is done");
}

#[test]
fn f15_a_list_past_the_queue_limit_is_named_and_the_rest_opens() {
    let folder = test_folder("rf-15c");
    let mut h = Harness::new(&folder.join("root"));
    let list: Vec<PathBuf> = (0..5000)
        .map(|i| folder.join(format!("missing{i}.txt")))
        .collect();
    h.store
        .open_paths(&list, OpenTarget::LastUsedWindow, None)
        .unwrap();
    h.pump();
    let told: Vec<_> = h
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                DocEvent::OpenFailed { error, .. } if error.kind == DocErrorKind::TooManyFiles
            )
        })
        .collect();
    assert_eq!(told.len(), 1, "named once, not 904 times");
}

#[test]
fn f15_a_network_path_that_waits_does_not_hold_up_local_files() {
    let folder = test_folder("rf-15d");
    let local = folder.join("local.txt");
    write(&local, b"text");
    let (cfg, wakes) = config(&folder.join("root"));
    cfg.faults.hold_remote_load.store(true, Ordering::Release);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let remote: Vec<PathBuf> = (0..8)
        .map(|i| PathBuf::from(format!(r"\\qe-no-such-host.invalid\share\{i}.txt")))
        .collect();
    let began = Instant::now();
    let waiting = h
        .store
        .open_paths(&remote, OpenTarget::NewWindow, None)
        .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "the UI thread waits for no network: {:?}",
        began.elapsed()
    );
    assert_eq!(waiting.len(), 8);
    h.until("the network loads hold", |_| {
        faults.remote_load_is_held.load(Ordering::Acquire)
    });
    let doc = h.open(&local);
    assert_eq!(h.text(doc), "text", "a local file opens meanwhile");
    assert!(waiting.iter().all(|d| h.store.is_loading(*d)));
    // Close them while they wait, so that no test here reaches for a real network.
    for d in waiting {
        h.store.request_close(d);
    }
    faults.hold_remote_load.store(false, Ordering::Release);
    h.settle(Duration::from_millis(20));
    assert_eq!(h.store.all_documents(), vec![doc]);
}

// ---- 21: the last window.

#[test]
fn f21_a_restored_file_that_fails_to_load_tells_the_host_nothing_is_on_screen() {
    let folder = test_folder("rf-21");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"text");
    {
        let mut h = quick(&root);
        h.open(&path);
        h.quit();
    }
    std::fs::remove_file(&path).unwrap();
    let mut h = Harness::new(&root);
    assert_eq!(
        h.events.iter().find_map(|e| match e {
            DocEvent::RestoreFinished { windows } => Some(*windows),
            _ => None,
        }),
        Some(1)
    );
    h.until("the host is told", |h| {
        h.count(|e| {
            matches!(
                e,
                DocEvent::LastWindowGone {
                    opened_nothing: true
                }
            )
        }) == 1
    });
    assert!(h.store.windows().is_empty());
    let (_, doc) = h.store.new_window(true).unwrap();
    assert!(h.store.is_transient(doc));
}

#[test]
fn f21_a_bad_path_at_launch_tells_the_host_and_a_users_close_says_so_too() {
    let folder = test_folder("rf-21b");
    let mut h = Harness::new(&folder.join("root"));
    h.store
        .open_paths(&[folder.join("missing.txt")], OpenTarget::NewWindow, None)
        .unwrap();
    h.until("the host is told", |h| {
        h.count(|e| {
            matches!(
                e,
                DocEvent::LastWindowGone {
                    opened_nothing: true
                }
            )
        }) == 1
    });
    let (_, doc) = h.store.new_window(false).unwrap();
    h.store.request_close(doc);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(
            e,
            DocEvent::LastWindowGone {
                opened_nothing: false
            }
        )),
        1
    );
}

// ---- 23: a leftover the person does not want.

#[test]
fn f23_an_answer_about_a_leftover_it_cannot_find_counts_as_done() {
    let folder = test_folder("rf-23d");
    let path = folder.join("f.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    h.store
        .discard_leftover(folder.join("not-there.tmp"), path.clone());
    h.until("the answer", |h| {
        h.count(|e| matches!(e, DocEvent::LeftoverDeleted { .. })) == 1
    });
}

// ---- A look at the file from before the document's own Save.

#[test]
fn a_check_that_began_before_a_save_does_not_read_as_an_outside_change() {
    let folder = test_folder("rf-epoch");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder.join("root"), Duration::from_secs(600));
    let doc = h.open(&path);
    for round in 0..30 {
        h.type_text(doc, "x");
        faults.hold_save.store(true, Ordering::Release);
        // Each look runs at a different moment around the Save.
        h.store.app_activated();
        h.store.save(doc).unwrap();
        h.type_text(doc, "y");
        faults.hold_save.store(false, Ordering::Release);
        h.until("the save ends", |h| !h.store.is_saving(doc));
        h.settle(Duration::from_millis(5));
        assert!(h.store.open_questions().is_empty(), "round {round}");
        h.store.save(doc).unwrap();
        h.until("the second save ends", |h| {
            !h.store.is_saving(doc) && !h.store.is_dirty(doc)
        });
        assert!(
            h.store.open_questions().is_empty(),
            "round {round}: {:?}",
            h.store.open_questions()
        );
    }
}

// ---- The review of the real app ----

#[test]
fn a_second_close_asks_nothing_new_and_one_cancel_ends_it() {
    let folder = test_folder("app-close-twice");
    let mut h = quick(&folder);
    let path = folder.join("a.txt");
    write(&path, b"text");
    let doc = h.open(&path);
    h.type_text(doc, "more");
    h.store.request_close(doc);
    h.store.request_close(doc);
    let questions = h.store.open_questions();
    assert_eq!(questions.len(), 1, "{questions:?}");
    h.store.answer(questions[0].id, Answer::Cancel).unwrap();
    assert!(h.store.open_questions().is_empty());
    assert!(h.store.view(doc).is_some());
}

#[test]
fn a_tab_that_moves_makes_its_new_window_the_one_in_use() {
    let folder = test_folder("app-move-tab");
    let mut h = quick(&folder);
    let (first, one) = h.store.new_window(false).unwrap();
    let (second, _) = h.store.new_window(false).unwrap();
    let _keep = h.store.new_tab(first).unwrap();
    h.store.window_activated(first);
    h.store.move_tab(one, second, 0);
    assert_eq!(h.store.last_used_window(), Some(second));
}

#[test]
fn a_failed_copy_stays_known_until_a_copy_succeeds() {
    let folder = test_folder("app-autosave-state");
    let mut h = quick(&folder);
    let path = folder.join("a.txt");
    write(&path, b"text");
    let doc = h.open(&path);
    let docs = folder.join("session").join("docs");
    h.type_text(doc, "one");
    h.until("a first copy", |_| {
        std::fs::read_dir(&docs).is_ok_and(|mut dir| dir.next().is_some())
    });
    std::fs::remove_dir_all(&docs).unwrap();
    std::fs::write(&docs, b"not a folder").unwrap();
    h.type_text(doc, "two");
    h.until("the failure", |h| h.store.autosave_failure(doc).is_some());
    h.type_text(doc, "three");
    assert!(h.store.autosave_failure(doc).is_some());
    std::fs::remove_file(&docs).unwrap();
    std::fs::create_dir_all(&docs).unwrap();
    h.type_text(doc, "four");
    h.until("a good copy", |h| h.store.autosave_failure(doc).is_none());
}
