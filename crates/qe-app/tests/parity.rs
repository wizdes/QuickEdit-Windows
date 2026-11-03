//! The parity sweep: rows of `docs/parity-windows.md` checked on the real exe, by row ID. A row
//! that an older test already proves names that test in the document, and has no test here.
mod support;
use std::path::Path;
use std::time::{Duration, Instant};
use support::{App, Folder, decode, encode};

fn field(answer: &str, name: &str) -> String {
    for part in answer.split(' ') {
        if let Some(value) = part.strip_prefix(&format!("{name}=")) {
            return decode(value);
        }
    }
    panic!("no {name} in {answer}");
}
fn view(app: &mut App, name: &str) -> String {
    app.ok("paint");
    let answer = app.ask("view-info");
    field(&answer, name)
}
fn number(app: &mut App, name: &str) -> f32 {
    view(app, name).parse().unwrap()
}
/// How many characters of the plain font fit across the text.
fn columns(app: &mut App) -> f32 {
    number(app, "textw") / number(app, "cell")
}
fn panel(app: &mut App, name: &str) -> String {
    app.wait_panel();
    let answer = app.ask("panel-info");
    field(&answer, name)
}
fn rows(app: &mut App) -> Vec<(String, bool)> {
    panel(app, "rows")
        .split('|')
        .map(|row| {
            let (name, on) = row.rsplit_once(':').unwrap();
            (name.to_owned(), on == "true")
        })
        .collect()
}
fn row_index(app: &mut App, name: &str) -> usize {
    rows(app)
        .iter()
        .position(|(row, _)| row == name)
        .unwrap_or_else(|| panic!("no {name}"))
}
fn path_word(path: &Path) -> String {
    encode(&path.to_string_lossy())
}
/// Opens through the real Open row, with this row of its encoding list.
fn open_with_row(app: &mut App, path: &Path, encoding: &str) {
    app.run("Open");
    let row = row_index(app, encoding);
    app.ok(&format!("panel-path {} {row}", path_word(path)));
}
fn menu(app: &mut App, group: usize) -> Vec<String> {
    decode(&app.ask(&format!("menu-dump {group}")))
        .lines()
        .map(str::to_owned)
        .collect()
}
fn labels(app: &mut App, group: usize) -> Vec<String> {
    menu(app, group)
        .iter()
        .map(|row| row.split('|').nth(1).unwrap().to_owned())
        .filter(|label| !label.is_empty())
        .collect()
}
fn has_all(have: &[String], want: &[&str]) {
    for row in want {
        assert!(have.iter().any(|label| label == row), "{row} in {have:?}");
    }
}
fn has_none(have: &[String], refused: &[&str]) {
    for row in refused {
        assert!(!have.iter().any(|label| label == row), "{row} in {have:?}");
    }
}
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(120);
    while !check() {
        assert!(Instant::now() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn big_file(folder: &Folder, name: &str, mib: usize) -> std::path::PathBuf {
    use std::io::Write;
    let file = folder.0.join(name);
    let mut out = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
    let row = [b'x'; 1023];
    for _ in 0..(mib * 1024) {
        out.write_all(&row).unwrap();
        out.write_all(b"\n").unwrap();
    }
    file
}

// ---- Documents ----

#[test]
fn p01_ctrl_n_opens_an_empty_window() {
    let folder = Folder::new("p01");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("key 78 ctrl");
    app.wait_for("the window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    assert_eq!(app.text(), "");
    assert_eq!(view(&mut app, "page"), "false");
    assert!(
        decode(&app.ask("menu-row 0 New")).starts_with("New|Ctrl+N|"),
        "the row names its key"
    );
    app.ok("use-window 0");
    app.quit();
}

#[test]
fn p02_open_takes_other_text_files_by_any_name() {
    let folder = Folder::new("p02");
    let mut app = App::start(&folder.root(), &[]);
    for (name, text) in [
        ("note.md", "# heading"),
        ("data.json", "{\"a\":1}"),
        ("page.html", "<b>x</b>"),
        ("LICENSE", "no extension"),
    ] {
        let file = folder.file(name, text.as_bytes());
        app.run("Open");
        assert_eq!(app.wait_panel(), "Open");
        app.ok(&format!("panel-path {}", path_word(&file)));
        app.wait_text(text);
        assert_eq!(app.doc("name"), name);
    }
    app.quit();
}

#[test]
fn p03_i43_typing_and_ctrl_s_write_utf8_with_no_bom() {
    let folder = Folder::new("p03");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("h\u{e9}llo \u{4e2d}");
    app.ok("key 83 ctrl");
    assert_eq!(app.wait_panel(), "Save As");
    let target = folder.0.join("out.txt");
    app.ok(&format!("panel-path {}", path_word(&target)));
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        "h\u{e9}llo \u{4e2d}".as_bytes()
    );
    app.quit();
}

#[test]
fn p04_the_txt_box_starts_on_and_each_state_does_its_job() {
    let folder = Folder::new("p04");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("one");
    app.run("Save");
    assert_eq!(panel(&mut app, "txt"), "Some(true)");
    let row = panel(&mut app, "selected");
    let plain = folder.0.join("plain");
    app.ok(&format!("panel-path {} {row} off", path_word(&plain)));
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert!(plain.exists(), "with the box off the name stays");
    app.quit();
}

#[test]
fn p05_p10_a_quit_asks_nothing_and_unsaved_text_comes_back() {
    let folder = Folder::new("p05");
    let file = folder.file("named.txt", b"on disk");
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text("on disk");
    app.ok("select 7 7");
    app.type_text(" more");
    app.run("NewTab");
    app.type_text("untitled words");
    app.quit();
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"on disk",
        "no save happened"
    );

    let mut again = App::start(&root, &[]);
    again.wait_for("two tabs", |app| app.tab_count() == 2);
    again.settled();
    assert_eq!(again.doc("dirty"), "true");
    let first = again.text();
    again.run("PreviousTab");
    again.settled();
    let second = again.text();
    let mut texts = [first, second];
    texts.sort();
    assert_eq!(texts, ["on disk more", "untitled words"]);
    again.quit();
}

#[test]
fn p06_a_new_window_holds_90_columns() {
    let folder = Folder::new("p06");
    let mut app = App::start(&folder.root(), &[]);
    let wide = columns(&mut app);
    assert!((90.0..91.0).contains(&wide), "{wide} columns");
    let high = number(&mut app, "texth") / number(&mut app, "row");
    assert!((30.0..30.5).contains(&high), "{high} lines");
    app.quit();
}

#[test]
fn p07_i17_the_plain_font_is_cascadia_mono_10_with_8_space_tabs() {
    let folder = Folder::new("p07");
    let mut app = App::start(&folder.root(), &[]);
    assert_eq!(view(&mut app, "family"), "Cascadia Mono");
    assert_eq!(view(&mut app, "points"), "10");
    assert_eq!(view(&mut app, "tab"), "8");
    app.quit();
}

#[test]
fn p11_duplicate_move_to_and_revert_do_their_jobs() {
    let folder = Folder::new("p11");
    let file = folder.file("first.txt", b"words");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("words");

    app.ok("key 83 ctrl shift");
    app.wait_for("the copy", |app| app.tab_count() == 2);
    app.settled();
    assert_eq!(app.text(), "words");
    assert_eq!(app.doc("path"), "", "a copy has no file yet");
    app.run("Close");
    app.press("Don't Save");
    app.wait_for("the copy to go", |app| app.tab_count() == 1);

    let moved_folder = folder.0.join("moved");
    std::fs::create_dir(&moved_folder).unwrap();
    let moved = moved_folder.join("first.txt");
    app.run("MoveTo");
    assert_eq!(app.wait_panel(), "Move To");
    app.ok(&format!("panel-path {}", path_word(&moved)));
    eventually("the move", || moved.exists() && !file.exists());
    app.wait_for("the new path", |app| {
        app.doc("path") == moved.to_string_lossy()
    });

    app.ok("select 0 0");
    app.type_text("x");
    app.run("RevertToSaved");
    app.press("Revert");
    app.wait_text("words");
    assert_eq!(app.doc("dirty"), "false");
    assert!(
        !labels(&mut app, 0)
            .iter()
            .any(|row| row.contains("Versions")),
        "no versions browser"
    );
    app.quit();
}

#[test]
fn p12_save_all_writes_every_document() {
    let folder = Folder::new("p12");
    let one = folder.file("one.txt", b"1");
    let two = folder.file("two.txt", b"2");
    let mut app = App::start(&folder.root(), &[&one, &two]);
    app.wait_for("two tabs", |app| app.tab_count() == 2);
    app.settled();
    for _ in 0..2 {
        app.ok("select 0 0");
        app.type_text("x");
        app.run("NextTab");
    }
    app.run("SaveAll");
    eventually("both files", || {
        // A read can meet the moment of the save's swap, so a failed read only means "not yet".
        std::fs::read(&one).is_ok_and(|bytes| bytes == b"x1")
            && std::fs::read(&two).is_ok_and(|bytes| bytes == b"x2")
    });
    app.quit();
}

#[test]
fn p13_open_recent_lists_the_file_and_clear_menu_empties_it() {
    let folder = Folder::new("p13");
    let file = folder.file("listed.txt", b"x");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("x");
    app.wait_for("the row", |app| {
        labels(app, 0).iter().any(|row| row.contains("listed.txt"))
    });
    has_all(&labels(&mut app, 0), &["Open Recent", "Clear Menu"]);
    app.run("ClearRecent");
    app.wait_for("the row to go", |app| {
        !labels(app, 0).iter().any(|row| row.contains("listed.txt"))
    });
    app.quit();
}

#[test]
fn p14_a_locked_file_offers_unlock_or_duplicate_and_keeps_its_bytes() {
    let folder = Folder::new("p14");
    let file = folder.file("locked.txt", b"keep");
    let mut marks = std::fs::metadata(&file).unwrap().permissions();
    marks.set_readonly(true);
    std::fs::set_permissions(&file, marks).unwrap();
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("keep");
    app.ok("select 0 0");
    app.type_text("x");
    let (_, _, buttons) = app.wait_value("the question", App::alert);
    assert_eq!(buttons, ["Unlock", "Duplicate", "Cancel"]);
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");

    app.press("Cancel");
    app.wait_text("keep");
    assert!(std::fs::metadata(&file).unwrap().permissions().readonly());

    app.type_text("x");
    app.press("Unlock");
    eventually("the flag to clear", || {
        !std::fs::metadata(&file).unwrap().permissions().readonly()
    });
    app.run("Save");
    // While the swap runs, the file can be held for a moment.
    eventually("the save", || {
        std::fs::read(&file).is_ok_and(|bytes| bytes == b"xkeep")
    });
    app.quit();
}

#[test]
fn p15_a_change_outside_a_dirty_document_warns_at_the_save() {
    let folder = Folder::new("p15");
    let file = folder.file("f.txt", b"first");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("first");
    app.ok("select 0 0");
    app.type_text("x");
    std::fs::write(&file, b"outside").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(app.text(), "xfirst", "nothing reloads over an edit");
    app.run("Save");
    let (_, _, buttons) = app.wait_value("the question", App::alert);
    assert!(buttons.iter().any(|b| b == "Save Anyway"), "{buttons:?}");
    app.press("Cancel");
    assert_eq!(std::fs::read(&file).unwrap(), b"outside");
    assert_eq!(app.text(), "xfirst");
    app.quit();
}

#[test]
fn p16_i47_selection_and_first_visible_text_come_back() {
    let folder = Folder::new("p16");
    let text: String = (0..300).map(|i| format!("line number {i}\n")).collect();
    let file = folder.file("long.txt", text.as_bytes());
    let root = folder.root();
    let mut app = App::start(&root, &[&file]);
    app.wait_text(&text);
    app.ok("select 3000 3010");
    app.run("JumpToSelection");
    app.wait_for("the view to move", |app| number(app, "top") > 0.0);
    let top = number(&mut app, "top");
    // The session list writes on its own delay, and a quit writes it at once.
    app.quit();

    let mut again = App::start(&root, &[]);
    again.wait_text(&text);
    assert_eq!(again.ask("selection"), "3000 3010");
    again.wait_for("the same top", |app| number(app, "top") == top);
    again.quit();
}

#[test]
fn p17_i49_open_replaces_the_launch_window_until_it_is_edited() {
    let folder = Folder::new("p17");
    let one = folder.file("one.txt", b"one");
    let two = folder.file("two.txt", b"two");
    let mut app = App::start(&folder.root(), &[]);
    app.run("Open");
    app.wait_panel();
    app.ok(&format!("panel-path {}", path_word(&one)));
    app.wait_text("one");
    assert_eq!(
        app.tab_count(),
        1,
        "the file took the launch window's place"
    );
    app.quit();

    let other = Folder::new("p17b");
    let mut app = App::start(&other.root(), &[]);
    app.type_text("typed");
    app.run("Open");
    app.wait_panel();
    app.ok(&format!("panel-path {}", path_word(&two)));
    app.wait_text("two");
    assert_eq!(app.tab_count(), 2, "typed text keeps its window");
    app.quit();
}

// ---- Encodings ----

#[test]
fn p20_open_offers_automatic_first_and_starts_at_the_setting() {
    let folder = Folder::new("p20");
    let mut app = App::start(&folder.root(), &[]);
    app.run("Open");
    let list = rows(&mut app);
    assert_eq!(list[0].0, "Automatic");
    assert_eq!(panel(&mut app, "selected"), "0");
    assert!(list.iter().any(|(row, _)| row == "UTF-8"), "{list:?}");
    app.ok("panel-cancel");

    app.ok("set-text OpenEncoding UTF-8");
    app.run("Open");
    let at = row_index(&mut app, "UTF-8");
    assert_eq!(
        panel(&mut app, "selected"),
        at.to_string(),
        "the list starts at the setting"
    );
    app.ok("panel-cancel");
    app.quit();
}

#[test]
fn p21_i41_automatic_reads_each_bom_then_utf8() {
    let folder = Folder::new("p21");
    let mut app = App::start(&folder.root(), &[]);
    let le = folder.file("le.txt", b"\xFF\xFEh\0i\0");
    let be = folder.file("be.txt", b"\xFE\xFF\0h\0i");
    let utf8 = folder.file("u.txt", "h\u{e9}".as_bytes());
    for (file, want) in [(&le, "hi"), (&be, "hi"), (&utf8, "h\u{e9}")] {
        open_with_row(&mut app, file, "Automatic");
        app.wait_text(want);
        assert_eq!(app.doc("dirty"), "false");
    }
    app.quit();
}

#[test]
fn p22_i63_a_chosen_encoding_that_does_not_fit_names_itself() {
    let folder = Folder::new("p22");
    let file = folder.file("bad.txt", b"ab\xFFcd");
    let mut app = App::start(&folder.root(), &[]);
    open_with_row(&mut app, &file, "UTF-8");
    let (title, message, _) = app.wait_value("the error", App::alert);
    assert!(
        format!("{title} {message}").contains("UTF-8"),
        "{title}: {message}"
    );
    app.press("OK");
    app.quit();
}

#[test]
fn p23_i40_save_as_turns_off_encodings_that_cannot_hold_the_text() {
    let folder = Folder::new("p23");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("\u{4e2d}");
    app.run("SaveAs");
    let list = rows(&mut app);
    let on = |name: &str| list.iter().find(|(row, _)| row == name).unwrap().1;
    assert!(on("UTF-8"), "{list:?}");
    assert!(!on("Windows-1252"), "{list:?}");
    assert!(!list.iter().any(|(row, _)| row == "Automatic"), "{list:?}");
    assert_eq!(panel(&mut app, "txt"), "Some(true)");
    app.ok("panel-cancel");
    app.quit();
}

#[test]
fn p25_an_edit_beyond_the_files_encoding_offers_save_as_or_undo() {
    let folder = Folder::new("p25");
    let file = folder.file("f.txt", b"abc");
    let mut app = App::start(&folder.root(), &[]);
    open_with_row(&mut app, &file, "Windows-1252");
    app.wait_text("abc");
    app.ok("select 0 0");
    app.type_text("\u{4e2d}");
    let (title, _, buttons) = app.wait_value("the question", App::alert);
    assert!(title.contains("Windows-1252"), "{title}");
    assert_eq!(buttons, ["Choose Encoding", "Cancel"]);
    app.press("Cancel");
    app.wait_text("abc");
    app.type_text("\u{4e2d}");
    app.press("Choose Encoding");
    assert_eq!(app.wait_panel(), "Save As");
    app.ok("panel-cancel");
    app.wait_text("abc");
    assert_eq!(std::fs::read(&file).unwrap(), b"abc");
    app.quit();
}

#[test]
fn p26_a_lossy_file_asks_at_its_first_edit_and_keeps_its_bytes() {
    let bytes = [0xFF, 0xFE, b'h', 0, b'i', 0, 0x41];
    let folder = Folder::new("p26");
    let file = folder.file("f.txt", &bytes);
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("hi");
    app.ok("select 0 0");
    app.type_text("x");
    let (_, _, buttons) = app.wait_value("the question", App::alert);
    assert_eq!(buttons, ["Duplicate", "Cancel", "Overwrite"]);
    app.press("Cancel");
    app.wait_text("hi");
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    app.quit();
}

#[test]
fn p27_mixed_endings_stay_and_return_types_the_common_one() {
    let folder = Folder::new("p27");
    let file = folder.file("f.txt", b"a\r\nb\r\nc\nd\re");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("a\r\nb\r\nc\nd\re");
    app.ok("select 11 11");
    app.ok("key 13");
    app.wait_text("a\r\nb\r\nc\nd\re\r\n");
    app.run("Save");
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(std::fs::read(&file).unwrap(), b"a\r\nb\r\nc\nd\re\r\n");
    app.quit();
}

#[test]
fn p28_i43_a_utf8_bom_stays_on_save() {
    let folder = Folder::new("p28");
    let file = folder.file("f.txt", b"\xEF\xBB\xBFhi");
    let mut app = App::start(&folder.root(), &[&file]);
    app.settled();
    let length = app.text().len();
    app.ok(&format!("select {length} {length}"));
    app.type_text("!");
    app.run("Save");
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(std::fs::read(&file).unwrap(), b"\xEF\xBB\xBFhi!");
    app.quit();
}

#[test]
fn p29_the_encoding_list_changes_what_open_and_save_offer() {
    let folder = Folder::new("p29");
    let mut app = App::start(&folder.root(), &[]);
    app.ok(&format!(
        "set-text EnabledEncodings {}",
        encode("UTF-8,Shift JIS (932)")
    ));
    app.run("Open");
    let open: Vec<String> = rows(&mut app).into_iter().map(|row| row.0).collect();
    assert_eq!(open.len(), 3, "{open:?}");
    assert_eq!(open[0], "Automatic");
    assert_eq!(open[1], "UTF-8");
    app.ok("panel-cancel");
    app.type_text("x");
    app.run("SaveAs");
    let save: Vec<String> = rows(&mut app).into_iter().map(|row| row.0).collect();
    assert_eq!(save, open[1..].to_vec());
    app.ok("panel-cancel");
    app.quit();
}

#[test]
fn i42_the_default_list_is_the_macs_ten_encodings() {
    let folder = Folder::new("i42");
    let mut app = App::start(&folder.root(), &[]);
    let list = decode(&app.ask("encodings"));
    assert_eq!(list.split('|').count(), 10, "{list}");
    for name in ["UTF-16", "UTF-8", "Windows-1252", "GB 18030"] {
        assert!(list.contains(name), "{name} in {list}");
    }
    app.quit();
}

#[test]
fn i44_utf16_with_and_without_a_bom_save_in_their_own_form() {
    let folder = Folder::new("i44");
    let mut app = App::start(&folder.root(), &[]);
    // The BOM a file came with stays, as in the Mac's `PlainTextIO`.
    for (name, start, want) in [
        ("le.txt", &b"\xFF\xFEa\0"[..], &b"\xFF\xFEa\0b\0"[..]),
        ("be.txt", &b"\xFE\xFF\0a"[..], &b"\xFE\xFF\0a\0b"[..]),
    ] {
        let with = folder.file(name, start);
        app.ok(&format!("open {}", path_word(&with)));
        app.wait_text("a");
        app.ok("select 1 1");
        app.type_text("b");
        app.run("Save");
        app.wait_for("the save", |app| app.doc("dirty") == "false");
        assert_eq!(std::fs::read(&with).unwrap(), want, "{name}");
    }
    // A new document in UTF-16 writes little-endian with a BOM.
    app.ok("set-text SaveEncoding UTF-16");
    app.run("NewTab");
    app.type_text("a");
    let fresh = folder.0.join("fresh.txt");
    app.run("Save");
    app.wait_panel();
    app.ok(&format!("panel-path {}", path_word(&fresh)));
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(std::fs::read(&fresh).unwrap(), b"\xFF\xFEa\0");

    let none = folder.file("none.txt", b"\0a");
    open_with_row(&mut app, &none, "UTF-16");
    app.wait_text("a");
    app.ok("select 1 1");
    app.type_text("b");
    app.run("Save");
    app.wait_for("the save", |app| app.doc("dirty") == "false");
    assert_eq!(
        std::fs::read(&none).unwrap(),
        b"\0a\0b",
        "a file read with no BOM writes big-endian with none"
    );
    app.quit();
}

#[test]
fn i46_paste_keeps_the_line_endings_it_copied() {
    let folder = Folder::new("i46");
    let file = folder.file("f.txt", b"a\r\nb\nc\rd");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("a\r\nb\nc\rd");
    app.ok("select 0 8");
    app.ok("key 67 ctrl");
    app.run("NewTab");
    app.ok("key 86 ctrl");
    app.wait_text("a\r\nb\nc\rd");
    app.quit();
}

// ---- Find and the other tools ----

#[test]
fn p30_i04_the_windows_keys_find_and_step() {
    let folder = Folder::new("p30");
    let file = folder.file("f.txt", b"one two one\nthree one\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("one two one\nthree one\n");
    app.ok("select 0 3");
    app.ok("key 69 ctrl"); // Use Selection for Find
    app.ok("key 114"); // F3
    app.wait_for("the next match", |app| app.ask("selection") == "8 11");
    app.ok("key 114");
    app.wait_for("the next match", |app| app.ask("selection") == "18 21");
    app.ok("key 114 shift");
    app.wait_for("the match before", |app| app.ask("selection") == "8 11");
    app.ok("key 74 ctrl"); // Jump to Selection
    assert_eq!(app.ask("selection"), "8 11");
    app.ok("key 70 ctrl");
    assert!(app.ask("tools-text").contains("bar=true"));
    app.ok("key 27");
    app.ok("key 72 ctrl");
    assert!(app.ask("tools-text").contains("bar=true"));
    app.ok("key 27");
    has_all(
        &labels(&mut app, 1),
        &[
            "Find…",
            "Find and Replace…",
            "Find Next",
            "Find Previous",
            "Use Selection for Find",
            "Jump to Selection",
            "Select Line…",
        ],
    );
    app.quit();
}

fn select_line(app: &mut App, entry: &str) {
    app.ok("key 76 ctrl");
    assert!(app.ask("tools-text").contains("line=true"));
    app.type_text(entry);
    app.ok("key 13");
    app.wait_for("the panel to close", |app| {
        app.ask("tools-text").contains("line=false")
    });
}

#[test]
fn p31_i56_i57_i58_select_line_takes_each_form_and_every_line_ending() {
    let folder = Folder::new("p31");
    // Each kind of line ending, in the order the row lists them.
    let text = "L1\nL2\rL3\r\nL4\u{85}L5\u{2028}L6\u{2029}L7";
    let file = folder.file("f.txt", text.as_bytes());
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text(text);
    let place = |line: &str| text.find(line).unwrap();
    for (entry, from, to) in [
        ("7", "L7", None),
        ("2", "L2", Some("L3")),
        ("4-5", "L4", Some("L6")),
    ] {
        select_line(&mut app, entry);
        let end = to.map_or(text.len(), place);
        assert_eq!(
            app.ask("selection"),
            format!("{} {end}", place(from)),
            "{entry}"
        );
    }
    // From lines 4 and 5, relative forms count from the selection.
    select_line(&mut app, "+1");
    assert_eq!(
        app.ask("selection"),
        format!("{} {}", place("L6"), place("L7")),
        "+1"
    );
    select_line(&mut app, "-1");
    assert_eq!(
        app.ask("selection"),
        format!("{} {}", place("L5"), place("L6")),
        "-1"
    );

    // A bad entry stays open and says why, in the Mac's words.
    app.ok("key 76 ctrl");
    app.type_text("0");
    app.ok("key 13");
    assert!(app.ask("tools-text").contains("line=true"));
    let shows = decode(&app.ask("frame-text"));
    assert!(shows.contains("Invalid line specification"), "{shows}");
    app.ok("key 27");
    app.ok("key 27");
    app.quit();
}

#[test]
fn p32_i05_check_document_finds_the_word_and_a_fix_row_changes_it() {
    let folder = Folder::new("p32");
    let file = folder.file("f.txt", b"good speling here\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("good speling here\n");
    app.run("CheckDocumentSpelling");
    app.wait_for("the word", |app| app.ask("selection") == "5 12");
    assert_eq!(app.ask("spell-menu 6"), "true");
    let spelling = labels(&mut app, 1);
    has_all(
        &spelling,
        &[
            "Show Spelling",
            "Check Document Now",
            "Check Spelling While Typing",
            "Correct Spelling Automatically",
        ],
    );
    assert!(
        !spelling.iter().any(|row| row.contains("Grammar")),
        "{spelling:?}"
    );
    app.quit();
}

#[test]
fn p33_i06_i19_dashes_and_copy_follow_their_rows_and_defaults() {
    let folder = Folder::new("p33");
    let mut app = App::start(&folder.root(), &[]);
    assert!(app.state("SmartCopyPaste").1, "on by default");
    assert!(!app.state("SmartQuotes").1);
    assert!(!app.state("SmartDashes").1);
    app.type_text("a--");
    assert_eq!(app.text(), "a--");
    app.run("SmartDashes");
    app.type_text("b--c");
    assert_eq!(app.text(), "a--b\u{2014}c");
    let edit = labels(&mut app, 1);
    has_all(&edit, &["Smart Copy/Paste", "Smart Quotes", "Smart Dashes"]);
    has_none(&edit, &["Data Detectors", "Text Replacement"]);
    app.quit();
}

#[test]
fn p34_i07_transformations_change_the_selection() {
    let folder = Folder::new("p34");
    let file = folder.file("f.txt", b"hello wORLD");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("hello wORLD");
    for (command, want) in [
        ("UpperCase", "HELLO WORLD"),
        ("LowerCase", "hello world"),
        ("Capitalize", "Hello World"),
    ] {
        app.ok("select 0 11");
        app.run(command);
        app.wait_text(want);
    }
    app.quit();
}

#[test]
fn p36_ctrl_space_completes_the_word() {
    let folder = Folder::new("p36");
    let mut app = App::start(&folder.root(), &[]);
    // A list opens only in a window with the keyboard, and a hidden one has it only when told.
    app.ok("focus");
    app.type_text("alphabet al");
    app.ok("key 32 ctrl");
    app.wait_for("the word list", |app| app.ask("tools-idle") == "true");
    app.ok("key 13");
    app.wait_text("alphabet alphabet");
    app.quit();
}

#[test]
fn p37_prevent_editing_stops_typing_and_lets_selection_work() {
    let folder = Folder::new("p37");
    let file = folder.file("f.txt", b"words");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("words");
    app.run("PreventEditing");
    assert!(app.state("PreventEditing").1);
    app.ok("select 0 0");
    app.type_text("x");
    app.ok("key 46");
    assert_eq!(app.text(), "words");
    app.ok("key 65 ctrl");
    assert_eq!(app.ask("selection"), "0 5");
    assert_eq!(app.doc("dirty"), "false");
    app.run("PreventEditing");
    app.ok("select 0 0");
    app.type_text("x");
    assert_eq!(app.text(), "xwords");
    app.quit();
}

// ---- Layout and view ----

#[test]
fn p40_p41_wrap_to_page_flips_its_title_and_shows_paper() {
    let folder = Folder::new("p40");
    let mut app = App::start(&folder.root(), &[]);
    assert!(decode(&app.ask("menu-row 2 WrapToPage")).starts_with("Wrap to Page|Ctrl+Shift+W|"));
    assert!(number(&mut app, "mapw") > 0.0);
    app.ok("key 87 ctrl shift");
    app.wait_for("the page", |app| view(app, "page") == "true");
    assert!(decode(&app.ask("menu-row 2 WrapToPage")).starts_with("Wrap to Window|"));
    assert_eq!(number(&mut app, "mapw"), 0.0, "pages hide the side views");
    let paper = view(&mut app, "paper");
    assert!(paper == "612x792" || paper == "595x842", "{paper}");
    app.run("WrapToPage");
    app.wait_for("the window wrap", |app| view(app, "page") == "false");
    assert!(decode(&app.ask("menu-row 2 WrapToPage")).starts_with("Wrap to Page|"));
    app.quit();
}

#[test]
fn p42_i35_zoom_doubles_and_halves_inside_25_and_1600_percent() {
    let folder = Folder::new("p42");
    let mut app = App::start(&folder.root(), &[]);
    assert_eq!(view(&mut app, "zoom"), "1");
    for want in ["2", "4", "8", "16"] {
        app.ok("key 187 ctrl");
        assert_eq!(view(&mut app, "zoom"), want);
    }
    assert!(!app.state("ZoomIn").0, "1600% is the top");
    app.ok("key 190 ctrl");
    assert_eq!(view(&mut app, "zoom"), "1");
    for want in ["0.5", "0.25"] {
        app.ok("key 189 ctrl");
        assert_eq!(view(&mut app, "zoom"), want);
    }
    assert!(!app.state("ZoomOut").0, "25% is the bottom");
    app.quit();
}

#[test]
fn p44_the_text_width_does_not_change_with_the_length_of_the_text() {
    let folder = Folder::new("p44");
    let long: String = (0..2000).map(|i| format!("line {i}\n")).collect();
    let file = folder.file("long.txt", long.as_bytes());
    let mut app = App::start(&folder.root(), &[]);
    let short = view(&mut app, "textw");
    app.run("Open");
    app.wait_panel();
    app.ok(&format!("panel-path {}", path_word(&file)));
    app.wait_text(&long);
    assert_eq!(
        view(&mut app, "textw"),
        short,
        "this app has no scrollbar to take room"
    );
    app.quit();
}

#[test]
fn i27_line_numbers_and_the_map_lie_outside_the_90_columns() {
    let folder = Folder::new("i27");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("set-bool ShowLineNumbers true");
    app.run("New");
    app.wait_for("a window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    assert!(number(&mut app, "gutterw") > 0.0, "the gutter shows");
    assert!(number(&mut app, "mapw") > 0.0, "the map shows");
    let wide = columns(&mut app);
    assert!((90.0..91.0).contains(&wide), "{wide} columns");
    app.ok("use-window 0");
    app.quit();
}

#[test]
fn i36_each_paper_size_has_its_points() {
    let folder = Folder::new("i36");
    let mut app = App::start(&folder.root(), &[]);
    for (name, want) in [
        ("Letter", "612x792"),
        ("A4", "595x842"),
        ("Legal", "612x1008"),
    ] {
        app.ok(&format!("set-text PaperSize {name}"));
        assert_eq!(view(&mut app, "paper"), want, "{name}");
    }
    assert!(app.ask("set-text PaperSize A3").starts_with("error"));
    app.quit();
}

// ---- The top row ----

#[test]
fn i60_w02_w03_one_tab_shows_and_the_plus_adds_one() {
    let folder = Folder::new("w02");
    let mut app = App::start(&folder.root(), &[]);
    assert_eq!(app.tab_count(), 1);
    let name = app.doc("name");
    app.ok(&format!("named invoke {}", encode(&name)));
    app.ok(&format!("named invoke {}", encode("New tab")));
    app.wait_for("a second tab", |app| app.tab_count() == 2);
    app.quit();
}

// ---- Settings ----

#[test]
fn p50_i14_i55_settings_reach_new_documents_and_leave_open_ones() {
    let folder = Folder::new("p50");
    let long: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let file = folder.file("long.txt", long.as_bytes());
    let mut app = App::start(&folder.root(), &[]);
    app.ok("set-int WindowWidthCharacters 60");
    app.ok("set-int WindowHeightLines 10");
    app.ok(&format!("set-text PlainTextFont {}", encode("Consolas 12")));
    app.ok("set-bool ShowPageBreaks true");
    app.ok("set-bool SmartQuotes true");
    // The open document keeps what it has.
    assert_eq!(view(&mut app, "family"), "Cascadia Mono");
    assert_eq!(view(&mut app, "page"), "false");
    assert!(columns(&mut app) > 80.0);

    app.run("New");
    app.wait_for("a window", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    assert_eq!(view(&mut app, "family"), "Consolas");
    assert_eq!(view(&mut app, "points"), "12");
    assert_eq!(
        view(&mut app, "page"),
        "true",
        "a new document takes the setting"
    );
    app.type_text("\"");
    assert_eq!(app.text(), "\u{201c}");
    app.run("WrapToPage");
    app.wait_for("window wrap", |app| view(app, "page") == "false");
    let wide = columns(&mut app);
    assert!((60.0..61.0).contains(&wide), "{wide} columns");

    // A file from disk starts in Wrap to Window, in a window of the new height.
    app.run("New");
    app.wait_for("a window", |app| app.windows().len() == 3);
    app.ok("use-window 2");
    app.run("Open");
    app.wait_panel();
    app.ok(&format!("panel-path {}", path_word(&file)));
    app.wait_text(&long);
    assert_eq!(
        view(&mut app, "page"),
        "false",
        "a file starts in Wrap to Window"
    );
    app.wait_for("the rows", |app| {
        app.ok("paint");
        let answer = app.ask("rows-on-screen");
        let mut parts = answer.split(' ');
        let rows: usize = parts.next().unwrap().parse().unwrap();
        parts.next() == Some("true") && (10..=12).contains(&rows)
    });
    app.ok("use-window 0");
    app.quit();
}

#[test]
fn p51_the_encoding_settings_and_txt_setting_reach_open_and_save() {
    let folder = Folder::new("p51");
    let file = folder.file("f.txt", "\u{e9}".as_bytes());
    let mut app = App::start(&folder.root(), &[]);
    app.ok("set-text OpenEncoding Windows-1252");
    app.ok("set-text SaveEncoding UTF-16");
    app.ok("set-bool AddTxtExtension false");

    app.ok(&format!("open {}", path_word(&file)));
    app.wait_text("\u{c3}\u{a9}");
    app.run("NewTab");
    app.type_text("x");
    app.run("SaveAs");
    assert_eq!(panel(&mut app, "txt"), "Some(false)");
    let at = row_index(&mut app, "UTF-16");
    assert_eq!(panel(&mut app, "selected"), at.to_string());
    app.ok("panel-cancel");
    app.quit();
}

#[test]
fn p52_settings_come_back_after_a_restart() {
    let folder = Folder::new("p52");
    let root = folder.root();
    let mut app = App::start(&root, &[]);
    app.ok("set-bool SmartQuotes true");
    app.ok("set-int WindowWidthCharacters 70");
    app.ok(&format!("set-text PlainTextFont {}", encode("Consolas 12")));
    app.quit();
    let mut again = App::start(&root, &[]);
    assert_eq!(again.ask("get-bool SmartQuotes"), "Some(true)");
    assert_eq!(view(&mut again, "family"), "Consolas");
    let wide = columns(&mut again);
    assert!((70.0..71.0).contains(&wide), "{wide} columns");
    again.quit();
}

// ---- PDF ----

fn pdf_pages(path: &Path) -> qe_pdf::check::Report {
    eventually("the PDF", || path.exists());
    qe_pdf::check::check_pdf(&std::fs::read(path).unwrap()).unwrap()
}
fn all_text(report: &qe_pdf::check::Report) -> String {
    report.page_rows(0).join("\n")
}

#[test]
fn p60_i59_the_pdf_dialog_has_both_choices_with_the_macs_defaults() {
    let folder = Folder::new("p60");
    let line = "word ".repeat(400);
    let file = folder.file("wide.txt", line.as_bytes());
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text(&line);

    // Each row, with the defaults: no header, and the text rewraps.
    for command in ["Print", "ExportPdf"] {
        app.run(command);
        assert_eq!(panel(&mut app, "header"), "Some(false)", "{command}");
        assert_eq!(panel(&mut app, "rewrap"), "Some(true)", "{command}");
        app.ok("panel-cancel");
    }

    // Header on, rewrap off: the choices reach the file, and the next dialog starts with them.
    let plain = folder.0.join("plain.pdf");
    app.run("ExportPdf");
    app.wait_panel();
    app.ok(&format!("panel-pdf {} false true", path_word(&plain)));
    let plain = pdf_pages(&plain);
    let header = folder.0.join("header.pdf");
    app.run("Print");
    app.wait_panel();
    app.ok(&format!("panel-pdf {} true false", path_word(&header)));
    let header = pdf_pages(&header);
    assert!(
        !all_text(&plain).contains("wide.txt"),
        "no header unless it is asked for"
    );
    assert!(all_text(&header).contains("wide.txt"), "the header's title");
    assert_eq!(app.ask("get-bool NumberPagesWhenPrinting"), "Some(true)");
    assert_eq!(app.ask("get-bool WrapToFitWhenPrinting"), "Some(false)");
    app.run("ExportPdf");
    assert_eq!(panel(&mut app, "header"), "Some(true)");
    assert_eq!(panel(&mut app, "rewrap"), "Some(false)");
    app.ok("panel-cancel");

    // Wrap to Page has fixed pages, so that dialog has no rewrap box, as on the Mac.
    app.run("WrapToPage");
    app.wait_for("the page", |app| view(app, "page") == "true");
    app.run("ExportPdf");
    assert_eq!(panel(&mut app, "rewrap"), "None");
    app.ok("panel-cancel");
    app.quit();
}

#[test]
fn p62_export_and_print_make_the_same_pages() {
    let folder = Folder::new("p62");
    let text: String = (0..300).map(|i| format!("row {i}\n")).collect();
    let file = folder.file("rows.txt", text.as_bytes());
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text(&text);
    let mut reports = Vec::new();
    for (command, name) in [("ExportPdf", "e.pdf"), ("Print", "p.pdf")] {
        let target = folder.0.join(name);
        app.run(command);
        app.wait_panel();
        app.ok(&format!("panel-path {}", path_word(&target)));
        reports.push(pdf_pages(&target));
    }
    assert!(reports[0].pages.len() > 1);
    assert_eq!(reports[0].pages.len(), reports[1].pages.len());
    for page in 0..reports[0].pages.len() {
        assert_eq!(reports[0].page_rows(page), reports[1].page_rows(page));
    }
    app.quit();
}

// ---- Menus ----

#[test]
fn i01_i02_the_file_menu_ends_in_settings_with_no_about_or_quit() {
    let folder = Folder::new("i02");
    let mut app = App::start(&folder.root(), &[]);
    let file = labels(&mut app, 0);
    has_all(
        &file,
        &[
            "New",
            "New Tab",
            "Open…",
            "Open Recent",
            "Close",
            "Save",
            "Duplicate",
            "Save As…",
            "Rename…",
            "Move To…",
            "Save All",
            "Revert to Saved",
            "Export as PDF…",
            "Print…",
        ],
    );
    assert_eq!(file.last().unwrap(), "Settings…");
    has_none(
        &file,
        &[
            "About",
            "Quit",
            "Share",
            "Hide",
            "Hide Others",
            "Show All",
            "Services",
        ],
    );
    app.quit();
}

#[test]
fn i02_close_reads_close_tab_at_two_tabs() {
    let folder = Folder::new("i02b");
    let mut app = App::start(&folder.root(), &[]);
    assert!(decode(&app.ask("menu-row 0 Close")).starts_with("Close|"));
    app.run("NewTab");
    assert!(decode(&app.ask("menu-row 0 Close")).starts_with("Close Tab|"));
    app.run("Close");
    app.wait_for("one tab", |app| app.tab_count() == 1);
    assert!(decode(&app.ask("menu-row 0 Close")).starts_with("Close|"));
    app.quit();
}

#[test]
fn i03_i08_i09_i10_i11_the_other_menus_hold_their_rows() {
    let folder = Folder::new("i03");
    let mut app = App::start(&folder.root(), &[]);
    has_all(
        &labels(&mut app, 1),
        &[
            "Undo",
            "Redo",
            "Cut",
            "Copy",
            "Paste",
            "Delete",
            "Complete",
            "Select All",
            "Find",
            "Spelling",
            "Substitutions",
            "Transformations",
        ],
    );
    has_none(&labels(&mut app, 1), &["Speech", "Start Speaking"]);
    assert_eq!(labels(&mut app, 2), ["Prevent Editing", "Wrap to Page"]);
    has_all(
        &labels(&mut app, 3),
        &[
            "Actual Size",
            "Zoom In",
            "Zoom Out",
            "Show Line Numbers",
            "Show Document Map",
            "Enter Full Screen",
        ],
    );
    let full = menu(&mut app, 3).concat();
    assert!(full.contains("Enter Full Screen|F11|"), "{full}");
    has_all(
        &labels(&mut app, 4),
        &[
            "Minimize",
            "Zoom",
            "Show Previous Tab",
            "Show Next Tab",
            "Tab 1",
            "Tab 9",
            "Last Tab",
            "Bring All to Front",
            "Move Tab to New Window",
            "Merge All Windows",
        ],
    );
    // I11: five groups, and no Help.
    assert_eq!(app.ask("menu-dump 5"), "");
    app.quit();
}

#[test]
fn i62_w08_the_tab_keys_and_the_windows_keys_work() {
    let folder = Folder::new("i62");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("key 84 ctrl");
    app.ok("key 84 ctrl");
    app.wait_for("three tabs", |app| app.tab_count() == 3);
    let active = |app: &mut App| {
        app.windows()[0]
            .iter()
            .position(|tab| tab.ends_with('*'))
            .unwrap()
    };
    assert_eq!(active(&mut app), 2);
    app.ok("key 49 ctrl");
    assert_eq!(active(&mut app), 0, "Ctrl+1");
    app.ok("key 48 ctrl");
    assert_eq!(active(&mut app), 2, "Ctrl+0");
    app.ok("key 9 ctrl");
    assert_eq!(active(&mut app), 0, "Ctrl+Tab");
    app.ok("key 9 ctrl shift");
    assert_eq!(active(&mut app), 2, "Ctrl+Shift+Tab");
    app.ok("key 50 ctrl");
    assert_eq!(active(&mut app), 1, "Ctrl+2");
    app.quit();
}

// ---- Blank windows, outside changes, large files ----

#[test]
fn i48_blank_untitled_text_closes_with_no_question_and_stays_gone() {
    let folder = Folder::new("i48");
    let root = folder.root();
    let mut app = App::start(&root, &[]);
    app.run("NewTab");
    app.type_text("  \n\t ");
    app.run("Close");
    app.wait_for("the tab to go", |app| app.tab_count() == 1);
    assert!(app.alert().is_none(), "white space is blank");
    app.quit();
    let mut again = App::start(&root, &[]);
    assert_eq!(again.tab_count(), 1);
    assert_eq!(again.text(), "");
    again.quit();
}

#[test]
fn i50_a_change_of_date_alone_keeps_text_caret_and_undo() {
    let folder = Folder::new("i50");
    let file = folder.file("f.txt", b"same bytes");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("same bytes");
    app.ok("select 0 0");
    app.type_text("x");
    app.run("Undo");
    app.wait_text("same bytes");
    assert_eq!(app.doc("dirty"), "false");
    app.ok("select 3 3");
    let out = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    out.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    drop(out);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(app.ask("selection"), "3 3");
    app.run("Redo");
    app.wait_text("xsame bytes");
    app.quit();
}

#[test]
fn i51_i64_over_50_mib_the_extras_turn_off_and_the_title_says_so() {
    let folder = Folder::new("i51");
    let file = big_file(&folder, "big.txt", 51);
    let mut app = App::start(&folder.root(), &[&file]);
    app.settled();
    app.wait_for("the note", |app| {
        app.doc("note") == "Large file: spelling and substitutions off"
    });
    for row in [
        "ShowLineNumbers",
        "ShowDocumentMap",
        "CheckSpellingWhileTyping",
        "CorrectSpellingAutomatically",
        "SmartQuotes",
        "SmartDashes",
    ] {
        assert_eq!(app.state(row), (false, false), "{row}");
    }
    assert_eq!(number(&mut app, "gutterw"), 0.0);
    assert_eq!(number(&mut app, "mapw"), 0.0);
    // The rows' settings stay as they were, so a small file is not changed by this one.
    app.ok("select 0 0");
    app.type_text("\"");
    app.wait_for("the mark", |app| app.doc("dirty") == "true");
    app.ok("select 0 1");
    app.run("Copy");
    app.ok("select 0 0");
    app.run("Undo");
    app.wait_for("the undo", |app| app.doc("dirty") == "false");
    app.quit();
}

#[test]
fn i52_i53_i64_over_512_mib_a_file_reads_finds_and_copies_its_bytes() {
    let folder = Folder::new("i52");
    let file = big_file(&folder, "huge.txt", 513);
    let mut app = App::start(&folder.root(), &[&file]);
    app.settled();
    app.wait_for("the note", |app| {
        app.doc("note")
            .starts_with("Read only: file too large to edit (")
    });
    // It does not edit, and it reads, selects and goes to a line.
    let before = app.doc("bytes");
    app.ok("select 0 0");
    app.type_text("x");
    assert_eq!(app.doc("dirty"), "false");
    assert_eq!(app.doc("bytes"), before);
    select_line(&mut app, "2");
    assert_eq!(app.ask("selection"), "1024 2048");

    // Save As copies the original bytes.
    let copy = folder.0.join("copy.txt");
    app.run("SaveAs");
    app.wait_panel();
    app.ok(&format!("panel-path {}", path_word(&copy)));
    let size = std::fs::metadata(&file).unwrap().len();
    eventually("the copy", || {
        std::fs::metadata(&copy).is_ok_and(|found| found.len() == size)
    });
    app.settled();
    app.quit();
    let (mut one, mut two) = (
        std::fs::File::open(&file).unwrap(),
        std::fs::File::open(&copy).unwrap(),
    );
    use std::io::Read;
    let (mut left, mut right) = (vec![0; 1 << 20], vec![0; 1 << 20]);
    loop {
        let count = one.read(&mut left).unwrap();
        if count == 0 {
            break;
        }
        two.read_exact(&mut right[..count]).unwrap();
        assert!(left[..count] == right[..count]);
    }
}

#[test]
fn i54_prevent_editing_and_wrap_belong_to_each_document() {
    let folder = Folder::new("i54");
    let mut app = App::start(&folder.root(), &[]);
    app.run("PreventEditing");
    app.run("WrapToPage");
    app.wait_for("the page", |app| view(app, "page") == "true");
    app.run("NewTab");
    assert!(!app.state("PreventEditing").1);
    assert_eq!(view(&mut app, "page"), "false");
    app.run("PreviousTab");
    assert!(app.state("PreventEditing").1);
    assert_eq!(view(&mut app, "page"), "true");
    app.quit();
}
