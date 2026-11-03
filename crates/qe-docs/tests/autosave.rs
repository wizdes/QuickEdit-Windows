//! Autosave and restore, within one process. `tests/kill.rs` has the real kills.
mod support;

use qe_api::{ByteOffset, EncodingChoice, TextEncoding};
use qe_codec::LineEnding;
use qe_docs::{Answer, DocErrorKind, DocEvent, QuestionKind, ShowState, WindowFrame};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::*;

/// Autosave after a moment, so that tests need no long waits.
fn quick(root: &Path) -> Harness {
    let (mut config, wakes) = config(root);
    config.autosave_delay = Duration::from_millis(5);
    Harness::with_config(config, wakes)
}

fn wait_for_copy(h: &mut Harness) {
    let root = h.root.clone();
    h.until("an autosaved copy appears", |_| {
        autosave_files(&root).iter().any(|n| n.ends_with(".qea"))
    });
}

fn restored_doc(h: &Harness) -> qe_docs::DocId {
    h.store.all_documents()[0]
}

fn finished_windows(h: &Harness) -> usize {
    h.events
        .iter()
        .find_map(|e| match e {
            DocEvent::RestoreFinished { windows } => Some(*windows),
            _ => None,
        })
        .unwrap()
}

fn all_loaded(h: &mut Harness) {
    h.until("every tab has its text", |h| {
        h.store
            .all_documents()
            .iter()
            .all(|d| !h.store.is_loading(*d))
    });
}

#[test]
fn the_copy_waits_for_the_delay_and_a_clean_document_never_writes_one() {
    let folder = test_folder("auto-delay");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.session_delay = Duration::from_secs(600);
    let mut h = Harness::with_config(cfg, wakes);
    let doc = h.open(&path);
    h.settle(Duration::from_millis(30));
    assert!(
        autosave_files(&root).is_empty(),
        "no unsaved change, no file"
    );
    let before = Instant::now();
    h.type_text(doc, "x");
    let due = h.store.next_deadline().expect("a timer for the copy");
    let wait = due.duration_since(before);
    assert!(
        wait > Duration::from_secs(29) && wait <= Duration::from_secs(31),
        "the setting's 30 seconds: {wait:?}"
    );
    h.settle(Duration::from_millis(30));
    assert!(autosave_files(&root).is_empty(), "the delay has not passed");
    h.type_text(doc, "y");
    let later = h.store.next_deadline().unwrap();
    assert!(
        later.duration_since(before) <= wait + Duration::from_millis(5),
        "more typing does not push the copy back"
    );
}

#[test]
fn no_timer_runs_at_idle_and_zero_turns_the_timer_off() {
    let folder = test_folder("auto-idle");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    let mut h = quick(&folder.join("root"));
    let doc = h.open(&path);
    h.quiet();
    assert!(autosave_files(&h.root).is_empty(), "clean, so no file");
    h.type_text(doc, "x");
    wait_for_copy(&mut h);
    h.quiet();
    h.settle(Duration::from_millis(30));
    assert_eq!(h.store.next_deadline(), None, "dirty but written");
    drop(h);

    let (mut config, wakes) = config(&folder.join("root2"));
    config.autosave_delay = Duration::ZERO;
    let mut h = Harness::with_config(config, wakes);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.quiet();
    h.type_text(doc, "x");
    h.quiet();
    assert!(autosave_files(&h.root).is_empty());
    let root = h.root.clone();
    h.quit();
    assert!(
        !autosave_files(&root).is_empty(),
        "a quit still keeps the text"
    );
}

#[test]
fn undo_to_the_save_point_and_a_save_each_remove_the_copy() {
    let folder = test_folder("auto-remove");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    let root = folder.join("root");
    let mut h = quick(&root);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    wait_for_copy(&mut h);
    h.quiet();
    h.undo(doc);
    h.until("Undo removes the copy", |_| {
        autosave_files(&root).is_empty()
    });
    h.type_text(doc, "y");
    wait_for_copy(&mut h);
    h.store.save(doc).unwrap();
    h.saved(doc);
    h.until("the save removes the copy", |_| {
        autosave_files(&root).is_empty()
    });
}

