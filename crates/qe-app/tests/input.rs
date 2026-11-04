//! Keys, menu rows and their check marks, what stays unloaded, and what the speed flags measure.
#[path = "../../qe-ui/tests/support/png.rs"]
#[allow(dead_code)]
mod png;
mod support;

use std::process::Command;
use std::time::Duration;
use support::{App, Folder, decode, encode, exe};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Accessibility::*;

fn focus_for_tsf(app: &mut App) {
    app.ok("focus");
    app.wait_for("text services", |app| app.ask("tsf-started") == "true");
    app.ok("focus");
    app.ask("ping");
}

// ---- TSF's focus ----

#[test]
fn the_find_field_takes_tsf_focus_away_from_the_text() {
    let folder = Folder::new("tsf-find");
    let mut app = App::start(&folder.root(), &[]);
    focus_for_tsf(&mut app);
    assert_eq!(app.ask("tsf-focus"), "doc");
    for open in ["Find", "FindReplace", "SelectLine"] {
        app.run(open);
        app.ask("ping");
        assert_eq!(app.ask("tsf-focus"), "other", "{open}");
        // Esc gives the keyboard back to the text, and TSF's focus comes back with it.
        app.ok("key 27");
        app.ask("ping");
        assert_eq!(app.ask("tsf-focus"), "doc", "after {open}");
    }
    app.quit();
}

#[test]
fn opening_find_commits_a_composition_into_the_text() {
    let folder = Folder::new("tsf-find-commit");
    let mut app = App::start(&folder.root(), &[]);
    focus_for_tsf(&mut app);
    app.ok("compose abc");
    app.run("Find");
    app.wait_for("the commit", |app| app.ask("composing") == "false");
    assert_eq!(app.text(), "abc");
    app.quit();
}

#[test]
fn a_tab_in_a_background_window_leaves_tsf_focus_alone() {
    let folder = Folder::new("tsf-background");
    let mut app = App::start(&folder.root(), &[]);
    app.run("New");
    app.wait_for("two windows", |app| app.windows().len() == 2);
    app.ok("use-window 1");
    focus_for_tsf(&mut app);
    assert_eq!(app.ask("tsf-focus"), "doc");
    // The other window has no keyboard. A new tab there must not take TSF's focus.
    app.ok("use-window 0");
    app.run("NewTab");
    app.ask("ping");
    app.ok("use-window 1");
    assert_eq!(app.ask("tsf-focus"), "doc");
    app.quit();
}

