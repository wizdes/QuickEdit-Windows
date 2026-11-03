//! Saving: exact bytes, dirty state, typing during a save, failures that lose nothing, the
//! questions, and the other file commands.
mod support;

use qe_api::{ByteOffset, EncodingChoice, InputEvent, InputKind, TextEncoding, View, ViewContext};
use qe_docs::{Answer, DocErrorKind, DocEvent, PanelPurpose, QuestionKind};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::Ordering;
use support::*;

fn failure(h: &mut Harness) -> (DocErrorKind, bool) {
    h.until("the save fails", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::SaveFailed { .. }))
    });
    h.events
        .iter()
        .find_map(|e| match e {
            DocEvent::SaveFailed {
                error,
                offer_save_as,
                ..
            } => Some((error.kind, *offer_save_as)),
            _ => None,
        })
        .unwrap()
}

/// Opens, types at the end, saves, and returns the bytes on disk.
fn round_trip(name: &str, bytes: &[u8], typed: &str) -> Vec<u8> {
    let folder = test_folder(&format!("save-rt-{name}"));
    let path = folder.join("f.txt");
    write(&path, bytes);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.caret_to_end(doc);
    h.type_text(doc, typed);
    assert!(h.store.is_dirty(doc));
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc));
    std::fs::read(&path).unwrap()
}

#[test]
fn line_endings_bom_and_every_kind_of_text_come_back_byte_for_byte() {
    assert_eq!(round_trip("crlf", b"a\r\nb\r\n", "c"), b"a\r\nb\r\nc");
    assert_eq!(
        round_trip("mixed", b"a\rb\nc\r\nd\n", "e"),
        b"a\rb\nc\r\nd\ne"
    );
    assert_eq!(
        round_trip("bom", b"\xEF\xBB\xBFx\n", "y"),
        b"\xEF\xBB\xBFx\ny"
    );
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} e\u{301}\n";
    assert_eq!(
        round_trip("emoji", family.as_bytes(), "\u{1f1ef}\u{1f1f5}"),
        format!("{family}\u{1f1ef}\u{1f1f5}").as_bytes()
    );
    assert_eq!(round_trip("empty", b"", "z"), b"z");
    assert_eq!(
        round_trip("utf16be", &[0xFE, 0xFF, 0, b'h'], "i"),
        &[0xFE, 0xFF, 0, b'h', 0, b'i']
    );
}

#[test]
fn a_legacy_encoding_stays_on_save() {
    let folder = test_folder("save-legacy");
    let path = folder.join("f.txt");
    write(&path, &[b'c', 0xE9]);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open_with(&path, EncodingChoice::Specific(TextEncoding::Windows1252));
    h.caret_to_end(doc);
    h.type_text(doc, "\u{e8}");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), &[b'c', 0xE9, 0xE8]);
}

#[test]
fn a_new_document_asks_for_a_name_then_writes_utf8_with_no_bom() {
    let folder = test_folder("save-new");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    assert!(!h.store.is_dirty(doc), "an untouched document is clean");
    h.type_text(doc, "h\u{e9}");
    h.store.save(doc).unwrap();
    h.pump();
    let request = h
        .events
        .iter()
        .find_map(|e| match e {
            DocEvent::NeedsSavePanel(request) => Some(request.clone()),
            _ => None,
        })
        .expect("the dialog is asked for");
    assert_eq!(request.purpose, PanelPurpose::Save);
    assert_eq!(request.suggested_name, "Untitled.txt");
    assert_eq!(request.initial_encoding, TextEncoding::Utf8);
    assert!(request.add_txt);
    let path = folder.join("new.txt");
    h.store.save_as(doc, path.clone(), None).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), "h\u{e9}".as_bytes());
    assert!(!h.store.is_dirty(doc));
    assert_eq!(h.store.display_name(doc), "new.txt");
    assert_eq!(h.store.recent()[0], path);
}

#[test]
fn dirty_follows_the_undo_position() {
    let folder = test_folder("save-undo");
    let path = folder.join("f.txt");
    write(&path, b"base");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    assert!(h.store.is_dirty(doc));
    h.undo(doc);
    assert!(!h.store.is_dirty(doc), "back at the save point");
    h.type_text(doc, "y");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc));
    h.undo(doc);
    assert!(h.store.is_dirty(doc), "behind the save point");
}

#[test]
fn typing_during_a_save_stays_unsaved_and_disk_has_the_known_snapshot() {
    let folder = test_folder("save-typing");
    let path = folder.join("f.txt");
    write(&path, b"one");
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.caret_to_end(doc);
    h.type_text(doc, "A");
    faults.hold_save.store(true, Ordering::Release);
    h.store.save(doc).unwrap();
    h.until("the save holds", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.type_text(doc, "B");
    faults.hold_save.store(false, Ordering::Release);
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"oneA", "what the save took");
    assert_eq!(h.text(doc), "oneAB");
    assert!(h.store.is_dirty(doc), "B is not on disk");
    let still = h.events.iter().find_map(|e| match e {
        DocEvent::SaveFinished { still_dirty, .. } => Some(*still_dirty),
        _ => None,
    });
    assert_eq!(still, Some(true));
    // Undo to the written text does not make it clean: the view cannot name that state.
    h.undo(doc);
    assert!(h.store.is_dirty(doc));
    h.clear_events();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn a_second_save_during_a_save_runs_at_its_end_with_the_newer_text() {
    let folder = test_folder("save-again");
    let path = folder.join("f.txt");
    write(&path, b"");
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "A");
    faults.hold_save.store(true, Ordering::Release);
    h.store.save(doc).unwrap();
    h.until("the save holds", |_| {
        faults.save_is_held.load(Ordering::Acquire)
    });
    h.type_text(doc, "B");
    h.store.save(doc).unwrap();
    faults.hold_save.store(false, Ordering::Release);
    h.until("both saves end", |h| {
        !h.store.is_saving(doc) && !h.store.is_dirty(doc)
    });
    assert_eq!(std::fs::read(&path).unwrap(), b"AB");
}

