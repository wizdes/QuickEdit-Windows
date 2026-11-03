//! Alerts, warnings, compositions and the end of the app, in real event order. A composition here
//! is a real one, started in real TSF as a text service starts it, on a hidden window.
mod support;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use support::{App, Folder, decode, encode, exe};

fn frame_text(app: &mut App) -> String {
    decode(&app.ask("frame-text")).replace('\n', " ")
}

/// Two windows: `a.txt` in the first, and `b.txt` with unsaved text in the second.
fn two_windows(folder: &Folder) -> App {
    let a = folder.file("a.txt", b"first");
    let b = folder.file("b.txt", b"second");
    let mut app = App::start(&folder.root(), &[&a]);
    app.wait_text("first");
    app.run("New");
    app.wait_for("a second window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    app.ok(&format!("open {}", encode(&b.display().to_string())));
    app.wait_text("second");
    app.ok("select 6 6");
    app.type_text("!");
    app
}

fn focus_for_tsf(app: &mut App) {
    app.ok("focus");
    app.wait_for("text services", |app| app.ask("tsf-started") == "true");
    app.ok("focus");
    app.ask("ping");
}

// ---- Alerts ----

#[test]
fn x_on_an_alert_with_no_owner_ends_it_and_the_app_can_still_close() {
    let folder = Folder::new("x-ownerless");
    let missing = folder.0.join("missing.txt");
    let mut app = App::start(&folder.root(), &[&missing]);
    app.wait_value("the error", App::alert);
    app.ok("alert-x");
    app.wait_for("the alert to go", |app| app.alert().is_none());
    app.type_text("typed");
    app.ok("close-window");
    let (title, _, _) = app.wait_value("the question", App::alert);
    assert!(title.contains("keep the changes"), "{title}");
    app.press("Don't Save");
    app.wait_exit();
}

#[test]
fn x_on_a_question_counts_as_cancel_and_the_next_alert_stays_open() {
    let folder = Folder::new("x-question");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("keep me");
    app.run("Close");
    app.wait_value("the question", App::alert);
    // A second alert waits behind it. A late message about the first must not dismiss it.
    let missing = folder.0.join("nothing-here.txt");
    app.ok(&format!("open {}", encode(&missing.display().to_string())));
    app.ask("ping");
    app.ok("alert-x");
    let (title, _, _) = app.wait_value("the next alert", |app| {
        app.alert()
            .filter(|(title, _, _)| title.contains("nothing-here"))
    });
    assert!(title.contains("could not open"), "{title}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(app.alert().is_some(), "the waiting alert stays open");
    app.ok("alert-x");
    app.wait_for("the alert to go", |app| app.alert().is_none());
    assert_eq!(app.text(), "keep me");
    app.type_text("!");
    assert_eq!(app.text(), "keep me!", "Cancel gave the keys back");
    app.quit();
}

#[test]
fn a_question_that_loses_its_window_to_a_merge_asks_again_in_the_new_window() {
    let folder = Folder::new("merge-owner");
    let mut app = two_windows(&folder);
    app.run("Close");
    app.wait_value("the question", App::alert);
    let second = app.ask("hwnd-at 1");
    assert_eq!(app.ask("alert-owner"), second);
    let first = app.ask("hwnd-at 0");
    app.ok("use-window 0");
    app.run("MergeAllWindows");
    app.wait_for("one window", |app| app.windows().len() == 1);
    let owner = {
        app.wait_for("the question again", |app| app.ask("alert-owner") == first);
        app.ask("alert-owner")
    };
    assert_eq!(owner, first);
    assert_eq!(app.ask("window-enabled"), "false");
    app.press("Cancel");
    app.wait_for("the question to go", |app| app.alert().is_none());
    app.quit();
}

#[test]
fn an_alert_waiting_behind_one_whose_owner_closes_keeps_its_text() {
    let folder = Folder::new("queued-owner");
    let a = folder.file("a.txt", b"first");
    let b = folder.file("b.txt", b"second");
    let mut app = App::start(&folder.root(), &[&a]);
    app.wait_text("first");
    app.run("New");
    app.wait_for("a second window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    app.ok(&format!("open {}", encode(&b.display().to_string())));
    app.wait_text("second");
    app.run("Rename");
    app.wait_value("the rename box", App::alert);
    let missing = folder.0.join("THE_MISSING_FILE.txt");
    app.ok(&format!("open {}", encode(&missing.display().to_string())));
    // A worker reads the file. If the tab closes before that read ends, the open ends with no
    // error, as the person closed it, and there is no alert to wait behind this one. So the close
    // comes only when the error is in the queue.
    app.wait_for("the error to wait behind the rename box", |app| {
        decode(&app.ask("alert-waiting")).contains("THE_MISSING_FILE")
    });
    let (open_title, _, _) = app.alert().expect("the rename box is still open");
    assert_eq!(open_title, "Rename");
    app.ok("close-window-at 1");
    let (title, message, _) = app.wait_value("the error", |app| {
        app.alert()
            .filter(|(title, _, _)| title.contains("THE_MISSING_FILE"))
    });
    assert!(title.contains("could not open"), "{title}: {message}");
    assert!(
        !app.notes().contains("could not show"),
        "the error showed as itself"
    );
    app.press("OK");
    app.quit();
}

#[test]
fn a_question_is_owned_by_its_documents_window_and_that_window_is_disabled() {
    let folder = Folder::new("own-window");
    let mut app = two_windows(&folder);
    // The window in use is the first, and the X of the second asks the question.
    app.ok("use-window 0");
    app.ok("close-window-at 1");
    app.wait_value("the question", App::alert);
    assert_eq!(app.ask("alert-owner"), app.ask("hwnd-at 1"));
    assert_eq!(app.ask("enabled-at 1"), "false");
    assert_eq!(app.ask("enabled-at 0"), "true");
    app.press("Cancel");
    app.wait_for("the window back", |app| app.ask("enabled-at 1") == "true");
    app.quit();
}

#[test]
fn every_path_that_edits_is_refused_while_a_question_is_open() {
    let folder = Folder::new("guard");
    let mut app = App::start(&folder.root(), &[]);
    app.run("CheckSpellingWhileTyping");
    focus_for_tsf(&mut app);
    app.type_text("speling here!");
    app.wait_for("the mark", |app| app.ask("marks") == "1");
    assert_eq!(app.ask("spell-menu 0"), "true");
    app.ok("replace-all here there");
    app.wait_text("speling there!");
    app.run("Close");
    app.wait_value("the question", App::alert);
    app.ok("focus");
    let before = app.text();
    // The right-click row.
    app.ok("spell-row fix%3Aspelling");
    // The bar's button, by a screen reader and by a click.
    app.ok("named invoke Replace%20All");
    app.ok("named click Replace%20All");
    // Keys and menu commands.
    app.type_text("x");
    app.ok("key 8");
    for command in ["Undo", "Paste", "UpperCase"] {
        app.run(command);
    }
    // The bar's own path, and an input method. A text store that is held refuses to write.
    app.ok("replace-all there nowhere");
    let composed = app.ask("compose q");
    app.ask("ping");
    assert_eq!(app.ask("composing"), "false", "{composed}");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(app.text(), before);
    app.press("Cancel");
    app.wait_for("the question to go", |app| app.alert().is_none());
    // Each path works again. The bar has the keyboard until Esc closes it.
    app.ok("key 27");
    app.ok("key 27");
    app.ok(&format!("select {0} {0}", before.len()));
    app.type_text("!");
    assert_eq!(app.text(), format!("{before}!"));
    app.quit();
}

#[test]
fn a_spelling_row_works_when_no_question_is_open() {
    let folder = Folder::new("spell-row");
    let mut app = App::start(&folder.root(), &[]);
    app.run("CheckSpellingWhileTyping");
    app.type_text("speling here!");
    app.wait_for("the mark", |app| app.ask("marks") == "1");
    assert_eq!(app.ask("spell-menu 0"), "true");
    app.ok("spell-row fix%3Aspelling");
    app.wait_text("spelling here!");
    app.quit();
}

#[test]
fn close_twice_asks_once() {
    let folder = Folder::new("twice");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("words");
    app.run("Close");
    app.wait_value("the question", App::alert);
    app.run("Close");
    app.ask("ping");
    app.press("Cancel");
    app.wait_for("the question to go", |app| app.alert().is_none());
    std::thread::sleep(Duration::from_millis(100));
    assert!(app.alert().is_none(), "the same question came twice");
    assert_eq!(app.text(), "words");
    app.quit();
}

// ---- Warnings ----

#[test]
fn launch_notes_still_paint_after_a_tab_loads_and_all_of_them_show() {
    let folder = Folder::new("notes-paint");
    let root = folder.root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("settings.txt"),
        "WindowWidthCharacters=banana\nWindowHeightLines=pear\n",
    )
    .unwrap();
    let file = folder.file("loaded.txt", b"text on disk\n");
    let mut app = App::start(&root, &[&file]);
    app.wait_text("text on disk\n");
    let shown = frame_text(&mut app);
    assert!(
        shown.contains("banana") || shown.contains("Window"),
        "{shown}"
    );
    let notes = app.notes();
    for line in notes.lines() {
        // Each note on its own, in the pixels' source, whichever way it wrapped.
        let start: String = line.chars().take(12).collect();
        assert!(shown.contains(&start), "{start:?} is not in {shown:?}");
    }
    // Its color is in the real pixels too.
    let image = folder.0.join("frame.bin");
    let with_notes = warning_pixels(&mut app, &image);
    // Typing does not take it away, and Esc does.
    app.type_text("x");
    assert!(frame_text(&mut app).contains(&notes.chars().take(12).collect::<String>()));
    while !frame_text(&mut app).is_empty() {
        app.ok("key 27");
        app.ask("ping");
    }
    let without = warning_pixels(&mut app, &image);
    assert!(with_notes > without + 100, "{with_notes} and {without}");
    app.quit();
}

/// How many pixels of a painted frame hold the light theme's warning red, or close to it.
fn warning_pixels(app: &mut App, image: &Path) -> usize {
    app.ok(&format!(
        "picture {} light",
        encode(&image.display().to_string())
    ));
    let bytes = std::fs::read(image).unwrap();
    let start = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    bytes[start..]
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[2] > 150 && p[1] < 90 && p[0] < 90)
        .count()
}

fn set_up_autosave(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("settings.txt"), "AutosavingDelaySeconds=1\n").unwrap();
}

