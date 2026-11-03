//! Opening: the encoding order, the size rules, finding an open file by its real identity,
//! the launch window, naming, and Open Recent.
mod support;

use qe_api::{EncodingChoice, TextEncoding};
use qe_codec::LineEnding;
use qe_docs::{DocErrorKind, DocEvent, OpenTarget, RECENT_LIMIT};
use support::*;

fn failed_kind(h: &mut Harness) -> DocErrorKind {
    h.until("the open fails", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::OpenFailed { .. }))
    });
    h.events
        .iter()
        .find_map(|e| match e {
            DocEvent::OpenFailed { error, .. } => Some(error.kind),
            _ => None,
        })
        .unwrap()
}

#[test]
fn utf8_opens_with_its_line_ending_and_marks_nothing_dirty() {
    let folder = test_folder("open-utf8");
    let path = folder.join("a.txt");
    write(
        &path,
        "one\r\ntwo\r\n\u{1f468}\u{200d}\u{1f469}e\u{301}".as_bytes(),
    );
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    assert_eq!(
        h.text(doc),
        "one\r\ntwo\r\n\u{1f468}\u{200d}\u{1f469}e\u{301}"
    );
    let format = h.store.format(doc).unwrap();
    assert_eq!(format.encoding, TextEncoding::Utf8);
    assert_eq!(format.main_line_ending, LineEnding::CrLf);
    assert!(!format.has_bom && !format.lossy);
    assert!(!h.store.is_dirty(doc));
    assert_eq!(h.store.display_name(doc), "a.txt");
}

#[test]
fn a_bom_beats_utf8_and_a_chosen_encoding_beats_the_bom_rules() {
    let folder = test_folder("open-order");
    let bom16 = folder.join("bom16.txt");
    write(&bom16, &[0xFF, 0xFE, b'h', 0, b'i', 0]);
    // These bytes are good UTF-8, so only the choice can make them read as Windows-1252.
    let latin = folder.join("latin.txt");
    write(&latin, &[b'c', 0xC3, 0xA9]);
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&bom16);
    assert_eq!(h.text(doc), "hi");
    let format = h.store.format(doc).unwrap();
    assert!(format.has_bom, "{format:?}");
    assert!(matches!(
        format.encoding,
        TextEncoding::Utf16 | TextEncoding::Utf16Le
    ));
    let doc = h.open_with(&latin, EncodingChoice::Specific(TextEncoding::Windows1252));
    assert_eq!(h.text(doc), "c\u{c3}\u{a9}");
    assert_eq!(
        h.store.format(doc).unwrap().encoding,
        TextEncoding::Windows1252
    );
}

#[test]
fn bytes_that_do_not_fit_a_chosen_encoding_give_a_named_error_and_no_tab() {
    let folder = test_folder("open-unfit");
    let path = folder.join("bad.txt");
    write(&path, &[0xFF, 0xFE, 0xFD]);
    let mut h = Harness::new(&folder.join("root"));
    let (window, _) = h.store.new_window(false).unwrap();
    h.store
        .open_paths(
            &[path],
            OpenTarget::Window(window),
            Some(EncodingChoice::Specific(TextEncoding::Utf8)),
        )
        .unwrap();
    assert_eq!(failed_kind(&mut h), DocErrorKind::EncodingDoesNotFit);
    assert_eq!(
        h.store.tabs(window).len(),
        1,
        "the failed file leaves no tab"
    );
}

#[test]
fn a_missing_file_and_a_folder_give_named_errors() {
    let folder = test_folder("open-missing");
    let mut h = Harness::new(&folder.join("root"));
    h.store
        .open_paths(&[folder.join("nope.txt")], OpenTarget::NewWindow, None)
        .unwrap();
    assert_eq!(failed_kind(&mut h), DocErrorKind::FileNotFound);
    h.clear_events();
    h.store
        .open_paths(std::slice::from_ref(&folder), OpenTarget::NewWindow, None)
        .unwrap();
    assert_eq!(failed_kind(&mut h), DocErrorKind::IsAFolder);
    assert_eq!(h.store.windows().len(), 0, "an empty window does not stay");
}