#[test]
fn close_with_dont_save_removes_the_copy_and_a_running_one_too() {
    let folder = test_folder("auto-close");
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(1);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "text");
    wait_for_copy(&mut h);
    faults.hold_autosave.store(true, Ordering::Release);
    h.type_text(doc, "more");
    h.until("the second copy holds", |_| {
        faults.autosave_is_held.load(Ordering::Acquire)
    });
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    faults.hold_autosave.store(false, Ordering::Release);
    h.until("no copy stays", |_| autosave_files(&root).is_empty());
    h.settle(Duration::from_millis(20));
    assert!(autosave_files(&root).is_empty());
}

#[test]
fn restore_brings_back_every_window_tab_order_text_and_place() {
    let folder = test_folder("auto-restore");
    let root = folder.join("root");
    let saved = folder.join("saved.txt");
    write(&saved, b"clean line one\nclean line two\n");
    let dirty = folder.join("dirty.txt");
    write(&dirty, b"caf\xE9\r\nsecond\r\n");
    let (first_window, second_window, frame, clean, edited, untitled, copy, other);
    {
        let mut h = quick(&root);
        let made = h.store.new_window(false).unwrap();
        first_window = made.0;
        untitled = made.1;
        h.type_text(untitled, "unsaved \u{1f600} words");
        clean = h.open(&saved);
        h.select(clean, 2, 9);
        edited = h.open_with(&dirty, EncodingChoice::Specific(TextEncoding::Windows1252));
        h.caret_to_end(edited);
        h.type_text(edited, "typed");
        h.select(edited, 9, 3);
        h.store.set_prevent_editing(clean, true);
        h.store.set_wrap_to_page(edited, true);
        copy = h.store.duplicate(clean).unwrap();
        h.loaded(copy);
        let made = h.store.new_window(false).unwrap();
        second_window = made.0;
        other = made.1;
        h.type_text(other, "second window");
        frame = WindowFrame {
            x: 40,
            y: 50,
            width: 900,
            height: 700,
        };
        h.store
            .set_window_frame(second_window, frame, ShowState::Maximized, true);
        h.store.set_active_tab(first_window, edited);
        h.store.window_activated(second_window);
        h.quit();
    }
    let mut h = Harness::new(&root);
    assert_eq!(finished_windows(&h), 2);
    all_loaded(&mut h);
    let windows = h.store.windows();
    assert_eq!(windows.len(), 2);
    assert_eq!(
        h.store.last_used_window(),
        Some(windows[1]),
        "the window used last stays so"
    );
    let first_tabs = h.store.tabs(windows[0]);
    assert_eq!(first_tabs.len(), 4);
    let names: Vec<String> = first_tabs
        .iter()
        .map(|d| h.store.display_name(*d))
        .collect();
    assert_eq!(
        names,
        ["Untitled", "saved.txt", "saved copy.txt", "dirty.txt"],
        "a tab order with Duplicate beside its source"
    );
    let (new_untitled, new_clean, new_copy, new_edited) =
        (first_tabs[0], first_tabs[1], first_tabs[2], first_tabs[3]);
    assert_eq!(h.text(new_untitled), "unsaved \u{1f600} words");
    assert!(h.store.is_dirty(new_untitled));
    assert_eq!(h.store.path(new_untitled), None);
    assert_eq!(h.text(new_clean), "clean line one\nclean line two\n");
    assert!(!h.store.is_dirty(new_clean));
    assert_eq!(h.store.path(new_clean).unwrap(), saved);
    assert!(h.store.prevent_editing(new_clean));
    let selection = h.store.view(new_clean).unwrap().selection();
    assert_eq!(selection, ByteOffset(2)..ByteOffset(9));
    assert!(h.store.is_dirty(new_copy));
    assert_eq!(h.store.path(new_copy), None);
    assert_eq!(h.text(new_edited), "caf\u{e9}\r\nsecond\r\ntyped");
    assert!(h.store.is_dirty(new_edited));
    assert_eq!(h.store.path(new_edited).unwrap(), dirty);
    let format = h.store.format(new_edited).unwrap();
    assert_eq!(format.encoding, TextEncoding::Windows1252);
    assert_eq!(format.main_line_ending, LineEnding::CrLf);
    assert!(h.store.wrap_to_page(new_edited));
    let view = h.store.view(new_edited).unwrap();
    assert_eq!(view.selection(), ByteOffset(3)..ByteOffset(9));
    assert_eq!(view.caret(), ByteOffset(3), "the caret end comes back too");
    assert_eq!(h.store.active_tab(windows[0]), Some(new_edited));
    let (frame_back, show) = h.store.window_frame(windows[1]).unwrap();
    assert_eq!((frame_back, show), (frame, ShowState::Maximized));
    assert_eq!(h.text(h.store.tabs(windows[1])[0]), "second window");
    // The restored file saves as it was, in its own encoding.
    h.store.save(new_edited).unwrap();
    h.saved(new_edited);
    assert_eq!(
        std::fs::read(&dirty).unwrap(),
        b"caf\xE9\r\nsecond\r\ntyped",
        "the encoding and line endings came back"
    );
    assert!(!h.store.is_dirty(new_edited));
    let _ = (clean, edited, untitled, copy, other, first_window);
}

