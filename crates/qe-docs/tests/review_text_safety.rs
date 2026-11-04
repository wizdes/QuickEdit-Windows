mod support;
use qe_docs::{Answer, DocEvent, QuestionKind};
use std::time::Duration;
use support::*;

#[test]
fn same_length_external_write_after_save_must_not_be_overwritten_silently() {
    let folder = test_folder("final3-same-length");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        // A same-size write inside one step of the file time, about 16 ms on NTFS, leaves the
        // stamp as it was, and the next save then overwrites it unseen. This wait does not
        // close that limit, which `FileStamp` also states. It only tests a write past it.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&target, b"THEM").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    *faults.after_swap.lock().unwrap() = None;
    h.store.app_activated();
    h.settle(Duration::from_millis(500));
    println!(
        "after outside write: disk={:?}, view={:?}, dirty={}",
        std::fs::read(&path).unwrap(),
        h.text(doc),
        h.store.is_dirty(doc)
    );
    h.type_text(doc, "y");
    h.events.clear();
    h.store.save(doc).unwrap();
    h.until("save or conflict", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFinished { .. } | DocEvent::Question(_)))
    });
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"THEM",
        "outside text must remain until the user agrees"
    );
    assert!(
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::Question(q) if q.kind == QuestionKind::ChangedOnDisk))
    );
}

#[test]
fn save_as_default_encoding_must_warn_before_losing_original_bytes() {
    let folder = test_folder("final3-lossy-save-as");
    let path = folder.join("f.txt");
    let original = [0xff, 0xfe, b'h', 0, b'i', 0, 0x41];
    write(&path, &original);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    assert!(h.store.format(doc).unwrap().lossy);
    let selected = h.store.format(doc).unwrap().encoding;
    h.events.clear();
    h.store.save_as(doc, path.clone(), Some(selected)).unwrap();
    h.until("save or lossy question", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFinished { .. } | DocEvent::Question(_)))
    });
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "unchanged default encoding must still warn about loss"
    );
    assert!(
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::Question(q) if q.kind == QuestionKind::LossyOverwrite))
    );
}

#[test]
fn save_as_lossy_overwrite_uses_the_dialog_encoding() {
    let folder = test_folder("final3-lossy-choice");
    let path = folder.join("f.txt");
    write(&path, &[0xff, 0xfe, b'h', 0, b'i', 0, 0x41]);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.store
        .save_as(doc, path.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let question = h.question("lossy Save As warning");
    assert_eq!(question.kind, QuestionKind::LossyOverwrite);
    h.store.answer(question.id, Answer::Overwrite).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"hi");
    assert_eq!(
        h.store.format(doc).unwrap().encoding,
        qe_api::TextEncoding::Utf8
    );
}

#[test]
fn damaged_window_reference_must_still_restore_unsaved_text() {
    let folder = test_folder("final3-damaged-window");
    let root = folder.join("root");
    let mut h = Harness::new(&root);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "only unsaved copy");
    h.quit();

    let manifest = root.join("session/session.txt");
    let original = std::fs::read_to_string(&manifest).unwrap();
    let damaged = original
        .lines()
        .map(|line| {
            if line.starts_with("doc ") {
                line.split(' ')
                    .map(|field| {
                        if field.starts_with("window=") {
                            "window=999999"
                        } else {
                            field
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&manifest, damaged).unwrap();
    let mut restored = Harness::new(&root);
    restored.until("loads finish", |h| {
        h.store
            .all_documents()
            .iter()
            .all(|d| !h.store.is_loading(*d))
    });
    println!(
        "restored documents={}, problems={:?}",
        restored.store.all_documents().len(),
        restored.events
    );
    assert!(
        restored
            .store
            .all_documents()
            .iter()
            .any(|d| restored.text(*d) == "only unsaved copy"),
        "an invalid window link must not hide the good autosave"
    );
    assert!(
        restored
            .events
            .iter()
            .any(|e| matches!(e, DocEvent::RestoreProblem(_)))
    );
}

#[test]
fn recent_menu_must_preserve_the_windows_path() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let mut units: Vec<u16> = r"C:\notes\".encode_utf16().collect();
    units.push(0xd800);
    units.extend(".txt".encode_utf16());
    let original = std::path::PathBuf::from(std::ffi::OsString::from_wide(&units));
    let rows = qe_docs::recent_menu_rows(&[original]);
    let action = qe_docs::recent_menu_path(&rows[0].1).expect("valid exact menu path");
    assert_eq!(
        action.as_os_str().encode_wide().collect::<Vec<_>>(),
        units,
        "the menu action must name the original file"
    );
}

#[test]
fn recreated_file_must_not_be_overwritten_between_watch_checks() {
    let folder = test_folder("final3-delete-return");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "my ");
    std::fs::remove_file(&path).unwrap();
    h.store.app_activated();
    h.until("the deletion is known", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileDeleted { .. }))
    });
    std::fs::write(&path, b"a newly created outside version").unwrap();
    h.events.clear();
    h.store.save(doc).unwrap();
    h.until("save or conflict", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFinished { .. } | DocEvent::Question(_)))
    });
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"a newly created outside version",
        "a recreated file must need overwrite consent"
    );
    assert!(
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::Question(q) if q.kind == QuestionKind::ChangedOnDisk))
    );
    let question = h.question("recreated file question");
    h.events.clear();
    h.store.answer(question.id, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"my old");
}