#[test]
fn open_returns_before_the_text_arrives_and_the_view_takes_no_edit_meanwhile() {
    let folder = test_folder("open-async");
    let path = folder.join("a.txt");
    write(&path, b"text");
    let mut h = Harness::new(&folder.join("root"));
    let opened = h
        .store
        .open_paths(&[path], OpenTarget::NewWindow, None)
        .unwrap();
    let doc = opened[0];
    assert!(
        h.store.is_loading(doc),
        "no poll has run, so nothing has arrived"
    );
    assert_eq!(h.text(doc), "");
    h.type_text(doc, "x");
    assert_eq!(h.text(doc), "", "a loading view refuses edits");
    h.loaded(doc);
    assert_eq!(h.text(doc), "text");
    assert!(!h.store.is_dirty(doc));
}

#[test]
fn the_same_file_is_found_by_path_text_in_any_case() {
    let folder = test_folder("open-same-text");
    let path = folder.join("Same.txt");
    write(&path, b"x");
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.clear_events();
    let again = h
        .store
        .open_paths(&[folder.join("SAME.TXT")], OpenTarget::LastUsedWindow, None)
        .unwrap();
    assert!(again.is_empty(), "no new tab");
    h.pump();
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::AlreadyOpen { doc: d, .. } if *d == doc)),
        1
    );
    assert_eq!(h.store.all_documents().len(), 1);
}

#[test]
fn the_same_file_is_found_by_identity_through_a_hard_link() {
    let folder = test_folder("open-same-identity");
    let path = folder.join("real.txt");
    write(&path, b"x");
    let link = folder.join("other-name.txt");
    std::fs::hard_link(&path, &link).unwrap();
    let mut h = Harness::new(&folder.join("root"));
    let doc = h.open(&path);
    h.clear_events();
    let again = h
        .store
        .open_paths(&[link], OpenTarget::LastUsedWindow, None)
        .unwrap();
    assert_eq!(
        again.len(),
        1,
        "the text of the path differs, so it starts a load"
    );
    h.until("the load finds the open tab", |h| {
        h.events
            .iter()
            .any(|e| matches!(e, DocEvent::AlreadyOpen { doc: d, .. } if *d == doc))
    });
    assert_eq!(
        h.store.all_documents(),
        vec![doc],
        "it was not opened twice"
    );
    assert_eq!(h.store.tabs(h.store.last_used_window().unwrap()), vec![doc]);
}

#[test]
fn two_files_with_the_same_name_in_two_folders_are_two_documents() {
    let folder = test_folder("open-two-folders");
    let (a, b) = (
        folder.join("1").join("n.txt"),
        folder.join("2").join("n.txt"),
    );
    write(&a, b"a");
    write(&b, b"b");
    let mut h = Harness::new(&folder.join("root"));
    let (one, two) = (h.open(&a), h.open(&b));
    assert_ne!(one, two);
    assert_eq!((h.text(one).as_str(), h.text(two).as_str()), ("a", "b"));
}

fn low_limits(root: &std::path::Path, large: u64, read_only: u64) -> Harness {
    let (mut config, wakes) = config(root);
    config.large_bytes = large;
    config.read_only_bytes = read_only;
    Harness::with_config(config, wakes)
}

#[test]
fn over_the_large_limit_the_title_says_so_and_the_text_stays_editable() {
    let folder = test_folder("open-large");
    let path = folder.join("big.txt");
    write(&path, &vec![b'a'; 2_000]);
    let mut h = low_limits(&folder.join("root"), 1_000, 1_000_000);
    let doc = h.open(&path);
    assert!(h.store.is_large(doc));
    assert!(!h.store.is_too_large_to_edit(doc));
    assert_eq!(
        h.store.title_note(doc).unwrap(),
        "Large file: spelling and substitutions off"
    );
    h.type_text(doc, "x");
    assert!(h.store.is_dirty(doc));
}