#[test]
fn a_failed_autosave_stays_on_its_tab_until_a_copy_succeeds() {
    let folder = Folder::new("autosave-fails");
    let root = folder.root();
    set_up_autosave(&root);
    let mut app = App::start(&root, &[]);
    app.type_text("first copy");
    let docs = root.join("session").join("docs");
    app.wait_for("the first copy", |_| {
        std::fs::read_dir(&docs).is_ok_and(|dir| {
            dir.filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|x| x == "qea"))
        })
    });
    // Copies now have nowhere to go.
    std::fs::remove_dir_all(&docs).unwrap();
    std::fs::write(&docs, b"not a folder").unwrap();
    app.type_text(" more");
    app.wait_for("the warning", |app| {
        frame_text(app).contains("no safe copy")
    });
    let name = app.doc("name");
    assert!(
        decode(&app.ask("windows")).contains(&name),
        "the tab keeps a name"
    );
    // It is not dismissed, and an edit does not clear it.
    app.ok("key 27");
    app.type_text(" and more");
    assert!(frame_text(&mut app).contains("no safe copy"));
    // Another tab says nothing of it, and it is still there on return.
    app.run("NewTab");
    assert!(!frame_text(&mut app).contains("no safe copy"));
    app.run("Tab1");
    assert!(frame_text(&mut app).contains("no safe copy"));
    // A copy succeeds, and the warning goes by itself.
    std::fs::remove_file(&docs).unwrap();
    std::fs::create_dir_all(&docs).unwrap();
    app.type_text("!");
    app.wait_for("the warning to go", |app| {
        !frame_text(app).contains("no safe copy")
    });
    app.quit();
}

