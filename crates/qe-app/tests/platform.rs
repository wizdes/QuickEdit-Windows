//! What needs the real Windows parts: UI Automation from another process, pictures of a whole
//! window, a real menu through real key messages, and what must stay unloaded.
#[path = "../../qe-ui/tests/support/png.rs"]
#[allow(dead_code)]
mod png;
mod support;

use qe_api::Bitmap;
use std::path::PathBuf;
use support::{App, Folder, encode};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Accessibility::*;
use windows::core::Interface;

/// UIA fails with several clients at once in one process, as `qe-a11y`'s README says.
static ONE_CLIENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn client() -> IUIAutomation {
    // SAFETY: plain COM set-up on this test's thread, and a repeat only counts up.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).expect("a UIA client")
    }
}

fn names_below(client: &IUIAutomation, root: &IUIAutomationElement) -> Vec<(String, i32)> {
    // SAFETY: live COM objects, on this thread.
    unsafe {
        let all = client.CreateTrueCondition().expect("a condition");
        let found = root
            .FindAll(TreeScope_Descendants, &all)
            .expect("FindAll answers");
        let count = found.Length().expect("a length");
        (0..count)
            .map(|at| {
                let element = found.GetElement(at).expect("an element");
                let name = element.CurrentName().expect("a name").to_string();
                let kind = element.CurrentControlType().expect("a type").0;
                (name, kind)
            })
            .collect()
    }
}

