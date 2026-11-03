//! The real exe: open, edit, save, questions, kill and relaunch, second launches.
mod support;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
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

/// A file whose name looks like a flag: a full path opens it, as does anything after `--`.
#[test]
fn a_file_named_like_a_flag_opens_by_full_path_or_after_two_dashes() {
    let folder = Folder::new("flag-name");
    let help = folder.file("--help", b"in the help file");
    let root = folder.root();
    let mut app = App::start(&root, &[&help]);
    app.wait_text("in the help file");
    app.quit();
    let other = folder.file("--notes.txt", b"in the notes file");
    let mut app = App::start(&root, &[Path::new("--"), &other]);
    app.wait_for("the notes file", |app| app.text() == "in the notes file");
    app.quit();
    // By its bare name, after the two dashes, from its own folder.
    let output = Command::new(exe())
        .current_dir(&folder.0)
        .args(["--hidden-smoke", "--data-root"])
        .arg(folder.0.join("smoke-data"))
        .args(["--", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.contains("hidden smoke passed"),
        "help ran instead: {said}"
    );
}

/// Bare `--help` stays a flag in a copy with no package, and `--` alone opens no file.
#[test]
fn an_unpackaged_run_keeps_its_help_flag_and_two_dashes_alone_are_fine() {
    let output = Command::new(exe()).arg("--help").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("--data-root"));
    let folder = Folder::new("two-dashes");
    let output = Command::new(exe())
        .args(["--hidden-smoke", "--data-root"])
        .arg(folder.root())
        .arg("--")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// The hook for the package rule. A real packaged run cannot start here, because an install is
/// ruled out. This asks the real exe how a package would read the line, and each word is a file.
#[test]
fn a_packaged_run_reads_every_flag_as_a_file() {
    let line = [
        "--help",
        "--hidden",
        "--test-channel",
        "--data-root",
        r"\\host\share",
        "--bench-keys",
        "0",
        "--no-such-flag",
        "--",
        r"C:\My Docs\a b.txt",
    ];
    let output = Command::new(exe())
        .arg("--print-packaged-args")
        .args(line)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let said = String::from_utf8_lossy(&output.stdout);
    let mut lines = said.lines();
    // This test's own run has no package. A package would read the flag as a file instead.
    assert_eq!(lines.next(), Some("package identity: false"));
    let files: Vec<&str> = lines.map(|l| l.strip_prefix("file: ").unwrap()).collect();
    assert_eq!(files, line);
}

/// What File Explorer does with `MultiSelectModel="Document"` and `Parameters="&quot;%1&quot;"`:
/// 15 processes at once, each with one quoted path that has spaces in it. They arrive in the
/// order the pipe takes them, which is not the order chosen in Explorer.
#[test]
fn fifteen_real_launches_at_once_with_spaces_land_in_one_window() {
    let folder = Folder::new("fifteen launches");
    let root = folder.root();
    let spaced = folder.0.join("My Docs and Notes");
    std::fs::create_dir_all(&spaced).unwrap();
    let paths: Vec<PathBuf> = (0..15)
        .map(|i| {
            let path = spaced.join(format!("file number {i:02}.txt"));
            std::fs::write(&path, format!("file {i}")).unwrap();
            path
        })
        .collect();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(paths.len()));
    let started = Instant::now();
    let launches: Vec<_> = paths
        .iter()
        .map(|path| {
            let (gate, root, path) = (gate.clone(), root.clone(), path.clone());
            std::thread::spawn(move || {
                gate.wait();
                App::launch(&root, &[&path], &[], None)
            })
        })
        .collect();
    let mut apps: Vec<App> = launches.into_iter().map(|l| l.join().unwrap()).collect();
    // All but the one that became the instance end, each with success.
    let limit = Instant::now() + Duration::from_secs(30);
    let mut alive = Vec::new();
    while alive.len() != 1 {
        alive.clear();
        for (at, app) in apps.iter_mut().enumerate() {
            match app.child.try_wait().unwrap() {
                Some(status) => assert!(status.success(), "launch {at}: {status}"),
                None => alive.push(at),
            }
        }
        assert!(Instant::now() < limit, "{} launches still run", alive.len());
        std::thread::sleep(Duration::from_millis(2));
    }
    let handed_off = started.elapsed();
    let mut app = apps.swap_remove(alive[0]);
    app.wait_for("15 tabs", |app| {
        app.ask("restored") == "true" && app.tab_count() >= 15
    });
    let all_open = started.elapsed();
    let windows = app.windows();
    assert_eq!(windows.len(), 1, "{windows:?}");
    println!(
        "15 launches at once: all handed off in {:.0} ms, 15 tabs in {:.0} ms, in this order: {:?}",
        handed_off.as_secs_f64() * 1000.0,
        all_open.as_secs_f64() * 1000.0,
        windows[0]
    );
    // A star marks the active tab.
    let mut got: Vec<String> = windows[0].iter().map(|t| t.replace('*', "")).collect();
    got.sort();
    let want: Vec<String> = (0..15).map(|i| format!("file number {i:02}.txt")).collect();
    assert_eq!(
        got, want,
        "each path stayed whole, and the blank tab gave way"
    );
    app.quit();
}

/// The pipe name a test's own data folder gives, as `app.rs` makes it.
fn pipe_for_root(root: &Path) -> String {
    let names = qe_docs::handoff::PipeNames::for_this_process().unwrap();
    let tag = root
        .to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!(r"\\.\pipe\{}-{tag:016x}-medium", names.base)
}

/// The whole road on the real exe: another program holds the name and answers wrong. The launch
/// says so and opens its own window with the file in it.
#[test]
fn a_launch_that_meets_a_fake_server_opens_its_own_window_and_says_why() {
    use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, ReadFile, WriteFile};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    let folder = Folder::new("fake-server");
    let root = folder.root();
    let file = folder.file("mine.txt", b"my words");
    let name = windows::core::HSTRING::from(pipe_for_root(&root));
    // SAFETY: the name outlives the call.
    let pipe = unsafe {
        CreateNamedPipeW(
            &name,
            FILE_FLAGS_AND_ATTRIBUTES(3),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        )
    };
    assert!(!pipe.is_invalid());
    let raw = pipe.0 as usize;
    let fake = std::thread::spawn(move || {
        let pipe = windows::Win32::Foundation::HANDLE(raw as *mut _);
        // SAFETY: the pipe is open and blocking. Each call ends when the client acts or leaves.
        unsafe {
            let _ = ConnectNamedPipe(pipe, None);
            let mut bytes = [0u8; 4096];
            let _ = ReadFile(pipe, Some(&mut bytes), None, None);
            WriteFile(pipe, Some(b"X"), None, None).unwrap();
            let _ = ReadFile(pipe, Some(&mut bytes), None, None);
            windows::Win32::Foundation::CloseHandle(pipe).unwrap();
        }
    });
    let mut app = App::start(&root, &[&file]);
    app.wait_text("my words");
    let notes = app.notes();
    assert!(notes.contains("opens its own window"), "{notes}");
    fake.join().unwrap();
    app.quit();
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