#[test]
fn an_app_that_had_the_keyboard_and_lost_it_goes_quiet() {
    let folder = Folder::new("quiet");
    let file = folder.file("idle.txt", b"resting\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("resting\n");
    focus_for_tsf(&mut app);
    app.ok("blur");
    // The session list's write ends within its delay, and only then does the app rest.
    std::thread::sleep(Duration::from_millis(2600));
    let read = |app: &mut App| -> (u64, u64) {
        let answer = app.ask("pumps");
        let mut parts = answer.split(' ');
        (
            parts.next().unwrap().parse().unwrap(),
            parts.next().unwrap().parse().unwrap(),
        )
    };
    let before = read(&mut app);
    std::thread::sleep(Duration::from_secs(2));
    let after = read(&mut app);
    assert_eq!(after.1 - before.1, 0, "frames over 2 s with no keyboard");
    // Reading the count takes a wake or two of its own, and nothing else may come.
    assert!(
        after.0 - before.0 <= 6,
        "{} wakes over 2 s",
        after.0 - before.0
    );
    app.quit();
}

// ---- Keys ----

#[test]
fn a_key_taken_out_of_the_table_stops_working() {
    let folder = Folder::new("old-key");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("words");
    const F2: u16 = 0x71;
    app.ok(&format!("set-key Undo {F2}"));
    // Ctrl+Z is not Undo now, and Ctrl+Y was never in the table.
    app.ok("key 90 ctrl");
    app.ask("ping");
    assert_eq!(app.text(), "words", "the old key still undid");
    app.ok(&format!("key {F2}"));
    app.wait_text("");
    app.ok("key 89 ctrl");
    app.ask("ping");
    assert_eq!(app.text(), "", "Ctrl+Y is no key of this app");
    // Redo is still on its own key, through the table.
    app.ok("key 90 ctrl shift");
    app.wait_text("words");
    app.quit();
}

#[test]
fn the_tables_edit_keys_work_and_delete_backspace_and_enter_stay() {
    let folder = Folder::new("table-keys");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("abc");
    app.ok("key 65 ctrl"); // Select All
    assert_eq!(app.ask("selection"), "0 3");
    app.ok("key 67 ctrl"); // Copy
    app.ok("key 88 ctrl"); // Cut
    app.wait_text("");
    app.ok("key 86 ctrl"); // Paste
    app.wait_text("abc");
    app.ok("key 90 ctrl"); // Undo
    app.wait_text("");
    app.ok("key 90 ctrl shift"); // Redo
    app.wait_text("abc");
    app.ok("key 8");
    app.wait_text("ab");
    app.ok("key 13");
    app.wait_text("ab\n");
    app.ok("select 0 0");
    app.ok("key 46");
    app.wait_text("b\n");
    app.quit();
}

// ---- Rows and their check marks ----

/// Writes a file of this many MiB of short lines.
fn big_file(folder: &Folder, mib: usize) -> std::path::PathBuf {
    use std::io::Write;
    let file = folder.0.join("big.txt");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
    let row = [b'x'; 1023];
    for _ in 0..(mib * 1024) {
        out.write_all(&row).unwrap();
        out.write_all(b"\n").unwrap();
    }
    file
}

#[test]
fn over_50_mib_rows_follow_the_mac_and_a_row_that_is_off_shows_no_check() {
    let folder = Folder::new("large-rows");
    let file = big_file(&folder, 60);
    let root = folder.root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("settings.txt"),
        "ShowLineNumbers=true\nShowDocumentMap=true\nCheckSpellingWhileTyping=true\nCorrectSpellingAutomatically=true\nSmartQuotes=true\nSmartDashes=true\n",
    )
    .unwrap();
    let mut app = App::start(&root, &[&file]);
    app.settled();
    app.wait_for("the note", |app| app.doc("note").contains("Large file"));
    // What the Mac turns off at this size.
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
    // What has no limit on the Mac.
    assert!(app.state("WrapToPage").0);
    assert!(app.state("ZoomIn").0);
    assert!(app.state("ExportPdf").0);
    app.run("ZoomIn");
    app.wait_for("the zoom", |app| app.state("ActualSize").0);
    app.run("ActualSize");
    app.run("WrapToPage");
    app.wait_for("the check", |app| app.state("WrapToPage").1);
    app.run("WrapToPage");
    app.run("ExportPdf");
    assert_eq!(app.wait_panel(), "Export as PDF");
    app.ok("panel-cancel");
    app.quit();
    std::fs::remove_file(&file).unwrap();
}

#[test]
fn the_document_map_row_reaches_every_tab_and_the_setting_reaches_it() {
    let folder = Folder::new("map-tabs");
    let mut app = App::start(&folder.root(), &[]);
    app.run("NewTab");
    assert!(app.state("ShowDocumentMap").1);
    app.run("ShowDocumentMap");
    app.wait_for("this tab", |app| !app.state("ShowDocumentMap").1);
    app.run("Tab1");
    app.wait_for("the other tab", |app| !app.state("ShowDocumentMap").1);
    assert_eq!(app.ask("get-bool ShowDocumentMap"), "Some(false)");
    app.ok("set-bool ShowDocumentMap true");
    app.wait_for("the row", |app| app.state("ShowDocumentMap").1);
    app.run("Tab2");
    app.wait_for("the other row", |app| app.state("ShowDocumentMap").1);
    app.quit();
}