#[test]
fn a_real_client_reads_the_tree_and_the_document_of_a_hidden_window() {
    let _turn = ONE_CLIENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = Folder::new("uia");
    let file = folder.file("spoken.txt", b"read me aloud\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("read me aloud\n");
    app.run("Find");
    let raw: isize = app.ask("hwnd").parse().expect("a window handle");
    let hwnd = HWND(raw as *mut _);
    let client = client();
    // SAFETY: as above. The handle belongs to the child, and UIA crosses that line itself.
    unsafe {
        let root = client.ElementFromHandle(hwnd).expect("ElementFromHandle");
        let names = names_below(&client, &root);
        let has = |want: &str| names.iter().any(|(name, _)| name == want);
        for want in [
            "File",
            "Edit",
            "Format",
            "View",
            "Window",
            "Find",
            "spoken.txt",
        ] {
            assert!(has(want), "{want} is not in {names:?}");
        }
        let all = client.CreateTrueCondition().expect("a condition");
        let found = root.FindAll(TreeScope_Descendants, &all).expect("FindAll");
        let mut text = None;
        for at in 0..found.Length().expect("a length") {
            let element = found.GetElement(at).expect("an element");
            if element.CurrentControlType().expect("a type") == UIA_DocumentControlTypeId {
                let pattern: IUIAutomationTextPattern = element
                    .GetCurrentPatternAs(UIA_TextPatternId)
                    .expect("the document has text");
                let range = pattern.DocumentRange().expect("a range");
                text = Some(range.GetText(-1).expect("its text").to_string());
            }
        }
        assert_eq!(text.as_deref(), Some("read me aloud\n"));
        // Access keys are in the tree.
        let file_label = (0..found.Length().expect("a length"))
            .map(|at| found.GetElement(at).expect("an element"))
            .find(|element| element.CurrentName().is_ok_and(|name| name == "File"))
            .expect("the File label");
        assert_eq!(
            file_label.CurrentAccessKey().expect("a key").to_string(),
            "Alt+F"
        );
        let _ = file_label.cast::<IUIAutomationElement>();
    }
    app.quit();
}

#[test]
fn a_real_client_can_read_a_question_and_press_its_button() {
    let _turn = ONE_CLIENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = Folder::new("uia-alert");
    let mut app = App::start(&folder.root(), &[]);
    app.type_text("unsaved");
    app.run("Close");
    app.wait_value("the question", App::alert);
    let raw: isize = app.ask("alert-hwnd").parse().expect("a window handle");
    let client = client();
    // SAFETY: as above.
    unsafe {
        let root = client.ElementFromHandle(HWND(raw as *mut _)).expect("root");
        let names = names_below(&client, &root);
        assert!(names.iter().any(|(name, _)| name == "Cancel"), "{names:?}");
        let all = client.CreateTrueCondition().expect("a condition");
        let found = root.FindAll(TreeScope_Descendants, &all).expect("FindAll");
        let cancel = (0..found.Length().expect("a length"))
            .map(|at| found.GetElement(at).expect("an element"))
            .find(|element| element.CurrentName().is_ok_and(|name| name == "Cancel"))
            .expect("the Cancel button");
        let invoke: IUIAutomationInvokePattern = cancel
            .GetCurrentPatternAs(UIA_InvokePatternId)
            .expect("Invoke");
        invoke.Invoke().expect("Invoke answers");
    }
    app.wait_for("the question to go", |app| app.alert().is_none());
    assert_eq!(app.text(), "unsaved");
    app.quit();
}

#[test]
fn a_real_client_can_set_the_find_field_and_the_count_follows() {
    let _turn = ONE_CLIENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = Folder::new("uia-find");
    let file = folder.file("f.txt", b"cat dog cat\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("cat dog cat\n");
    app.run("Find");
    let raw: isize = app.ask("hwnd").parse().expect("a window handle");
    let client = client();
    // SAFETY: as above.
    unsafe {
        let root = client.ElementFromHandle(HWND(raw as *mut _)).expect("root");
        let all = client.CreateTrueCondition().expect("a condition");
        let found = root.FindAll(TreeScope_Descendants, &all).expect("FindAll");
        let field = (0..found.Length().expect("a length"))
            .map(|at| found.GetElement(at).expect("an element"))
            .find(|element| {
                element.CurrentName().is_ok_and(|name| name == "Find")
                    && element.CurrentControlType().expect("a type") == UIA_EditControlTypeId
            })
            .expect("the find field");
        let value: IUIAutomationValuePattern = field
            .GetCurrentPatternAs(UIA_ValuePatternId)
            .expect("Value");
        value
            .SetValue(&windows::core::BSTR::from("cat"))
            .expect("SetValue");
    }
    app.wait_for("the count", |app| app.ask("find-count") == "2 true");
    // Reports to a client that is here must work, on typing and on the bar's changes.
    app.ok("key 27");
    app.type_text("more");
    app.ok("ping");
    let notes = app.notes();
    assert!(!notes.contains("screen reader"), "{notes}");
    app.quit();
}

fn read_picture(path: &std::path::Path) -> Bitmap {
    let bytes = std::fs::read(path).expect("the picture");
    let line = bytes.iter().position(|b| *b == b'\n').expect("a header");
    let head = String::from_utf8_lossy(&bytes[..line]).into_owned();
    let mut parts = head.split(' ');
    let width = parts.next().unwrap().parse().unwrap();
    let height = parts.next().unwrap().parse().unwrap();
    Bitmap {
        width,
        height,
        bgra: bytes[line + 1..].to_vec(),
    }
}
fn pixel(image: &Bitmap, x: u32, y: u32) -> (u8, u8, u8) {
    let at = ((y * image.width + x) * 4) as usize;
    (image.bgra[at + 2], image.bgra[at + 1], image.bgra[at])
}
fn image_folder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("images")
}

#[test]
fn whole_window_pictures_in_light_dark_and_high_contrast() {
    let folder = Folder::new("pictures");
    let file = folder.file(
        "shown.txt",
        b"The quick brown fox\njumps over\nthe lazy dog\n",
    );
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("The quick brown fox\njumps over\nthe lazy dog\n");
    let update = std::env::var_os("QE_APP_UPDATE_IMAGES").is_some();
    for theme in ["light", "dark", "contrast"] {
        let raw = folder.0.join(format!("{theme}.raw"));
        app.ok(&format!(
            "picture {} {theme}",
            encode(&raw.to_string_lossy())
        ));
        let image = read_picture(&raw);
        assert!(
            image.width > 400 && image.height > 300,
            "{}x{}",
            image.width,
            image.height
        );
        let saved = image_folder().join(format!("{theme}.png"));
        if update {
            std::fs::create_dir_all(image_folder()).unwrap();
            std::fs::write(&saved, png::encode(&image)).unwrap();
        }
        // The file with the eyes on it: root looks at these. A different monitor's text settings
        // can move an edge's pixels, so the colors below are the exact check, and this one
        // says only that the layout has not moved.
        if let Ok(bytes) = std::fs::read(&saved) {
            let (width, height, _) = png::decode(&bytes).expect("a saved picture");
            assert_eq!((width, height), (image.width, image.height), "{theme}");
        }
        if theme == "dark" {
            // Exact Notepad colors. The row of tabs is at the top, with the bar under it.
            assert_eq!(
                pixel(&image, image.width - 300, 4),
                (0x20, 0x20, 0x20),
                "tab row"
            );
            let bar_y = (0..image.height / 4)
                .find(|y| pixel(&image, image.width - 300, *y) == (0x2c, 0x2c, 0x2c))
                .expect("the menu bar's color");
            assert!(bar_y > 4, "the bar is under the tab row, at {bar_y}");
            assert_eq!(
                pixel(&image, image.width / 2, image.height - 40),
                (0x27, 0x27, 0x27),
                "text area"
            );
        }
        if theme == "contrast" {
            assert_eq!(
                pixel(&image, image.width / 2, image.height - 40),
                (0, 0, 0),
                "the system's window color"
            );
        }
        if theme == "light" {
            let text = pixel(&image, image.width / 2, image.height - 40);
            assert!(text.0 > 200 && text.1 > 200 && text.2 > 200, "{text:?}");
        }
    }
    app.quit();
}

#[test]
fn alt_f_and_enter_run_new_through_real_key_messages() {
    let folder = Folder::new("menu");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("post-syskey 70");
    app.wait_for("the File menu", |app| app.ask("menu-open") == "true");
    // Enter on the first row: New.
    app.ok("post-key 13");
    app.wait_for("a second window", |app| app.windows().len() == 2);
    assert_eq!(app.ask("menu-open"), "false");
    app.quit();
}

#[test]
fn a_screen_reader_can_invoke_a_menu_row_and_a_tab() {
    let folder = Folder::new("invoke");
    let mut app = App::start(&folder.root(), &[]);
    app.run("NewTab");
    let before = app.ask("windows");
    // The first tab, by its widget number in the shell's range.
    app.ok("invoke-tab 0");
    app.wait_for("the first tab", |app| app.ask("windows") != before);
    app.ok("post-syskey 70");
    app.wait_for("the File menu", |app| app.ask("menu-open") == "true");
    let (windows, _) = (app.windows().len(), ());
    app.ok("invoke-row 0");
    app.wait_for("the row's job", |app| app.windows().len() == windows + 1);
    app.quit();
}

#[test]
fn spelling_and_text_services_stay_out_until_they_are_wanted() {
    let folder = Folder::new("lazy");
    let file = folder.file("lazy.txt", b"plain words\n");
    let mut app = App::start(&folder.root(), &[&file]);
    app.wait_text("plain words\n");
    app.type_text("x");
    assert_eq!(app.ask("loaded MsSpellCheckingFacility.dll"), "false");
    assert_eq!(app.ask("tsf-started"), "false");
    // The first focus starts text services, and typing still works through them.
    app.ok("focus");
    app.wait_for("text services", |app| app.ask("tsf-started") == "true");
    app.ok("blur");
    app.type_text("y");
    assert!(app.text().contains('y'));
    app.quit();
}

#[test]
fn a_setting_for_line_numbers_applies_to_every_open_window() {
    let folder = Folder::new("numbers");
    let mut app = App::start(&folder.root(), &[]);
    app.run("New");
    app.wait_for("two windows", |app| app.windows().len() == 2);
    assert!(!app.state("ShowLineNumbers").1);
    app.ok("set-bool ShowLineNumbers true");
    app.wait_for("the row", |app| app.state("ShowLineNumbers").1);
    app.ok("use-window 0");
    app.wait_for("the other window's row", |app| {
        app.state("ShowLineNumbers").1
    });
    app.quit();
}

#[test]
fn a_bad_settings_file_is_shown_and_the_app_still_runs() {
    let folder = Folder::new("badsettings");
    let root = folder.root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("settings.txt"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
    let mut app = App::start(&root, &[]);
    app.wait_for("the note", |app| !app.notes().is_empty());
    let notes = app.notes();
    assert!(notes.contains("settings"), "{notes}");
    app.type_text("still works");
    assert_eq!(app.text(), "still works");
    app.quit();
}

/// A DLL with one of these names beside a loose copy of the exe must never load. They are real
/// copies from System32, so a load does no harm, and it shows in the module list.
const PLANTED: [&str; 8] = [
    "TextShaping.dll",
    "UIAutomationCore.dll",
    "dwrite.dll",
    "d2d1.dll",
    "dxgi.dll",
    "MSCTF.dll",
    "propsys.dll",
    "version.dll",
];

/// The review's case: `TextShaping.dll` loaded from the exe's folder at every start, and
/// `UIAutomationCore.dll` at the first `WM_GETOBJECT`. Fails too if `main` sets no rule at all.
/// Text services, UI Automation, the file dialogs and spelling must still start from System32.
#[test]
fn no_dll_planted_beside_the_exe_loads_and_com_still_works() {
    let _one = ONE_CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    let folder = Folder::new("planted");
    let beside = folder.0.join("loose");
    std::fs::create_dir_all(&beside).unwrap();
    let copy = beside.join("QuickEdit.exe");
    std::fs::copy(support::exe(), &copy).unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    for name in PLANTED {
        std::fs::copy(system.join(name), beside.join(name)).unwrap();
    }
    let file = folder.file("planted.txt", b"plain words\n");
    let root = folder.root();
    let mut app = App::launch_exe(copy.to_str().unwrap(), &root, &[&file], &[], None);
    app.wait_for("the first window", |app| {
        app.ask("restored") == "true" && !app.ask("windows").is_empty()
    });
    app.wait_text("plain words\n");
    app.ok("focus");
    app.wait_for("text services", |app| app.ask("tsf-started") == "true");
    assert_eq!(app.ask("com-check"), "ok");
    // A real client's first look sends the window its first `WM_GETOBJECT`.
    let hwnd: isize = app.ask("hwnd").parse().unwrap();
    let uia = client();
    // SAFETY: a live COM object on this thread, and the window of a running process.
    let element = unsafe { uia.ElementFromHandle(HWND(hwnd as *mut _)) }.expect("the window");
    names_below(&uia, &element);

    let paths = support::decode(&app.ask("module-paths")).to_lowercase();
    println!("{paths}");
    let lines: Vec<&str> = paths.lines().collect();
    let beside = beside.display().to_string().to_lowercase();
    for line in &lines {
        assert!(
            !line.starts_with(&beside) || line.ends_with("quickedit.exe"),
            "a planted DLL loaded: {line}"
        );
    }
    // So that this test cannot pass by the DLLs not loading at all.
    let system = system.display().to_string().to_lowercase();
    for wanted in [
        "textshaping.dll",
        "uiautomationcore.dll",
        "msctf.dll",
        "dwrite.dll",
    ] {
        assert!(
            lines.contains(&format!("{system}\\{wanted}").as_str()),
            "{wanted} did not load from System32"
        );
    }
    app.quit();
}

/// The review's case for a bad flag: with `--hidden` it must not stop on a message box. No
/// stream here has a reader, so the old code would open a box and never end.
#[test]
fn a_bad_flag_on_a_hidden_run_opens_no_box_and_ends_with_the_error_code() {
    let mut child = std::process::Command::new(support::exe())
        .args(["--hidden", "--no-such-flag"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if std::time::Instant::now() > end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the run did not end, so it waits on a box");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(code, Some(2));
}