#[test]
fn queued_typing_goes_in_before_a_save() {
    let folder = test_folder("save-flush");
    let path = folder.join("f.txt");
    write(&path, "\u{4e2d}".repeat(400).as_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    // Home on a wrapped line with no layout waits for it, and typing behind it waits too.
    let measure = qe_layout::DirectWriteLayout::new().unwrap();
    let mut ctx = ViewContext::default();
    let view = h.store.view_mut(doc).unwrap();
    view.set_wrap(true).unwrap();
    view.layout(
        qe_api::Rect {
            x: qe_api::Dip(0.0),
            y: qe_api::Dip(0.0),
            width: qe_api::Dip(300.0),
            height: qe_api::Dip(600.0),
        },
        &measure,
    );
    view.test_pause_workers(true);
    view.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: 0x24,
                modifiers: qe_api::Modifiers::default(),
                repeat: false,
            },
        },
        &mut ctx,
    );
    view.handle(
        &InputEvent {
            time_us: 1,
            kind: InputKind::Char('Q'),
        },
        &mut ctx,
    );
    assert!(!view.text().unwrap().contains('Q'), "the key really waits");
    h.store.save(doc).unwrap();
    h.saved(doc);
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.contains(&b'Q'), "the typed key reached the file");
    assert!(!h.store.is_dirty(doc), "and the view agrees");
}

#[test]
fn a_failed_write_keeps_the_file_the_text_and_the_dirty_mark_and_names_the_error() {
    let folder = test_folder("save-full");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "new");
    faults.save_error_code.store(112, Ordering::Relaxed);
    h.store.save(doc).unwrap();
    let (kind, offer) = failure(&mut h);
    assert_eq!(kind, DocErrorKind::DiskFull);
    assert!(offer, "Save As to another disk can still work");
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.text(doc), "newold");
    faults.save_error_code.store(0, Ordering::Relaxed);
    h.clear_events();
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"newold");
}

#[test]
fn a_file_another_program_holds_gives_a_named_error_and_stays_whole() {
    let folder = test_folder("save-held");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    h.store.save(doc).unwrap();
    let (kind, offer) = failure(&mut h);
    assert!(
        matches!(kind, DocErrorKind::FileInUse | DocErrorKind::AccessDenied),
        "{kind:?}"
    );
    assert!(offer);
    drop(held);
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert!(h.store.is_dirty(doc));
}

#[test]
fn a_save_into_a_missing_folder_gives_a_named_error() {
    let folder = test_folder("save-no-folder");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "x");
    h.store
        .save_as(doc, folder.join("nope").join("f.txt"), None)
        .unwrap();
    let (kind, _) = failure(&mut h);
    assert_ne!(kind, DocErrorKind::Cancelled);
    assert!(h.store.is_dirty(doc));
    assert_eq!(
        h.store.path(doc),
        None,
        "the document did not take that name"
    );
}

#[test]
fn a_file_changed_during_a_save_asks_and_each_answer_does_what_it_says() {
    for answer in [Answer::Cancel, Answer::SaveAnyway, Answer::Revert] {
        let folder = test_folder(&format!("save-changed-{answer:?}"));
        let path = folder.join("f.txt");
        write(&path, b"old");
        let (config, wakes) = config(&folder.join("root"));
        let faults = config.faults.clone();
        let mut h = Harness::with_config(config, wakes);
        let doc = h.open(&path);
        h.type_text(doc, "mine ");
        let target = path.clone();
        *faults.before_swap.lock().unwrap() = Some(Box::new(move || {
            std::fs::write(&target, b"theirs, longer").unwrap();
        }));
        h.store.save(doc).unwrap();
        let question = h.question("the conflict question");
        assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"theirs, longer",
            "their bytes stay until the user answers"
        );
        assert!(h.store.is_dirty(doc));
        h.clear_events();
        h.store.answer(question.id, answer).unwrap();
        match answer {
            Answer::SaveAnyway => {
                h.saved(doc);
                assert_eq!(std::fs::read(&path).unwrap(), b"mine old");
                assert!(!h.store.is_dirty(doc));
            }
            Answer::Revert => {
                h.until("the revert ends", |h| !h.store.is_dirty(doc));
                assert_eq!(h.text(doc), "theirs, longer");
            }
            _ => {
                h.settle(std::time::Duration::from_millis(20));
                assert_eq!(std::fs::read(&path).unwrap(), b"theirs, longer");
                assert_eq!(h.text(doc), "mine old");
                assert!(h.store.is_dirty(doc));
            }
        }
    }
}

#[test]
fn a_change_since_the_open_asks_at_save_even_with_no_watch() {
    let folder = test_folder("save-stale");
    let path = folder.join("f.txt");
    write(&path, b"old");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&path, b"changed by another").unwrap();
    h.store.save(doc).unwrap();
    let question = h.question("the conflict question");
    assert_eq!(question.kind, QuestionKind::ChangedOnDisk);
    assert_eq!(std::fs::read(&path).unwrap(), b"changed by another");
}

/// A BOM and an odd last byte make a file that cannot write back the same bytes.
fn lossy_bytes() -> Vec<u8> {
    vec![0xFF, 0xFE, b'h', 0, b'i', 0, 0x41]
}