#[test]
fn correct_spelling_writes_its_setting_and_new_tabs_correct_with_true_checks() {
    let folder = Folder::new("correct-tabs");
    let mut app = App::start(&folder.root(), &[]);
    // Correct alone, as the Mac allows: it fixes words, with no underlines.
    app.run("CorrectSpellingAutomatically");
    assert!(app.state("CorrectSpellingAutomatically").1);
    assert!(!app.state("CheckSpellingWhileTyping").1);
    assert_eq!(
        app.ask("get-bool CorrectSpellingAutomatically"),
        "Some(true)"
    );
    app.run("NewTab");
    assert!(app.state("CorrectSpellingAutomatically").1);
    assert!(!app.state("CheckSpellingWhileTyping").1);
    app.type_text("speling ");
    app.wait_text("spelling ");
    assert_eq!(app.ask("underlines"), "0");
    // Each row acts on its own tab, as on the Mac, and the setting serves new tabs.
    app.run("CorrectSpellingAutomatically");
    assert!(!app.state("CorrectSpellingAutomatically").1);
    app.run("Tab1");
    assert!(app.state("CorrectSpellingAutomatically").1);
    app.run("NewTab");
    assert!(!app.state("CorrectSpellingAutomatically").1);
    app.quit();
}

#[test]
fn check_spelling_off_stops_the_marks_and_correct_alone_keeps_working() {
    let folder = Folder::new("check-off");
    let mut app = App::start(&folder.root(), &[]);
    app.run("CheckSpellingWhileTyping");
    app.run("CorrectSpellingAutomatically");
    app.type_text("speling");
    app.wait_for("the underline", |app| app.ask("underlines") == "1");
    app.run("CheckSpellingWhileTyping");
    app.wait_for("no underline", |app| app.ask("underlines") == "0");
    assert!(!app.state("CheckSpellingWhileTyping").1);
    assert!(app.state("CorrectSpellingAutomatically").1);
    app.type_text(" ");
    app.wait_text("spelling ");
    app.quit();
}

// ---- Screen readers' actions ----

#[test]
fn a_control_that_cannot_act_says_so_and_collapse_closes_the_menu() {
    let folder = Folder::new("actions");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("invoke-id 987654");
    app.wait_for("the note", |app| app.notes().contains("could not act"));
    app.ok("post-syskey 70");
    app.wait_for("the File menu", |app| app.ask("menu-open") == "true");
    app.ok("collapse");
    app.wait_for("the menu to close", |app| app.ask("menu-open") == "false");
    app.quit();
}

// ---- What a wiring list asks after an edit ----

#[test]
fn an_edit_from_an_input_method_brings_the_gutter_up_to_date_at_once() {
    let folder = Folder::new("wiring");
    let root = folder.root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("settings.txt"), "ShowLineNumbers=true\n").unwrap();
    let mut app = App::start(&root, &[]);
    focus_for_tsf(&mut app);
    app.ok(&format!("compose {}", encode("abc")));
    app.ask("ping");
    let before: u64 = app.ask("gutter-reads").parse().unwrap();
    app.ok("commit-now");
    app.wait_text("abc");
    app.ask("ping");
    let after: u64 = app.ask("gutter-reads").parse().unwrap();
    assert!(after > before, "{before} {after}");
    app.quit();
}

// ---- An open alert follows its window ----

#[test]
fn an_open_alert_follows_a_new_scale_and_its_clicks_land_on_the_buttons() {
    let folder = Folder::new("alert-dpi");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("words");
    app.run("Close");
    app.wait_value("the question", App::alert);
    let first: f32 = app.ask("alert-scale").parse().unwrap();
    let new_dpi = (first * 96.0) as u32 * 2;
    app.ok(&format!("alert-dpi {new_dpi}"));
    let scale: f32 = app.ask("alert-scale").parse().unwrap();
    assert_eq!(scale, first * 2.0);
    // Cancel is the third button, and its click lands there at the new scale.
    app.ok("alert-mouse 2");
    app.wait_for("the answer", |app| app.alert().is_none());
    assert_eq!(app.text(), "words");
    app.quit();
}

