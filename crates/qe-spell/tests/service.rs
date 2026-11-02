//! The service with a fake checker, so these run on any account and show exactly what goes
//! to the checker. `real_checker.rs` has the same paths with Windows' checker.
mod support;

use qe_api::{ByteOffset, ViewContext};
use qe_buffer::{MappedStore, PieceStore};
use qe_editor::{EditorCommand, EditorView, LoadedFile};
use qe_spell::{
    AHEAD_BYTES, Checker, FindResult, FixItem, MAX_BYTES, SpellOff, SpellService, SpellState,
    choose_language, why_off, without_sort_order,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use support::{fake_service, finish, laid_out, marks, no_wake};

fn b(n: usize) -> ByteOffset {
    ByteOffset(n)
}
fn ctx() -> ViewContext {
    ViewContext::default()
}

// ---- When it is off ----

#[test]
fn off_is_off_and_each_reason_says_so() {
    let view = EditorView::from_text("speling").unwrap();
    assert!(matches!(
        SpellService::start(&view, false, None, no_wake()),
        Err(SpellOff::NotOn)
    ));
    let mut read_only = EditorView::from_text("speling").unwrap();
    read_only.set_prevent_editing(true);
    assert!(matches!(
        SpellService::start(&read_only, true, None, no_wake()),
        Err(SpellOff::ReadOnly)
    ));
    assert_eq!(why_off(&view, true), None);
}

#[test]
fn the_limit_is_50_mib_and_the_exact_size_stays_on() {
    let at_limit = EditorView::from_text(&"x".repeat(MAX_BYTES)).unwrap();
    assert_eq!(why_off(&at_limit, true), None);
    let over = EditorView::from_text(&"x".repeat(MAX_BYTES + 1)).unwrap();
    assert_eq!(why_off(&over, true), Some(SpellOff::TooLarge));
    assert!(matches!(
        SpellService::start(&over, true, None, no_wake()),
        Err(SpellOff::TooLarge)
    ));
}

#[test]
fn crossing_the_limit_turns_it_off_and_takes_the_marks_away() {
    let mut view = laid_out("speling here");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Working);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(0, 7)]);
    let more = "x".repeat(MAX_BYTES);
    view.replace_range(b(12)..b(12), &more, &mut c);
    assert_eq!(
        service.refresh(&mut view, &mut c),
        SpellState::Off(SpellOff::TooLarge)
    );
    assert_eq!(marks(&view), vec![]);
    assert_eq!(service.menu_at(&view, b(1)), None);
    // Coming back under the limit, it works again.
    view.replace_range(b(12)..b(12 + MAX_BYTES), "", &mut c);
    let state = service.refresh(&mut view, &mut c);
    assert_ne!(state, SpellState::Off(SpellOff::TooLarge));
}

#[test]
fn a_mapped_file_stays_off_even_while_its_index_knows_little() {
    let path = std::env::temp_dir().join(format!("qe-spell-{}-mapped.txt", std::process::id()));
    std::fs::write(&path, "speling\n".repeat(50_000)).unwrap();
    let store = MappedStore::open(&path).unwrap();
    store.set_test_read_delay(Duration::from_millis(5));
    let view = EditorView::from_mapped(store).unwrap();
    assert!(view.store_status().known_bytes.0 < MAX_BYTES);
    assert_eq!(why_off(&view, true), Some(SpellOff::TooLarge));
    assert!(matches!(
        SpellService::start(&view, true, None, no_wake()),
        Err(SpellOff::TooLarge)
    ));
    drop(view);
    for _ in 0..200 {
        match std::fs::remove_file(&path) {
            Ok(()) => return,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    eprintln!("could not remove {}", path.display());
}

#[test]
fn a_view_that_turns_read_only_loses_its_marks_and_its_fixes() {
    let mut view = laid_out("speling here");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(1)).unwrap();
    view.set_prevent_editing(true);
    assert_eq!(
        service.refresh(&mut view, &mut c),
        SpellState::Off(SpellOff::ReadOnly)
    );
    assert_eq!(marks(&view), vec![]);
    assert_eq!(service.menu_at(&view, b(1)), None);
    let refused = service.apply_fix(&mut view, &menu, "spelling", &mut c);
    assert!(refused.unwrap_err().contains("cannot be edited"));
    assert_eq!(view.text().unwrap(), "speling here");
    log.clear();
    view.set_prevent_editing(false);
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Working);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(
        marks(&view),
        vec![(0, 7)],
        "it checks again when it can edit"
    );
}

// ---- Marks ----