#[test]
fn over_the_read_only_limit_nothing_edits_or_autosaves_and_the_title_says_so() {
    let folder = test_folder("open-read-only");
    let path = folder.join("huge.txt");
    write(&path, "line\n".repeat(400).as_bytes());
    let root = folder.join("root");
    let mut h = low_limits(&root, 100, 1_000);
    let doc = h.open(&path);
    assert!(h.store.is_too_large_to_edit(doc));
    assert!(h.store.prevent_editing(doc));
    assert_eq!(
        h.store.title_note(doc).unwrap(),
        "Read only: file too large to edit (0 MB)"
    );
    h.type_text(doc, "x");
    assert!(!h.store.is_dirty(doc), "the view refused the edit");
    let commands = h.store.commands(doc);
    assert!(!commands.save && !commands.revert && !commands.prevent_editing);
    assert!(commands.save_as && commands.duplicate);
    h.store.save(doc).unwrap();
    h.settle(std::time::Duration::from_millis(30));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "line\n".repeat(400).as_bytes()
    );
    assert!(autosave_files(&root).is_empty());
}

#[test]
fn the_title_note_uses_the_macs_size_style() {
    assert_eq!(
        qe_docs::read_only_title_note(600_000_000),
        "Read only: file too large to edit (600 MB)"
    );
    assert_eq!(
        qe_docs::read_only_title_note(1_070_000_000),
        "Read only: file too large to edit (1.07 GB)"
    );
}

#[test]
fn a_large_file_in_another_encoding_reads_through_a_utf8_copy_and_the_copy_goes_at_close() {
    let folder = test_folder("open-large-16");
    let path = folder.join("wide.txt");
    let mut bytes = vec![0xFF, 0xFE];
    for unit in "h\u{e9}llo\n".repeat(400).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    write(&path, &bytes);
    let root = folder.join("root");
    let mut h = low_limits(&root, 100, 1_000);
    let doc = h.open(&path);
    assert!(h.store.is_too_large_to_edit(doc));
    h.until("the index ends", |h| {
        h.store.view(doc).unwrap().store_status().complete
    });
    assert!(h.text(doc).starts_with("h\u{e9}llo\n"));
    assert!(h.store.format(doc).unwrap().has_bom);
    let copies = || std::fs::read_dir(root.join("large")).unwrap().count();
    assert_eq!(copies(), 1);
    h.store.request_close(doc);
    h.until("the copy goes", |_| copies() == 0);
}

#[test]
fn a_large_file_whose_bad_byte_lies_past_the_first_64_kib_still_opens() {
    let folder = test_folder("open-large-deep");
    let path = folder.join("deep.txt");
    let mut bytes = vec![b'a'; 200 * 1024];
    bytes.push(0xE9);
    bytes.extend_from_slice(&[b'z'; 1024]);
    write(&path, &bytes);
    let mut h = low_limits(&folder.join("root"), 100, 1_000);
    let doc = h.open(&path);
    h.until("the UTF-8 copy takes over", |h| {
        let status = h.store.view(doc).unwrap().store_status();
        status.failure.is_none() && status.complete && status.known_bytes.0 > 200 * 1024
    });
    assert!(h.store.format(doc).unwrap().encoding != TextEncoding::Utf8);
}

#[test]
fn a_launch_window_gives_way_to_the_first_file_in_its_place() {
    let folder = test_folder("open-transient");
    let path = folder.join("a.txt");
    write(&path, b"file text");
    let mut h = Harness::new(&folder.join("root"));
    let (window, launch) = h.store.new_window(true).unwrap();
    assert!(h.store.is_transient(launch));
    let doc = h.open(&path);
    assert_eq!(h.store.tabs(window), vec![doc], "the launch document went");
    assert_eq!(h.store.windows(), vec![window], "the same window stays");
    assert_eq!(h.store.active_tab(window), Some(doc));
    assert!(h.store.view(launch).is_none());
}