/// Puts a copy with the same bytes and written time under the name, so the name now has
/// a new file index. FAT, exFAT and some network shares do this on a plain rename.
fn give_a_new_file_index(path: &std::path::Path) {
    let meta = std::fs::metadata(path).unwrap();
    let copy = path.with_extension("moved");
    std::fs::write(&copy, std::fs::read(path).unwrap()).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&copy).unwrap();
    file.set_modified(meta.modified().unwrap()).unwrap();
    drop(file);
    std::fs::rename(&copy, path).unwrap();
}

#[test]
fn a_new_file_index_after_the_swap_leaves_a_saved_document_clean() {
    let folder = test_folder("fix3-new-index");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || give_a_new_file_index(&target)));
    h.store.save(doc).unwrap();
    h.saved(doc);
    *faults.after_swap.lock().unwrap() = None;
    assert!(
        !h.store.is_dirty(doc),
        "a new file index alone is not an outside write"
    );
    // The stamp kept must match the disk, so the next Save goes through with no question.
    h.type_text(doc, "y");
    h.events.clear();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.open_questions().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"xyold");
}

const LOSSY_UTF16: [u8; 7] = [0xff, 0xfe, b'h', 0, b'i', 0, 0x41];
const UTF16_AS_READ: [u8; 6] = [0xff, 0xfe, b'h', 0, b'i', 0];

/// A lossy document whose Save As onto its own file, as UTF-8, waits on the question.
fn asking_about_save_as(
    name: &str,
) -> (
    Harness,
    qe_docs::DocId,
    std::path::PathBuf,
    qe_docs::QuestionId,
    std::sync::Arc<qe_docs::Faults>,
) {
    let folder = test_folder(name);
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.store
        .save_as(doc, path.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let question = h.question("lossy Save As warning");
    assert_eq!(question.kind, QuestionKind::LossyOverwrite);
    (h, doc, path, question.id, faults)
}

#[test]
fn a_later_plain_save_does_not_use_the_target_of_a_failed_answer() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, path, question, faults) = asking_about_save_as("fix3-failed-answer");
    faults.refuse_threads.store(true, Ordering::Relaxed);
    assert!(h.store.answer(question, Answer::Overwrite).is_err());
    faults.refuse_threads.store(false, Ordering::Relaxed);
    assert_eq!(std::fs::read(&path).unwrap(), LOSSY_UTF16);
    // The plain Save is the newest request, so this answer saves in the document's encoding.
    h.store.save(doc).unwrap();
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), UTF16_AS_READ);
}

#[test]
fn a_failed_answer_keeps_its_question_about_the_same_save_as() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, path, question, faults) = asking_about_save_as("fix3-retry-answer");
    faults.refuse_threads.store(true, Ordering::Relaxed);
    assert!(h.store.answer(question, Answer::Overwrite).is_err());
    faults.refuse_threads.store(false, Ordering::Relaxed);
    // The same question is still open, so its answer still means the dialog's encoding.
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"hi");
}

#[test]
fn a_lossy_answer_does_not_write_to_a_name_the_document_has_left() {
    let (mut h, doc, path, question, _faults) = asking_about_save_as("fix3-left-name");
    let other = path.with_file_name("other.txt");
    h.store.save_as(doc, other.clone(), None).unwrap();
    h.saved(doc);
    h.events.clear();
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), LOSSY_UTF16);
    assert_eq!(std::fs::read(&other).unwrap(), UTF16_AS_READ);
}

