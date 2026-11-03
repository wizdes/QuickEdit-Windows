//! The final text-safety review, through the real exe on hidden windows: typed keys that still
//! wait, a failed Save during a quit, an outside change during a composition, and the end of a
//! session while the store is in use.
mod support;

use std::os::windows::fs::OpenOptionsExt;
use std::time::Duration;
use support::{App, Folder};

fn focus_for_tsf(app: &mut App) {
    app.ok("focus");
    app.wait_for("text services", |app| app.ask("tsf-started") == "true");
    app.ok("focus");
    app.ask("ping");
}

fn long_body() -> String {
    "\u{4e2d}\u{6587} text\n".repeat(2000)
}

/// The review's steps: End waits for a layout the paused workers hold back, and typing queues
/// behind it.
fn queue_typing(app: &mut App) {
    app.ok("pause-workers true");
    app.run("ZoomIn");
    app.ok("key 35");
    app.type_text("TYPED");
    assert_ne!(
        app.ask("queued-keys"),
        "0",
        "this test needs keys that wait"
    );
    assert!(
        !app.text().contains("TYPED"),
        "the keys are not in the text yet"
    );
    assert_eq!(app.doc("dirty"), "true", "and they count as unsaved");
}

fn alert_title(app: &mut App) -> String {
    app.wait_value("an alert", |app| app.alert().map(|(title, _, _)| title))
}

#[test]
fn f2_close_window_asks_about_typing_that_still_waits() {
    let folder = Folder::new("final-close-window");
    let body = long_body();
    let a = folder.file("a.txt", body.as_bytes());
    let b = folder.file("b.txt", b"other");
    let mut app = App::start(&folder.root(), &[&b, &a]);
    app.wait_text(&body);
    queue_typing(&mut app);
    app.ok("close-window");
    let title = alert_title(&mut app);
    assert!(title.contains("keep the changes"), "{title}");
    assert!(app.text().contains("TYPED"), "the question covers the keys");
    app.press("Cancel");
    assert_eq!(app.tab_count(), 2);
    assert!(app.text().contains("TYPED"));
    app.ok("pause-workers false");
    app.quit();
}

#[test]
fn f2_close_asks_about_typing_that_still_waits() {
    let folder = Folder::new("final-close");
    let body = long_body();
    let a = folder.file("a.txt", body.as_bytes());
    let mut app = App::start(&folder.root(), &[&a]);
    app.wait_text(&body);
    queue_typing(&mut app);
    app.run("Close");
    let title = alert_title(&mut app);
    assert!(title.contains("keep the changes"), "{title}");
    app.press("Save");
    app.ok("pause-workers false");
    app.wait_exit();
    let saved = std::fs::read_to_string(&a).unwrap();
    assert!(saved.contains("TYPED"), "the file holds the typing");
}

#[test]
fn f2_revert_asks_about_typing_that_still_waits() {
    let folder = Folder::new("final-revert");
    let body = long_body();
    let a = folder.file("a.txt", body.as_bytes());
    let mut app = App::start(&folder.root(), &[&a]);
    app.wait_text(&body);
    queue_typing(&mut app);
    app.run("RevertToSaved");
    let title = alert_title(&mut app);
    assert!(title.contains("revert"), "{title}");
    assert!(app.text().contains("TYPED"));
    app.press("Cancel");
    assert!(app.text().contains("TYPED"));
    app.ok("pause-workers false");
    app.quit();
}

#[test]
fn f2_merge_keeps_typing_that_still_waits() {
    let folder = Folder::new("final-merge");
    let body = long_body();
    let a = folder.file("a.txt", b"first");
    let b = folder.file("b.txt", body.as_bytes());
    let mut app = App::start(&folder.root(), &[&a]);
    app.wait_text("first");
    app.run("New");
    app.wait_for("a second window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    app.ok(&format!(
        "open {}",
        support::encode(&b.display().to_string())
    ));
    app.wait_text(&body);
    queue_typing(&mut app);
    app.run("MergeAllWindows");
    app.wait_for("one window", |app| app.windows().len() == 1);
    app.ok("pause-workers false");
    app.wait_for("the keys", |app| {
        app.doc("dirty") == "true" && app.ask("queued-keys") == "0"
    });
    assert!(app.text().contains("TYPED"));
    app.quit();
}

#[test]
fn f1_cancel_on_the_locked_question_keeps_earlier_typing() {
    let folder = Folder::new("final-locked");
    let file = folder.file("f.txt", b"base\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("base\n");
    app.ok("select 5 5");
    app.type_text("long typed sentence ");
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file, permissions.clone()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    app.settled();
    app.type_text("x");
    let title = alert_title(&mut app);
    assert!(title.contains("is locked"), "{title}");
    app.press("Cancel");
    app.wait_text("base\nlong typed sentence ");
    assert_eq!(app.doc("dirty"), "true");
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&file, permissions).unwrap();
    app.quit();
}