#[test]
fn the_first_edit_of_a_lossy_file_asks_and_cancel_takes_it_back() {
    let folder = test_folder("save-lossy-cancel");
    let path = folder.join("f.txt");
    write(&path, &lossy_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    assert!(h.store.format(doc).unwrap().lossy);
    h.type_text(doc, "x");
    let question = h.question("the lossy question");
    assert_eq!(question.kind, QuestionKind::LossyFirstEdit);
    assert_eq!(
        question.choices,
        vec![Answer::Duplicate, Answer::Cancel, Answer::Overwrite]
    );
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), "hi");
    assert!(!h.store.is_dirty(doc));
    assert_eq!(std::fs::read(&path).unwrap(), lossy_bytes());
}

#[test]
fn the_first_edit_duplicate_copies_with_the_edit_and_the_file_stays() {
    let folder = test_folder("save-lossy-dup");
    let path = folder.join("f.txt");
    write(&path, &lossy_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let question = h.question("the lossy question");
    h.store.answer(question.id, Answer::Duplicate).unwrap();
    assert_eq!(h.text(doc), "hi", "the edit goes from the original");
    let copy = *h.store.all_documents().iter().find(|d| **d != doc).unwrap();
    h.loaded(copy);
    assert_eq!(h.text(copy), "xhi", "the copy has the edit");
    assert_eq!(h.store.path(copy), None);
    assert!(h.store.is_dirty(copy));
    assert_eq!(std::fs::read(&path).unwrap(), lossy_bytes());
}

#[test]
fn overwrite_lets_later_saves_write_with_no_second_question() {
    let folder = test_folder("save-lossy-over");
    let path = folder.join("f.txt");
    write(&path, &lossy_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let question = h.question("the lossy question");
    h.store.answer(question.id, Answer::Overwrite).unwrap();
    assert_eq!(h.text(doc), "xhi");
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert!(h.store.open_questions().is_empty());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
    assert_eq!(bytes.len(), 8, "BOM and three units: {bytes:?}");
}

#[test]
fn a_save_over_a_lossy_file_asks_first_and_cancel_keeps_the_bytes() {
    let folder = test_folder("save-lossy-save");
    let path = folder.join("f.txt");
    write(&path, &lossy_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.store.save(doc).unwrap();
    let question = h.question("the lossy question");
    assert_eq!(question.kind, QuestionKind::LossyOverwrite);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.settle(std::time::Duration::from_millis(20));
    assert_eq!(std::fs::read(&path).unwrap(), lossy_bytes());
    // Save As onto the same file asks too, and to a new name it does not.
    h.store.save_as(doc, path.clone(), None).unwrap();
    let question = h.question("the lossy question for Save As");
    assert_eq!(question.kind, QuestionKind::LossyOverwrite);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.clear_events();
    let other = folder.join("other.txt");
    h.store.save_as(doc, other.clone(), None).unwrap();
    h.saved(doc);
    assert!(h.store.open_questions().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), lossy_bytes());
    assert!(other.exists());
}

#[test]
fn text_a_legacy_encoding_cannot_hold_asks_at_the_edit_and_cancel_undoes() {
    let folder = test_folder("save-unfit-edit");
    let path = folder.join("f.txt");
    write(&path, b"abc");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open_with(&path, EncodingChoice::Specific(TextEncoding::Windows1252));
    h.type_text(doc, "\u{4e2d}");
    let question = h.question("the encoding question");
    assert_eq!(
        question.kind,
        QuestionKind::EncodingNoLongerFits { on_edit: true }
    );
    assert!(
        question.title.contains("Windows-1252"),
        "{}",
        question.title
    );
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), "abc");
    assert!(!h.store.is_dirty(doc));
    // OK opens the dialog, and cancelling that takes the edit back as well.
    h.type_text(doc, "\u{4e2d}");
    let question = h.question("the encoding question");
    h.clear_events();
    h.store.answer(question.id, Answer::ChooseEncoding).unwrap();
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::NeedsSavePanel(_))),
        1,
        "{:?}",
        h.events
    );
    h.store.panel_cancelled(doc);
    assert_eq!(h.text(doc), "abc");
}

#[test]
fn an_untitled_document_and_a_unicode_file_are_not_checked_at_the_edit() {
    let folder = test_folder("save-unfit-skip");
    let path = folder.join("f.txt");
    write(&path, b"abc");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "\u{4e2d}");
    let (_, untitled) = h.store.new_window(false).unwrap();
    h.type_text(untitled, "\u{4e2d}");
    assert!(h.store.open_questions().is_empty());
}

#[test]
fn a_long_paste_that_does_not_fit_is_caught_at_the_save_with_the_file_intact() {
    let folder = test_folder("save-unfit-save");
    let path = folder.join("f.txt");
    write(&path, b"abc");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open_with(&path, EncodingChoice::Specific(TextEncoding::Windows1252));
    let long = "\u{4e2d}".repeat(30_000);
    let mut ctx = ViewContext::default();
    h.store
        .view_mut(doc)
        .unwrap()
        .replace_range(ByteOffset(0)..ByteOffset(0), &long, &mut ctx);
    h.store.note_view_changed(doc);
    assert!(
        h.store.open_questions().is_empty(),
        "too long to check here"
    );
    h.store.save(doc).unwrap();
    let question = h.question("the encoding question");
    assert_eq!(
        question.kind,
        QuestionKind::EncodingNoLongerFits { on_edit: false }
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    assert!(h.store.is_dirty(doc));
}

#[test]
fn a_locked_file_offers_unlock_or_duplicate_and_keeps_its_bytes_until_the_user_picks() {
    let folder = test_folder("save-locked");
    let path = folder.join("f.txt");
    write(&path, b"keep me");
    set_read_only(&path, true);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    let question = h.question("the locked question");
    assert_eq!(question.kind, QuestionKind::LockedFile { on_edit: true });
    assert_eq!(
        question.choices,
        vec![Answer::Unlock, Answer::Duplicate, Answer::Cancel]
    );
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.text(doc), "keep me", "Cancel takes the edit back");
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me");

    h.type_text(doc, "x");
    let question = h.question("the locked question again");
    h.store.answer(question.id, Answer::Unlock).unwrap();
    h.until("the mark goes", |_| {
        !std::fs::metadata(&path).unwrap().permissions().readonly()
    });
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"xkeep me");
}