/// A dirty lossy document whose Save As onto its own file, as UTF-8, met an outside write.
/// The edit keeps the outside write from reloading the text while the question is open.
fn changed_during_lossy_save_as(name: &str) -> (Harness, qe_docs::DocId, std::path::PathBuf) {
    let folder = test_folder(name);
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.store
        .save_as(doc, path.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let lossy = open_question(&mut h, QuestionKind::LossyOverwrite);
    std::fs::write(&path, b"an outside version").unwrap();
    h.store.answer(lossy, Answer::Overwrite).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
    (h, doc, path)
}

fn open_question(h: &mut Harness, kind: QuestionKind) -> qe_docs::QuestionId {
    h.until("the question", |h| {
        h.store.open_questions().iter().any(|q| q.kind == kind)
    });
    let open = h.store.open_questions();
    open.iter().find(|q| q.kind == kind).unwrap().id
}

#[test]
fn save_anyway_after_a_lossy_save_as_keeps_the_dialog_encoding() {
    let (mut h, doc, path) = changed_during_lossy_save_as("fix3-anyway-keeps");
    let changed = open_question(&mut h, QuestionKind::ChangedOnDisk);
    h.events.clear();
    h.store.answer(changed, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"xhi");
    assert_eq!(
        h.store.format(doc).unwrap().encoding,
        qe_api::TextEncoding::Utf8
    );
}

#[test]
fn cancel_of_the_changed_question_drops_the_dialog_encoding() {
    let (mut h, doc, path) = changed_during_lossy_save_as("fix3-anyway-cancel");
    let changed = open_question(&mut h, QuestionKind::ChangedOnDisk);
    h.store.answer(changed, Answer::Cancel).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
    // A later plain Save meets the same outside text, and now means the document's encoding.
    h.store.save(doc).unwrap();
    let changed = open_question(&mut h, QuestionKind::ChangedOnDisk);
    h.events.clear();
    h.store.answer(changed, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        [0xff, 0xfe, b'x', 0, b'h', 0, b'i', 0]
    );
}

// ---- The second review of the save checks.

const OUTSIDE_WAIT: Duration = Duration::from_millis(20);

fn with_faults(folder: &std::path::Path) -> (Harness, std::sync::Arc<qe_docs::Faults>) {
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    (Harness::with_config(config, wakes), faults)
}

fn add_named_stream(path: &std::path::Path) {
    let stream = format!("{}:Zone.Identifier", path.display());
    std::fs::write(stream, b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
}

#[test]
fn a_file_with_a_named_stream_comes_clean_after_each_save() {
    let folder = test_folder("fix4-stream");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc), "the first Save wrote only our text");
    assert!(h.store.open_questions().is_empty());
    h.type_text(doc, "y");
    h.events.clear();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc), "the later Save wrote only our text");
    assert!(h.store.open_questions().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"xyold");
}

#[test]
fn an_outside_write_is_still_caught_on_a_file_with_a_named_stream() {
    let folder = test_folder("fix4-stream-outside");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::thread::sleep(OUTSIDE_WAIT);
        std::fs::write(&target, b"THEM").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.is_dirty(doc), "the text is not on disk");
    h.events.clear();
    h.store.save(doc).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"THEM");
}

#[test]
fn a_session_end_copy_keeps_text_an_outside_write_replaced() {
    let folder = test_folder("fix4-session-end");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let copies = h.store.session_end_copies().expect("a round of copies");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::thread::sleep(OUTSIDE_WAIT);
        std::fs::write(&target, b"THEM").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.is_dirty(doc));
    let failed = copies.write();
    assert!(failed.is_empty(), "{failed:?}");
    let names = autosave_files(&folder.join("root"));
    assert!(!names.is_empty(), "the only copy of the text was skipped");
    let holds = names.iter().any(|name| {
        let file = folder.join("root/session/docs").join(name);
        std::fs::read(file).is_ok_and(|bytes| bytes.windows(4).any(|part| part == b"xold"))
    });
    assert!(holds, "no copy holds the text: {names:?}");
}

#[test]
fn a_save_that_met_another_version_trusts_only_a_file_holding_its_text() {
    let folder = test_folder("fix4-other-version");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.before_replace.lock().unwrap() = Some(Box::new(move || {
        std::fs::write(&target, b"another program").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    let kept = |h: &Harness| {
        h.events.iter().any(|e| {
            matches!(e, DocEvent::SaveFinished { other_version, .. } if other_version.is_some())
        })
    };
    assert!(kept(&h), "the other program's bytes stay in a backup");
    assert!(!h.store.is_dirty(doc), "the name holds our text");
    h.type_text(doc, "y");
    h.events.clear();
    let target = path.clone();
    let again = path.clone();
    *faults.before_replace.lock().unwrap() = Some(Box::new(move || {
        std::fs::write(&target, b"another program").unwrap();
    }));
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::thread::sleep(OUTSIDE_WAIT);
        std::fs::write(&again, b"ZZZZZ").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(kept(&h));
    assert!(
        h.store.is_dirty(doc),
        "the same-size text there is not ours"
    );
    h.events.clear();
    h.store.save(doc).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"ZZZZZ");
}

/// Save As through `alias` of a lossy file asks, and Overwrite then uses the dialog's encoding.
fn alias_asks(
    h: &mut Harness,
    doc: qe_docs::DocId,
    path: &std::path::Path,
    alias: std::path::PathBuf,
) {
    h.events.clear();
    h.store
        .save_as(doc, alias, Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    h.until("save or question", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFinished { .. } | DocEvent::Question(_)))
    });
    assert_eq!(
        std::fs::read(path).unwrap(),
        LOSSY_UTF16,
        "bytes went with no question"
    );
    let question = open_question(h, QuestionKind::LossyOverwrite);
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(path).unwrap(), b"hi");
}