#[test]
fn an_open_alert_follows_a_change_of_theme() {
    let folder = Folder::new("alert-theme");
    let image = folder.0.join("frame.bin");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("words");
    app.run("Close");
    app.wait_value("the question", App::alert);
    // The text window goes dark, and then the system says that its colors changed.
    app.ok(&format!(
        "picture {} dark",
        encode(&image.display().to_string())
    ));
    app.ok("alert-setting-change");
    app.wait_for("the alert's colors", |app| app.ask("alert-theme") == "Dark");
    app.ok(&format!(
        "picture {} light",
        encode(&image.display().to_string())
    ));
    app.ok("alert-setting-change");
    app.wait_for("the alert's colors", |app| {
        app.ask("alert-theme") == "Light"
    });
    app.press("Cancel");
    app.wait_for("the answer", |app| app.alert().is_none());
    app.quit();
}

// ---- What stays unloaded ----

#[test]
fn heavy_libraries_stay_out_of_the_first_frame() {
    let folder = Folder::new("modules");
    let file = folder.file("light.txt", b"plain words\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("plain words\n");
    let modules = app.ask("modules");
    assert!(modules.contains("quickedit"), "{modules}");
    for heavy in [
        "uiautomationcore",
        "shell32",
        "windows.storage",
        "propsys",
        "msspell",
        "msctfmonitor",
    ] {
        assert!(!modules.contains(heavy), "{heavy} is loaded: {modules}");
    }
    app.quit();
}

// ---- The speed flags ----

fn number_after(text: &str, word: &str) -> f64 {
    let at = text
        .find(word)
        .unwrap_or_else(|| panic!("no {word} in {text}"));
    text[at + word.len()..]
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no number after {word} in {text}"))
}

#[test]
fn bench_keys_sends_real_messages_through_the_message_loop() {
    let folder = Folder::new("bench");
    let file = folder.file("bench.txt", "some text to type in\n".repeat(50).as_bytes());
    let output = Command::new(exe())
        .args(["--bench-keys", "40", "--data-root"])
        .arg(folder.root())
        .arg(&file)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{said}");
    let line = said
        .lines()
        .find(|line| line.starts_with("input to present"))
        .unwrap_or_else(|| panic!("no timers in {said}"));
    assert_eq!(number_after(line, "editable file:"), 40.0, "{line}");
    // Each key came from a message the loop took. A bench that calls the app directly has none.
    assert!(number_after(line, "keys from") >= 40.0, "{line}");
    assert!(number_after(line, "p50") > 0.0, "{line}");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        "some text to type in\n".repeat(50).as_bytes()
    );
}

#[test]
fn bench_keys_can_run_with_tab_motion_and_reports_what_each_frame_cost() {
    let folder = Folder::new("bench motion");
    let file = folder.file("bench.txt", "some text to type in\n".repeat(50).as_bytes());
    let output = Command::new(exe())
        .args(["--bench-keys", "40", "--bench-tab-motion", "--data-root"])
        .arg(folder.root())
        .arg(&file)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{said}");
    let line = said
        .lines()
        .find(|line| line.starts_with("input to present"))
        .unwrap_or_else(|| panic!("no timers in {said}"));
    assert!(line.contains("with tab motion"), "{line}");
    assert!(number_after(line, "keys from") >= 40.0, "{line}");
    let motion = said
        .lines()
        .find(|line| line.starts_with("motion:"))
        .unwrap_or_else(|| panic!("no motion in {said}"));
    // Keys and frames took turns: 40 keys take 320 ms, which holds about 20 frames.
    assert!(number_after(motion, "motion:") >= 5.0, "{motion}");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        "some text to type in\n".repeat(50).as_bytes()
    );
}