#[test]
fn marks_land_on_the_right_bytes_through_emoji_combining_marks_and_crlf() {
    // Bytes: emoji 0..4, space, cafe + mark 5..11, CRLF, speling 13..20.
    let mut view = laid_out("\u{1f600} cafe\u{301}\r\nspeling");
    let (mut service, _log) = fake_service(&view, &["cafe\u{301}", "speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(5, 11), (13, 20)]);
    let word = service.marks()[0].word.clone();
    assert_eq!(word, "cafe\u{301}");
}

#[test]
fn an_empty_document_and_a_clean_one_have_no_marks() {
    let mut view = laid_out("");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![]);
    assert!(log.checked_text().iter().all(String::is_empty));
}

#[test]
fn nothing_waits_for_layout_that_has_not_come() {
    let mut view = EditorView::from_text("speling").unwrap();
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    assert_eq!(
        service.refresh(&mut view, &mut c),
        SpellState::WaitingForLayout
    );
    assert_eq!(service.counts().jobs_sent, 0);
    assert!(log.checked_text().is_empty());
}

#[test]
fn it_checks_what_shows_and_a_little_ahead_and_not_the_whole_file() {
    let line = "plain words on a line of text\n";
    let text = line.repeat(100_000);
    let mut view = laid_out(&text);
    let (mut service, log) = fake_service(&view, &[]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let visible = view.visible_range().unwrap();
    let asked = service.counts().bytes_asked as usize;
    assert_eq!(asked, visible.end.0 - visible.start.0 + AHEAD_BYTES);
    let sent: usize = log.checked_text().iter().map(String::len).sum();
    assert!(
        sent < asked + 1024,
        "{sent} went to the checker, for {asked} asked"
    );
    // A second look finds nothing new to do.
    let jobs = service.counts().jobs_sent;
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Done);
    assert_eq!(service.counts().jobs_sent, jobs);
}

#[test]
fn a_range_edge_inside_a_word_grows_to_the_whole_word() {
    // The window ends inside some word of this long run, which must not be cut.
    let text = "word ".repeat(20_000);
    let mut view = laid_out(&text);
    let (mut service, log) = fake_service(&view, &[]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let sent = log.checked_text();
    assert!(!sent.is_empty());
    for piece in &sent {
        for token in piece.split_whitespace() {
            assert_eq!(token, "word", "a cut word went to the checker");
        }
    }
}

#[test]
fn urls_emails_domains_and_doubled_words_get_no_mark() {
    let text = "speling http://speling.org/speling www.speling.com speling@speling.org \
                speling.org dupe speling.";
    let mut view = laid_out(text);
    let (mut service, _log) =
        fake_service(&view, &["speling", "org", "com", "http", "www", "dupe"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let last = text.rfind("speling.").unwrap();
    assert_eq!(
        marks(&view),
        vec![(0, 7), (last, last + 7)],
        "the first word and the last, whose full stop is only punctuation"
    );
}

#[test]
fn a_zero_in_the_text_does_not_end_the_check_early() {
    let mut view = laid_out("ok\0 speling");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(4, 11)]);
}

// ---- A recheck is limited to what changed ----

#[test]
fn an_edit_rechecks_only_the_word_it_touched() {
    let text = format!("speling {}", "good words here ".repeat(100));
    let mut view = laid_out(&text);
    let (mut service, log) = fake_service(&view, &["speling", "wurds", "goood"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(0, 7)]);
    log.clear();
    let asked = service.counts().bytes_asked;

    // Types one letter into "good" at byte 9.
    view.replace_range(b(9)..b(9), "o", &mut c);
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Working);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(
        log.checked_text(),
        vec!["goood".to_owned()],
        "only that word"
    );
    assert_eq!(
        service.counts().bytes_asked - asked,
        3,
        "the hole around one letter"
    );
    assert_eq!(marks(&view), vec![(0, 7), (8, 13)]);

    // Fixing the word takes its mark away, with the same small check.
    log.clear();
    view.replace_range(b(9)..b(10), "", &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(log.checked_text(), vec!["good".to_owned()]);
    assert_eq!(marks(&view), vec![(0, 7)]);
}

#[test]
fn deleting_a_space_checks_the_joined_word() {
    let mut view = laid_out("spel ing ok");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![]);
    log.clear();
    view.replace_range(b(4)..b(5), "", &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(log.checked_text(), vec!["speling".to_owned()]);
    assert_eq!(marks(&view), vec![(0, 7)]);
}

#[test]
fn undo_is_an_edit_like_any_other() {
    let mut view = laid_out("speling");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    view.replace_range(b(0)..b(7), "fine", &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![]);
    log.clear();
    view.run(EditorCommand::Undo, &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(view.text().unwrap(), "speling");
    assert_eq!(marks(&view), vec![(0, 7)]);
}

// ---- A key never waits ----

#[test]
fn typing_goes_on_during_a_check_and_its_answer_moves_with_the_text() {
    let mut view = laid_out("speling and more");
    let (mut service, log) = fake_service(&view, &["speling", "more"]);
    let mut c = ctx();
    log.hold();
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Working);
    // The checker is stuck. The UI thread's calls must still come straight back.
    let started = Instant::now();
    for i in 0..5 {
        view.replace_range(b(0)..b(0), "ab ", &mut c);
        assert_eq!(service.refresh(&mut view, &mut c), SpellState::Working);
        assert!(service.poll(&mut view, &mut c).is_ok());
        assert_eq!(service.counts().jobs_sent, 1, "one job at a time, key {i}");
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the keys waited: {:?}",
        started.elapsed()
    );
    log.release();
    finish(&mut service, &mut view, &mut c);
    // The answer was for the old text. Five insertions of 3 bytes moved it.
    let word = view.text().unwrap().find("speling").unwrap();
    assert_eq!(word, 15);
    let more = view.text().unwrap().find("more").unwrap();
    assert_eq!(marks(&view), vec![(word, word + 7), (more, more + 4)]);
}

#[test]
fn a_word_edited_during_its_check_gets_the_new_answer_not_the_old_one() {
    let mut view = laid_out("speling ok");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    log.hold();
    service.refresh(&mut view, &mut c);
    // Corrects the word while the old answer is on its way.
    view.replace_range(b(4)..b(4), "l", &mut c);
    log.release();
    finish(&mut service, &mut view, &mut c);
    assert_eq!(view.text().unwrap(), "spelling ok");
    assert_eq!(
        marks(&view),
        vec![],
        "the old answer must not mark the new word"
    );
}

#[test]
fn dropping_the_service_during_a_check_does_not_wait_for_it() {
    let view = laid_out("speling");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut view = view;
    let mut c = ctx();
    log.hold();
    service.refresh(&mut view, &mut c);
    let started = Instant::now();
    drop(service);
    assert!(started.elapsed() < Duration::from_secs(1));
    log.release();
}

// ---- A new document ----

#[test]
fn a_new_document_starts_clean_and_an_old_answer_does_not_touch_it() {
    let mut view = laid_out("speling");
    let (mut service, log) = fake_service(&view, &["speling", "wrongg"]);
    let mut c = ctx();
    log.hold();
    service.refresh(&mut view, &mut c);
    view.adopt(LoadedFile::Editable {
        store: PieceStore::new("some wrongg words"),
        ending: "\n",
    })
    .unwrap();
    log.release();
    support::settle(&mut view);
    finish(&mut service, &mut view, &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(5, 11)]);
    assert!(service.counts().answers_dropped >= 1);
}

// ---- The fix menu ----

#[test]
fn the_menu_gives_the_word_up_to_five_suggestions_and_the_two_actions() {
    let mut view = laid_out("ok speling ok");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(service.menu_at(&view, b(1)), None, "not on a mark");
    assert_eq!(service.menu_at(&view, b(10)), None, "just past the mark");
    let menu = service.menu_at(&view, b(3)).unwrap();
    assert_eq!(menu.range, b(3)..b(10));
    assert_eq!(menu.word, "speling");
    assert_eq!(menu.suggestions.len(), 5);
    let items = menu.items();
    assert_eq!(items.len(), 7);
    assert_eq!(items[0], FixItem::Suggestion("speling-0".into()));
    assert_eq!(items[5], FixItem::Ignore);
    assert_eq!(items[6], FixItem::AddToDictionary);
    let end_of_word = service.menu_at(&view, b(9)).unwrap();
    assert_eq!(end_of_word.range, menu.range);
}

#[test]
fn a_sure_replacement_is_the_only_suggestion_and_no_suggestions_says_so() {
    let mut view = laid_out("fixme");
    let (mut service, _log) = fake_service(&view, &["fixme"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(0)).unwrap();
    assert_eq!(menu.suggestions, vec!["fixed".to_owned()]);
    let empty = qe_spell::FixItem::NoSuggestions;
    assert_ne!(menu.items()[0], empty);
}

#[test]
fn the_menu_comes_from_a_point_through_the_view() {
    let mut view = laid_out("speling");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let rect = view.range_rects(b(0)..b(7)).unwrap()[0].rect;
    let inside = qe_api::Point {
        x: qe_api::Dip(rect.x.0 + rect.width.0 / 2.0),
        y: qe_api::Dip(rect.y.0 + rect.height.0 / 2.0),
    };
    assert_eq!(
        service.menu_at_point(&view, inside).unwrap().word,
        "speling"
    );
    let below = qe_api::Point {
        x: inside.x,
        y: qe_api::Dip(rect.y.0 + 200.0),
    };
    assert_eq!(service.menu_at_point(&view, below), None);
}

#[test]
fn a_fix_is_one_undo_step_and_it_clears_the_mark() {
    let mut view = laid_out("\u{1f600} speling\r\nnext");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    // Typing just before the fix must not join its undo step.
    view.replace_range(b(0)..b(0), "x", &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(6)).unwrap();
    service
        .apply_fix(&mut view, &menu, "spelling", &mut c)
        .unwrap();
    assert_eq!(view.text().unwrap(), "x\u{1f600} spelling\r\nnext");
    assert_eq!(
        view.selection(),
        b(14)..b(14),
        "the caret is after the new word"
    );
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![]);
    view.run(EditorCommand::Undo, &mut c);
    assert_eq!(
        view.text().unwrap(),
        "x\u{1f600} speling\r\nnext",
        "one undo, one fix"
    );
    view.run(EditorCommand::Undo, &mut c);
    assert_eq!(view.text().unwrap(), "\u{1f600} speling\r\nnext");
}

#[test]
fn a_menu_from_before_an_edit_is_refused() {
    let mut view = laid_out("speling ok");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(0)).unwrap();
    view.replace_range(b(0)..b(0), "ab ", &mut c);
    let refused = service.apply_fix(&mut view, &menu, "spelling", &mut c);
    assert!(refused.unwrap_err().contains("changed"));
    assert_eq!(view.text().unwrap(), "ab speling ok");
}