#[test]
fn a_verbatim_name_of_a_lossy_file_still_asks() {
    let folder = test_folder("fix4-verbatim");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    let alias = std::fs::canonicalize(&path).unwrap();
    assert!(alias.to_string_lossy().starts_with(r"\\?\"));
    alias_asks(&mut h, doc, &path, alias);
}

#[test]
fn a_symlink_to_a_lossy_file_still_asks() {
    let folder = test_folder("fix4-symlink");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let link = folder.join("link.txt");
    if let Err(error) = std::os::windows::fs::symlink_file(&path, &link) {
        println!("SKIPPED: this account cannot make a symlink: {error}");
        return;
    }
    if std::fs::read(&link).is_err() {
        println!("SKIPPED: this PC cannot follow a symlink");
        return;
    }
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    alias_asks(&mut h, doc, &path, link);
}

#[test]
fn an_overwrite_answer_during_a_reload_is_refused_and_can_be_given_again() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, path, question, faults) = asking_about_save_as("fix4-reload-busy");
    faults.hold_reload.store(true, Ordering::Release);
    std::fs::write(&path, [0xff, 0xfe, b'h', 0, b'i', 0, b'!', 0, 0x41]).unwrap();
    h.store.app_activated();
    h.until("the reload is under way", |_| {
        faults.reload_is_held.load(Ordering::Acquire)
    });
    assert!(h.store.answer(question, Answer::Overwrite).is_err());
    assert!(h.store.has_waiting_save_as(doc), "the target stays with it");
    assert!(
        h.store.open_questions().iter().any(|q| q.id == question),
        "the question stays open"
    );
    faults.hold_reload.store(false, Ordering::Release);
    h.events.clear();
    h.until("the answer is taken", |h| {
        h.store.answer(question, Answer::Overwrite).is_ok()
    });
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"hi!");
}

#[test]
fn a_failed_overwrite_keeps_the_lossy_question() {
    use std::sync::atomic::Ordering;
    let folder = test_folder("fix4-failed-overwrite");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.store.save(doc).unwrap();
    let question = open_question(&mut h, QuestionKind::LossyOverwrite);
    faults.save_error_code.store(5, Ordering::Relaxed);
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.until("the save fails", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFailed { .. }))
    });
    faults.save_error_code.store(0, Ordering::Relaxed);
    assert!(h.store.format(doc).unwrap().lossy);
    h.events.clear();
    h.store.save(doc).unwrap();
    open_question(&mut h, QuestionKind::LossyOverwrite);
    assert_eq!(std::fs::read(&path).unwrap(), LOSSY_UTF16);
}

#[test]
fn a_plain_save_during_the_changed_question_replaces_the_dialog_encoding() {
    let (mut h, doc, path) = changed_during_lossy_save_as("fix4-newest-save");
    let changed = open_question(&mut h, QuestionKind::ChangedOnDisk);
    h.store.save(doc).unwrap();
    assert!(!h.store.has_waiting_save_as(doc));
    h.events.clear();
    h.until("the answer is taken", |h| {
        h.store.answer(changed, Answer::SaveAnyway).is_ok()
    });
    h.saved(doc);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        [0xff, 0xfe, b'x', 0, b'h', 0, b'i', 0]
    );
}

#[test]
fn a_recreated_read_only_file_asks_about_the_change_first() {
    let folder = test_folder("fix4-recreated-locked");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "my ");
    std::fs::remove_file(&path).unwrap();
    h.store.app_activated();
    h.until("the deletion is known", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileDeleted { .. }))
    });
    std::fs::write(&path, b"theirs").unwrap();
    set_read_only(&path, true);
    h.events.clear();
    h.store.save(doc).unwrap();
    let question = h.question("the save question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
    assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
}

#[test]
fn a_file_made_during_the_temp_write_of_a_deleted_document_asks() {
    let folder = test_folder("fix4-made-during");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "my ");
    std::fs::remove_file(&path).unwrap();
    h.store.app_activated();
    h.until("the deletion is known", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileDeleted { .. }))
    });
    let target = path.clone();
    *faults.before_commit.lock().unwrap() = Some(Box::new(move || {
        std::fs::write(&target, b"theirs").unwrap();
    }));
    h.events.clear();
    h.store.save(doc).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert!(
        !h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFailed { .. })),
        "a plain failure hides the choice"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
}