#[test]
fn a_mark_set_after_the_open_stops_the_save_with_the_same_choice() {
    let folder = test_folder("save-locked-late");
    let path = folder.join("f.txt");
    write(&path, b"keep me");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    set_read_only(&path, true);
    h.store.save(doc).unwrap();
    let question = h.question("the locked question");
    assert_eq!(question.kind, QuestionKind::LockedFile { on_edit: false });
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    h.store.answer(question.id, Answer::Unlock).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"xkeep me");
}

#[test]
fn save_as_changes_the_encoding_and_a_revert_reads_it_back_the_same_way() {
    let folder = test_folder("save-as-encoding");
    let path = folder.join("f.txt");
    write(&path, "caf\u{e9}".as_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    let other = folder.join("g.txt");
    h.store
        .save_as(doc, other.clone(), Some(TextEncoding::Windows1252))
        .unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&other).unwrap(), &[b'c', b'a', b'f', 0xE9]);
    assert_eq!(h.store.path(doc).unwrap(), other);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "caf\u{e9}".as_bytes(),
        "the old file stays"
    );
    h.type_text(doc, "x");
    h.store.request_revert(doc).unwrap();
    let question = h.question("the revert question");
    h.store.answer(question.id, Answer::Revert).unwrap();
    h.until("the revert ends", |h| !h.store.is_dirty(doc));
    assert_eq!(h.text(doc), "caf\u{e9}");
    assert_eq!(
        h.store.format(doc).unwrap().encoding,
        TextEncoding::Windows1252
    );
}

#[test]
fn save_as_with_an_encoding_that_cannot_hold_the_text_names_the_error() {
    let folder = test_folder("save-as-unfit");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "\u{4e2d}");
    let path = folder.join("g.txt");
    h.store
        .save_as(doc, path.clone(), Some(TextEncoding::Windows1252))
        .unwrap();
    let (kind, offer) = failure(&mut h);
    assert_eq!(kind, DocErrorKind::EncodingDoesNotFit);
    assert!(offer);
    assert!(!path.exists());
    assert!(h.store.is_dirty(doc));
}

#[test]
fn save_as_over_a_file_open_in_another_tab_is_refused() {
    let folder = test_folder("save-as-open");
    let path = folder.join("f.txt");
    write(&path, b"other tab");
    let mut h = Harness::new(&folder.join("root"));
    h.open(&path);
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "x");
    h.store.save_as(doc, path.clone(), None).unwrap();
    let (kind, _) = failure(&mut h);
    assert_eq!(kind, DocErrorKind::OpenInAnotherTab);
    assert_eq!(std::fs::read(&path).unwrap(), b"other tab");
}

#[test]
fn save_as_on_a_file_too_large_to_edit_copies_the_original_bytes() {
    let folder = test_folder("save-as-large");
    let path = folder.join("huge.txt");
    // Mixed line endings show the bytes were copied, not the text encoded again.
    let bytes: Vec<u8> = b"a\r\nb\nc\r".repeat(400);
    write(&path, &bytes);
    let (mut config, wakes) = config(&folder.join("root"));
    config.read_only_bytes = 1_000;
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    assert!(h.store.is_too_large_to_edit(doc));
    // Over itself is refused at once.
    let same = h.store.save_as(doc, path.clone(), None).unwrap_err();
    assert_eq!(same.kind, DocErrorKind::TooLargeToEdit);
    let copy = folder.join("copy.txt");
    h.store.save_as(doc, copy.clone(), None).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(h.store.path(doc).unwrap(), copy);
    h.until("the view reads the new file", |h| {
        !h.store.is_loading(doc) && h.store.commands(doc).save_as
    });
    assert!(h.store.is_too_large_to_edit(doc));
}

#[test]
fn duplicate_makes_a_dirty_untitled_copy_and_leaves_the_file_alone() {
    let folder = test_folder("save-duplicate");
    let path = folder.join("a b.txt");
    write(&path, b"text\r\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "more ");
    let copy = h.store.duplicate(doc).unwrap();
    h.loaded(copy);
    assert_eq!(h.text(copy), "more text\r\n");
    assert_eq!(h.store.display_name(copy), "a b copy.txt");
    assert_eq!(h.store.path(copy), None);
    assert!(h.store.is_dirty(copy), "a copy has no file to match");
    assert_eq!(
        h.store.format(copy).unwrap().main_line_ending,
        qe_codec::LineEnding::CrLf
    );
    let window = h.store.window_of(doc).unwrap();
    assert_eq!(h.store.tabs(window), vec![doc, copy], "beside its source");
    assert_eq!(std::fs::read(&path).unwrap(), b"text\r\n");
    let second = h.store.duplicate(doc).unwrap();
    assert_eq!(h.store.display_name(second), "a b copy 2.txt");
    h.loaded(second);
    h.store.save(copy).unwrap();
    h.pump();
    let suggested = h.events.iter().find_map(|e| match e {
        DocEvent::NeedsSavePanel(r) => Some(r.suggested_name.clone()),
        _ => None,
    });
    assert_eq!(suggested.as_deref(), Some("a b copy.txt"));
}

#[test]
fn rename_and_move_to_carry_the_file_and_keep_unsaved_text_unsaved() {
    let folder = test_folder("save-rename");
    let path = folder.join("old.txt");
    write(&path, b"disk");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    h.store.rename(doc, "new.txt").unwrap();
    h.until("the rename ends", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileMoved { .. }))
    });
    let renamed = folder.join("new.txt");
    assert_eq!(h.store.path(doc).unwrap(), renamed);
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(&renamed).unwrap(),
        b"disk",
        "the text stays unsaved"
    );
    assert!(h.store.is_dirty(doc));
    assert_eq!(h.store.recent()[0], renamed);
    assert!(!h.store.recent().contains(&path), "the old row goes");
    h.clear_events();
    let moved = folder.join("sub").join("moved.txt");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    h.store.move_to(doc, moved.clone(), false).unwrap();
    h.until("the move ends", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::FileMoved { .. }))
    });
    assert_eq!(h.store.path(doc).unwrap(), moved);
    // A later save goes to the new place, with no question.
    h.store.save(doc).unwrap();
    h.saved(doc);
    assert_eq!(std::fs::read(&moved).unwrap(), b"xdisk");
    assert!(h.store.open_questions().is_empty());
}