#[test]
fn ignore_stays_here_and_add_reaches_the_checker_and_every_mark_of_the_word_goes() {
    let mut view = laid_out("speling and speling and teh");
    let (mut service, log) = fake_service(&view, &["speling", "teh"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view).len(), 3);
    let menu = service.menu_at(&view, b(0)).unwrap();
    service.ignore(&mut view, &menu, &mut c).unwrap();
    assert_eq!(
        marks(&view),
        vec![(24, 27)],
        "at once, before the worker answers"
    );
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(24)).unwrap();
    service.add_to_dictionary(&mut view, &menu, &mut c).unwrap();
    assert_eq!(marks(&view), vec![]);
    let until = Instant::now() + Duration::from_secs(10);
    while log.added.lock().unwrap().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(log.added.lock().unwrap().as_slice(), ["teh".to_owned()]);
    // A later recheck of the word does not bring its mark back.
    view.replace_range(b(0)..b(0), " ", &mut c);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![]);
}

#[test]
fn an_answer_on_its_way_cannot_bring_back_an_ignored_word() {
    let mut view = laid_out("speling ok");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(0)).unwrap();
    log.hold();
    view.replace_range(b(10)..b(10), " x", &mut c);
    view.replace_range(b(0)..b(0), " ", &mut c);
    service.refresh(&mut view, &mut c);
    let _ = service.ignore(&mut view, &menu, &mut c);
    log.release();
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![]);
}