#[test]
fn each_misplaced_document_gets_its_own_notice() {
    let folder = test_folder("fix4-two-misplaced");
    let root = folder.join("root");
    let mut h = Harness::new(&root);
    let (_, first) = h.store.new_window(false).unwrap();
    h.type_text(first, "first text");
    let (_, second) = h.store.new_window(false).unwrap();
    h.type_text(second, "second text");
    h.quit();
    let manifest = root.join("session/session.txt");
    let damaged = std::fs::read_to_string(&manifest)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("window "))
        .map(|line| {
            if let Some(counts) = line.strip_prefix("end ") {
                let docs = counts.split(' ').nth(1).unwrap();
                return format!("end 0 {docs}");
            }
            if !line.starts_with("doc ") {
                return line.to_owned();
            }
            line.split(' ')
                .map(|field| {
                    if field.starts_with("window=") {
                        "window=999999"
                    } else {
                        field
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&manifest, damaged).unwrap();
    let mut restored = Harness::new(&root);
    restored.until("loads finish", |h| {
        let docs = h.store.all_documents();
        docs.len() >= 2 && docs.iter().all(|d| !h.store.is_loading(*d))
    });
    let texts: Vec<String> = restored
        .store
        .all_documents()
        .iter()
        .map(|d| restored.text(*d))
        .collect();
    assert!(texts.iter().any(|t| t == "first text"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "second text"), "{texts:?}");
    assert_eq!(
        restored.count(|e| matches!(e, DocEvent::RestoreProblem(_))),
        2,
        "one notice for each: {:#?}",
        restored.events
    );
}

#[test]
fn duplicate_drops_the_chosen_target() {
    let (mut h, doc, _path, question, _faults) = asking_about_save_as("fix4-drop-duplicate");
    assert!(h.store.has_waiting_save_as(doc));
    h.store.answer(question, Answer::Duplicate).unwrap();
    assert!(!h.store.has_waiting_save_as(doc));
}

#[test]
fn cancel_drops_the_chosen_target() {
    let (mut h, doc, _path, question, _faults) = asking_about_save_as("fix4-drop-cancel");
    assert!(h.store.has_waiting_save_as(doc));
    h.store.answer(question, Answer::Cancel).unwrap();
    assert!(!h.store.has_waiting_save_as(doc));
}

#[test]
fn revert_drops_the_chosen_target() {
    let (mut h, doc, _path) = changed_during_lossy_save_as("fix4-drop-revert");
    let changed = open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert!(h.store.has_waiting_save_as(doc));
    h.store.answer(changed, Answer::Revert).unwrap();
    assert!(!h.store.has_waiting_save_as(doc));
}

#[test]
fn a_lossy_save_as_onto_a_recreated_file_asks_both_questions() {
    let folder = test_folder("fix4-lossy-recreated");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    std::fs::remove_file(&path).unwrap();
    h.store.app_activated();
    h.until("the deletion is known", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileDeleted { .. }))
    });
    std::fs::write(&path, b"an outside version").unwrap();
    h.store
        .save_as(doc, path.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let lossy = open_question(&mut h, QuestionKind::LossyOverwrite);
    h.store.answer(lossy, Answer::Overwrite).unwrap();
    let changed = open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
    h.events.clear();
    h.store.answer(changed, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"xhi");
}

#[test]
fn a_write_right_after_the_codec_swap_is_caught() {
    let folder = test_folder("fix4-after-commit");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_commit.lock().unwrap() = Some(Box::new(move || {
        std::thread::sleep(OUTSIDE_WAIT);
        std::fs::write(&target, b"THEM").unwrap();
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.is_dirty(doc), "the text there is not ours");
    h.events.clear();
    h.store.save(doc).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"THEM");
}

#[test]
fn recent_menu_path_refuses_damaged_data() {
    for bad in ["", "abc", "zzzz", "+123", "00é0"] {
        assert_eq!(qe_docs::recent_menu_path(bad), None, "{bad:?}");
    }
    assert_eq!(
        qe_docs::recent_menu_path("0041"),
        Some(std::path::PathBuf::from("A"))
    );
}

// ---- The third review of the save checks.

fn hold_exclusively(path: &std::path::Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .unwrap()
}

type HeldFile = std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>>;

/// After the next swap, the file denies reading until the test empties the slot.
fn hold_after_swap(faults: &qe_docs::Faults, path: &std::path::Path) -> HeldFile {
    let held: HeldFile = HeldFile::default();
    let (slot, target) = (held.clone(), path.to_path_buf());
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        *slot.lock().unwrap() = Some(hold_exclusively(&target));
    }));
    held
}

fn outside_write_after_swap(faults: &qe_docs::Faults, path: &std::path::Path) {
    let target = path.to_path_buf();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::thread::sleep(OUTSIDE_WAIT);
        std::fs::write(&target, b"THEM").unwrap();
    }));
}