#[test]
fn a_restore_with_nothing_to_restore_says_no_windows() {
    let folder = test_folder("auto-empty");
    let h = Harness::new(&folder.join("root"));
    assert_eq!(finished_windows(&h), 0);
    assert_eq!(h.store.windows(), Vec::new());
}

#[test]
fn a_blank_untitled_window_never_comes_back() {
    let folder = test_folder("auto-blank");
    let root = folder.join("root");
    {
        let mut h = quick(&root);
        let (_, doc) = h.store.new_window(false).unwrap();
        h.type_text(doc, "  \n ");
        h.settle(Duration::from_millis(40));
        assert!(
            autosave_files(&root).is_empty(),
            "blank has nothing to keep"
        );
        h.quit();
    }
    let h = Harness::new(&root);
    assert_eq!(finished_windows(&h), 0);
}

#[test]
fn text_turned_blank_after_a_copy_loses_the_copy_at_quit() {
    let folder = test_folder("auto-blank-late");
    let root = folder.join("root");
    {
        let mut h = quick(&root);
        let (_, doc) = h.store.new_window(false).unwrap();
        h.type_text(doc, "x");
        wait_for_copy(&mut h);
        let mut ctx = qe_api::ViewContext::default();
        h.store
            .view_mut(doc)
            .unwrap()
            .replace_range(ByteOffset(0)..ByteOffset(1), " ", &mut ctx);
        h.store.note_view_changed(doc);
        h.quit();
    }
    assert!(autosave_files(&root).is_empty());
    let h = Harness::new(&root);
    assert_eq!(finished_windows(&h), 0);
}

#[test]
fn a_file_changed_while_away_keeps_the_unsaved_text_says_so_and_asks_at_save() {
    let folder = test_folder("auto-newer");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"old");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "mine ");
        h.quit();
    }
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(&path, b"newer from another program").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = restored_doc(&h);
    assert_eq!(h.text(doc), "mine old", "the unsaved text wins the window");
    assert!(h.store.is_dirty(doc));
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::RestoredOverNewerFile { .. })),
        1
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"newer from another program");
    h.store.save(doc).unwrap();
    let question = h.question("the conflict question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"newer from another program");
    h.store.answer(question.id, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"mine old");
}

#[test]
fn a_clean_file_changed_while_away_opens_as_it_is_now_with_the_caret_inside() {
    let folder = test_folder("auto-clean-newer");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"a long text that will shrink\n");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.select(doc, 20, 25);
        h.quit();
    }
    std::fs::write(&path, b"tiny").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = restored_doc(&h);
    assert_eq!(h.text(doc), "tiny");
    assert!(!h.store.is_dirty(doc));
    h.until("the place is applied", |h| {
        h.store.view(doc).unwrap().selection().end.0 <= 4
    });
}

#[test]
fn a_file_deleted_while_away_keeps_unsaved_text_and_a_clean_one_is_named_and_gone() {
    let folder = test_folder("auto-deleted");
    let root = folder.join("root");
    let (dirty, clean) = (folder.join("dirty.txt"), folder.join("clean.txt"));
    write(&dirty, b"d");
    write(&clean, b"c");
    {
        let mut h = quick(&root);
        let doc = h.open(&dirty);
        h.open(&clean);
        h.type_text(doc, "kept ");
        h.quit();
    }
    std::fs::remove_file(&dirty).unwrap();
    std::fs::remove_file(&clean).unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::OpenFailed { path, .. } if *path == clean)),
        1,
        "{:?}",
        h.events
    );
    assert_eq!(h.store.all_documents().len(), 1);
    let doc = restored_doc(&h);
    assert_eq!(h.text(doc), "kept d");
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.store.title_note(doc).as_deref(), Some("Deleted"));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(
        std::fs::read(&dirty).unwrap(),
        b"kept d",
        "a Save writes it again"
    );
    assert_eq!(h.store.title_note(doc), None);
}

