//! The real exe: open, edit, save, questions, kill and relaunch, second launches.
mod support;
use std::process::Command;
use support::{App, Folder, encode, exe};

#[test]
fn hidden_smoke_passes_with_a_file_and_with_none() {
    let folder = Folder::new("smoke");
    let file = folder.file("a.txt", b"hello\n");
    for files in [vec![], vec![file.as_path()]] {
        let output = Command::new(exe())
            .arg("--hidden-smoke")
            .arg("--data-root")
            .arg(folder.root())
            .args(files)
            .output()
            .expect("the smoke runs");
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("hidden smoke passed"));
    }
}

#[test]
fn an_unknown_flag_opens_nothing() {
    let output = Command::new(exe()).arg("--no-such-flag").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option"));
}

#[test]
fn open_type_save_and_reload_keeps_exact_bytes() {
    let folder = Folder::new("save");
    let file = folder.file("crlf.txt", b"one\r\ntwo\r\n");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("one\r\ntwo\r\n");
    assert_eq!(app.doc("dirty"), "false");
    app.ok("select 5 5");
    app.type_text("X");
    assert_eq!(app.doc("dirty"), "true");
    app.run("Save");
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(std::fs::read(&file).unwrap(), b"one\r\nXtwo\r\n");
    app.quit();

    let mut again = App::start(&root, &[&file]);
    again.wait_text("one\r\nXtwo\r\n");
    again.quit();
    assert_eq!(std::fs::read(&file).unwrap(), b"one\r\nXtwo\r\n");
}

#[test]
fn a_new_document_saves_through_the_save_panel_with_the_txt_rule() {
    let folder = Folder::new("saveas");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("fresh");
    app.run("Save");
    assert_eq!(app.wait_panel(), "Save As");
    let target = folder.0.join("named");
    app.ok(&format!(
        "panel-path {} 1 txt",
        encode(&target.to_string_lossy())
    ));
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(std::fs::read(folder.0.join("named.txt")).unwrap(), b"fresh");
    assert_eq!(app.doc("name"), "named.txt");
    app.quit();
}

#[test]
fn a_cancelled_save_panel_leaves_the_text_dirty() {
    let folder = Folder::new("cancel");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("fresh");
    app.run("SaveAs");
    app.wait_panel();
    app.ok("panel-cancel");
    assert_eq!(app.doc("dirty"), "true");
    assert_eq!(app.text(), "fresh");
    app.quit();
}

#[test]
fn a_file_that_cannot_open_shows_its_error() {
    let folder = Folder::new("missing");
    let mut app = App::start(&folder.root(), &[]);
    let missing = folder.0.join("no-such.txt");
    app.ok(&format!("open {}", encode(&missing.to_string_lossy())));
    let (title, _, _) = app.wait_value("the error", App::alert);
    assert!(title.contains("no-such.txt"), "{title}");
    app.press("OK");
    app.quit();
}

#[test]
fn a_failed_save_names_the_problem_and_loses_nothing() {
    let folder = Folder::new("readonly");
    let file = folder.file("locked.txt", b"old");
    let mut perms = std::fs::metadata(&file).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&file, perms.clone()).unwrap();
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("old");
    app.ok("select 3 3");
    app.type_text("new");
    // The store asks about the read-only mark, on the first edit or at the Save.
    let (_, _, buttons) = app.wait_value("the question", App::alert);
    assert!(buttons.iter().any(|b| b == "Cancel"), "{buttons:?}");
    app.press("Cancel");
    assert_eq!(std::fs::read(&file).unwrap(), b"old");
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&file, perms).unwrap();
    app.quit();
}

#[test]
fn closing_a_dirty_document_asks_and_each_answer_does_its_job() {
    let folder = Folder::new("close");
    let file = folder.file("doc.txt", b"base");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("base");
    app.ok("select 4 4");
    app.type_text("+");

    // Cancel: the text stays open and unsaved.
    app.run("NewTab");
    app.run("PreviousTab");
    app.run("Close");
    let (_, _, buttons) = app.wait_value("the question", App::alert);
    assert!(buttons.contains(&"Don't Save".to_owned()), "{buttons:?}");
    app.press("Cancel");
    assert_eq!(app.text(), "base+");
    assert_eq!(std::fs::read(&file).unwrap(), b"base");

    // Save: the file has the text, and the tab goes.
    app.run("Close");
    app.press("Save");
    app.wait_for("the tab to close", |app| app.tab_count() == 1);
    assert_eq!(std::fs::read(&file).unwrap(), b"base+");

    // Don't Save: the file keeps what it has.
    app.ok(&format!("open {}", encode(&file.to_string_lossy())));
    app.wait_text("base+");
    app.ok("select 5 5");
    app.type_text("!");
    app.run("Close");
    app.press("Don't Save");
    app.wait_for("the tab to close", |app| app.alert().is_none());
    assert_eq!(std::fs::read(&file).unwrap(), b"base+");
    app.quit();
}

#[test]
fn a_document_with_an_open_question_takes_no_typing() {
    let folder = Folder::new("asking");
    let file = folder.file("doc.txt", b"base");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("base");
    app.ok("select 4 4");
    app.type_text("+");
    app.run("Close");
    app.wait_value("the question", App::alert);
    app.type_text("more");
    assert_eq!(app.text(), "base+");
    app.press("Cancel");
    app.type_text("!");
    assert_eq!(app.text(), "base+!");
    app.quit();
}