// ---- Find next ----

#[test]
fn find_next_goes_from_the_selection_and_around_the_end() {
    let mut view = laid_out("teh ok speling");
    let (mut service, _log) = fake_service(&view, &["teh", "speling"]);
    let mut c = ctx();
    view.select(b(5)..b(5), &mut c);
    service.find_next(&view).unwrap();
    let found = wait_found(&mut service, &mut view);
    assert!(matches!(found, FindResult::Found(m) if m.word == "speling"));
    view.select(b(10)..b(10), &mut c);
    service.find_next(&view).unwrap();
    let found = wait_found(&mut service, &mut view);
    assert!(
        matches!(found, FindResult::Found(m) if m.word == "teh"),
        "it went around"
    );
    view.replace_range(b(0)..b(14), "clean", &mut c);
    service.find_next(&view).unwrap();
    assert_eq!(wait_found(&mut service, &mut view), FindResult::NoneLeft);
}

fn wait_found(service: &mut SpellService, view: &mut EditorView) -> FindResult {
    let until = Instant::now() + Duration::from_secs(30);
    let mut c = ctx();
    loop {
        if let Some(found) = service.poll(view, &mut c).unwrap().found {
            return found;
        }
        assert!(Instant::now() < until, "no answer");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ---- Failures ----

#[test]
fn a_checker_that_cannot_start_says_why_once_and_spelling_stays_off() {
    let mut view = laid_out("speling");
    let woken = Arc::new(Mutex::new(0));
    let count = woken.clone();
    let mut service = SpellService::start_with(
        &view,
        Box::new(|| -> Result<Box<dyn Checker>, String> {
            Err("Windows has no spelling dictionary for xx.".into())
        }),
        Arc::new(move || *count.lock().unwrap() += 1),
    );
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    let until = Instant::now() + Duration::from_secs(30);
    let message = loop {
        match service.poll(&mut view, &mut c) {
            Err(message) => break message,
            Ok(_) => {
                assert!(Instant::now() < until, "no failure came");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    assert_eq!(message, "Windows has no spelling dictionary for xx.");
    assert!(*woken.lock().unwrap() >= 1, "the host was woken to read it");
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Failed);
    assert!(service.find_next(&view).is_err() || service.poll(&mut view, &mut c).is_ok());
    assert_eq!(view.text().unwrap(), "speling");
}

#[test]
fn a_check_that_fails_later_clears_the_marks_and_keeps_the_text() {
    let mut view = laid_out("speling ok");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view).len(), 1);
    *log.fail_next.lock().unwrap() = true;
    view.replace_range(b(10)..b(10), " more", &mut c);
    service.refresh(&mut view, &mut c);
    let until = Instant::now() + Duration::from_secs(30);
    let message = loop {
        match service.poll(&mut view, &mut c) {
            Err(message) => break message,
            Ok(_) => {
                assert!(Instant::now() < until, "no failure came");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    assert_eq!(message, "The fake checker failed.");
    assert_eq!(marks(&view), vec![]);
    assert_eq!(view.text().unwrap(), "speling ok more");
}

#[test]
fn the_wake_fires_for_each_answer_and_needs_no_timer() {
    let mut view = laid_out("speling");
    let woken = Arc::new(Mutex::new(0_u32));
    let count = woken.clone();
    let log = Arc::new(support::FakeLog::default());
    let checker = support::FakeChecker {
        bad: ["speling".to_owned()].into_iter().collect(),
        log,
    };
    let mut service = SpellService::start_with(
        &view,
        Box::new(move || Ok(Box::new(checker) as Box<dyn Checker>)),
        Arc::new(move || *count.lock().unwrap() += 1),
    );
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    let until = Instant::now() + Duration::from_secs(30);
    while *woken.lock().unwrap() == 0 {
        assert!(Instant::now() < until, "no wake came");
        std::thread::sleep(Duration::from_millis(1));
    }
    service.poll(&mut view, &mut c).unwrap();
    assert_eq!(marks(&view), vec![(0, 7)]);
}

// ---- Finding 1: separators wider than one byte ----

/// Every separator here is more than one byte long, so an edit's hole can end inside one.
const WIDE_SEPARATORS: [&str; 8] = [
    "\u{3000}", "\u{a0}", "\u{85}", "\u{2003}", "\u{2009}", "\u{2028}", "\u{2029}", "\u{202f}",
];

#[test]
fn an_edit_next_to_a_wide_separator_at_any_place_ends_with_nothing_to_do() {
    for separator in WIDE_SEPARATORS {
        let text = format!("abc{separator}def{separator}{separator}ghi");
        for at in (0..=text.len()).filter(|at| text.is_char_boundary(*at)) {
            for insert in ["x", "\u{3000}", ""] {
                let mut view = laid_out(&text);
                let (mut service, _log) = fake_service(&view, &["speling"]);
                let mut c = ctx();
                service.refresh(&mut view, &mut c);
                finish_soon(&mut service, &mut view, &mut c);
                let before = service.counts().jobs_sent;
                let end = if insert.is_empty() {
                    // Deletes the character at this place, if any.
                    text[at..]
                        .chars()
                        .next()
                        .map_or(at, |ch| at + ch.len_utf8())
                } else {
                    at
                };
                view.replace_range(b(at)..b(end), insert, &mut c);
                service.refresh(&mut view, &mut c);
                finish_soon(&mut service, &mut view, &mut c);
                support::settle(&mut view);
                service.refresh(&mut view, &mut c);
                finish_soon(&mut service, &mut view, &mut c);
                assert_eq!(
                    service.refresh(&mut view, &mut c),
                    SpellState::Done,
                    "{separator:?} at {at} with {insert:?}: a gap stayed open"
                );
                let jobs = service.counts().jobs_sent - before;
                assert!(
                    jobs <= 2,
                    "{separator:?} at {at} with {insert:?}: {jobs} jobs"
                );
            }
        }
    }
}

#[test]
fn a_view_whose_window_ends_inside_a_wide_separator_finishes_too() {
    // Rows of wide spaces, so that the window's end falls among them at many lengths.
    for extra in 0..4 {
        let text = format!("{}{}", "a".repeat(extra), "\u{3000}".repeat(30_000));
        let mut view = laid_out(&text);
        let (mut service, _log) = fake_service(&view, &[]);
        let mut c = ctx();
        service.refresh(&mut view, &mut c);
        finish_soon(&mut service, &mut view, &mut c);
        assert_eq!(service.refresh(&mut view, &mut c), SpellState::Done);
        assert!(service.counts().jobs_sent <= 2, "{:?}", service.counts());
    }
}

/// Like `finish`, but fails at once on a loop rather than after 30 seconds.
fn finish_soon(service: &mut SpellService, view: &mut EditorView, c: &mut ViewContext) {
    let until = Instant::now() + Duration::from_secs(5);
    while service.is_working() {
        service.poll(view, c).unwrap();
        assert!(
            Instant::now() < until,
            "the checks never ended: {:?}",
            service.counts()
        );
        std::thread::yield_now();
    }
}

// ---- Review fixes ----

#[test]
fn a_late_answer_after_spelling_turned_off_brings_no_underlines_back() {
    let mut view = laid_out("speling here");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    log.hold();
    service.refresh(&mut view, &mut c);
    log.wait_until_held();
    view.set_prevent_editing(true);
    assert_eq!(why_off(&view, true), Some(SpellOff::ReadOnly));
    log.release();
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![], "the answer came after the turn-off");
    assert_eq!(service.marks(), &[]);
}

#[test]
fn turning_off_twice_with_the_same_reason_still_clears_marks_that_came_between() {
    let mut view = laid_out("speling here");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    view.set_prevent_editing(true);
    assert_eq!(
        service.refresh(&mut view, &mut c),
        SpellState::Off(SpellOff::ReadOnly)
    );
    view.set_decorations(
        qe_editor::DecorationKind::SpellingUnderline,
        vec![b(0)..b(7)],
        &mut c,
    );
    assert_eq!(
        service.refresh(&mut view, &mut c),
        SpellState::Off(SpellOff::ReadOnly)
    );
    assert_eq!(marks(&view), vec![]);
}

fn big_clean_text() -> String {
    format!("head {}", "plain words here\n".repeat(200_000))
}

#[test]
fn dropping_the_service_stops_a_long_search() {
    let view = laid_out(&big_clean_text());
    let (mut service, log) = fake_service(&view, &["speling"]);
    log.hold();
    service.find_next(&view).unwrap();
    // The worker is now waiting in its first piece, or about to.
    drop(service);
    log.release();
    let until = Instant::now() + Duration::from_secs(30);
    while !log.dropped.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(Instant::now() < until, "the worker went on");
        std::thread::sleep(Duration::from_millis(1));
    }
    let pieces = log.checked_text().len();
    assert!(pieces <= 2, "the search went on for {pieces} pieces");
}

#[test]
fn an_edit_stops_a_search_and_the_host_hears() {
    let mut view = laid_out(&big_clean_text());
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    log.hold();
    service.find_next(&view).unwrap();
    view.replace_range(b(0)..b(0), "x", &mut c);
    log.release();
    assert_eq!(wait_found(&mut service, &mut view), FindResult::TextChanged);
    assert!(
        service.poll(&mut view, &mut c).unwrap().found.is_none(),
        "one answer"
    );
    let until = Instant::now() + Duration::from_secs(30);
    while service.is_working() {
        service.poll(&mut view, &mut c).unwrap();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(log.checked_text().len() < 200, "the search stopped early");
}

#[test]
fn spelling_turning_off_stops_a_search_and_the_host_hears() {
    let mut view = laid_out(&big_clean_text());
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    log.hold();
    service.find_next(&view).unwrap();
    view.set_prevent_editing(true);
    assert_eq!(
        service.refresh(&mut view, &mut c),
        SpellState::Off(SpellOff::ReadOnly)
    );
    log.release();
    assert_eq!(wait_found(&mut service, &mut view), FindResult::TextChanged);
}

#[test]
fn a_check_runs_during_a_long_search_and_does_not_wait_for_its_end() {
    let mut view = laid_out(&big_clean_text());
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    log.hold();
    service.find_next(&view).unwrap();
    assert_eq!(service.refresh(&mut view, &mut c), SpellState::Working);
    log.release();
    assert_eq!(wait_found(&mut service, &mut view), FindResult::NoneLeft);
    finish(&mut service, &mut view, &mut c);
    let checked = log.checked_text();
    assert!(checked.len() > 2, "{} pieces", checked.len());
    // The check's text is short of a whole piece, and starts at the document's start. It came
    // as one of the first two, ahead of all the other pieces of the search.
    let at = checked
        .iter()
        .position(|text| text.starts_with("head") && text.len() < 16 * 1024)
        .expect("the check ran");
    assert!(at <= 1, "the check waited behind {at} pieces");
}

#[test]
fn a_second_search_replaces_the_first_and_gets_the_only_answer() {
    let mut view = laid_out("teh ok speling");
    let (mut service, log) = fake_service(&view, &["teh", "speling"]);
    let mut c = ctx();
    log.hold();
    service.find_next(&view).unwrap();
    log.wait_until_held();
    view.select(b(5)..b(5), &mut c);
    service.find_next(&view).unwrap();
    log.release();
    assert!(matches!(
        wait_found(&mut service, &mut view),
        FindResult::Found(m) if m.word == "speling"
    ));
    let until = Instant::now() + Duration::from_millis(50);
    while Instant::now() < until {
        assert!(service.poll(&mut view, &mut c).unwrap().found.is_none());
    }
}

#[test]
fn a_search_skips_words_ignored_in_this_document() {
    let mut view = laid_out("teh ok speling");
    let (mut service, _log) = fake_service(&view, &["teh", "speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(0)).unwrap();
    service.ignore(&mut view, &menu, &mut c).unwrap();
    view.select(b(5)..b(5), &mut c);
    service.find_next(&view).unwrap();
    assert!(matches!(
        wait_found(&mut service, &mut view),
        FindResult::Found(m) if m.word == "speling"
    ));
    view.select(b(14)..b(14), &mut c);
    let menu = service.menu_at(&view, b(7)).unwrap();
    service.ignore(&mut view, &menu, &mut c).unwrap();
    service.find_next(&view).unwrap();
    assert_eq!(wait_found(&mut service, &mut view), FindResult::NoneLeft);
}

#[test]
fn an_ignored_word_ends_with_its_document() {
    let mut view = laid_out("speling");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(0)).unwrap();
    service.ignore(&mut view, &menu, &mut c).unwrap();
    assert_eq!(marks(&view), vec![]);

    view.adopt(LoadedFile::Editable {
        store: PieceStore::new("speling again"),
        ending: "
",
    })
    .unwrap();
    support::settle(&mut view);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(0, 7)], "a new document starts clean");
}

#[test]
fn a_new_look_at_the_same_document_keeps_its_ignored_words() {
    let mut view = laid_out("speling");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(0)).unwrap();
    service.ignore(&mut view, &menu, &mut c).unwrap();
    // More edits than the log holds, so the service has to look at all of the text again.
    for _ in 0..5_000 {
        view.replace_range(b(7)..b(7), " ", &mut c);
        c.requests.clear();
    }
    support::settle(&mut view);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![], "this document still ignores it");
}

#[test]
fn a_word_cut_by_the_margin_gets_no_mark_from_either_side() {
    // Longer than a piece and its margins, so each piece sees only part of the word.
    let long = "q".repeat(60_000);
    let mut view = laid_out(&format!("{long} after"));
    let (mut service, log) = fake_service(&view, &["qqq"]);
    let mut c = ctx();
    let first = service.refresh(&mut view, &mut c);
    assert_eq!(first, SpellState::Working);
    finish(&mut service, &mut view, &mut c);
    assert!(
        !log.checked_text().is_empty(),
        "the checker saw the pieces, and flagged them"
    );
    assert_eq!(marks(&view), vec![], "a piece of a word is not a word");
    // The end of the word, where only the left side of a piece is cut.
    view.set_scroll_position(1e12, &mut c);
    support::settle(&mut view);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![], "the tail of a word is not a word");
}