fn was_unconfirmed(h: &Harness) -> bool {
    h.events.iter().any(|e| {
        matches!(
            e,
            DocEvent::SaveFinished {
                unconfirmed: true,
                ..
            }
        )
    })
}

fn copy_holds(folder: &std::path::Path, text: &[u8]) -> bool {
    autosave_files(&folder.join("root")).iter().any(|name| {
        let file = folder.join("root/session/docs").join(name);
        std::fs::read(file).is_ok_and(|bytes| bytes.windows(text.len()).any(|part| part == text))
    })
}

#[test]
fn a_repeated_save_that_ends_untrusted_still_gets_its_shutdown_copy() {
    let folder = test_folder("fix5-repeat");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let copies = h.store.session_end_copies().expect("a round of copies");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc));
    outside_write_after_swap(&faults, &path);
    h.events.clear();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.is_dirty(doc), "the text is not on disk any more");
    let failed = copies.write();
    assert!(failed.is_empty(), "{failed:?}");
    assert!(copy_holds(&folder, b"xold"), "the only copy was skipped");
}

#[test]
fn a_short_hold_after_the_swap_is_waited_out() {
    let folder = test_folder("fix5-short-hold");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let target = path.clone();
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        let held = hold_exclusively(&target);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            drop(held);
        });
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!was_unconfirmed(&h));
    assert!(!h.store.is_dirty(doc), "the read came back after the hold");
    h.type_text(doc, "y");
    h.events.clear();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.open_questions().is_empty());
}

#[test]
fn a_hold_that_lasts_keeps_the_text_dirty_says_so_and_asks_at_the_next_save() {
    let folder = test_folder("fix5-long-hold");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let copies = h.store.session_end_copies().expect("a round of copies");
    let held = hold_after_swap(&faults, &path);
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(was_unconfirmed(&h), "the person hears about it at once");
    assert!(h.store.is_dirty(doc), "the save is not confirmed");
    assert!(copies.write().is_empty());
    assert!(copy_holds(&folder, b"xold"), "its shutdown copy stays");
    *held.lock().unwrap() = None;
    h.events.clear();
    h.store.save(doc).unwrap();
    let question = open_question(&mut h, QuestionKind::ChangedOnDisk);
    h.store.answer(question, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc), "Save Anyway clears it");
    assert_eq!(std::fs::read(&path).unwrap(), b"xold");
}

#[test]
fn a_close_that_saves_to_an_unreadable_file_can_end_through_dont_save() {
    let folder = test_folder("fix5-close-hold");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    // A second document keeps the window open.
    let other = h.open(&folder.join("other.txt"));
    assert_ne!(doc, other);
    h.type_text(doc, "x");
    let held = hold_after_swap(&faults, &path);
    h.store.request_close(doc);
    let question = open_question(&mut h, QuestionKind::SaveBeforeClose);
    h.store.answer(question, Answer::Save).unwrap();
    h.saved(doc);
    assert!(was_unconfirmed(&h));
    let question = open_question(&mut h, QuestionKind::SaveBeforeClose);
    h.store.answer(question, Answer::DontSave).unwrap();
    h.until("the document closes", |h| {
        !h.store.all_documents().contains(&doc)
    });
    drop(held);
}

#[test]
fn a_plain_overwrite_during_a_save_is_refused_and_keeps_its_question() {
    use std::sync::atomic::Ordering;
    let folder = test_folder("fix5-plain-busy");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.store.save(doc).unwrap();
    let question = open_question(&mut h, QuestionKind::LossyOverwrite);
    faults.hold_save.store(true, Ordering::Release);
    h.store
        .save_as(doc, folder.join("other.txt"), None)
        .unwrap();
    h.until("the save is held", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    assert!(h.store.answer(question, Answer::Overwrite).is_err());
    assert!(h.store.open_questions().iter().any(|q| q.id == question));
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), LOSSY_UTF16);
}

#[test]
fn a_target_overwrite_during_a_save_is_refused_and_keeps_its_target() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, path, question, faults) = asking_about_save_as("fix5-target-busy");
    faults.hold_save.store(true, Ordering::Release);
    let other = path.with_file_name("other.txt");
    h.store.save_as(doc, other, None).unwrap();
    h.until("the save is held", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    assert!(h.store.answer(question, Answer::Overwrite).is_err());
    assert!(h.store.open_questions().iter().any(|q| q.id == question));
    assert!(h.store.has_waiting_save_as(doc));
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), LOSSY_UTF16);
}