#[test]
fn the_window_close_message_asks_the_same_question_and_the_last_window_quits() {
    let folder = Folder::new("altf4");
    let file = folder.file("doc.txt", b"base");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("base");
    app.ok("select 4 4");
    app.type_text("+");
    app.ok("close-window");
    app.press("Don't Save");
    app.wait_exit();
    assert_eq!(std::fs::read(&file).unwrap(), b"base");
}

#[test]
fn cancel_at_a_window_close_keeps_the_clean_tabs_and_the_dirty_one() {
    let folder = Folder::new("close-cancel");
    let files = [
        folder.file("hello.txt", b"hello"),
        folder.file("saved.txt", b"saved"),
        folder.file("second.txt", b"second"),
    ];
    let mut app = App::start(&folder.root(), &[&files[0], &files[1], &files[2]]);
    app.wait_for("three tabs", |app| app.tab_count() == 3);
    app.run("NewTab");
    app.wait_for("the fourth tab", |app| app.tab_count() == 4);
    app.type_text("unsaved words that must survive");
    let before = app.windows();
    app.ok("close-window");
    app.wait_value("the question", App::alert);
    assert_eq!(app.tab_count(), 4, "no tab closes before the answer");
    app.press("Cancel");
    app.settled();
    assert_eq!(app.windows(), before, "Cancel leaves the window as it was");
    assert_eq!(app.text(), "unsaved words that must survive");
    // Don't Save now closes all four, and the last window ends the app.
    app.ok("close-window");
    app.press("Don't Save");
    app.wait_exit();
}

#[test]
fn kill_and_relaunch_keeps_unsaved_text_across_two_windows() {
    let folder = Folder::new("kill");
    let file = folder.file("saved.txt", b"on disk");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("on disk");
    app.ok("select 7 7");
    app.type_text(" edited");
    app.run("New");
    app.wait_for("a second window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    app.type_text("untitled words");
    // A short autosave delay is not on: Save All is not the point here. The session writes
    // on its own delay, and the copy on the setting's. The test waits for the copy.
    let session = root.join("session").join("docs");
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        let copies = std::fs::read_dir(&session)
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "qea"))
                    .count()
            })
            .unwrap_or(0);
        if copies >= 2 {
            break;
        }
        assert!(std::time::Instant::now() < limit, "no autosave came");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(2500));
    app.kill();
    assert_eq!(std::fs::read(&file).unwrap(), b"on disk");

    let mut again = App::start(&root, &[]);
    again.wait_for("both windows", |app| app.windows().len() == 2);
    let mut texts = Vec::new();
    for index in 0..2 {
        again.ok(&format!("use-window {index}"));
        again.settled();
        texts.push(again.text());
    }
    texts.sort();
    assert_eq!(texts, ["on disk edited", "untitled words"]);
    again.ok("use-window 0");
    again.quit();
}

#[test]
fn fifteen_files_from_a_second_launch_land_in_one_window() {
    let folder = Folder::new("fifteen");
    let root = folder.root();
    let paths: Vec<_> = (0..15)
        .map(|i| folder.file(&format!("f{i:02}.txt"), format!("file {i}").as_bytes()))
        .collect();
    let mut app = App::start(&root, &[]);
    let status = Command::new(exe())
        .arg("--hidden")
        .arg("--data-root")
        .arg(&root)
        .args(&paths)
        .status()
        .expect("the second launch");
    assert!(status.success());
    app.wait_for("15 tabs", |app| app.tab_count() >= 15);
    let windows = app.windows();
    assert_eq!(windows.len(), 1, "{windows:?}");
    assert_eq!(windows[0].len(), 15, "the blank tab gives way: {windows:?}");
    app.quit();
}

#[test]
fn a_launch_file_that_fails_still_leaves_a_window() {
    let folder = Folder::new("failed-restore");
    let root = folder.root();
    let file = folder.file("gone-soon.txt", b"text");
    let mut app = App::start(&root, &[&file]);
    app.wait_text("text");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    app.kill();
    // The session names a clean file, which is now not there.
    std::fs::remove_file(&file).unwrap();
    let mut again = App::start(&root, &[]);
    again.wait_for("a window", |app| !app.windows().is_empty());
    again.settled();
    assert!(!again.windows().is_empty());
    again.ok("ping");
    again.quit();
}

#[test]
fn a_path_that_is_no_file_is_refused_by_name() {
    let folder = Folder::new("refused");
    let root = folder.root();
    let mut app = App::start(&root, &[]);
    let status = Command::new(exe())
        .arg("--hidden")
        .arg("--data-root")
        .arg(&root)
        .arg(r"C:\NUL")
        .status()
        .unwrap();
    assert!(status.success());
    let (_, message, _) = app.wait_value("the error", App::alert);
    assert!(!message.is_empty());
    app.press("OK");
    assert_eq!(app.tab_count(), 1);
    app.quit();
}

#[test]
fn a_second_launch_with_no_file_leaves_the_first_alone() {
    let folder = Folder::new("forward");
    let root = folder.root();
    let mut app = App::start(&root, &[]);
    let status = Command::new(exe())
        .arg("--hidden")
        .arg("--data-root")
        .arg(&root)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(app.windows().len(), 1);
    app.quit();
}

#[test]
fn a_file_changed_outside_reloads_a_clean_document() {
    let folder = Folder::new("outside");
    let file = folder.file("watched.txt", b"before");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("before");
    std::fs::write(&file, b"after").unwrap();
    app.wait_text("after");
    assert_eq!(app.doc("dirty"), "false");
    app.quit();
}