#[test]
fn a_whole_word_near_the_edge_of_a_piece_keeps_its_mark() {
    // The margin finds both ends of this word, so it is whole, and its mark is real.
    let mut view = laid_out(&format!("qqqq {}", "a ".repeat(40_000)));
    let (mut service, _log) = fake_service(&view, &["qqq"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(0, 4)]);
}

#[test]
fn the_marks_list_is_cut_back_far_from_the_screen_and_those_words_get_checked_again() {
    let line = "speling speling speling speling\n";
    let mut view = laid_out(&line.repeat(5_000));
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let at_top = service.marks().len();
    assert!(at_top > 40, "{at_top} marks");
    service.set_limits_for_test(40, 100);
    view.set_scroll_position(50_000.0, &mut c);
    support::settle(&mut view);
    log.clear();
    service.refresh(&mut view, &mut c);
    assert!(
        service.marks().iter().all(|m| m.range.start.0 > 200),
        "the marks at the top went"
    );
    finish(&mut service, &mut view, &mut c);
    assert!(service.marks().iter().all(|m| m.range.start.0 > 200));

    // Going back finds those words not checked, so they are looked at again.
    view.set_scroll_position(0.0, &mut c);
    support::settle(&mut view);
    log.clear();
    service.set_limits_for_test(usize::MAX, 100);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert!(!log.checked_text().is_empty());
    assert!(service.marks().iter().any(|m| m.range.start == b(0)));
}