#[test]
fn a_failed_copy_keeps_the_last_good_one_says_so_once_and_tries_again() {
    let folder = test_folder("auto-full");
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_millis(2);
    let faults = cfg.faults.clone();
    let mut h = Harness::with_config(cfg, wakes);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "good");
    wait_for_copy(&mut h);
    h.quiet();
    let good = autosave_files(&root);
    // A disk that fills part way through the next copy.
    faults.autosave_fail_after.store(1, Ordering::Relaxed);
    faults.autosave_error_code.store(112, Ordering::Relaxed);
    h.type_text(doc, " better");
    h.until("the failure is named", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::AutosaveFailed { .. }))
    });
    let kind = h.events.iter().find_map(|e| match e {
        DocEvent::AutosaveFailed { error, .. } => Some(error.kind),
        _ => None,
    });
    assert_eq!(kind, Some(DocErrorKind::DiskFull));
    assert_eq!(
        autosave_files(&root),
        good,
        "the last good copy, and no partial file"
    );
    h.settle(Duration::from_millis(30));
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::AutosaveFailed { .. })),
        1,
        "the same failure is said once"
    );
    h.until("it will try again", |h| {
        h.store.next_deadline().is_some() || h.store.is_autosaving(doc)
    });
    faults.autosave_error_code.store(0, Ordering::Relaxed);
    h.until("a newer copy replaces the old one", |_| {
        let now = autosave_files(&root);
        now.len() == 1 && now != good
    });
}

/// Writes one quit's worth of an untitled document, and gives its copy's path.
fn one_copy(root: &Path, text: &str) -> std::path::PathBuf {
    let mut h = quick(root);
    let (_, doc) = h.store.new_window(false).unwrap();
    let mut ctx = qe_api::ViewContext::default();
    h.store
        .view_mut(doc)
        .unwrap()
        .replace_range(ByteOffset(0)..ByteOffset(0), text, &mut ctx);
    h.store.note_view_changed(doc);
    h.quit();
    let name = autosave_files(root)
        .into_iter()
        .find(|n| n.ends_with(".qea"))
        .unwrap();
    root.join("session").join("docs").join(name)
}

fn with_generation(path: &Path, generation: u64) -> std::path::PathBuf {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let key = name.split('.').next().unwrap();
    path.with_file_name(format!("{key}.{generation}.qea"))
}

#[test]
fn a_damaged_newest_copy_gives_way_to_the_older_and_stays_on_disk_by_another_name() {
    let folder = test_folder("auto-damaged");
    let root = folder.join("root");
    let good = one_copy(&root, "the good text");
    let newer = with_generation(&good, 9_000);
    let mut bytes = std::fs::read(&good).unwrap();
    let last = bytes.len() - 30;
    bytes[last] ^= 0x20;
    std::fs::write(&newer, &bytes).unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = restored_doc(&h);
    assert_eq!(h.text(doc), "the good text");
    h.pump();
    let problem = h.events.iter().find_map(|e| match e {
        DocEvent::RestoreProblem(error) => Some(error.clone()),
        _ => None,
    });
    let problem = problem.expect("the damage is named");
    assert_eq!(problem.kind, DocErrorKind::AutosaveDamaged);
    assert!(newer.with_extension("damaged").exists());
    assert!(!newer.exists());
}