#[test]
fn a_launch_window_edited_during_a_slow_open_stays_and_keeps_the_text() {
    let folder = test_folder("open-transient-typed");
    let path = folder.join("a.txt");
    write(&path, b"file");
    let mut h = Harness::new(&folder.join("root"));
    let (window, launch) = h.store.new_window(true).unwrap();
    let opened = h
        .store
        .open_paths(&[path], OpenTarget::LastUsedWindow, None)
        .unwrap();
    assert_eq!(
        h.store.tabs(window),
        vec![launch],
        "the file waits out of sight"
    );
    h.type_text(launch, "typed");
    h.loaded(opened[0]);
    assert_eq!(h.store.tabs(window), vec![launch, opened[0]]);
    assert_eq!(h.text(launch), "typed");
}

#[test]
fn an_edit_a_move_or_a_second_document_ends_the_launch_state() {
    let folder = test_folder("open-transient-ends");
    let root = folder.join("root");
    let mut h = Harness::new(&root);
    let (window, launch) = h.store.new_window(true).unwrap();
    h.type_text(launch, "x");
    assert!(!h.store.is_transient(launch), "an edit");
    h.undo(launch);
    let (_, second) = h.store.new_window(true).unwrap();
    assert!(
        !h.store.is_transient(second),
        "not alone, so not a launch window"
    );
    drop(h);

    let mut h = Harness::new(&folder.join("root2"));
    let (window2, launch) = h.store.new_window(true).unwrap();
    h.store.set_window_frame(
        window2,
        qe_docs::WindowFrame::default(),
        qe_docs::ShowState::Normal,
        false,
    );
    assert!(
        h.store.is_transient(launch),
        "the host placing it is not the user"
    );
    h.store.set_window_frame(
        window2,
        qe_docs::WindowFrame::default(),
        qe_docs::ShowState::Normal,
        true,
    );
    assert!(!h.store.is_transient(launch), "a move by the user");
    drop(h);

    let mut h = Harness::new(&folder.join("root3"));
    let (window3, launch) = h.store.new_window(true).unwrap();
    h.store.new_tab(window3).unwrap();
    assert!(!h.store.is_transient(launch), "a second document");
    let _ = window;
}

#[test]
fn fifteen_files_land_in_order_in_tabs_of_one_window_over_the_launch_window() {
    let folder = test_folder("open-fifteen");
    let mut h = Harness::new(&folder.join("root"));
    let (window, _) = h.store.new_window(true).unwrap();
    let mut paths = Vec::new();
    for i in 0..15 {
        let path = folder.join(format!("f{i:02}.txt"));
        write(&path, format!("text {i}").as_bytes());
        paths.push(path);
    }
    let opened = h
        .store
        .open_paths(&paths, OpenTarget::LastUsedWindow, None)
        .unwrap();
    assert_eq!(opened.len(), 15);
    h.until("all load", |h| {
        opened.iter().all(|d| !h.store.is_loading(*d))
    });
    assert_eq!(h.store.windows(), vec![window]);
    assert_eq!(h.store.tabs(window), opened, "in order, with no launch tab");
    for (i, doc) in opened.iter().enumerate() {
        assert_eq!(h.text(*doc), format!("text {i}"));
    }
    assert_eq!(h.store.active_tab(window), opened.last().copied());
}

#[test]
fn explorer_files_go_to_the_window_used_last() {
    let folder = test_folder("open-last-used");
    let mut h = Harness::new(&folder.join("root"));
    let (first, _) = h.store.new_window(false).unwrap();
    let (second, _) = h.store.new_window(false).unwrap();
    assert_eq!(h.store.last_used_window(), Some(second));
    h.store.window_activated(first);
    let path = folder.join("a.txt");
    write(&path, b"x");
    let doc = h.open(&path);
    assert_eq!(h.store.window_of(doc), Some(first));
}