#[test]
fn a_long_warning_wraps_and_its_end_can_be_read() {
    let folder = Folder::new("long-warning");
    let mut app = App::start(&folder.root(), &[]);
    // A path with no space in it, longer than any window is wide, ending in a mark.
    let long = format!("{}END_OF_THE_PATH", "folder\\".repeat(90));
    app.ok(&format!("say {}", encode(&long)));
    let mut seen = String::new();
    for _ in 0..30 {
        let shown = decode(&app.ask("frame-text"));
        let shown = shown.replace('\n', "");
        seen.push_str(&shown);
        if seen.contains("END_OF_THE_PATH") {
            break;
        }
        app.ok("key 27");
        app.ask("ping");
    }
    assert!(seen.contains("END_OF_THE_PATH"), "{seen}");
    app.quit();
}

#[test]
fn closing_a_focused_window_paints_no_false_warning() {
    let folder = Folder::new("false-warning");
    let mut app = App::start(&folder.root(), &[]);
    app.run("New");
    app.wait_for("two windows", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    focus_for_tsf(&mut app);
    app.run("Close");
    app.wait_for("one window", |app| app.windows().len() == 1);
    app.ask("ping");
    let notes = app.notes();
    assert!(!notes.contains("TSF"), "{notes}");
    app.quit();
}

// ---- Compositions ----

#[test]
fn close_commits_a_composition_and_a_clean_document_then_asks() {
    let folder = Folder::new("close-commit");
    let file = folder.file("clean.txt", b"abc");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("abc");
    focus_for_tsf(&mut app);
    app.ok("select 3 3");
    app.ok(&format!("compose {}", encode("\u{4e2d}\u{6587}")));
    assert_eq!(app.ask("composing"), "true");
    assert_eq!(app.doc("dirty"), "false");
    app.run("Close");
    let (title, _, _) = app.wait_value("the question", App::alert);
    assert!(title.contains("keep the changes"), "{title}");
    app.press("Save");
    app.wait_for("the file", |_| {
        std::fs::read(&file).is_ok_and(|bytes| bytes == "abc\u{4e2d}\u{6587}".as_bytes())
    });
    // It was the only tab, so the app ends, and it ended with a commit, not a lost note.
    app.wait_exit();
}

#[test]
fn the_tabs_x_and_the_windows_x_commit_a_composition() {
    for way in ["tab-x", "close-window"] {
        let folder = Folder::new(way);
        let file = folder.file("clean.txt", b"abc");
        let mut app = App::start(&folder.root(), &[&file]);
        app.wait_text("abc");
        focus_for_tsf(&mut app);
        app.ok("select 3 3");
        app.ok("compose xyz");
        app.ok(way);
        app.wait_value("the question", App::alert);
        assert_eq!(app.text(), "abcxyz", "{way}");
        app.press("Cancel");
        app.wait_for("the question to go", |app| app.alert().is_none());
        app.quit();
    }
}

#[test]
fn save_all_commits_every_documents_composition() {
    let folder = Folder::new("save-all-commit");
    let a = folder.file("a.txt", b"one");
    let b = folder.file("b.txt", b"two");
    let mut app = App::start(&folder.root(), &[&a, &b]);
    app.settled();
    app.run("Tab1");
    app.settled();
    focus_for_tsf(&mut app);
    app.ok("select 3 3");
    app.ok("compose AAA");
    // The composition goes on in a tab that is not on show.
    app.run("Tab2");
    app.ask("ping");
    app.run("SaveAll");
    app.wait_for("the file", |_| {
        std::fs::read(&a).is_ok_and(|bytes| bytes == b"oneAAA")
    });
    app.quit();
}

#[test]
fn quit_keeps_a_composition_in_a_clean_document() {
    let folder = Folder::new("quit-commit");
    let file = folder.file("clean.txt", b"abc");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("abc");
    focus_for_tsf(&mut app);
    app.ok("select 3 3");
    app.ok("compose xyz");
    app.quit();
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_text("abcxyz");
    assert_eq!(std::fs::read(&file).unwrap(), b"abc");
    again.quit();
}

#[test]
fn session_end_commits_before_the_copies_are_written() {
    let folder = Folder::new("session-end");
    let file = folder.file("clean.txt", b"abc");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("abc");
    focus_for_tsf(&mut app);
    app.ok("select 3 3");
    app.ok("compose xyz");
    // WM_ENDSESSION, as Windows sends it.
    app.ok("post 22 1 0");
    app.wait_exit();
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_text("abcxyz");
    again.quit();
}

/// Text long enough that End on the last line waits for a layout.
fn waiting_key(folder: &Folder, flags: &[&str]) -> (App, std::path::PathBuf) {
    let body = "\u{4e2d}\u{6587} text\n".repeat(2000);
    let file = folder.file("long.txt", body.as_bytes());
    let mut app = App::start_with(&folder.root(), &[&file], flags);
    app.wait_text(&body);
    focus_for_tsf(&mut app);
    app.ok("select 0 0");
    app.ok("compose x");
    assert_eq!(app.ask("composing"), "true");
    app.ok("pause-workers true");
    // A new zoom needs a new layout, which the paused workers hold back, so End waits for it.
    app.run("ZoomIn");
    app.ok("key 35");
    assert_ne!(
        app.ask("queued-keys"),
        "0",
        "this test needs a key that waits"
    );
    (app, file)
}

#[test]
fn save_with_a_composition_that_cannot_commit_yet_leaves_the_app_answering() {
    let folder = Folder::new("spin-save");
    let (mut app, file) = waiting_key(&folder, &["--watchdog", "8000"]);
    app.run("Save");
    // The old code ran this in one pump for good. Now the message loop has its turn.
    for _ in 0..20 {
        assert_eq!(app.try_ask("ping", Duration::from_secs(5)).unwrap(), "ok");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.ask("composing"), "true", "the key still goes first");
    app.ok("pause-workers false");
    app.wait_for("the save", |_| {
        std::fs::read_to_string(&file).is_ok_and(|text| text.starts_with('x'))
    });
    assert_eq!(app.ask("composing"), "false");
    app.quit();
}

#[test]
fn quit_with_a_composition_that_cannot_commit_yet_ends_when_it_can() {
    let folder = Folder::new("spin-quit");
    let root = folder.root();
    let (mut app, _file) = waiting_key(&folder, &["--watchdog", "8000"]);
    app.ok("quit");
    for _ in 0..20 {
        assert_eq!(app.try_ask("ping", Duration::from_secs(5)).unwrap(), "ok");
        std::thread::sleep(Duration::from_millis(10));
    }
    app.ok("pause-workers false");
    app.wait_exit();
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_for("the text", |app| app.text().starts_with('x'));
    again.quit();
}

#[test]
fn tsf_focus_then_dont_save_many_times_leaves_no_dead_store() {
    let folder = Folder::new("dont-save");
    let mut app = App::start_with(&folder.root(), &[], &["--watchdog", "8000"]);
    for round in 0..25 {
        app.run("NewTab");
        focus_for_tsf(&mut app);
        app.type_text("x");
        app.run("Close");
        app.press("Don't Save");
        app.wait_for("the tab to go", |app| app.alert().is_none());
        assert_eq!(
            app.try_ask("ping", Duration::from_secs(5)).unwrap(),
            "ok",
            "round {round}"
        );
    }
    app.quit();
}

// ---- The end of the app ----

#[test]
fn an_export_running_at_exit_stops_and_says_so_with_no_temp_file() {
    let folder = Folder::new("pdf-exit");
    let body = "words on a page\n".repeat(400_000);
    let file = folder.file("long.txt", body.as_bytes());
    let log = folder.0.join("errors.txt");
    let mut app = App::start_logged(&folder.root(), &[&file], &[], Some(&log));
    app.settled();
    app.run("ExportPdf");
    app.wait_panel();
    let out = folder.0.join("long.pdf");
    app.ok(&format!(
        "panel-path {}",
        encode(&out.display().to_string())
    ));
    app.ok("quit");
    app.wait_exit();
    let leftovers: Vec<_> = std::fs::read_dir(&folder.0)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".qe-pdf-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let said = std::fs::read_to_string(&log).unwrap();
    assert!(
        out.exists() || said.contains("was not finished"),
        "no PDF and no word of it: {said}"
    );
}

#[test]
fn a_closed_documents_waiting_export_ends_and_leaves_no_temp_file() {
    let folder = Folder::new("pdf-closed");
    let file = folder.0.join("mapped.txt");
    // Over the limit for editing, so the job waits for the whole of a file's index.
    std::fs::File::create(&file)
        .unwrap()
        .set_len(600 * 1024 * 1024)
        .unwrap();
    let mut app = App::start_with(&folder.root(), &[&file], &["--watchdog", "15000"]);
    app.wait_for("the file", |app| app.ask("windows").contains("mapped.txt"));
    app.wait_for("the row", |app| app.state("ExportPdf").0);
    app.run("ExportPdf");
    app.wait_panel();
    let out = folder.0.join("mapped.pdf");
    app.ok(&format!(
        "panel-path {}",
        encode(&out.display().to_string())
    ));
    app.run("Close");
    app.ask("ping");
    app.ok("quit");
    app.wait_exit();
    let leftovers: Vec<_> = std::fs::read_dir(&folder.0)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".qe-pdf-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    std::fs::remove_file(&file).unwrap();
}

#[test]
fn a_tab_moved_to_another_window_makes_that_window_the_one_in_use() {
    let folder = Folder::new("moved-tab");
    let mut app = two_windows(&folder);
    app.ok("tab-into 0");
    app.ask("ping");
    assert_eq!(app.ask("last-used-window"), "0");
    app.quit();

    // A tab that moved, with its old window still open, by the row for a new window.
    let folder = Folder::new("moved-tab-2");
    let a = folder.file("a.txt", b"a");
    let b = folder.file("b.txt", b"b");
    let c = folder.file("c.txt", b"c");
    let mut app = App::start(&folder.root(), &[&a, &b]);
    app.settled();
    app.run("New");
    app.wait_for("two windows", |app| app.windows().len() == 2);
    app.ok("use-window 0");
    app.ok("tab-into 0");
    app.ask("ping");
    assert_eq!(app.ask("last-used-window"), "1");
    // Opening a file that is open already raises the window that has it.
    app.ok("use-window 0");
    app.ok(&format!("open {}", encode(&a.display().to_string())));
    app.ok(&format!("open {}", encode(&c.display().to_string())));
    app.settled();
    app.quit();
}

#[test]
fn a_file_already_open_raises_its_own_window() {
    let folder = Folder::new("raise");
    let a = folder.file("a.txt", b"a");
    let mut app = App::start(&folder.root(), &[&a]);
    app.wait_text("a");
    app.run("New");
    app.wait_for("two windows", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    app.ok(&format!("open {}", encode(&a.display().to_string())));
    app.wait_for("the raise", |app| app.ask("forward-window") == "0");
    app.quit();
}

/// The real time of a second launch, by Windows' own record of the process. Set
/// `QE_TEST_EXE` to the release build to read the number the user meets.
#[test]
fn a_second_launch_reports_its_time_and_windows_agrees() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{FILETIME, HANDLE};
    use windows::Win32::System::Threading::GetProcessTimes;

    let folder = Folder::new("second-launch-time");
    let root = folder.root();
    let first = folder.file("first.txt", b"first");
    let later = folder.file("later.txt", b"later");
    let mut app = App::start(&root, &[&first]);
    app.wait_text("first");
    let mut samples = Vec::new();
    for round in 0..5 {
        let log = folder.0.join(format!("second-{round}.txt"));
        let started = std::time::Instant::now();
        let mut child = Command::new(exe())
            .arg("--timings")
            .arg("--hidden")
            .arg("--data-root")
            .arg(&root)
            .arg(&later)
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
        let waited = started.elapsed().as_secs_f64() * 1000.0;
        let empty = FILETIME::default();
        let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
        // SAFETY: `child` still holds its handle, and the four values live through the call.
        unsafe {
            GetProcessTimes(
                HANDLE(child.as_raw_handle()),
                &raw mut created,
                &raw mut exited,
                &raw mut kernel,
                &raw mut user,
            )
        }
        .unwrap();
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        let lived = (ticks(exited) - ticks(created)) as f64 / 10_000.0;
        let said = std::fs::read_to_string(&log).unwrap();
        let line = said
            .lines()
            .find(|line| line.starts_with("second launch:"))
            .unwrap_or_else(|| panic!("no second-launch time in {said}"));
        println!("{line}; Windows says it lived {lived:.3} ms; the parent waited {waited:.3} ms");
        let handed: f64 = line
            .split("handed off after ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no number in {line}"));
        assert!(handed > 0.0, "{line}");
        // The clock behind `lived` ticks coarsely, so the app's own number may pass it a little.
        assert!(handed <= lived + 20.0, "{line}, lived {lived}");
        samples.push(lived);
    }
    app.wait_for("the later file", |app| {
        app.windows().iter().flatten().count() >= 2
    });
    app.quit();
    samples.sort_by(f64::total_cmp);
    println!("a second launch lives {:.3} ms at the median", samples[2]);
}