#[test]
fn a_save_anyway_during_a_save_is_refused_and_keeps_its_question() {
    use std::sync::atomic::Ordering;
    let folder = test_folder("fix5-anyway-busy");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    std::fs::write(&path, b"an outside version").unwrap();
    h.store.save(doc).unwrap();
    let question = open_question(&mut h, QuestionKind::ChangedOnDisk);
    faults.hold_save.store(true, Ordering::Release);
    h.store
        .save_as(doc, folder.join("other.txt"), None)
        .unwrap();
    h.until("the save is held", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    assert!(h.store.answer(question, Answer::SaveAnyway).is_err());
    assert!(h.store.open_questions().iter().any(|q| q.id == question));
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
}

#[test]
fn a_plain_save_during_a_save_as_beats_its_changed_result() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, path, question, faults) = asking_about_save_as("fix5-newest-changed");
    faults.hold_save.store(true, Ordering::Release);
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.until("the save is held", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.store.save(doc).unwrap();
    std::fs::write(&path, b"an outside version").unwrap();
    faults.hold_save.store(false, Ordering::Release);
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert!(
        !h.store.has_waiting_save_as(doc),
        "the older choice came back"
    );
}

#[test]
fn a_plain_save_during_a_save_as_beats_its_own_file_result() {
    use std::sync::atomic::Ordering;
    let folder = test_folder("fix5-newest-own");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    let alias = std::fs::canonicalize(&path).unwrap();
    faults.hold_save.store(true, Ordering::Release);
    h.store
        .save_as(doc, alias, Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    h.until("the save is held", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.store.save(doc).unwrap();
    faults.hold_save.store(false, Ordering::Release);
    let question = open_question(&mut h, QuestionKind::LossyOverwrite);
    assert!(
        !h.store.has_waiting_save_as(doc),
        "the older choice came back"
    );
    h.store.answer(question, Answer::Overwrite).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), UTF16_AS_READ);
}

#[test]
fn file_revert_drops_the_chosen_target() {
    let (mut h, doc, _path, _question, _faults) = asking_about_save_as("fix5-file-revert");
    assert!(h.store.has_waiting_save_as(doc));
    h.store.request_revert(doc).unwrap();
    for question in h.store.open_questions() {
        if question.kind == QuestionKind::ConfirmRevert {
            h.store.answer(question.id, Answer::Revert).unwrap();
        }
    }
    assert!(!h.store.has_waiting_save_as(doc));
}

#[test]
fn an_overwrite_through_another_name_keeps_its_stamp_check() {
    let folder = test_folder("fix5-alias-stamp");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let alias = std::fs::canonicalize(&path).unwrap();
    h.store
        .save_as(doc, alias, Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let lossy = open_question(&mut h, QuestionKind::LossyOverwrite);
    std::fs::write(&path, b"an outside version").unwrap();
    h.store.answer(lossy, Answer::Overwrite).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
}

#[test]
fn an_overwrite_asks_when_a_stat_has_adopted_a_recreated_file() {
    let folder = test_folder("fix5-adopted");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    std::fs::remove_file(&path).unwrap();
    h.store.app_activated();
    h.until("the deletion is known", |h| {
        h.store.title_note(doc).as_deref() == Some("Deleted")
    });
    std::fs::write(&path, b"an outside version").unwrap();
    h.store.app_activated();
    h.until("the stat has adopted the new file", |h| {
        h.store.title_note(doc).is_none()
    });
    h.store
        .save_as(doc, path.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let lossy = open_question(&mut h, QuestionKind::LossyOverwrite);
    h.store.answer(lossy, Answer::Overwrite).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
}

#[test]
fn an_8_3_name_of_a_lossy_file_still_asks() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    use windows::core::PCWSTR;
    let folder = test_folder("fix5-short-name");
    let path = folder.join("a long file name for the test.text");
    write(&path, &LOSSY_UTF16);
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut short = vec![0u16; 1024];
    // SAFETY: the name ends in a zero, and the buffer outlives the call.
    let got = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut short)) } as usize;
    assert!(got > 0 && got < short.len(), "GetShortPathNameW failed");
    let alias = std::path::PathBuf::from(std::ffi::OsString::from_wide(&short[..got]));
    if alias.file_name() == path.file_name() {
        println!("SKIPPED: 8.3 names are off on this volume");
        return;
    }
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    alias_asks(&mut h, doc, &path, alias);
}