#[test]
fn rename_onto_a_file_that_exists_and_a_bad_name_are_named_errors() {
    let folder = test_folder("save-rename-clash");
    let (path, other) = (folder.join("a.txt"), folder.join("b.txt"));
    write(&path, b"a");
    write(&other, b"b");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.store.rename(doc, "b.txt").unwrap();
    h.until("the rename fails", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::MoveFailed { .. }))
    });
    let kind = h.events.iter().find_map(|e| match e {
        DocEvent::MoveFailed { error, .. } => Some(error.kind),
        _ => None,
    });
    assert_eq!(kind, Some(DocErrorKind::TargetExists));
    assert_eq!(h.store.path(doc).unwrap(), path);
    assert_eq!(std::fs::read(&other).unwrap(), b"b");
    assert!(h.store.rename(doc, "a\\b.txt").is_err());
    assert!(h.store.rename(doc, "").is_err());
}

#[test]
fn revert_asks_then_brings_the_file_back_with_the_caret_at_the_start_and_no_undo() {
    let folder = test_folder("save-revert");
    let path = folder.join("f.txt");
    write(&path, b"saved text\n");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.select(doc, 6, 6);
    h.type_text(doc, "EDIT ");
    h.store.request_revert(doc).unwrap();
    let question = h.question("the revert question");
    assert_eq!(question.kind, QuestionKind::ConfirmRevert);
    assert_eq!(
        h.text(doc),
        "saved EDIT text\n",
        "nothing changes before the answer"
    );
    h.store.answer(question.id, Answer::Revert).unwrap();
    h.until("the revert ends", |h| !h.store.is_dirty(doc));
    assert_eq!(h.text(doc), "saved text\n");
    assert_eq!(h.store.view(doc).unwrap().caret(), ByteOffset(0));
    h.undo(doc);
    assert_eq!(
        h.text(doc),
        "saved text\n",
        "undo cannot reach the old text"
    );
}

#[test]
fn a_failed_revert_keeps_the_text_and_the_dirty_mark() {
    let folder = test_folder("save-revert-fail");
    let path = folder.join("f.txt");
    write(&path, b"saved");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.type_text(doc, "x");
    std::fs::remove_file(&path).unwrap();
    h.store.request_revert(doc).unwrap();
    let question = h.question("the revert question");
    h.store.answer(question.id, Answer::Revert).unwrap();
    h.until("the revert fails", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::ReloadFailed { .. }))
    });
    assert_eq!(h.text(doc), "xsaved");
    assert!(h.store.is_dirty(doc));
    h.type_text(doc, "y");
    assert_eq!(h.text(doc), "xysaved", "the view takes edits again");
}

#[test]
fn save_all_saves_named_files_at_once_and_asks_for_each_untitled_name_in_turn() {
    let folder = test_folder("save-all");
    let (a, b) = (folder.join("a.txt"), folder.join("b.txt"));
    write(&a, b"a");
    write(&b, b"b");
    let mut h = Harness::new(&folder.join("root"));
    let (window, blank) = h.store.new_window(false).unwrap();
    let doc_a = h.open(&a);
    let doc_b = h.open(&b);
    let (clean_file, one, two) = (
        doc_b,
        h.store.new_tab(window).unwrap(),
        h.store.new_tab(window).unwrap(),
    );
    h.type_text(doc_a, "1");
    h.type_text(one, "first");
    h.type_text(two, "second");
    h.type_text(blank, "   ");
    let failed = h.store.save_all();
    assert!(failed.is_empty());
    h.saved(doc_a);
    assert_eq!(std::fs::read(&a).unwrap(), b"1a");
    assert_eq!(std::fs::read(&b).unwrap(), b"b");
    assert!(!h.store.is_dirty(clean_file));
    let panels = |h: &Harness| -> Vec<qe_docs::DocId> {
        h.events
            .iter()
            .filter_map(|e| match e {
                DocEvent::NeedsSavePanel(r) => Some(r.doc),
                _ => None,
            })
            .collect()
    };
    assert_eq!(panels(&h), vec![one], "one dialog at a time");
    h.store.save_as(one, folder.join("one.txt"), None).unwrap();
    h.until("the next dialog", |h| panels(h).len() == 2);
    assert_eq!(panels(&h), vec![one, two]);
    h.store.save_as(two, folder.join("two.txt"), None).unwrap();
    h.until("the last save ends", |h| !h.store.is_dirty(two));
    assert_eq!(std::fs::read(folder.join("two.txt")).unwrap(), b"second");
    assert!(
        h.store.is_dirty(blank),
        "a blank document has nothing to keep and stays as it is"
    );
}