fn second_launch(root: &Path, file: &Path) -> std::process::Child {
    Command::new(exe())
        .arg("--test-channel")
        .arg("--data-root")
        .arg(root)
        .arg(file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap()
}

#[test]
fn a_launch_handing_off_during_exit_keeps_the_app_open_with_its_file() {
    let folder = Folder::new("handoff-exit");
    let root = folder.root();
    let first = folder.file("first.txt", b"one");
    let later = folder.file("later.txt", b"two");
    let mut app = App::start(&root, &[&first]);
    app.wait_text("one");
    // The exit waits here, with the pipe still open, as a launch arrives.
    app.ok("exit-pauses 1500 0");
    app.ok("quit");
    std::thread::sleep(Duration::from_millis(300));
    let mut launch = second_launch(&root, &later);
    let code = launch.wait().unwrap();
    assert!(code.success(), "{code}");
    app.wait_for("the file", |app| app.ask("windows").contains("later.txt"));
    app.ok("exit-pauses 0 0");
    app.quit();
}

#[test]
fn a_launch_after_the_pipe_closes_starts_as_the_instance() {
    let folder = Folder::new("handoff-after");
    let root = folder.root();
    let first = folder.file("first.txt", b"one");
    let later = folder.file("later.txt", b"two");
    let mut app = App::start(&root, &[&first]);
    app.wait_text("one");
    // The exit waits here, with the pipe already closed.
    app.ok("exit-pauses 0 1500");
    app.ok("quit");
    std::thread::sleep(Duration::from_millis(400));
    let mut next = App::launch(&root, &[&later], &[], None);
    let limit = Instant::now() + Duration::from_secs(30);
    loop {
        if next.ask("windows").contains("later.txt") {
            break;
        }
        assert!(Instant::now() < limit, "the file never opened");
        std::thread::sleep(Duration::from_millis(20));
    }
    app.wait_exit();
    next.quit();
}

#[test]
fn a_window_that_cannot_open_gives_its_tabs_to_another() {
    let folder = Folder::new("window-fails");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("kept");
    app.ok("fail-windows 1");
    app.run("New");
    let (title, message, _) = app.wait_value("the error", App::alert);
    assert!(title.contains("could not open"), "{title}: {message}");
    app.press("OK");
    app.wait_for("one window with both tabs", |app| {
        let windows = app.windows();
        windows.len() == 1 && windows[0].len() == 2
    });
    app.quit();
}

#[test]
fn the_only_window_failing_ends_the_app_so_no_invisible_process_stays() {
    let folder = Folder::new("only-window-fails");
    let root = folder.root();
    let log = folder.0.join("errors.txt");
    let mut app = App::launch(&root, &[], &["--test-fail-windows", "1"], Some(&log));
    let code = app.wait_code(Duration::from_secs(20));
    assert_eq!(code, Some(1), "it ended with a failure, not silently");
    let said = std::fs::read_to_string(&log).unwrap();
    assert!(said.contains("could not open"), "{said}");
    // The pipe is free, so the next launch is a real instance.
    let next = App::start(&root, &[]);
    next.quit();
}

#[test]
fn a_message_loop_error_keeps_the_text_and_says_what_happened() {
    let folder = Folder::new("loop-fails");
    let root = folder.root();
    let log = folder.0.join("errors.txt");
    let mut app = App::start_logged(&root, &[], &[], Some(&log));
    focus_for_tsf(&mut app);
    app.type_text("typed before ");
    app.ok("compose words");
    app.ok("break-loop");
    let code = app.wait_code(Duration::from_secs(20));
    assert_eq!(code, Some(1));
    let said = std::fs::read_to_string(&log).unwrap();
    assert!(said.contains("message loop failed"), "{said}");
    assert!(!said.contains("could not start"), "{said}");
    assert!(said.contains("was kept"), "{said}");
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_text("typed before words");
    again.quit();
}

#[test]
fn a_dropped_file_name_that_is_not_valid_unicode_still_opens() {
    let folder = Folder::new("odd-name");
    let mut wide: Vec<u16> =
        std::os::windows::ffi::OsStrExt::encode_wide(folder.0.as_os_str()).collect();
    wide.push(u16::from(b'\\'));
    // A lone surrogate is a legal part of a Windows file name.
    wide.extend([u16::from(b'x'), 0xD800, u16::from(b'y')]);
    let path = std::path::PathBuf::from(
        <std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(&wide),
    );
    std::fs::write(&path, b"odd name's text").unwrap();
    let mut app = App::start(&folder.root(), &[]);
    let hex: String = wide.iter().map(|unit| format!("{unit:04x}")).collect();
    app.ok(&format!("drop-file {hex}"));
    app.wait_text("odd name's text");
    assert!(!app.notes().contains("could not"), "{}", app.notes());
    assert!(app.alert().is_none());
    app.quit();
}

#[test]
fn settings_open_at_exit_closes_with_no_trouble() {
    let folder = Folder::new("settings-exit");
    let mut app = App::start(&folder.root(), &[]);
    app.run("Settings");
    app.wait_for("Settings", |app| app.ask("settings-open") == "true");
    app.quit();
}