#[test]
fn a_sort_order_in_a_locale_name_does_not_stop_a_dictionary_being_found() {
    assert_eq!(without_sort_order("de-DE_phoneb"), "de-DE");
    assert_eq!(without_sort_order("en-US"), "en-US");
    let have = ["en-US".to_owned(), "de-DE".to_owned()];
    assert_eq!(
        choose_language("de-DE_phoneb", &[], &have),
        Some("de-DE".into())
    );
}

#[test]
fn the_dictionary_is_the_tag_then_its_language_then_the_display_languages() {
    let have = ["en-US".to_owned(), "fr-FR".to_owned()];
    assert_eq!(choose_language("fr-FR", &[], &have), Some("fr-FR".into()));
    assert_eq!(choose_language("en-CA", &[], &have), Some("en-US".into()));
    assert_eq!(choose_language("ja-JP", &[], &have), None);
    assert_eq!(
        choose_language("ja-JP", &["fr-CA".to_owned()], &have),
        Some("fr-FR".into()),
        "Windows' own display language is next"
    );
    assert_eq!(
        choose_language("FR-fr", &[], &have),
        Some("fr-FR".into()),
        "tags ignore case"
    );
}

#[test]
fn the_menu_order_is_suggestions_then_ignore_then_add() {
    let mut view = laid_out("speling");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let items = service.menu_at(&view, b(0)).unwrap().items();
    assert_eq!(items[items.len() - 2], FixItem::Ignore);
    assert_eq!(items[items.len() - 1], FixItem::AddToDictionary);
}