#[test]
fn untitled_documents_take_the_lowest_free_number() {
    let folder = test_folder("open-names");
    let mut h = Harness::new(&folder.join("root"));
    let (window, one) = h.store.new_window(false).unwrap();
    let two = h.store.new_tab(window).unwrap();
    let three = h.store.new_tab(window).unwrap();
    assert_eq!(h.store.display_name(one), "Untitled");
    assert_eq!(h.store.display_name(two), "Untitled 2");
    assert_eq!(h.store.display_name(three), "Untitled 3");
    h.store.request_close(two);
    let next = h.store.new_tab(window).unwrap();
    assert_eq!(h.store.display_name(next), "Untitled 2");
}

#[test]
fn open_recent_is_newest_first_capped_clearable_and_survives_a_restart() {
    let folder = test_folder("open-recent");
    let root = folder.join("root");
    let mut h = Harness::new(&root);
    let mut paths = Vec::new();
    for i in 0..(RECENT_LIMIT + 3) {
        let path = folder.join(format!("r{i}.txt"));
        write(&path, b"x");
        h.open(&path);
        paths.push(path);
    }
    assert_eq!(h.store.recent().len(), RECENT_LIMIT);
    assert_eq!(h.store.recent()[0], *paths.last().unwrap());
    assert!(h.count(|e| matches!(e, DocEvent::RecentChanged)) > 0);
    let kept: Vec<_> = h.store.recent().to_vec();
    h.quit();
    let mut h = Harness::new(&root);
    assert_eq!(h.store.recent(), kept.as_slice());
    h.store.clear_recent();
    assert!(h.store.recent().is_empty());
    h.quit();
    let h = Harness::new(&root);
    assert!(h.store.recent().is_empty(), "Clear Menu is saved too");
}

#[test]
fn a_txt_rule_matches_the_macs_cases() {
    use qe_docs::filename_with_txt_rule as rule;
    assert_eq!(rule("plain", true), "plain.txt");
    assert_eq!(rule("plain", false), "plain");
    assert_eq!(rule("x.md", true), "x.md");
    assert_eq!(rule(".gitignore", true), ".gitignore");
    assert_eq!(rule("dot.", true), "dot..txt");
    assert_eq!(rule("", true), "");
}

/// The person closed the tab, so the failure of its read is not news. The store takes a worker's
/// result only in a poll, so here the close always comes before the failure.
#[test]
fn closing_a_tab_while_its_missing_file_is_read_gives_no_error() {
    let folder = test_folder("open-close-missing");
    let closed = folder.join("closed.txt");
    let kept = folder.join("kept.txt");
    let mut h = Harness::new(&folder.join("root"));
    let (window, keep) = h.store.new_window(false).unwrap();
    let opened = h
        .store
        .open_paths(
            std::slice::from_ref(&closed),
            OpenTarget::Window(window),
            None,
        )
        .unwrap();
    h.store.request_close(opened[0]);
    // A tab that stays open reports the same failure, so the store is still reporting.
    h.store
        .open_paths(
            std::slice::from_ref(&kept),
            OpenTarget::Window(window),
            None,
        )
        .unwrap();
    h.until("the open that stayed fails", |h| {
        h.count(|e| matches!(e, DocEvent::OpenFailed { path, .. } if *path == kept)) == 1
    });
    h.settle(std::time::Duration::from_millis(50));
    assert_eq!(
        h.count(|e| matches!(e, DocEvent::OpenFailed { path, .. } if *path == closed)),
        0
    );
    assert_eq!(h.store.tabs(window), vec![keep]);
    assert_eq!(h.store.all_documents(), vec![keep]);
}

#[test]
fn closing_a_document_during_its_load_drops_the_load() {
    let folder = test_folder("open-close-early");
    let path = folder.join("a.txt");
    write(&path, b"x");
    let mut h = Harness::new(&folder.join("root"));
    let (window, keep) = h.store.new_window(false).unwrap();
    let opened = h
        .store
        .open_paths(&[path], OpenTarget::Window(window), None)
        .unwrap();
    h.store.request_close(opened[0]);
    h.settle(std::time::Duration::from_millis(50));
    assert_eq!(h.store.tabs(window), vec![keep]);
    assert_eq!(h.store.all_documents(), vec![keep]);
}