#[test]
fn close_of_a_clean_document_is_silent_and_dirty_asks_with_three_answers() {
    let folder = test_folder("save-close");
    let path = folder.join("f.txt");
    write(&path, b"x");
    let mut h = Harness::new(&folder.join("root"));
    let clean = h.open(&path);
    h.store.request_close(clean);
    assert!(h.store.view(clean).is_none());
    assert!(h.store.open_questions().is_empty());

    let (window, doc) = h.store.new_window(false).unwrap();
    let keep = h.store.new_tab(window).unwrap();
    h.type_text(doc, "text");
    // Cancel keeps it.
    h.store.request_close(doc);
    let question = h.question("the close question");
    assert_eq!(question.kind, QuestionKind::SaveBeforeClose);
    assert_eq!(
        question.choices,
        vec![Answer::Save, Answer::DontSave, Answer::Cancel]
    );
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert!(h.store.view(doc).is_some());
    // Don't Save closes.
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    assert!(h.store.view(doc).is_none());
    assert_eq!(h.store.tabs(window), vec![keep]);
}

#[test]
fn close_with_save_closes_only_after_the_write_succeeds() {
    let folder = test_folder("save-close-save");
    let path = folder.join("f.txt");
    write(&path, b"x");
    let (config, wakes) = config(&folder.join("root"));
    let faults = config.faults.clone();
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    h.type_text(doc, "y");
    faults.save_error_code.store(112, Ordering::Relaxed);
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::Save).unwrap();
    failure(&mut h);
    assert!(
        h.store.view(doc).is_some(),
        "a failed save keeps the tab and its text"
    );
    faults.save_error_code.store(0, Ordering::Relaxed);
    h.clear_events();
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::Save).unwrap();
    h.until("the tab closes", |h| h.store.view(doc).is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"yx");
}

#[test]
fn close_with_save_on_untitled_goes_through_the_dialog_and_cancel_stops_it() {
    let folder = test_folder("save-close-untitled");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "words");
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::Save).unwrap();
    h.pump();
    assert_eq!(h.count(|e| matches!(e, DocEvent::NeedsSavePanel(_))), 1);
    h.store.panel_cancelled(doc);
    h.settle(std::time::Duration::from_millis(10));
    assert!(h.store.view(doc).is_some());
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::Save).unwrap();
    let path = folder.join("kept.txt");
    h.store.save_as(doc, path.clone(), None).unwrap();
    h.until("the tab closes", |h| h.store.view(doc).is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"words");
}

#[test]
fn closing_a_window_asks_first_and_closes_every_tab_after_the_last_answer() {
    let folder = test_folder("save-close-window");
    let mut h = Harness::new(&folder.join("root"));
    let (window, a) = h.store.new_window(false).unwrap();
    let b = h.store.new_tab(window).unwrap();
    let c = h.store.new_tab(window).unwrap();
    h.type_text(a, "a");
    h.type_text(c, "c");
    h.store.request_close_window(window);
    let question = h.question("about the first tab");
    assert_eq!(question.doc, a);
    h.store.answer(question.id, Answer::DontSave).unwrap();
    assert_eq!(
        h.store.tabs(window),
        vec![a, b, c],
        "no tab closes while a question is still to come"
    );
    let question = h.question("about the third tab");
    assert_eq!(question.doc, c);
    h.store.answer(question.id, Answer::DontSave).unwrap();
    assert_eq!(h.store.windows(), Vec::new());
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::WindowClosed(w) if *w == window)),
        1
    );
}

#[test]
fn a_cancel_while_closing_a_window_leaves_every_tab_as_it_was() {
    let folder = test_folder("save-close-window-cancel");
    let path = folder.join("clean.txt");
    write(&path, b"on disk");
    let mut h = Harness::new(&folder.join("root"));
    let clean = h.open(&path);
    let window = h.store.window_of(clean).unwrap();
    let blank = h.store.new_tab(window).unwrap();
    let first = h.store.new_tab(window).unwrap();
    let second = h.store.new_tab(window).unwrap();
    h.type_text(first, "first");
    h.type_text(second, "second");
    let all = h.store.tabs(window);
    assert_eq!(all, vec![clean, blank, first, second]);
    h.store.request_close_window(window);
    let question = h.question("about the first dirty tab");
    assert_eq!(question.doc, first);
    assert_eq!(
        h.store.tabs(window),
        all,
        "the clean tabs wait for the answers"
    );
    h.store.answer(question.id, Answer::DontSave).unwrap();
    let question = h.question("about the second dirty tab");
    assert_eq!(question.doc, second);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.pump();
    assert_eq!(h.store.tabs(window), all, "Cancel closes nothing");
    assert_eq!(
        h.text(first),
        "first",
        "a Don't Save before a Cancel loses nothing"
    );
    assert!(h.store.is_dirty(first));
    assert_eq!(h.count(|e| matches!(e, DocEvent::DocClosed(_))), 0);
    // A later Close starts anew: the earlier Don't Save no longer counts.
    h.store.request_close_window(window);
    let question = h.question("about the first dirty tab again");
    assert_eq!(question.doc, first);
    h.store.answer(question.id, Answer::Cancel).unwrap();
}

#[test]
fn a_save_while_closing_a_window_keeps_its_tab_until_the_last_answer() {
    let folder = test_folder("save-close-window-save");
    let path = folder.join("doc.txt");
    write(&path, b"old");
    let mut h = Harness::new(&folder.join("root"));
    let saved = h.open(&path);
    let window = h.store.window_of(saved).unwrap();
    let other = h.store.new_tab(window).unwrap();
    h.caret_to_end(saved);
    h.type_text(saved, "+new");
    h.type_text(other, "words");
    h.store.request_close_window(window);
    let question = h.question("about the file");
    assert_eq!(question.doc, saved);
    h.store.answer(question.id, Answer::Save).unwrap();
    let question = h.question("about the other tab");
    assert_eq!(question.doc, other);
    assert_eq!(std::fs::read(&path).unwrap(), b"old+new");
    assert_eq!(h.store.tabs(window), vec![saved, other]);
    // Cancel keeps both. The save stands, as it was the person's own answer.
    h.store.answer(question.id, Answer::Cancel).unwrap();
    h.pump();
    assert_eq!(h.store.tabs(window), vec![saved, other]);
    assert!(!h.store.is_dirty(saved));
    // All answered: everything closes.
    h.store.request_close_window(window);
    let question = h.question("about the other tab again");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    assert_eq!(h.store.windows(), Vec::new());
}

