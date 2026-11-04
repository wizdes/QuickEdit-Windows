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