#[test]
fn every_kind_of_cut_file_is_refused_and_named() {
    let folder = test_folder("auto-cut");
    let root = folder.join("root");
    let good = one_copy(&root, "whole text with\nQEAEND 5 inside it\n");
    let bytes = std::fs::read(&good).unwrap();
    // Each length is a place a crash with no flush could have cut the file.
    for keep in [0, 3, 40, bytes.len() / 2, bytes.len() - 3] {
        std::fs::write(&good, &bytes[..keep]).unwrap();
        let mut h = Harness::new(&root);
        all_loaded(&mut h);
        h.pump();
        assert!(
            h.count(|e| matches!(e, DocEvent::RestoreProblem(_))) >= 1,
            "cut at {keep}: {:?}",
            h.events
        );
        assert!(h.store.all_documents().is_empty(), "cut at {keep}");
        drop(h);
        // Restore the file for the next round, without the rename a damaged copy got.
        let damaged = good.with_extension("damaged");
        if damaged.exists() {
            std::fs::remove_file(&damaged).unwrap();
        }
        std::fs::write(&good, &bytes).unwrap();
    }
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    assert_eq!(
        h.text(restored_doc(&h)),
        "whole text with\nQEAEND 5 inside it\n",
        "text that looks like the end mark is safe"
    );
}

#[test]
fn a_damaged_list_loses_places_but_not_text() {
    let folder = test_folder("auto-damaged-list");
    let root = folder.join("root");
    one_copy(&root, "survives");
    let list = root.join("session").join("session.txt");
    std::fs::write(&list, b"not a session at all").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    assert_eq!(h.text(restored_doc(&h)), "survives");
    h.pump();
    assert_eq!(
        h.count(|e| matches!(
            e,
            DocEvent::RestoreProblem(error) if error.kind == DocErrorKind::SessionDamaged
        )),
        1
    );
    assert!(root.join("session").join("session.broken.txt").exists());
}

#[test]
fn a_copy_with_no_row_comes_back_and_an_unfinished_one_goes() {
    let folder = test_folder("auto-orphan");
    let root = folder.join("root");
    let good = one_copy(&root, "written just before a kill");
    std::fs::remove_file(root.join("session").join("session.txt")).unwrap();
    let unfinished = good.with_file_name("abc.77.tmp");
    std::fs::write(&unfinished, b"partial").unwrap();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = restored_doc(&h);
    assert_eq!(h.text(doc), "written just before a kill");
    assert!(h.store.is_dirty(doc));
    assert!(!unfinished.exists(), "an unfinished copy never counts");
    assert_eq!(h.store.windows().len(), 1, "it gets a window");
}

#[test]
fn read_only_files_have_no_copy_and_come_back_by_their_path() {
    let folder = test_folder("auto-mapped");
    let root = folder.join("root");
    let path = folder.join("huge.txt");
    write(&path, "line of text\n".repeat(300).as_bytes());
    {
        let (mut cfg, wakes) = config(&root);
        cfg.autosave_delay = Duration::from_millis(1);
        cfg.read_only_bytes = 1_000;
        let mut h = Harness::with_config(cfg, wakes);
        let doc = h.open(&path);
        h.until("the index ends", |h| {
            h.store.view(doc).unwrap().store_status().complete
        });
        h.select(doc, 26, 39);
        h.settle(Duration::from_millis(20));
        assert!(autosave_files(&root).is_empty());
        h.quit();
    }
    assert!(autosave_files(&root).is_empty());
    let (mut cfg, wakes) = config(&root);
    cfg.read_only_bytes = 1_000;
    let mut h = Harness::with_config(cfg, wakes);
    all_loaded(&mut h);
    let doc = restored_doc(&h);
    assert!(h.store.is_too_large_to_edit(doc));
    h.until("the place comes back", |h| {
        h.store.view(doc).unwrap().selection() == (ByteOffset(26)..ByteOffset(39))
    });
}

#[test]
fn a_read_only_copy_with_no_file_comes_back_from_its_source() {
    let folder = test_folder("auto-mapped-copy");
    let root = folder.join("root");
    let path = folder.join("huge.txt");
    write(&path, "line of text\n".repeat(300).as_bytes());
    {
        let (mut cfg, wakes) = config(&root);
        cfg.read_only_bytes = 1_000;
        cfg.autosave_delay = Duration::from_millis(1);
        let mut h = Harness::with_config(cfg, wakes);
        let doc = h.open(&path);
        let copy = h.store.duplicate(doc).unwrap();
        h.loaded(copy);
        assert!(h.store.is_too_large_to_edit(copy));
        assert!(h.store.is_dirty(copy));
        assert_eq!(h.store.path(copy), None);
        h.quiet();
        h.settle(Duration::from_millis(30));
        assert!(
            autosave_files(&root).is_empty(),
            "its bytes are in its source"
        );
        h.store.request_close(doc);
        h.quit();
    }
    let (mut cfg, wakes) = config(&root);
    cfg.read_only_bytes = 1_000;
    let mut h = Harness::with_config(cfg, wakes);
    all_loaded(&mut h);
    let copy = restored_doc(&h);
    assert!(h.store.is_too_large_to_edit(copy));
    assert_eq!(h.store.path(copy), None);
    let target = folder.join("kept.txt");
    h.store.save_as(copy, target.clone(), None).unwrap();
    h.saved(copy);
    assert_eq!(
        std::fs::read(&target).unwrap(),
        std::fs::read(&path).unwrap()
    );
}