#[test]
fn a_save_as_dialog_cancelled_while_closing_a_window_closes_nothing() {
    let folder = test_folder("save-close-window-panel");
    let path = folder.join("clean.txt");
    write(&path, b"on disk");
    let mut h = Harness::new(&folder.join("root"));
    let clean = h.open(&path);
    let window = h.store.window_of(clean).unwrap();
    let untitled = h.store.new_tab(window).unwrap();
    h.type_text(untitled, "words");
    h.store.request_close_window(window);
    let question = h.question("about the untitled tab");
    h.store.answer(question.id, Answer::Save).unwrap();
    h.pump();
    assert_eq!(h.count(|e| matches!(e, DocEvent::NeedsSavePanel(_))), 1);
    h.store.panel_cancelled(untitled);
    h.pump();
    assert_eq!(h.store.tabs(window), vec![clean, untitled]);
    assert_eq!(h.text(untitled), "words");
    assert!(h.store.open_questions().is_empty(), "the close has ended");
}

#[test]
fn a_tab_opened_or_typed_in_during_the_questions_counts() {
    let folder = test_folder("save-close-window-late");
    let mut h = Harness::new(&folder.join("root"));
    let (window, a) = h.store.new_window(false).unwrap();
    let b = h.store.new_tab(window).unwrap();
    h.type_text(a, "a");
    h.type_text(b, "b");
    h.store.request_close_window(window);
    let question = h.question("about the first tab");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    // The first tab's text changes after its answer, so the answer no longer covers it.
    h.type_text(a, " more");
    let question = h.question("about the second tab");
    assert_eq!(question.doc, b);
    h.store.answer(question.id, Answer::DontSave).unwrap();
    let question = h.question("about the first tab's new text");
    assert_eq!(question.doc, a);
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert_eq!(h.store.tabs(window), vec![a, b]);
    assert_eq!(h.text(a), "a more");
}

#[test]
fn blank_untitled_documents_close_with_no_question() {
    let folder = test_folder("save-blank");
    let mut h = Harness::new(&folder.join("root"));
    for text in ["", " ", "\n\n\n", " \t\r\n  \u{a0}\u{2028}"] {
        let (_, doc) = h.store.new_window(false).unwrap();
        h.type_text(doc, text);
        assert!(h.store.is_blank_untitled(doc), "{text:?}");
        h.store.request_close(doc);
        assert!(h.store.open_questions().is_empty(), "{text:?}");
        assert!(h.store.view(doc).is_none(), "{text:?}");
    }
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "  x  ");
    assert!(!h.store.is_blank_untitled(doc));
    h.store.request_close(doc);
    assert_eq!(h.store.open_questions().len(), 1, "text asks");
}

#[test]
fn over_the_scan_limit_counts_as_not_blank_and_a_named_empty_file_is_never_blank() {
    let folder = test_folder("save-blank-limit");
    let path = folder.join("empty.txt");
    write(&path, b"");
    let mut h = Harness::new(&folder.join("root"));
    let named = h.open(&path);
    assert!(!h.store.is_blank_untitled(named));
    let (_, doc) = h.store.new_window(false).unwrap();
    let spaces = " ".repeat(qe_docs::BLANK_SCAN_LIMIT_UTF16 + 1);
    let mut ctx = ViewContext::default();
    h.store
        .view_mut(doc)
        .unwrap()
        .replace_range(ByteOffset(0)..ByteOffset(0), &spaces, &mut ctx);
    h.store.note_view_changed(doc);
    assert!(!h.store.is_blank_untitled(doc));
    h.store.request_close(doc);
    assert_eq!(h.store.open_questions().len(), 1);
}

#[test]
fn a_question_refuses_an_answer_it_did_not_offer() {
    let folder = test_folder("save-wrong-answer");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "x");
    h.store.request_close(doc);
    let question = h.question("the close question");
    assert!(h.store.answer(question.id, Answer::Overwrite).is_err());
    assert_eq!(h.store.open_questions().len(), 1, "it stays open");
    h.store.answer(question.id, Answer::Cancel).unwrap();
    assert!(
        h.store.answer(question.id, Answer::Cancel).is_err(),
        "it has closed"
    );
}

#[test]
fn prevent_editing_and_wrap_belong_to_the_tab_and_do_not_mark_it_dirty() {
    let folder = test_folder("save-tab-state");
    let path = folder.join("f.txt");
    write(&path, b"x");
    let (mut config, wakes) = config(&folder.join("root"));
    config.new_docs_wrap_to_page = true;
    let mut h = Harness::with_config(config, wakes);
    let doc = h.open(&path);
    assert!(
        !h.store.wrap_to_page(doc),
        "a file from disk starts in Wrap to Window"
    );
    let (_, new_doc) = h.store.new_window(false).unwrap();
    assert!(
        h.store.wrap_to_page(new_doc),
        "a new document follows the setting"
    );
    h.store.set_prevent_editing(doc, true);
    h.store.set_wrap_to_page(doc, true);
    assert!(!h.store.is_dirty(doc));
    h.type_text(doc, "y");
    assert_eq!(h.text(doc), "x", "Prevent Editing refuses");
    h.store.set_prevent_editing(doc, false);
    h.type_text(doc, "y");
    assert_eq!(h.text(doc), "yx");
}

