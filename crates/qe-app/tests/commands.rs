//! Every `CommandId`, in a table the compiler keeps whole: each row says what it must do.
mod support;
use qe_api::CommandId;
use support::{App, Folder, decode, encode};

/// What a row must do. The test sets up, runs it, and checks.
fn check_row(app: &mut App, folder: &Folder, command: CommandId) {
    use CommandId::*;
    let name = format!("{command:?}");
    match command {
        New => {
            let before = app.windows().len();
            app.run(&name);
            app.wait_for("a new window", |app| app.windows().len() == before + 1);
            app.ok("close-window");
            app.wait_for("it to close", |app| app.windows().len() == before);
        }
        NewTab => {
            let before = app.tab_count();
            app.run(&name);
            assert_eq!(app.tab_count(), before + 1);
            app.run("Close");
            app.wait_for("it to close", |app| app.tab_count() == before);
        }
        Open => {
            app.run(&name);
            assert_eq!(app.wait_panel(), "Open");
            app.ok("panel-cancel");
        }
        OpenRecent => {
            // The row has a submenu of files. Its own path is tested below with a file.
            let row = decode(&app.ask("menu-row 0 ClearRecent"));
            assert!(row.starts_with("Clear Menu"), "{row}");
        }
        ClearRecent => {
            assert!(
                app.state(&name).0,
                "a file was opened, so the list has rows"
            );
            app.run(&name);
            app.wait_for("the list to clear", |app| !app.state("ClearRecent").0);
        }
        Close => {
            app.run("NewTab");
            let before = app.tab_count();
            app.run(&name);
            app.wait_for("the tab to close", |app| app.tab_count() == before - 1);
        }
        Save => {
            app.ok("select 0 0");
            app.type_text("s");
            assert_eq!(app.doc("dirty"), "true");
            app.run(&name);
            app.wait_for("the save", |app| app.doc("dirty") == "false");
        }
        Duplicate => {
            let before = app.tab_count();
            app.run(&name);
            app.wait_for("the copy", |app| app.tab_count() == before + 1);
            app.settled();
            app.run("Close");
            app.press("Don't Save");
            app.wait_for("it to close", |app| app.tab_count() == before);
        }
        SaveAs => {
            app.run(&name);
            assert_eq!(app.wait_panel(), "Save As");
            app.ok("panel-cancel");
        }
        Rename => {
            app.run(&name);
            let (title, _, _) = app.wait_value("the question", App::alert);
            assert_eq!(title, "Rename");
            app.press("Cancel");
        }
        MoveTo => {
            app.run(&name);
            assert_eq!(app.wait_panel(), "Move To");
            app.ok("panel-cancel");
        }
        SaveAll => {
            app.ok("select 0 0");
            app.type_text("a");
            app.run(&name);
            app.wait_for("the save", |app| app.doc("dirty") == "false");
        }
        RevertToSaved => {
            let text = app.text();
            app.ok("select 0 0");
            app.type_text("r");
            app.run(&name);
            app.press("Revert");
            app.wait_text(&text);
        }
        ExportPdf => {
            app.run(&name);
            assert_eq!(app.wait_panel(), "Export as PDF");
            app.ok("panel-cancel");
        }
        Print => {
            app.run(&name);
            assert_eq!(app.wait_panel(), "Print to PDF");
            app.ok("panel-cancel");
        }
        Settings => {
            app.run(&name);
            app.wait_for("Settings", |app| app.ask("settings-open") == "true");
        }
        Undo => {
            let text = app.text();
            app.ok("select 0 0");
            app.type_text("u");
            app.run(&name);
            app.wait_text(&text);
            app.run("Redo");
            app.wait_text(&format!("u{text}"));
            app.run(&name);
            app.wait_text(&text);
        }
        Redo => {
            // Undo's row runs it, and checks the text it gives back.
            assert!(app.state(&name).0);
        }
        Cut => {
            let text = app.text();
            app.ok("select 0 1");
            app.run(&name);
            app.wait_text(&text[1..]);
            app.ok("select 0 0");
            app.run("Paste");
            app.wait_text(&text);
        }
        Copy => {
            let text = app.text();
            app.ok("select 0 1");
            app.run(&name);
            app.ok("select 0 0");
            app.run("Paste");
            app.wait_text(&format!("{}{text}", &text[..1]));
            app.run("Undo");
            app.wait_text(&text);
        }
        Paste => {
            // Cut and Copy's rows run it, and check its text.
            assert!(app.state(&name).0);
        }
        Delete => {
            let text = app.text();
            app.ok("select 0 1");
            app.run(&name);
            app.wait_text(&text[1..]);
            app.run("Undo");
            app.wait_text(&text);
        }
        Complete => {
            app.ok("select 5 5");
            app.run(&name);
            // The list opens when its worker ends. Esc then closes it.
            app.wait_for("the word list", |app| app.ask("tools-idle") == "true");
            app.ok("key 27");
            app.ok("select 0 0");
        }
        SelectAll => {
            app.ok("select 0 0");
            app.run(&name);
            let want = format!("0 {}", app.text().len());
            assert_eq!(app.ask("selection"), want);
            app.ok("select 0 0");
        }
        Find => {
            app.run(&name);
            assert!(app.ask("tools-text").contains("bar=true"));
            app.ok("key 27");
            assert!(app.ask("tools-text").contains("bar=false"));
        }
        FindReplace => {
            app.run(&name);
            assert!(app.ask("tools-text").contains("bar=true"));
            app.ok("key 27");
        }
        FindNext | FindPrevious => {
            // With nothing to find, the row beeps and the text stays. `find_next_moves_the_selection`
            // checks the row with a query.
            let text = app.text();
            app.run(&name);
            assert_eq!(app.text(), text);
        }
        UseSelectionForFind | JumpToSelection => {
            let text = app.text();
            app.ok("select 0 1");
            app.run(&name);
            assert_eq!(app.text(), text);
            app.ok("select 0 0");
        }
        SelectLine => {
            app.run(&name);
            assert!(app.ask("tools-text").contains("line=true"));
            app.ok("key 27");
            assert!(app.ask("tools-text").contains("line=false"));
        }
        ShowSpelling => {
            let text = app.text();
            app.ok(&format!("select {} {}", text.len(), text.len()));
            app.type_text(" speling");
            app.run(&name);
            let (title, message, _) = app.wait_value("the word", App::alert);
            assert_eq!(title, "Show Spelling");
            assert!(message.contains("speling"), "{message}");
            app.press("Stop");
            app.run("Undo");
            app.wait_text(&text);
        }
        CheckDocumentSpelling => {
            assert!(app.state(&name).0);
            app.run(&name);
        }
        CheckSpellingWhileTyping | CorrectSpellingAutomatically => {
            let (_, was) = app.state(&name);
            app.run(&name);
            assert_eq!(app.state(&name).1, !was);
            app.run(&name);
            assert_eq!(app.state(&name).1, was);
        }
        ShowSubstitutions => {
            // Off, with the reason in the report: the three rows beside it do the job.
            assert_eq!(app.state(&name), (false, false));
        }
        SmartCopyPaste | SmartQuotes | SmartDashes => {
            let (_, was) = app.state(&name);
            app.run(&name);
            assert_eq!(app.state(&name).1, !was);
            app.run(&name);
            assert_eq!(app.state(&name).1, was);
        }
        UpperCase | LowerCase | Capitalize => {
            let text = app.text();
            app.ok(&format!("select 0 {}", text.len()));
            app.run(&name);
            app.wait_for("the change to end", |app| app.ask("tools-idle") == "true");
            if app.text() != text {
                app.run("Undo");
                app.wait_text(&text);
            }
            app.ok("select 0 0");
        }
        PreventEditing | WrapToPage | ShowLineNumbers | ShowDocumentMap => {
            let (_, was) = app.state(&name);
            app.run(&name);
            app.wait_for("the row to change", |app| app.state(&name).1 != was);
            app.run(&name);
            app.wait_for("the row to change back", |app| app.state(&name).1 == was);
        }
        ActualSize => {
            assert!(!app.state(&name).0, "100% already");
            app.run("ZoomIn");
            assert!(app.state(&name).0);
            app.run(&name);
            assert!(!app.state(&name).0);
        }
        ZoomIn | ZoomOut => {
            app.run(&name);
            assert!(app.state("ActualSize").0);
            app.run("ActualSize");
        }
        // These act on the screen, and a hidden run must not show. The rows are on.
        FullScreen | Minimize | ZoomWindow | BringAllToFront => {
            assert!(app.state(&name).0);
            app.run(&name);
        }
        PreviousTab | NextTab => {
            app.run("NewTab");
            let active = |app: &mut App| app.ask("windows");
            let before = active(app);
            app.run(&name);
            assert_ne!(active(app), before);
            app.run("LastTab");
            app.run("Close");
            app.wait_for("the tab to close", |app| app.tab_count() == 1);
        }
        Tab1 | Tab2 | Tab3 | Tab4 | Tab5 | Tab6 | Tab7 | Tab8 | Tab9 | LastTab => {
            let index = match command {
                Tab1 => 1,
                Tab2 => 2,
                Tab3 => 3,
                Tab4 => 4,
                Tab5 => 5,
                Tab6 => 6,
                Tab7 => 7,
                Tab8 => 8,
                Tab9 => 9,
                _ => 3,
            };
            for _ in 1..index {
                app.run("NewTab");
            }
            app.run("Tab1");
            app.run(&name);
            let windows = app.windows();
            let tab = &windows[0][index - 1];
            assert!(tab.ends_with('*'), "{name} shows tab {index}: {windows:?}");
            for _ in 1..index {
                app.run("LastTab");
                app.run("Close");
            }
            app.wait_for("the tabs to close", |app| app.tab_count() == 1);
            app.run("Tab1");
        }
        MoveTabToNewWindow => {
            app.run("NewTab");
            app.run(&name);
            app.wait_for("two windows", |app| app.windows().len() == 2);
            app.run("MergeAllWindows");
            app.wait_for("one window", |app| app.windows().len() == 1);
            app.ok("use-window 0");
            // A blank tab may go away as the windows merge, by the blank-window rule.
            if app.tab_count() > 1 {
                let tabs = app.windows().remove(0);
                let blank = tabs
                    .iter()
                    .position(|tab| tab.starts_with("Untitled"))
                    .unwrap();
                app.run(&format!("Tab{}", blank + 1));
                app.run("Close");
                app.wait_for("the tab to close", |app| app.tab_count() == 1);
            }
        }
        MergeAllWindows => {
            assert!(!app.state(&name).0, "one window has nothing to merge");
        }
    }
    let _ = folder;
    assert!(app.alert().is_none(), "{name} left a question open");
    assert_eq!(app.ask("panels"), "0", "{name} left a dialog open");
}