#[test]
fn f5_a_failed_save_stops_the_quit_and_shows_its_error() {
    let folder = Folder::new("final-quit-save");
    let file = folder.file("held.txt", b"base");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("base");
    app.type_text("typed ");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    app.run("Save");
    app.ok("quit");
    let title = alert_title(&mut app);
    assert!(title.contains("could not be saved"), "{title}");
    assert_eq!(app.ask("ping"), "ok", "the app stays");
    drop(held);
    app.press("OK");
    app.quit();
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_text("typed base");
    again.quit();
}

#[test]
fn f5_a_failed_save_at_session_end_is_told_at_the_next_launch() {
    let folder = Folder::new("final-session-save");
    let file = folder.file("held.txt", b"base");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("base");
    app.type_text("typed ");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    app.run("Save");
    // WM_ENDSESSION, as Windows sends it.
    app.ok("post 22 1 0");
    app.wait_exit();
    drop(held);
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_text("typed base");
    again.wait_for("the word of it", |app| app.notes().contains("held.txt"));
    let notes = again.notes();
    assert!(notes.contains("could not be saved"), "{notes}");
    again.quit();
}

#[test]
fn f7_an_outside_change_waits_for_the_composition_to_end() {
    let folder = Folder::new("final-compose-change");
    let file = folder.file("f.txt", b"abc");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("abc");
    focus_for_tsf(&mut app);
    app.ok("select 3 3");
    app.ok("compose xyz");
    assert_eq!(app.ask("composing"), "true");
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(&file, b"another program").unwrap();
    // The watch hears it, the read ends, and the new text waits. The composition goes on.
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(app.ask("composing"), "true", "nothing forces it to end");
        assert!(app.alert().is_none(), "no alert about a failed read");
    }
    app.ok("commit-now");
    app.wait_for("the composition's end", |app| {
        app.ask("composing") == "false"
    });
    app.wait_text("abcxyz");
    assert_eq!(app.doc("dirty"), "true");
    app.settled();
    assert!(app.alert().is_none(), "no alert about a failed read");
    // The text stays, and a Save finds the change and asks.
    app.run("Save");
    let title = alert_title(&mut app);
    assert!(title.contains("changed by another"), "{title}");
    app.press("Cancel");
    assert_eq!(std::fs::read(&file).unwrap(), b"another program");
    app.quit();
}

#[test]
fn f7_a_cancelled_composition_lets_the_new_text_in() {
    let folder = Folder::new("final-compose-cancel");
    let file = folder.file("f.txt", b"abc");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("abc");
    focus_for_tsf(&mut app);
    app.ok("select 3 3");
    app.ok("compose xyz");
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(&file, b"another program").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(app.ask("composing"), "true");
    app.ok("compose-cancel");
    app.wait_text("another program");
    assert_eq!(app.doc("dirty"), "false");
    app.quit();
}

#[test]
fn session_end_during_a_borrow_writes_the_copies_before_its_answer() {
    let folder = Folder::new("final-borrow");
    let file = folder.file("f.txt", b"base");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("base");
    app.type_text("typed ");
    // Long enough for the ready copies to catch up, and short of any autosave.
    std::thread::sleep(Duration::from_millis(600));
    app.ask("ping");
    // This ends the process when the handler's answer comes, as Windows may, so nothing that
    // runs after the answer can write a copy.
    let _ = app.try_ask("session-end-then-die", Duration::from_secs(5));
    app.wait_exit();
    let mut again = App::start(&root, &[]);
    again.settled();
    again.wait_text("typed base");
    again.quit();
}

/// The whole road in the real app: a copy written while only queued keys count as unsaved goes
/// when they run and change nothing, with no later key or quit to prompt it.
#[test]
fn a_copy_goes_when_queued_keys_change_nothing_in_the_app() {
    let folder = Folder::new("final-queued-nothing");
    let root = folder.root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("settings.txt"), "AutosavingDelaySeconds=1\n").unwrap();
    let body = long_body();
    let a = folder.file("a.txt", body.as_bytes());
    let mut app = App::start(&root, &[&a]);
    app.wait_text(&body);
    // Typing that Undo takes back leaves a copy due, on a clean document.
    app.type_text("x");
    app.run("Undo");
    assert_eq!(app.doc("dirty"), "false");
    app.ok("pause-workers true");
    app.run("ZoomIn");
    // Home waits for a layout the paused workers hold back. Backspace at the start changes nothing.
    app.ok("key 36");
    app.ok("key 8");
    assert_ne!(
        app.ask("queued-keys"),
        "0",
        "this test needs keys that wait"
    );
    assert_eq!(app.doc("dirty"), "true", "and they count as unsaved");
    let docs = root.join("session").join("docs");
    let copies = |docs: &std::path::Path| {
        std::fs::read_dir(docs).map_or(0, |dir| {
            dir.filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "qea"))
                .count()
        })
    };
    app.wait_for("the copy", |_| copies(&docs) > 0);
    app.ok("pause-workers false");
    app.wait_for("the keys", |app| app.ask("queued-keys") == "0");
    assert_eq!(app.doc("dirty"), "false", "the keys changed nothing");
    assert_eq!(app.text(), body);
    app.wait_for("the copy goes", |_| copies(&docs) == 0);
    app.quit();
}