#[test]
fn a_quit_that_cannot_keep_the_text_asks_and_cancel_keeps_the_app() {
    let folder = test_folder("auto-quit-fail");
    let root = folder.join("root");
    let (mut cfg, wakes) = config(&root);
    cfg.autosave_delay = Duration::from_secs(600);
    let faults = cfg.faults.clone();
    faults.autosave_error_code.store(112, Ordering::Relaxed);
    let mut h = Harness::with_config(cfg, wakes);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "precious");
    h.store.begin_quit();
    let question = h.question("the quit question");
    assert_eq!(question.kind, QuestionKind::AutosaveFailedAtQuit);
    assert!(h.count(|e| matches!(e, DocEvent::ReadyToQuit)) == 0);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.settle(Duration::from_millis(10));
    assert_eq!(h.count(|e| matches!(e, DocEvent::ReadyToQuit)), 0);
    assert_eq!(h.text(doc), "precious");
    // Asking again and saying Don't Save lets it go.
    faults.autosave_error_code.store(112, Ordering::Relaxed);
    h.store.begin_quit();
    let question = h.question("the quit question again");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    h.until("the quit ends", |h| {
        h.events.iter().any(|e| matches!(e, DocEvent::ReadyToQuit))
    });
}

#[test]
fn the_end_of_a_windows_session_can_wait_for_the_text() {
    let folder = test_folder("auto-end-session");
    let root = folder.join("root");
    {
        let mut h = Harness::new(&root);
        let (_, doc) = h.store.new_window(false).unwrap();
        h.type_text(doc, "kept at shutdown");
        assert!(h.store.wait_for_quit(WAIT));
    }
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    assert_eq!(h.text(restored_doc(&h)), "kept at shutdown");
}

#[test]
fn a_second_running_copy_keeps_its_own_session_and_the_next_launch_merges_them() {
    let folder = test_folder("auto-two");
    let root = folder.join("root");
    let mut first = Harness::new(&root);
    let (_, a) = first.store.new_window(false).unwrap();
    first.type_text(a, "from the first");
    let mut second = Harness::new(&root);
    let (_, b) = second.store.new_window(false).unwrap();
    second.type_text(b, "from the second");
    assert!(
        root.join("session-2").exists(),
        "the first holds its folder"
    );
    first.quit();
    second.quit();
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let mut texts: Vec<String> = h.store.all_documents().iter().map(|d| h.text(*d)).collect();
    texts.sort();
    assert_eq!(texts, ["from the first", "from the second"]);
    assert_eq!(h.store.windows().len(), 2);
    assert!(!root.join("session-2").exists());
    h.settle(Duration::from_millis(20));
    h.quit();
    // Nothing is lost in the merged session's next round either.
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    assert_eq!(h.store.all_documents().len(), 2);
}

#[test]
fn undo_after_a_restored_copy_cannot_reach_a_clean_state_by_accident() {
    let folder = test_folder("auto-undo-restored");
    let root = folder.join("root");
    let path = folder.join("f.txt");
    write(&path, b"disk");
    {
        let mut h = quick(&root);
        let doc = h.open(&path);
        h.type_text(doc, "x");
        h.quit();
    }
    let mut h = Harness::new(&root);
    all_loaded(&mut h);
    let doc = restored_doc(&h);
    h.type_text(doc, "y");
    h.undo(doc);
    assert_eq!(h.text(doc), "xdisk");
    assert!(h.store.is_dirty(doc), "disk still does not hold this text");
}