#[test]
fn another_name_of_a_replaced_file_asks_before_the_stat_has_seen_it() {
    let folder = test_folder("fix5-replaced-alias");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let alias = std::fs::canonicalize(&path).unwrap();
    // A new file under the old name, with no stat yet: it has another identity.
    let outside = path.with_extension("new");
    std::fs::write(&outside, b"an outside version").unwrap();
    std::fs::rename(&outside, &path).unwrap();
    h.store
        .save_as(doc, alias, Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let lossy = open_question(&mut h, QuestionKind::LossyOverwrite);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
    h.store.answer(lossy, Answer::Overwrite).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
}

#[test]
fn a_target_that_cannot_be_checked_asks_the_lossy_question() {
    let (mut h, doc, path) = {
        let folder = test_folder("fix5-cannot-tell");
        let path = folder.join("f.txt");
        write(&path, &LOSSY_UTF16);
        let mut h = Harness::new(&folder.join("root"));
        let doc = h.open(&path);
        (h, doc, path)
    };
    let bad = path.with_file_name("a?b.txt");
    h.store
        .save_as(doc, bad, Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    open_question(&mut h, QuestionKind::LossyOverwrite);
    assert_eq!(std::fs::read(&path).unwrap(), LOSSY_UTF16);
}

// ---- The fourth review of the save checks.

#[test]
fn an_outside_write_under_a_lasting_hold_is_not_overwritten_silently() {
    let folder = test_folder("fix6-write-under-hold");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let held = HeldFile::default();
    let (slot, target) = (held.clone(), path.clone());
    *faults.after_swap.lock().unwrap() = Some(Box::new(move || {
        std::thread::sleep(OUTSIDE_WAIT);
        std::fs::write(&target, b"THEM").unwrap();
        *slot.lock().unwrap() = Some(hold_exclusively(&target));
    }));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(was_unconfirmed(&h), "the notice still shows");
    assert!(h.store.is_dirty(doc));
    *held.lock().unwrap() = None;
    h.events.clear();
    h.store.save(doc).unwrap();
    let question = open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"THEM",
        "the other program's bytes stay until the person agrees"
    );
    h.store.answer(question, Answer::SaveAnyway).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"xold");
}

/// A lossy document, and another file that exists, as bytes the document never saw.
fn lossy_and_another_file(
    name: &str,
) -> (
    Harness,
    qe_docs::DocId,
    std::path::PathBuf,
    std::sync::Arc<qe_docs::Faults>,
) {
    let folder = test_folder(name);
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let other = folder.join("other.txt");
    write(&other, b"another file");
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    (h, doc, other, faults)
}

#[test]
fn a_changed_result_keeps_the_lossy_mark_when_the_target_was_not_known_to_be_own() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, other, faults) = lossy_and_another_file("fix6-unsure-lossy");
    faults.fail_target_check.store(5, Ordering::Relaxed);
    h.store
        .save_as(doc, other.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    let lossy = open_question(&mut h, QuestionKind::LossyOverwrite);
    faults.fail_target_check.store(0, Ordering::Relaxed);
    h.store.answer(lossy, Answer::Overwrite).unwrap();
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&other).unwrap(), b"another file");
    assert!(
        h.store.format(doc).unwrap().lossy,
        "the document's own file was never touched, so its warning still holds"
    );
}

#[test]
fn a_changed_result_clears_the_lossy_mark_when_the_target_was_the_own_file() {
    let (mut h, doc, path) = changed_during_lossy_save_as("fix6-sure-lossy");
    open_question(&mut h, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"an outside version");
    assert!(!h.store.format(doc).unwrap().lossy);
}

#[test]
fn a_repeated_save_that_ends_unconfirmed_still_gets_its_shutdown_copy() {
    let folder = test_folder("fix6-repeat-unconfirmed");
    let path = folder.join("f.txt");
    write(&path, b"old");
    add_named_stream(&path);
    let (mut h, faults) = with_faults(&folder);
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let copies = h.store.session_end_copies().expect("a round of copies");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc), "the first save is confirmed");
    let held = hold_after_swap(&faults, &path);
    h.events.clear();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(was_unconfirmed(&h));
    assert!(copies.write().is_empty());
    assert!(copy_holds(&folder, b"xold"), "the only copy was skipped");
    drop(held);
}

#[test]
fn save_as_onto_the_new_name_of_a_file_another_program_renamed_asks() {
    let folder = test_folder("fix6-renamed");
    let path = folder.join("f.txt");
    write(&path, &LOSSY_UTF16);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let renamed = folder.join("renamed.txt");
    std::fs::rename(&path, &renamed).unwrap();
    h.store
        .save_as(doc, renamed.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    open_question(&mut h, QuestionKind::LossyOverwrite);
    assert_eq!(std::fs::read(&renamed).unwrap(), LOSSY_UTF16);
}

#[test]
fn an_own_name_that_cannot_be_checked_asks_the_lossy_question() {
    use std::sync::atomic::Ordering;
    let (mut h, doc, other, faults) = lossy_and_another_file("fix6-own-name-error");
    faults.fail_own_name_check.store(5, Ordering::Relaxed);
    h.store
        .save_as(doc, other.clone(), Some(qe_api::TextEncoding::Utf8))
        .unwrap();
    open_question(&mut h, QuestionKind::LossyOverwrite);
    assert_eq!(std::fs::read(&other).unwrap(), b"another file");
}