#[test]
fn every_menu_row_does_its_job() {
    let folder = Folder::new("rows");
    let file = folder.file("table.txt", b"alpha beta\ngamma\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("alpha beta\ngamma\n");
    for command in CommandId::ALL {
        println!("row {command:?}");
        check_row(&mut app, &folder, *command);
        assert!(
            !decode(&app.ask("notes")).contains("failed"),
            "{command:?} made a note: {}",
            app.notes()
        );
    }
    app.ok("use-window 0");
    app.quit();
}

#[test]
fn open_recent_opens_its_file_through_the_row_data() {
    let folder = Folder::new("recent");
    let file = folder.file("r.txt", b"recent text");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("recent text");
    // A second tab keeps the app open: closing the last window quits.
    app.run("NewTab");
    app.run("PreviousTab");
    app.run("Close");
    app.wait_for("the list to fill", |app| app.state("ClearRecent").0);
    app.wait_for("one tab", |app| app.tab_count() == 1);
    app.ok(&format!("open-recent {}", encode(&file.to_string_lossy())));
    app.wait_text("recent text");
    app.quit();
}

#[test]
fn rename_changes_the_file_and_the_tab() {
    let folder = Folder::new("rename");
    let file = folder.file("old.txt", b"words");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("words");
    app.run("Rename");
    app.wait_value("the question", App::alert);
    app.ok("alert-field new.txt");
    app.press("Rename");
    app.wait_for("the new name", |app| app.doc("name") == "new.txt");
    assert!(folder.0.join("new.txt").exists());
    assert!(!file.exists());
    app.quit();
}

#[test]
fn a_new_window_shows_30_lines_by_the_setting() {
    let folder = Folder::new("size");
    let text: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let file = folder.file("long.txt", text.as_bytes());
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text(&text);
    app.wait_for("the rows", |app| {
        app.ok("paint");
        let answer = app.ask("rows-on-screen");
        let mut parts = answer.split(' ');
        let rows: usize = parts.next().unwrap().parse().unwrap();
        parts.next() == Some("true") && (30..=31).contains(&rows)
    });
    app.quit();
}

#[test]
fn enter_in_the_rename_box_presses_rename_through_real_key_messages() {
    let folder = Folder::new("rename-key");
    let file = folder.file("before.txt", b"words");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("words");
    app.run("Rename");
    app.wait_value("the question", App::alert);
    app.ok("alert-field after.txt");
    app.ok("alert-post 13");
    app.wait_for("the new name", |app| app.doc("name") == "after.txt");
    assert!(folder.0.join("after.txt").exists());
    app.quit();
}

#[test]
fn esc_in_a_question_is_its_cancel_through_real_key_messages() {
    let folder = Folder::new("esc");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("keep me");
    app.run("Close");
    app.wait_value("the question", App::alert);
    app.ok("alert-post 27");
    app.wait_for("the alert to go", |app| app.alert().is_none());
    assert_eq!(app.text(), "keep me");
    app.quit();
}

#[test]
fn a_large_file_says_so_and_turns_off_what_it_must() {
    let folder = Folder::new("large");
    let file = folder.0.join("large.txt");
    {
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
        let row = [b'x'; 1023];
        for _ in 0..(51 * 1024) {
            out.write_all(&row).unwrap();
            out.write_all(b"\n").unwrap();
        }
    }
    let mut app = App::start(&folder.root(), &[&file]);
    app.settled();
    app.wait_for("the note", |app| app.doc("note").contains("Large file"));
    assert!(!app.state("ShowLineNumbers").0);
    assert!(!app.state("ShowDocumentMap").0);
    assert!(!app.state("CheckSpellingWhileTyping").0);
    app.ok("select 0 0");
    app.run("SmartQuotes");
    app.type_text("\"");
    app.wait_for("the mark", |app| app.doc("dirty") == "true");
    app.run("Undo");
    app.wait_for("the undo", |app| app.doc("dirty") == "false");
    app.quit();
    std::fs::remove_file(&file).unwrap();
}

#[test]
fn a_settings_change_to_a_key_takes_effect_in_a_running_window() {
    let folder = Folder::new("keys");
    let mut app = App::start(&folder.root(), &[]);
    const F9: u16 = 0x78;
    // A real key message, through the window procedure.
    app.ok(&format!("post-key {F9}"));
    app.ask("ping");
    assert!(
        app.ask("tools-text").contains("bar=false"),
        "F9 is no key yet"
    );
    app.ok(&format!("set-key Find {F9}"));
    let row = decode(&app.ask("menu-row 1 Find"));
    assert!(row.contains("|F9|"), "the menu shows the new key: {row}");
    app.ok(&format!("post-key {F9}"));
    app.wait_for("the bar", |app| app.ask("tools-text").contains("bar=true"));
    app.quit();
}

#[test]
fn find_replace_all_and_select_line_reach_the_document() {
    let folder = Folder::new("tools");
    let file = folder.file("t.txt", b"one two one\nsecond line\nthird\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("one two one\nsecond line\nthird\n");

    app.ok("replace-all one ONE");
    app.wait_text("ONE two ONE\nsecond line\nthird\n");
    assert_eq!(app.doc("dirty"), "true");
    app.ok("key 27");

    app.run("Find");
    app.type_text("line");
    app.wait_for("the count", |app| app.ask("find-count") == "1 true");
    app.ok("key 13");
    app.wait_for("the match", |app| app.ask("selection") == "19 23");
    app.ok("key 27");

    app.run("SelectLine");
    app.type_text("3");
    app.ok("key 13");
    app.wait_for("the line", |app| app.ask("selection") == "24 30");
    app.quit();
}

#[test]
fn spelling_underlines_with_the_stub_checker_only_when_on() {
    let folder = Folder::new("spell");
    let file = folder.file("s.txt", b"good speling here\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("good speling here\n");
    app.ok("paint");
    assert_eq!(app.ask("underlines"), "0", "spelling starts off");
    app.run("CheckSpellingWhileTyping");
    app.wait_for("the underline", |app| {
        app.ok("paint");
        app.ask("underlines") == "1"
    });
    app.run("CheckSpellingWhileTyping");
    app.wait_for("the underline to go", |app| app.ask("underlines") == "0");
    app.quit();
}

#[test]
fn correct_spelling_automatically_fixes_a_word_when_a_break_is_typed() {
    let folder = Folder::new("correct");
    let mut app = App::start(&folder.root(), &[]);
    app.run("CorrectSpellingAutomatically");
    app.type_text("speling ");
    app.wait_text("spelling ");
    assert_eq!(app.ask("selection"), "9 9");
    app.quit();
}

#[test]
fn print_and_export_write_a_valid_pdf() {
    let folder = Folder::new("pdf");
    let file = folder.file("p.txt", b"Printed words\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("Printed words\n");
    for (command, name) in [("ExportPdf", "e.pdf"), ("Print", "p.pdf")] {
        let target = folder.0.join(name);
        app.run(command);
        app.wait_panel();
        app.ok(&format!("panel-path {}", encode(&target.to_string_lossy())));
        app.wait_for("the PDF", |_| target.exists());
        let bytes = std::fs::read(&target).unwrap();
        assert!(bytes.starts_with(b"%PDF-"), "{name}");
        let tail = &bytes[bytes.len().saturating_sub(16)..];
        assert!(
            String::from_utf8_lossy(tail).contains("%%EOF"),
            "{name} ends as a PDF does"
        );
        qe_pdf::check::check_pdf(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    app.quit();
}

#[test]
fn smart_quotes_change_typing_for_one_document() {
    let folder = Folder::new("smart");
    let mut app = App::start(&folder.root(), &[]);
    app.run("SmartQuotes");
    app.type_text("\"a");
    assert_eq!(app.text(), "\u{201c}a");
    app.run("NewTab");
    app.type_text("\"b\"");
    assert_eq!(app.text(), "\"b\"", "the row is for one document");
    // Back on the first tab, the row is on again.
    app.run("PreviousTab");
    app.type_text("\"");
    assert_eq!(app.text(), "\u{201c}a\u{201d}");
    app.quit();
}

#[test]
fn tabs_move_between_windows_and_out_to_a_new_one() {
    let folder = Folder::new("tabs");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("first");
    app.run("NewTab");
    app.type_text("second");
    assert_eq!(app.windows().len(), 1);
    app.ok("tab-drop-new");
    app.wait_for("two windows", |app| app.windows().len() == 2);
    app.ok("tab-into 0");
    app.wait_for("one window", |app| app.windows().len() == 1);
    assert_eq!(app.tab_count(), 2);
    app.quit();
}

#[test]
fn widget_ids_are_unique_in_the_window_tree() {
    let folder = Folder::new("ids");
    let mut app = App::start(&folder.root(), &[]);
    app.run("NewTab");
    app.run("FindReplace");
    let answer = app.ask("widget-ids");
    let mut parts = answer.split(' ');
    let (total, distinct) = (parts.next().unwrap(), parts.next().unwrap());
    assert_eq!(total, distinct, "{answer}");
    app.quit();
}

#[test]
fn a_tab_that_opens_in_a_window_with_no_keyboard_sets_no_caret_timer() {
    let folder = Folder::new("idle-tab");
    let mut app = App::start(&folder.root(), &[]);
    app.run("NewTab");
    app.run("NewTab");
    app.run("Tab1");
    std::thread::sleep(std::time::Duration::from_millis(2600));
    let read =
        |app: &mut App| -> u64 { app.ask("pumps").split(' ').nth(1).unwrap().parse().unwrap() };
    let before = read(&mut app);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = read(&mut app);
    assert_eq!(after - before, 0, "frames over 2 s with no keyboard");
    app.quit();
}

#[test]
fn an_idle_app_wakes_for_nothing() {
    let folder = Folder::new("idle");
    let file = folder.file("idle.txt", b"resting\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("resting\n");
    // The session list's write ends within its delay, and only then does the app rest.
    std::thread::sleep(std::time::Duration::from_millis(2600));
    let read =
        |app: &mut App| -> u64 { app.ask("pumps").split(' ').next().unwrap().parse().unwrap() };
    let before = read(&mut app);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = read(&mut app);
    // Reading the count takes a wake or two of its own, and nothing else may come.
    assert!(after - before <= 6, "{} wakes over 2 s", after - before);
    app.quit();
}