// ---- Second review ----

#[test]
fn a_word_edited_at_its_edge_gets_no_fix_until_it_is_checked_again() {
    let mut view = laid_out("speling here");
    let (mut service, log) = fake_service(&view, &["speling", "spelingx"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    log.hold();
    view.replace_range(b(7)..b(7), "x", &mut c);
    assert_eq!(service.menu_at(&view, b(3)), None, "at the end of the word");
    log.release();
    support::settle(&mut view);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    let menu = service.menu_at(&view, b(3)).unwrap();
    assert_eq!(menu.word, "spelingx");
    assert_eq!(menu.range, b(0)..b(8));
    service
        .apply_fix(&mut view, &menu, "spelling", &mut c)
        .unwrap();
    assert_eq!(view.text().unwrap(), "spelling here");
}

#[test]
fn a_word_edited_at_its_start_gets_no_fix_until_it_is_checked_again() {
    let mut view = laid_out("a speling");
    let (mut service, log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    log.hold();
    view.replace_range(b(2)..b(2), "x", &mut c);
    assert_eq!(service.menu_at(&view, b(5)), None);
    log.release();
}

#[test]
fn a_space_after_a_word_brings_its_mark_back_after_a_new_check() {
    let mut view = laid_out("speling");
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    view.replace_range(b(7)..b(7), " ", &mut c);
    support::settle(&mut view);
    service.refresh(&mut view, &mut c);
    finish(&mut service, &mut view, &mut c);
    assert_eq!(marks(&view), vec![(0, 7)]);
}

#[test]
fn checked_ranges_are_cut_back_after_a_long_scroll_with_few_marks() {
    let mut view = laid_out(&"plain words here\n".repeat(50_000));
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.set_limits_for_test(usize::MAX, 100);
    service.set_range_limit_for_test(3);
    let mut most = 0;
    for step in 0..12 {
        view.set_scroll_position(f64::from(step) * 8_000.0, &mut c);
        support::settle(&mut view);
        service.refresh(&mut view, &mut c);
        finish(&mut service, &mut view, &mut c);
        most = most.max(service.checked_range_count());
    }
    assert!(most <= 5, "{most} separate ranges");
}

#[test]
fn an_edit_with_full_lists_stays_under_the_key_budget_in_release() {
    let mut view = laid_out(&"plain words here\n".repeat(50_000));
    let (mut service, _log) = fake_service(&view, &["speling"]);
    let mut c = ctx();
    service.fill_lists_for_test(&view);
    const KEYS: u32 = 200;
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    for at in 0..KEYS as usize {
        view.replace_range(b(at)..b(at), "x", &mut c);
        c.requests.clear();
        let timer = Instant::now();
        service.refresh(&mut view, &mut c);
        let spent = timer.elapsed();
        worst = worst.max(spent);
        total += spent;
        finish(&mut service, &mut view, &mut c);
    }
    println!(
        "MEASURE full lists: mean={:?} worst={worst:?} keys={KEYS}",
        total / KEYS
    );
    if !cfg!(debug_assertions) {
        assert!(
            total / KEYS < Duration::from_millis(2),
            "mean {:?}",
            total / KEYS
        );
    }
}