#[test]
fn tabs_move_merge_and_split_and_an_empty_window_closes() {
    let folder = test_folder("save-tabs");
    let mut h = Harness::new(&folder.join("root"));
    let (first, a) = h.store.new_window(false).unwrap();
    let b = h.store.new_tab(first).unwrap();
    let (second, c) = h.store.new_window(false).unwrap();
    h.store.move_tab(b, second, 0);
    assert_eq!(h.store.tabs(second), vec![b, c]);
    assert_eq!(h.store.tabs(first), vec![a]);
    let third = h.store.move_tab_to_new_window(b).unwrap();
    assert_eq!(h.store.tabs(third), vec![b]);
    h.store.window_activated(first);
    h.store.merge_all_windows();
    assert_eq!(h.store.windows(), vec![first]);
    assert_eq!(h.store.tabs(first).len(), 3);
    // Closing the active tab brings the one to its right forward, else the one to its left.
    let tabs = h.store.tabs(first);
    h.store.set_active_tab(first, tabs[1]);
    h.store.request_close(tabs[1]);
    assert_eq!(h.store.active_tab(first), Some(tabs[2]));
    h.store.request_close(tabs[2]);
    assert_eq!(h.store.active_tab(first), Some(tabs[0]));
}

#[test]
fn the_encoding_list_learns_which_encodings_cannot_hold_the_text() {
    let folder = test_folder("save-fit");
    let mut h = Harness::new(&folder.join("root"));
    let (_, doc) = h.store.new_window(false).unwrap();
    h.type_text(doc, "\u{4e2d}");
    h.store
        .check_encodings(doc, vec![TextEncoding::Utf8, TextEncoding::Windows1252]);
    h.until("the answer", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::EncodingFit { .. }))
    });
    let unfit = h.events.iter().find_map(|e| match e {
        DocEvent::EncodingFit { unfit, .. } => Some(unfit.clone()),
        _ => None,
    });
    assert_eq!(unfit, Some(vec![TextEncoding::Windows1252]));
}

#[test]
fn the_config_follows_the_settings_file() {
    let folder = test_folder("save-settings");
    let path = folder.join("settings.txt");
    let settings = qe_settings::SettingsStore::open(&path);
    settings
        .set_integer(qe_api::SettingKey::AutosavingDelaySeconds, 7)
        .unwrap();
    settings
        .set_bool(qe_api::SettingKey::AddTxtExtension, false)
        .unwrap();
    settings
        .set_bool(qe_api::SettingKey::ShowPageBreaks, true)
        .unwrap();
    let config = qe_docs::StoreConfig::from_settings(&settings, std::sync::Arc::new(|| {}));
    assert_eq!(config.root, folder);
    assert_eq!(config.autosave_delay, std::time::Duration::from_secs(7));
    assert!(!config.add_txt);
    assert!(config.new_docs_wrap_to_page);
    assert_eq!(config.read_only_bytes, 512 * 1024 * 1024);
    assert_eq!(config.large_bytes, 50 * 1024 * 1024);
    settings.flush().unwrap();
}

#[test]
fn the_next_document_follows_a_change_in_settings_and_open_ones_stay() {
    let folder = test_folder("save-settings-change");
    let settings = qe_settings::SettingsStore::open(folder.join("settings.txt"));
    let mut h = Harness::new(&folder.join("root"));
    let (_, first) = h.store.new_window(false).unwrap();
    assert!(!h.store.wrap_to_page(first));
    settings
        .set_bool(qe_api::SettingKey::ShowPageBreaks, true)
        .unwrap();
    h.store.follow_settings(&settings);
    let (_, second) = h.store.new_window(false).unwrap();
    assert!(h.store.wrap_to_page(second), "the next document");
    assert!(!h.store.wrap_to_page(first), "the open one stays");
    settings.flush().unwrap();
}

/// The editor's README: Cut, then something that replaces the text, must not lose the cut.
#[test]
fn a_revert_and_a_close_wait_for_a_cut_on_its_way_to_the_clipboard() {
    let folder = test_folder("save-cut");
    let path = folder.join("f.txt");
    let body = "line of text\n".repeat(200_000);
    write(&path, body.as_bytes());
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    let mut ctx = ViewContext::default();
    let view = h.store.view_mut(doc).unwrap();
    view.run(qe_editor::EditorCommand::SelectAll, &mut ctx);
    view.run(qe_editor::EditorCommand::Cut, &mut ctx);
    assert!(view.cut_is_running(), "a big Cut's text is on its way");
    h.store.note_view_changed(doc);
    assert!(h.store.is_dirty(doc));
    h.store.request_revert(doc).unwrap();
    let question = h.question("the revert question");
    h.store.answer(question.id, Answer::Revert).unwrap();
    h.until("the file comes back", |h| {
        if let Some(view) = h.store.view_mut(doc) {
            view.poll_layout(&mut ViewContext::default());
        }
        h.text(doc) == body
    });
    assert!(!h.store.is_dirty(doc));
    assert!(!h.store.view(doc).unwrap().cut_is_running());
    // A close during a Cut keeps the view alive until the text has gone.
    let mut ctx = ViewContext::default();
    let view = h.store.view_mut(doc).unwrap();
    view.run(qe_editor::EditorCommand::SelectAll, &mut ctx);
    view.run(qe_editor::EditorCommand::Cut, &mut ctx);
    assert!(view.cut_is_running());
    h.store.note_view_changed(doc);
    h.store.request_close(doc);
    let question = h.question("the close question");
    h.store.answer(question.id, Answer::DontSave).unwrap();
    assert!(h.store.view(doc).is_none());
    h.store.begin_quit();
    h.until("the quit waits for the Cut, then ends", |h| {
        h.events.iter().any(|e| matches!(e, DocEvent::ReadyToQuit))
    });
}