#[test]
fn a_flag_of_the_bench_alone_is_an_error() {
    let output = Command::new(exe())
        .args(["--hidden", "--bench-tab-motion"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn timings_records_real_keys_in_a_normal_run() {
    let folder = Folder::new("timings");
    let log = folder.0.join("errors.txt");
    let mut app = App::start_logged(&folder.root(), &[], &["--timings"], Some(&log));
    for _ in 0..10 {
        app.ok("post-key 13");
        app.ask("ping");
        std::thread::sleep(Duration::from_millis(5));
    }
    app.wait_text(&"\n".repeat(10));
    app.ok("paint");
    app.ask("ping");
    app.quit();
    let said = std::fs::read_to_string(&log).unwrap();
    let line = said
        .lines()
        .find(|line| line.starts_with("input to present"))
        .unwrap_or_else(|| panic!("no timers in {said}"));
    assert!(number_after(line, "this run:") >= 1.0, "{line}");
}

// ---- A real UIA client ----

/// UIA fails with several clients at once in one process, as `qe-a11y`'s README says.
static ONE_CLIENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn client() -> IUIAutomation {
    // SAFETY: plain COM set-up on this test's thread, and a repeat only counts up.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).expect("a UIA client")
    }
}

fn descendants(client: &IUIAutomation, root: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
    // SAFETY: live COM objects, on this thread.
    unsafe {
        let all = client.CreateTrueCondition().expect("a condition");
        let found = root
            .FindAll(TreeScope_Descendants, &all)
            .expect("FindAll answers");
        (0..found.Length().expect("a length"))
            .map(|at| found.GetElement(at).expect("an element"))
            .collect()
    }
}

#[test]
fn a_client_that_asks_a_popup_first_leaves_every_window_readable() {
    let _turn = ONE_CLIENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = Folder::new("popup-first");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("post-syskey 70");
    app.wait_for("the File menu", |app| app.ask("menu-open") == "true");
    let popup: isize = app.wait_value("the popup", |app| app.ask("popup-hwnd").parse().ok());
    let main: isize = app.ask("hwnd").parse().unwrap();
    let client = client();
    // SAFETY: live COM objects, on this thread.
    unsafe {
        let root = client
            .ElementFromHandle(HWND(popup as *mut _))
            .expect("popup");
        let below = descendants(&client, &root);
        assert!(below.len() > 5, "the popup shows {} items", below.len());
    }
    app.ok("post-key 27");
    app.wait_for("the menu to close", |app| app.ask("menu-open") == "false");
    // SAFETY: as above.
    unsafe {
        let root = client
            .ElementFromHandle(HWND(main as *mut _))
            .expect("main");
        assert!(!root.CurrentName().expect("a name").to_string().is_empty());
        let below = descendants(&client, &root);
        assert!(below.len() > 3, "the window shows {} items", below.len());
    }
    app.quit();
}

#[test]
fn a_real_client_can_work_a_checkbox_in_settings() {
    let _turn = ONE_CLIENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = Folder::new("settings-uia");
    let mut app = App::start(&folder.root(), &[]);
    app.run("Settings");
    app.wait_for("Settings", |app| app.ask("settings-open") == "true");
    let raw: isize = app.wait_value("its window", |app| app.ask("settings-hwnd").parse().ok());
    let client = client();
    assert_eq!(app.ask("get-bool CheckSpellingWhileTyping"), "Some(false)");
    // SAFETY: live COM objects, on this thread.
    unsafe {
        let root = client.ElementFromHandle(HWND(raw as *mut _)).expect("root");
        let below = descendants(&client, &root);
        let names: Vec<String> = below
            .iter()
            .map(|e| e.CurrentName().map(|n| n.to_string()).unwrap_or_default())
            .collect();
        let check = below
            .iter()
            .find(|e| {
                e.CurrentName()
                    .is_ok_and(|n| n == "Check spelling as you type")
            })
            .unwrap_or_else(|| panic!("no checkbox in {names:?}"));
        let toggle: IUIAutomationTogglePattern = check
            .GetCurrentPatternAs(UIA_TogglePatternId)
            .expect("Toggle");
        toggle.Toggle().expect("Toggle answers");
    }
    app.wait_for("the setting", |app| {
        app.ask("get-bool CheckSpellingWhileTyping") == "Some(true)"
    });
    app.quit();
}

#[test]
fn a_run_with_a_client_ends_with_no_word_of_a_bad_close() {
    let _turn = ONE_CLIENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = Folder::new("uia-close");
    let log = folder.0.join("errors.txt");
    let mut app = App::start_logged(&folder.root(), &[], &[], Some(&log));
    let raw: isize = app.ask("hwnd").parse().unwrap();
    let client = client();
    // SAFETY: live COM objects, on this thread.
    unsafe {
        let root = client.ElementFromHandle(HWND(raw as *mut _)).expect("root");
        assert!(!descendants(&client, &root).is_empty());
    }
    app.quit();
    let said = std::fs::read_to_string(&log).unwrap();
    assert!(!said.contains("did not close cleanly"), "{said}");
    let _ = decode("");
}

/// With no timer running, a Blur has nothing to stop, and that is no error.
#[test]
fn losing_focus_with_no_caret_timer_prints_no_error() {
    let folder = Folder::new("blur-timer");
    let log = folder.0.join("errors.txt");
    let mut app = App::start_logged(&folder.root(), &[], &[], Some(&log));
    // The real order: focus, the caret's timer fires once with nothing more to do, then Blur.
    app.ok("focus");
    app.settled();
    std::thread::sleep(Duration::from_millis(1500));
    app.ok("blur");
    app.ok("focus");
    app.ok("blur");
    app.ok("blur");
    app.ask("ping");
    app.quit();
    let printed = std::fs::read_to_string(&log).unwrap();
    assert!(!printed.contains("KillTimer"), "{printed}");
    assert!(!printed.contains("caret's timer"), "{printed}");
}

/// A file with the read-only mark, which goes back to normal when this drops, so that its
/// folder can go too, even after a failed assertion. Declare it after the folder.
struct MarkedReadOnly(std::path::PathBuf);
impl MarkedReadOnly {
    fn new(folder: &Folder) -> Self {
        let path = folder.file("empty.txt", b"");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();
        Self(path)
    }
}
impl Drop for MarkedReadOnly {
    fn drop(&mut self) {
        if let Ok(meta) = std::fs::metadata(&self.0) {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&self.0, perms);
        }
    }
}

#[test]
fn a_paced_bench_whose_keys_reach_no_frame_stops_at_its_limit_and_says_so() {
    let folder = Folder::new("bench paced limit");
    // A file with the read-only mark is still editable, so the bench types into it. The first
    // key reaches a frame and opens the store's question, and the rest reach no frame. See
    // `the_read_only_mark_lets_the_first_key_open_a_question`.
    let file = MarkedReadOnly::new(&folder);
    // To a file: a full pipe that no one reads would stop the app.
    let log = folder.0.join("errors.txt");
    let mut child = Command::new(exe())
        .args(["--bench-keys", "10", "--bench-paced", "--data-root"])
        .arg(folder.root())
        .arg(&file.0)
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let began = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if began.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the bench still waits after 30 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let said = std::fs::read_to_string(&log).unwrap();
    assert_eq!(status.code(), Some(1), "{said}");
    assert!(said.contains("stopped waiting"), "{said}");
    assert!(
        !said.contains("input to present"),
        "no timings from a part of the keys: {said}"
    );
}

#[test]
fn the_read_only_mark_lets_the_first_key_open_a_question() {
    let folder = Folder::new("read only key");
    let file = MarkedReadOnly::new(&folder);
    let mut app = App::start(&folder.root(), &[&file.0]);
    app.settled();
    assert_eq!(app.alert(), None);
    app.ok("post-key 81");
    app.wait_value("the question", App::alert);
    // That key went in, and it is the one frame. The later keys do not reach the text.
    app.ok("post-key 8");
    app.ok("post-key 81");
    app.ask("ping");
    assert_eq!(app.text(), "q");
    app.press("Cancel");
    app.quit();
}
