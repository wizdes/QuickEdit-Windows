//! The file and the values: defaults against the Mac's table, junk, limits, round trips,
//! broken, empty, unknown-key and read-only files, and a failed write's message.
mod support;

use qe_api::{CommandId, EncodingChoice, KeyBinding, SettingKey, TextEncoding};
use qe_settings::{
    FontChoice, PaperSize, SETTINGS_FILE_NAME, SettingChange, SettingsStore, StoredValue,
    default_settings_path, paper_for_region,
};
use std::cell::RefCell;
use std::rc::Rc;
use support::{settings_path, test_folder};

fn fresh(name: &str) -> SettingsStore {
    SettingsStore::open(settings_path(name))
}
fn on_disk(store: &SettingsStore) -> String {
    store.flush().expect("the write worked");
    std::fs::read_to_string(store.path()).expect("the file is there")
}
fn write_file(name: &str, text: &str) -> SettingsStore {
    let path = settings_path(name);
    std::fs::write(&path, text).unwrap();
    SettingsStore::open(path)
}

/// `Defaults.registeredValues()` on the Mac, by this crate's keys, with the parity rows'
/// changes. The Mac keys left out have no place here: grammar (I18), text replacement
/// (I19), Writing Tools and data detectors (off for good on the Mac).
#[test]
fn defaults_match_the_mac_table() {
    let store = fresh("defaults");
    let bools = [
        (SettingKey::ShowPageBreaks, false),
        (SettingKey::AddTxtExtension, true),
        (SettingKey::CheckSpellingWhileTyping, false),
        (SettingKey::CorrectSpellingAutomatically, false),
        (SettingKey::SmartCopyPaste, true),
        (SettingKey::SmartQuotes, false),
        (SettingKey::SmartDashes, false),
        (SettingKey::NumberPagesWhenPrinting, false),
        (SettingKey::WrapToFitWhenPrinting, true),
        (SettingKey::ShowLineNumbers, false),
        (SettingKey::ShowDocumentMap, true),
    ];
    for (key, value) in bools {
        assert_eq!(store.get_bool(key), Some(value), "{key:?}");
    }
    let numbers = [
        (SettingKey::WindowWidthCharacters, 90),
        (SettingKey::WindowHeightLines, 30),
        (SettingKey::TabWidth, 8),
        (SettingKey::AutosavingDelaySeconds, 30),
    ];
    for (key, value) in numbers {
        assert_eq!(store.get_integer(key), Some(value), "{key:?}");
    }
    assert_eq!(
        store.encoding_choice(SettingKey::OpenEncoding),
        EncodingChoice::Automatic
    );
    assert_eq!(
        store.encoding_choice(SettingKey::SaveEncoding),
        EncodingChoice::Automatic
    );
    assert_eq!(
        store.font(),
        FontChoice {
            family: "Cascadia Mono".into(),
            points: 10.0
        }
    );
    assert_eq!(store.font().display_text(), "Cascadia Mono 10 pt");
    assert_eq!(store.enabled_encodings(), TextEncoding::DEFAULT_LIST);
    // Every key was checked above, or has its own test.
    assert_eq!(SettingKey::ALL.len(), bools.len() + numbers.len() + 6);
}
#[test]
fn paper_follows_the_region_with_no_control() {
    assert_eq!(paper_for_region(Some("US")), PaperSize::Letter);
    assert_eq!(paper_for_region(Some("CA")), PaperSize::Letter);
    assert_eq!(paper_for_region(Some("DE")), PaperSize::A4);
    assert_eq!(paper_for_region(None), PaperSize::A4);
    assert_eq!(
        write_file("paper-a4", "PaperSize=A4\r\n").paper_size(),
        PaperSize::A4
    );
    assert_eq!(
        write_file("paper-letter", "PaperSize=Letter\r\n").paper_size(),
        PaperSize::Letter
    );
}
#[test]
fn default_path_is_in_local_app_data_under_quickedit() {
    let path = default_settings_path().unwrap();
    assert_eq!(path.file_name().unwrap(), SETTINGS_FILE_NAME);
    assert_eq!(path.parent().unwrap().file_name().unwrap(), "QuickEdit");
    let local = std::env::var_os("LOCALAPPDATA").unwrap();
    assert_eq!(
        path.parent().unwrap().parent().unwrap(),
        std::path::Path::new(&local)
    );
}

#[test]
fn set_refuses_junk_and_limits_and_keeps_the_old_value() {
    let store = fresh("limits");
    for key in [
        SettingKey::WindowWidthCharacters,
        SettingKey::WindowHeightLines,
    ] {
        assert!(store.set_integer(key, 0).is_err());
        assert!(store.set_integer(key, 10_001).is_err());
        assert!(store.set_integer(key, -5).is_err());
        store.set_integer(key, 1).unwrap();
        store.set_integer(key, 10_000).unwrap();
        assert_eq!(store.get_integer(key), Some(10_000));
    }
    assert!(store.set_integer(SettingKey::TabWidth, 0).is_err());
    assert!(store.set_bool(SettingKey::TabWidth, true).is_err());
    assert!(store.set_integer(SettingKey::SmartQuotes, 1).is_err());
    assert!(store.set_text(SettingKey::OpenEncoding, "Klingon").is_err());
    assert!(store.set_text(SettingKey::PlainTextFont, "NoSize").is_err());
    assert!(
        store
            .set_text(SettingKey::PlainTextFont, "Consolas 0")
            .is_err()
    );
    assert!(
        store
            .set_text(SettingKey::PlainTextFont, "Consolas 9\n")
            .is_err()
    );
    assert!(store.set_text(SettingKey::KeyboardShortcuts, "x").is_err());
    assert!(store.set_text(SettingKey::PaperSize, "Tabloid").is_err());
    store.set_text(SettingKey::PaperSize, "Legal").unwrap();
    store.set_text(SettingKey::PaperSize, "A4").unwrap();
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(8));
}
#[test]
fn round_trip_keeps_every_kind_of_value() {
    let path = settings_path("round");
    let store = SettingsStore::open(&path);
    store
        .set_integer(SettingKey::WindowWidthCharacters, 123)
        .unwrap();
    store.set_bool(SettingKey::SmartCopyPaste, false).unwrap();
    store.set_bool(SettingKey::ShowLineNumbers, true).unwrap();
    store
        .set_font(&FontChoice {
            family: "Segoe UI Mono".into(),
            points: 11.5,
        })
        .unwrap();
    store
        .set_encoding_choice(
            SettingKey::SaveEncoding,
            EncodingChoice::Specific(TextEncoding::ShiftJis),
        )
        .unwrap();
    store
        .set_enabled_encodings(&[TextEncoding::Utf8, TextEncoding::Gb18030])
        .unwrap();
    store
        .set_key(CommandId::Find, KeyBinding::new(0x47, true, false, true))
        .unwrap();
    store.clear_key(CommandId::Save);
    store.flush().unwrap();

    let again = SettingsStore::open(&path);
    assert_eq!(again.load_problem(), None);
    assert_eq!(
        again.get_integer(SettingKey::WindowWidthCharacters),
        Some(123)
    );
    assert_eq!(again.get_bool(SettingKey::SmartCopyPaste), Some(false));
    assert_eq!(again.get_bool(SettingKey::ShowLineNumbers), Some(true));
    assert_eq!(again.font().display_text(), "Segoe UI Mono 11.5 pt");
    assert_eq!(
        again.encoding_choice(SettingKey::SaveEncoding),
        EncodingChoice::Specific(TextEncoding::ShiftJis)
    );
    assert_eq!(
        again.enabled_encodings(),
        vec![TextEncoding::Utf8, TextEncoding::Gb18030]
    );
    assert_eq!(
        again.key_of(CommandId::Find),
        Some(KeyBinding::new(0x47, true, false, true))
    );
    assert_eq!(again.key_of(CommandId::Save), None);
    assert!(again.is_key_changed(CommandId::Save));
    assert_eq!(again.key_of(CommandId::Open), CommandId::Open.default_key());
}
#[test]
fn an_empty_encodings_list_round_trips_and_stays_empty() {
    let store = fresh("no-encodings");
    store.set_enabled_encodings(&[]).unwrap();
    assert_eq!(store.enabled_encodings(), []);
    store.flush().unwrap();
    let again = SettingsStore::open(store.path());
    assert_eq!(again.load_problem(), None);
    assert_eq!(again.enabled_encodings(), []);
}
#[test]
fn only_changes_are_saved_and_a_default_value_leaves_the_file() {
    let store = fresh("sparse");
    store
        .set_integer(SettingKey::WindowHeightLines, 40)
        .unwrap();
    assert_eq!(on_disk(&store), "WindowHeightLines=40\r\n");
    store
        .set_integer(SettingKey::WindowHeightLines, 30)
        .unwrap();
    assert_eq!(on_disk(&store), "");
}
#[test]
fn missing_and_empty_files_are_the_defaults_with_no_problem() {
    let store = fresh("missing");
    assert_eq!(store.load_problem(), None);
    assert_eq!(
        store.get_integer(SettingKey::WindowWidthCharacters),
        Some(90)
    );
    let empty = write_file("empty", "");
    assert_eq!(empty.load_problem(), None);
    assert_eq!(
        empty.get_integer(SettingKey::WindowWidthCharacters),
        Some(90)
    );
}
#[test]
fn unknown_keys_comments_and_commands_stay_on_save() {
    let store = write_file(
        "unknown",
        "# My note\r\nSomeFutureSetting=hello = world\r\nKeyboardShortcut.FutureCommand=Ctrl+Q\r\nTabWidth=4\r\n",
    );
    assert_eq!(store.load_problem(), None);
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(4));
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let text = on_disk(&store);
    for line in [
        "# My note",
        "SomeFutureSetting=hello = world",
        "KeyboardShortcut.FutureCommand=Ctrl+Q",
        "TabWidth=4",
        "SmartQuotes=true",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} in {text}");
    }
    let parent = store.path();
    let copies = std::fs::read_dir(parent.parent().unwrap()).unwrap().count();
    assert_eq!(copies, 1, "a good file leaves no copy and no temp file");
}
fn broken_copy(store: &SettingsStore) -> std::path::PathBuf {
    store.path().parent().unwrap().join("settings.broken.txt")
}
#[test]
fn broken_file_is_kept_beside_itself_and_told_to_the_caller() {
    let junk = "This is not a settings file\r\nWindowHeightLines=40\r\n";
    let store = write_file("broken", junk);
    let problem = store.load_problem().expect("the caller hears of it");
    assert!(problem.contains("line 1"), "{problem}");
    assert!(problem.contains("have their defaults"), "{problem}");
    assert!(problem.contains("settings.broken.txt"), "{problem}");
    assert_eq!(std::fs::read_to_string(broken_copy(&store)).unwrap(), junk);
    assert_eq!(store.get_integer(SettingKey::WindowHeightLines), Some(40));
    // A later save edits the file in place, so the stray line stays. The copy stays too.
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(on_disk(&store), format!("{junk}SmartQuotes=true\r\n"));
    assert_eq!(std::fs::read_to_string(broken_copy(&store)).unwrap(), junk);
}
#[test]
fn a_second_broken_file_does_not_overwrite_the_first_copy() {
    let store = write_file("broken-twice", "first junk\r\n");
    assert!(broken_copy(&store).exists());
    std::fs::write(store.path(), "second junk\r\n").unwrap();
    let again = SettingsStore::open(store.path());
    assert!(
        again
            .load_problem()
            .unwrap()
            .contains("settings.broken-2.txt")
    );
    assert_eq!(
        std::fs::read_to_string(broken_copy(&store)).unwrap(),
        "first junk\r\n"
    );
}
#[test]
fn not_utf8_and_too_long_are_broken_files() {
    let path = settings_path("not-utf8");
    std::fs::write(&path, [0xff, 0xfe, 0x00, 0x41]).unwrap();
    let store = SettingsStore::open(&path);
    assert!(store.load_problem().unwrap().contains("not UTF-8"));
    assert!(broken_copy(&store).exists());
}
/// The review: a 3 MB file got a 1 MB "copy", and the next save lost the other 2 MB.
#[test]
fn a_file_over_the_limit_is_not_read_and_never_saved_over() {
    let path = settings_path("too-long");
    let bytes: Vec<u8> = (0..3_145_728u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let store = SettingsStore::open(&path);
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("over 1 MB"), "{problem}");
    assert!(problem.contains("will not be saved"), "{problem}");
    assert!(problem.contains(&path.display().to_string()), "{problem}");
    assert!(!problem.contains("A copy"), "no copy, so no word of one");
    assert!(!broken_copy(&store).exists());

    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert!(store.write_error().unwrap().contains("not saved"));
    assert!(store.flush().is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "every byte is still there"
    );
    let names = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
    assert_eq!(names, 1, "no temp file and no copy");
    // Once the user moves it, settings save again.
    std::fs::rename(&path, path.with_extension("moved")).unwrap();
    let again = SettingsStore::open(&path);
    assert_eq!(again.load_problem(), None);
    again.set_bool(SettingKey::SmartQuotes, true).unwrap();
    again.flush().unwrap();
}
fn copies_in(path: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "settings.txt")
        .collect();
    names.sort();
    names
}
/// The review: 1,001 launches with no change left 1,000 files, then saving stopped.
#[test]
fn launching_a_thousand_times_with_a_broken_file_adds_no_file() {
    let path = settings_path("repeat-launch");
    std::fs::write(&path, "TabWidth=abc\r\n").unwrap();
    let (mut first, mut slowest, mut problem) = (None, std::time::Duration::ZERO, None);
    let mut times = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let started = std::time::Instant::now();
        let store = SettingsStore::open(&path);
        let took = started.elapsed();
        first.get_or_insert(took);
        slowest = slowest.max(took);
        times.push(took);
        problem = store.load_problem();
    }
    times.sort();
    println!(
        "broken file, 1000 launches: first {:?}, median {:?}, slowest {slowest:?}",
        first.unwrap(),
        times[500]
    );
    assert_eq!(copies_in(&path), ["settings.broken.txt"]);
    assert!(problem.unwrap().contains("settings.broken.txt"));
    let store = SettingsStore::open(&path);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
}
#[test]
fn copies_stop_at_a_small_limit_and_saving_never_stops() {
    let path = settings_path("copy-limit");
    let folder = path.parent().unwrap().to_owned();
    // Files that look close to a copy, and are not this code's.
    let not_ours = [
        "settings.broken-old.txt",
        "settings.broken-1x.txt",
        "settings.broken-.txt",
        "settings.broken-2.txt.bak",
        "mysettings.broken-2.txt",
    ];
    for name in not_ours {
        std::fs::write(folder.join(name), "mine").unwrap();
    }
    let mut last = String::new();
    for round in 0..1100 {
        last = format!("junk {round}\r\n");
        std::fs::write(&path, &last).unwrap();
        let store = SettingsStore::open(&path);
        assert!(
            store.load_problem().unwrap().contains("A copy of the file"),
            "round {round}"
        );
    }
    let names = copies_in(&path);
    for name in not_ours {
        assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
    }
    assert_eq!(names.len(), not_ours.len() + 5, "{names:?}");
    // The newest copy has the newest text, and a save still works.
    let store = SettingsStore::open(&path);
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("settings.broken-1100.txt"), "{problem}");
    assert_eq!(
        std::fs::read_to_string(folder.join("settings.broken-1100.txt")).unwrap(),
        last
    );
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(on_disk(&store), format!("{last}SmartQuotes=true\r\n"));
}
#[test]
fn a_very_long_bad_value_gives_a_short_message_that_still_names_the_copy() {
    let text = format!("TabWidth={}\r\n", "9x".repeat(400_000));
    let store = write_file("long-value", &text);
    let problem = store.load_problem().unwrap();
    assert!(problem.chars().count() < 1000, "{} chars", problem.len());
    assert!(problem.contains("settings.broken.txt"), "{problem}");
    assert_eq!(std::fs::read_to_string(broken_copy(&store)).unwrap(), text);
}
#[test]
fn a_huge_function_key_number_is_not_a_key_and_does_not_panic() {
    let store = write_file("f65535", "KeyboardShortcut.Find=Ctrl+F65535\r\n");
    assert!(store.load_problem().unwrap().contains("is not a key"));
    assert_eq!(qe_settings::parse_key("Ctrl+F65535"), None);
    assert_eq!(qe_settings::parse_key("F0"), None);
    assert_eq!(qe_settings::parse_key("F25"), None);
    assert_eq!(
        qe_settings::parse_key("F24"),
        Some(KeyBinding::new(0x87, false, false, false))
    );
}
#[test]
fn a_key_for_a_command_that_opens_a_submenu_is_refused_out_loud() {
    let store = fresh("submenu-key");
    let refused = store
        .set_key(
            CommandId::OpenRecent,
            KeyBinding::new(0x47, true, false, false),
        )
        .unwrap_err();
    assert!(
        refused.message().contains("submenu"),
        "{}",
        refused.message()
    );
    assert_eq!(store.key_of(CommandId::OpenRecent), None);
}
#[test]
fn restoring_keyboard_defaults_also_drops_an_indented_unknown_shortcut() {
    let store = write_file(
        "indented",
        "  KeyboardShortcut.Future=Ctrl+Q\r\n\tKeyboardShortcut.Other=Ctrl+R\r\nKeepMe=1\r\n",
    );
    store.restore_keyboard_defaults();
    let text = on_disk(&store);
    assert!(!text.contains("KeyboardShortcut."), "{text}");
    assert!(text.contains("KeepMe=1"), "{text}");
}
#[test]
fn one_bad_value_defaults_that_setting_only_and_keeps_a_copy() {
    let text = "WindowWidthCharacters=abc\r\nWindowHeightLines=99999\r\nSmartQuotes=maybe\r\nTabWidth=4\r\nKeyboardShortcut.Find=Ctrl+Nonsense\r\nKeyboardShortcut.Open=Alt+F4\r\n";
    let store = write_file("bad-values", text);
    let problem = store.load_problem().unwrap();
    for part in [
        "\"abc\" is not a whole number",
        "WindowHeightLines must be a whole number from 1 to 10000",
        "\"maybe\" is not true or false",
        "\"Ctrl+Nonsense\" is not a key",
        "Alt+F4 is kept by Windows",
    ] {
        assert!(problem.contains(part), "{part} in {problem}");
    }
    assert_eq!(
        store.get_integer(SettingKey::WindowWidthCharacters),
        Some(90)
    );
    assert_eq!(store.get_integer(SettingKey::WindowHeightLines), Some(30));
    assert_eq!(store.get_bool(SettingKey::SmartQuotes), Some(false));
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(4));
    assert_eq!(store.key_of(CommandId::Find), CommandId::Find.default_key());
    assert_eq!(store.key_of(CommandId::Open), CommandId::Open.default_key());
    assert_eq!(std::fs::read_to_string(broken_copy(&store)).unwrap(), text);
}
#[test]
fn a_file_naming_one_key_twice_keeps_only_the_first() {
    let store = write_file(
        "same-key",
        "KeyboardShortcut.Open=Ctrl+G\r\nKeyboardShortcut.New=Ctrl+G\r\n",
    );
    assert!(store.load_problem().unwrap().contains("both name Ctrl+G"));
    assert_eq!(
        store.command_for_key(KeyBinding::new(0x47, true, false, false)),
        Some(CommandId::Open)
    );
    assert_eq!(store.key_of(CommandId::New), CommandId::New.default_key());
}
fn set_read_only(path: &std::path::Path, on: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(on);
    std::fs::set_permissions(path, permissions).unwrap();
}
#[test]
fn read_only_file_reads_and_a_save_fails_with_a_message() {
    let path = settings_path("read-only");
    std::fs::write(&path, "TabWidth=4\r\n").unwrap();
    set_read_only(&path, true);
    let store = SettingsStore::open(&path);
    assert_eq!(store.load_problem(), None);
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(4));

    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    store.subscribe(move |change| log.borrow_mut().push(change.clone()));
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let error = store.flush().unwrap_err();
    assert!(error.contains("read-only"), "{error}");
    assert_eq!(store.write_error().as_deref(), Some(error.as_str()));
    assert!(
        seen.borrow()
            .iter()
            .any(|change| matches!(change, SettingChange::WriteFailed(text) if *text == error))
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=4\r\n");
    // The change holds in memory, and the file is not touched.
    assert_eq!(store.get_bool(SettingKey::SmartQuotes), Some(true));

    // Once the file can be written, the next change saves everything, and the error goes.
    set_read_only(&path, false);
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(store.write_error(), None);
    assert!(seen.borrow().contains(&SettingChange::WriteRecovered));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("SmartQuotes=true") && text.contains("SmartDashes=true"));
}
#[test]
fn a_write_to_an_impossible_folder_fails_with_a_message_and_no_panic() {
    let folder = test_folder("no-folder");
    let in_the_way = folder.join("file");
    std::fs::write(&in_the_way, "x").unwrap();
    let store = SettingsStore::open(in_the_way.join("settings.txt"));
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let error = store.flush().unwrap_err();
    assert!(error.starts_with("Settings were not saved:"), "{error}");
}
#[test]
fn a_failed_write_leaves_the_old_file_whole_and_no_temp_file() {
    let path = settings_path("no-temp");
    let store = SettingsStore::open(&path);
    store.set_integer(SettingKey::TabWidth, 2).unwrap();
    store.flush().unwrap();
    set_read_only(&path, true);
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    assert!(store.flush().is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=2\r\n");
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["settings.txt"]);
    set_read_only(&path, false);
}
#[test]
fn a_file_that_cannot_be_read_is_never_saved_over() {
    // A folder in the file's place: opening it fails, and it is not "missing".
    let path = settings_path("unreadable");
    std::fs::create_dir(&path).unwrap();
    let store = SettingsStore::open(&path);
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("could not be read"), "{problem}");
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let error = store.write_error().expect("a block is an error at once");
    assert!(error.contains("not saved"), "{error}");
    assert!(path.is_dir());
}
#[test]
fn a_burst_of_changes_ends_with_the_last_value_on_disk() {
    let store = fresh("burst");
    for width in 1..=300 {
        store
            .set_integer(SettingKey::WindowWidthCharacters, width)
            .unwrap();
    }
    assert_eq!(on_disk(&store), "WindowWidthCharacters=300\r\n");
}
#[test]
fn changes_send_one_event_and_a_same_value_sends_none() {
    let store = fresh("events");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let id = store.subscribe(move |change| log.borrow_mut().push(change.clone()));
    store.set_bool(SettingKey::ShowLineNumbers, true).unwrap();
    store.set_bool(SettingKey::ShowLineNumbers, true).unwrap();
    assert_eq!(
        *seen.borrow(),
        [SettingChange::Value(SettingKey::ShowLineNumbers)]
    );
    store.clear_key(CommandId::New);
    assert_eq!(seen.borrow().last(), Some(&SettingChange::Keys));
    store.unsubscribe(id);
    store.set_bool(SettingKey::ShowLineNumbers, false).unwrap();
    assert_eq!(seen.borrow().len(), 2);
}
#[test]
fn a_listener_may_read_and_change_the_store() {
    let store = fresh("listener");
    let inner = store.clone();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    store.subscribe(move |change| {
        log.borrow_mut().push(change.clone());
        if *change == SettingChange::Value(SettingKey::SmartQuotes) {
            inner.set_bool(SettingKey::SmartDashes, true).unwrap();
        }
    });
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(
        *seen.borrow(),
        [
            SettingChange::Value(SettingKey::SmartQuotes),
            SettingChange::Value(SettingKey::SmartDashes)
        ]
    );
}
#[test]
fn restore_all_is_the_mac_rule() {
    let store = write_file("restore", "Future=1\r\nKeyboardShortcut.Future=Ctrl+Q\r\n");
    store
        .set_integer(SettingKey::WindowWidthCharacters, 5)
        .unwrap();
    store
        .set_font(&FontChoice {
            family: "Consolas".into(),
            points: 12.0,
        })
        .unwrap();
    store.set_enabled_encodings(&[TextEncoding::Utf8]).unwrap();
    store.clear_key(CommandId::New);
    store.restore_all();
    assert_eq!(
        store.get_integer(SettingKey::WindowWidthCharacters),
        Some(90)
    );
    assert_eq!(store.font().family, "Cascadia Mono");
    assert_eq!(store.key_of(CommandId::New), CommandId::New.default_key());
    // The encodings list stays, as in TextEdit.
    assert_eq!(store.enabled_encodings(), [TextEncoding::Utf8]);
    let text = on_disk(&store);
    assert!(text.contains("Future=1"), "{text}");
    assert!(!text.contains("KeyboardShortcut."), "{text}");
    assert!(text.contains("EnabledEncodings=UTF-8"), "{text}");
}
#[test]
fn stored_values_compare_by_type() {
    let store = fresh("typed");
    assert_eq!(store.get(SettingKey::TabWidth), StoredValue::Integer(8));
    assert_eq!(store.get_bool(SettingKey::TabWidth), None);
    assert_eq!(store.get_integer(SettingKey::SmartQuotes), None);
    assert_eq!(store.get_text(SettingKey::SmartQuotes), None);
}

// The second review.

fn names_beside(path: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
fn good_store(name: &str) -> (SettingsStore, std::path::PathBuf) {
    let path = settings_path(name);
    std::fs::write(&path, "TabWidth=4\r\n").unwrap();
    (SettingsStore::open(&path), path)
}
fn events_of(store: &SettingsStore) -> Rc<RefCell<Vec<SettingChange>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    store.subscribe(move |change| log.borrow_mut().push(change.clone()));
    seen
}

// Finding 1: a save looks at the disk first.
#[test]
fn a_file_that_turned_broken_after_open_gets_a_copy_and_keeps_its_lines() {
    let (store, path) = good_store("broken-later");
    let hand = "no equals sign here\r\nTabWidth=5\r\n";
    std::fs::write(&path, hand).unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(broken_copy(&store)).unwrap(),
        hand,
        "a true copy exists"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "no equals sign here\r\nTabWidth=5\r\nSmartQuotes=true\r\n"
    );
    // Memory follows what the person wrote, and the store says what it found.
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(5));
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("line 1"), "{problem}");
    assert!(problem.contains("settings.broken.txt"), "{problem}");
}
#[test]
fn a_file_that_stopped_being_text_after_open_is_copied_then_replaced() {
    let (store, path) = good_store("not-text-later");
    let junk = [0xff_u8, 0xfe, 0x00, 0x41];
    std::fs::write(&path, junk).unwrap();
    let seen = events_of(&store);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read(broken_copy(&store)).unwrap(), junk);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\nTabWidth=4\r\n",
        "from memory, in the order of the table"
    );
    assert!(store.load_problem().unwrap().contains("not UTF-8"));
    assert!(
        seen.borrow()
            .contains(&SettingChange::FileProblem(store.load_problem()))
    );
}
#[test]
fn a_file_over_the_limit_after_open_is_not_saved_over() {
    let path = settings_path("long-later");
    let store = SettingsStore::open(&path);
    let bytes = vec![b'#'; 3_145_728];
    std::fs::write(&path, &bytes).unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let error = store.flush().unwrap_err();
    assert!(error.contains("over 1 MB"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(std::fs::read(&path).unwrap() == bytes, "every byte stays");
    assert_eq!(names_beside(&path), ["settings.txt"]);
}
#[test]
fn a_hand_edit_and_another_stores_keys_survive_a_save() {
    let (first, path) = good_store("merge");
    let second = SettingsStore::open(&path);
    first.set_bool(SettingKey::SmartQuotes, true).unwrap();
    first.flush().unwrap();
    second.set_bool(SettingKey::SmartDashes, true).unwrap();
    second.flush().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "TabWidth=4\r\nSmartQuotes=true\r\nSmartDashes=true\r\n"
    );
    // The second store's memory follows the first store's key.
    assert_eq!(second.get_bool(SettingKey::SmartQuotes), Some(true));

    let hand = format!("# mine\r\n{text}Future=1\r\n");
    std::fs::write(&path, &hand).unwrap();
    first
        .set_integer(SettingKey::WindowHeightLines, 40)
        .unwrap();
    first.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{hand}WindowHeightLines=40\r\n")
    );
    assert_eq!(first.load_problem(), None);
}
/// A saved change is done: a later hand edit of the same key is the newer word.
#[test]
fn a_hand_edit_of_a_key_saved_earlier_stays_and_memory_follows_it() {
    let (store, path) = good_store("hand-same-key");
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    store.flush().unwrap();
    std::fs::write(&path, "TabWidth=6\r\n").unwrap();
    let seen = events_of(&store);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "TabWidth=6\r\nSmartQuotes=true\r\n"
    );
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(6));
    assert!(
        seen.borrow()
            .contains(&SettingChange::Value(SettingKey::TabWidth))
    );
}
/// A change that never reached the disk is newer than what the file holds.
#[test]
fn an_unsaved_change_wins_over_a_hand_edit_of_the_same_key() {
    let (store, path) = good_store("unsaved-wins");
    set_read_only(&path, true);
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    assert!(store.flush().is_err());
    set_read_only(&path, false);
    std::fs::write(&path, "# mine\r\nTabWidth=6\r\nFuture=1\r\n").unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# mine\r\nTabWidth=5\r\nFuture=1\r\n"
    );
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(5));
}
#[test]
fn restoring_keys_after_a_hand_edit_also_drops_the_shortcuts_it_added() {
    let (store, path) = good_store("restore-after-hand");
    store.clear_key(CommandId::Find);
    store.flush().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}KeyboardShortcut.Future=Ctrl+Q\r\n")).unwrap();
    store.restore_keyboard_defaults();
    store
        .set_key(CommandId::Save, KeyBinding::new(0x47, true, false, true))
        .unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "TabWidth=4\r\nKeyboardShortcut.Save=Ctrl+Shift+G\r\n"
    );
}

// Finding 2: a failed save stays unsaved.
#[test]
fn flush_tries_a_failed_save_again() {
    let (store, path) = good_store("flush-retry");
    set_read_only(&path, true);
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    let first = store.flush().unwrap_err();
    assert!(first.contains("read-only"), "{first}");
    assert!(store.flush().is_err(), "still unsaved, so still an error");
    set_read_only(&path, false);
    store.flush().unwrap();
    assert_eq!(store.write_error(), None);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=3\r\n");
}
#[test]
fn setting_the_same_value_tries_an_unsaved_change_again() {
    let (store, path) = good_store("same-value-retry");
    set_read_only(&path, true);
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    assert!(store.flush().is_err());
    set_read_only(&path, false);
    let seen = events_of(&store);
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    // The earlier `flush` means this save is waited for, so the set itself did this.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=3\r\n");
    store.flush().unwrap();
    assert_eq!(
        *seen.borrow(),
        [SettingChange::WriteRecovered],
        "the value did not change, so no value event"
    );
}
#[test]
fn setting_the_same_key_tries_an_unsaved_change_again() {
    let (store, path) = good_store("same-key-retry");
    set_read_only(&path, true);
    store.clear_key(CommandId::Find);
    assert!(store.flush().is_err());
    set_read_only(&path, false);
    store.clear_key(CommandId::Find);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "TabWidth=4\r\nKeyboardShortcut.Find=none\r\n"
    );
}
#[test]
fn a_saved_value_set_again_writes_nothing() {
    let (store, path) = good_store("same-value-saved");
    store.flush().unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    store.set_integer(SettingKey::TabWidth, 4).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
}

// Finding 3: a block is looked at again at each save.
#[test]
fn a_block_clears_once_the_long_file_is_moved() {
    let path = settings_path("block-moved");
    let bytes = vec![b'#'; 2_000_000];
    std::fs::write(&path, &bytes).unwrap();
    let store = SettingsStore::open(&path);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert!(store.flush().is_err());
    std::fs::rename(&path, path.with_extension("moved")).unwrap();
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(store.write_error(), None);
    assert_eq!(
        store.load_problem(),
        None,
        "the message about the file is over"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\nSmartDashes=true\r\n"
    );
    assert!(std::fs::read(path.with_extension("moved")).unwrap() == bytes);
}
#[test]
fn a_block_clears_if_the_long_file_shrinks_and_its_lines_are_read() {
    let path = settings_path("block-shrunk");
    std::fs::write(&path, vec![b'#'; 2_000_000]).unwrap();
    let store = SettingsStore::open(&path);
    std::fs::write(&path, "TabWidth=4\r\n").unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "TabWidth=4\r\nSmartQuotes=true\r\n"
    );
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(4));
    assert_eq!(store.load_problem(), None);
}

// Finding 4: the copy is checked, and it counts only after the swap.
#[test]
fn a_failed_swap_and_then_a_lost_copy_still_keeps_the_broken_text() {
    let junk = [0xff_u8, 0x41];
    let path = settings_path("swap-then-lost-copy");
    std::fs::write(&path, junk).unwrap();
    let store = SettingsStore::open(&path);
    set_read_only(&path, true);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert!(store.flush().is_err(), "the swap failed");
    set_read_only(&path, false);
    std::fs::remove_file(broken_copy(&store)).unwrap();
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read(broken_copy(&store)).unwrap(),
        junk,
        "the broken text exists somewhere"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\nSmartDashes=true\r\n"
    );
}
#[test]
fn a_copy_with_changed_content_gets_a_true_copy_before_the_save() {
    let junk = [0xff_u8, 0xfe, 0x41];
    let path = settings_path("emptied-copy");
    std::fs::write(&path, junk).unwrap();
    let store = SettingsStore::open(&path);
    std::fs::write(broken_copy(&store), "").unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\n"
    );
    assert_eq!(
        std::fs::read(path.with_file_name("settings.broken-2.txt")).unwrap(),
        junk
    );
}
/// This replaces `a_copy_that_went_away_stops_the_save_over_the_original`. The original
/// still holds the bytes, so the save makes the copy again instead of stopping.
#[test]
fn a_copy_that_went_away_is_made_again_before_the_save() {
    let junk = [0xff_u8, 0x41];
    let path = settings_path("copy-gone");
    std::fs::write(&path, junk).unwrap();
    let store = SettingsStore::open(&path);
    std::fs::remove_file(broken_copy(&store)).unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read(broken_copy(&store)).unwrap(), junk);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\n"
    );
}
/// Denies making files in the folder for as long as it lives.
struct NoNewFiles(std::path::PathBuf);
impl NoNewFiles {
    fn new(folder: &std::path::Path) -> Self {
        Self::icacls(folder, &["/deny", "*S-1-1-0:(WD)"]);
        Self(folder.to_owned())
    }
    fn icacls(folder: &std::path::Path, what: &[&str]) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let done = std::process::Command::new("icacls")
            .arg(folder)
            .args(what)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert!(done.status.success(), "{done:?}");
    }
}
impl Drop for NoNewFiles {
    fn drop(&mut self) {
        Self::icacls(&self.0, &["/remove:d", "*S-1-1-0"]);
    }
}
#[test]
fn no_way_to_make_a_copy_blocks_the_save_until_there_is_one() {
    let junk = [0xff_u8, 0x41];
    let path = settings_path("no-copy-possible");
    let folder = path.parent().unwrap().to_owned();
    std::fs::write(&path, junk).unwrap();
    let denied = NoNewFiles::new(&folder);
    let store = SettingsStore::open(&path);
    assert!(
        store
            .load_problem()
            .unwrap()
            .contains("No copy could be kept")
    );
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let error = store.flush().unwrap_err();
    assert!(error.contains("not saved"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), junk);
    drop(denied);
    store.flush().unwrap();
    assert_eq!(std::fs::read(broken_copy(&store)).unwrap(), junk);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\n"
    );
}

// Finding 5: nothing escapes `flush`.
#[test]
fn a_save_after_flush_is_on_disk_when_the_call_returns() {
    let (store, path) = good_store("after-flush");
    store.flush().unwrap();
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=5\r\n");
}
#[test]
fn a_failed_save_after_flush_is_an_error_at_once() {
    let (store, path) = good_store("after-flush-failed");
    store.flush().unwrap();
    set_read_only(&path, true);
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    assert!(store.write_error().unwrap().contains("read-only"));
    assert!(store.flush().is_err());
    set_read_only(&path, false);
}
#[test]
fn flush_waits_for_a_save_its_own_listener_causes() {
    let (store, path) = good_store("listener-in-flush");
    set_read_only(&path, true);
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    let (inner, target) = (store.clone(), path.clone());
    store.subscribe(move |change| {
        if matches!(change, SettingChange::WriteFailed(_)) {
            set_read_only(&target, false);
            inner.set_bool(SettingKey::SmartDashes, true).unwrap();
        }
    });
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "TabWidth=3\r\nSmartDashes=true\r\n"
    );
}
#[test]
fn a_listener_that_never_stops_saving_ends_flush_with_an_error() {
    let (store, path) = good_store("endless-listener");
    set_read_only(&path, true);
    let inner = store.clone();
    // Never 4, which the file holds: that would need no write, and so could not fail.
    let next = std::cell::Cell::new(10);
    store.subscribe(move |change| {
        if matches!(change, SettingChange::WriteFailed(_)) {
            next.set(next.get() % 50 + 10);
            let _ = inner.set_integer(SettingKey::TabWidth, next.get());
        }
    });
    store.set_integer(SettingKey::TabWidth, 3).unwrap();
    assert!(store.flush().is_err());
    set_read_only(&path, false);
}
#[test]
fn dropping_the_last_store_waits_for_its_save() {
    let (store, path) = good_store("drop-waits");
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    drop(store);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=5\r\n");
}

// Finding 6: the app never writes a file it would refuse.
const LIMIT: usize = 1024 * 1024;
fn comment_bytes(length: usize, line_end: &str) -> String {
    let mut text = String::with_capacity(length);
    let line = format!("# {}{line_end}", "x".repeat(60));
    while length - text.len() >= 2 * line.len() {
        text.push_str(&line);
    }
    let rest = length - text.len();
    text.push_str(&format!(
        "#{}{line_end}",
        "y".repeat(rest - 1 - line_end.len())
    ));
    assert_eq!(text.len(), length);
    text
}
/// The review's case: LF lines did not grow a 921,601-byte file past the limit once saves
/// keep each line's own end.
#[test]
fn a_saved_file_stays_under_the_limit_and_opens_clean() {
    let path = settings_path("lf-file");
    let text = comment_bytes(921_601, "\n");
    std::fs::write(&path, &text).unwrap();
    let store = SettingsStore::open(&path);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved == format!("{text}SmartQuotes=true\r\n"));
    let again = SettingsStore::open(&path);
    assert_eq!(again.load_problem(), None);
    assert_eq!(again.get_bool(SettingKey::SmartQuotes), Some(true));
}
#[test]
fn a_save_that_would_pass_the_limit_is_refused_and_the_old_file_stays() {
    // `SmartQuotes=true` and its CRLF are 18 bytes, so the CRLF's last byte is one too many.
    let path = settings_path("past-limit");
    let text = comment_bytes(LIMIT - 17, "\r\n");
    std::fs::write(&path, &text).unwrap();
    let store = SettingsStore::open(&path);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    let error = store.flush().unwrap_err();
    assert!(error.contains("over 1 MB"), "{error}");
    assert!(std::fs::read_to_string(&path).unwrap() == text, "unchanged");
    assert_eq!(store.get_bool(SettingKey::SmartQuotes), Some(true));
    assert_eq!(names_beside(&path), ["settings.txt"]);
}
#[test]
fn a_save_that_lands_exactly_on_the_limit_works() {
    let path = settings_path("on-limit");
    let text = comment_bytes(LIMIT - 18, "\r\n");
    std::fs::write(&path, &text).unwrap();
    let store = SettingsStore::open(&path);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), LIMIT as u64);
    assert_eq!(SettingsStore::open(&path).load_problem(), None);
}
#[test]
fn the_read_limit_counts_to_the_exact_byte() {
    let path = settings_path("read-limit");
    std::fs::write(&path, comment_bytes(LIMIT, "\r\n")).unwrap();
    assert_eq!(SettingsStore::open(&path).load_problem(), None);
    std::fs::write(&path, comment_bytes(LIMIT + 1, "\r\n")).unwrap();
    assert!(
        SettingsStore::open(&path)
            .load_problem()
            .unwrap()
            .contains("over 1 MB")
    );
}

// Findings 8 and 9: a save edits the file in place.
#[test]
fn a_stray_line_stays_with_everything_around_it() {
    let text = "note to self\r\nTabWidth=4\r\nFuture=1\r\n# comment\r\n";
    let store = write_file("stray", text);
    // The other lines still count, so a line that stays never costs them.
    assert_eq!(store.get_integer(SettingKey::TabWidth), Some(4));
    assert!(store.load_problem().unwrap().contains("line 1"));
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(on_disk(&store), format!("{text}SmartQuotes=true\r\n"));
    assert_eq!(std::fs::read_to_string(broken_copy(&store)).unwrap(), text);
    // Later saves make no more copies of a file this store wrote.
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    on_disk(&store);
    assert_eq!(copies_in(&store.path()), ["settings.broken.txt"]);
}
#[test]
fn a_setting_changes_its_own_line_and_comments_stay_where_they_are() {
    let store = write_file(
        "in-place",
        "# width for my laptop\r\nWindowWidthCharacters=120\r\n\r\n  # tabs\r\nTabWidth=4\r\nFuture = 1\r\n",
    );
    store
        .set_integer(SettingKey::WindowWidthCharacters, 100)
        .unwrap();
    assert_eq!(
        on_disk(&store),
        "# width for my laptop\r\nWindowWidthCharacters=100\r\n\r\n  # tabs\r\nTabWidth=4\r\nFuture = 1\r\n"
    );
    // Back to its default: the line goes, and its comment stays where it was.
    store.set_integer(SettingKey::TabWidth, 8).unwrap();
    assert_eq!(
        on_disk(&store),
        "# width for my laptop\r\nWindowWidthCharacters=100\r\n\r\n  # tabs\r\nFuture = 1\r\n"
    );
}
#[test]
fn a_line_that_holds_a_default_or_odd_spacing_stays_until_it_changes() {
    let text = "TabWidth=8\r\n  SmartQuotes = true  \r\n";
    let store = write_file("spacing", text);
    store.set_bool(SettingKey::SmartDashes, true).unwrap();
    assert_eq!(on_disk(&store), format!("{text}SmartDashes=true\r\n"));
    store.set_bool(SettingKey::SmartQuotes, false).unwrap();
    assert_eq!(on_disk(&store), "TabWidth=8\r\nSmartDashes=true\r\n");
}
#[test]
fn line_ends_a_byte_order_mark_and_a_missing_last_end_are_kept() {
    let store = write_file("ends", "\u{feff}# a\nTabWidth=4\r\n# no end");
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(
        on_disk(&store),
        "\u{feff}# a\nTabWidth=5\r\n# no end\r\nSmartQuotes=true\r\n"
    );
}
#[test]
fn a_change_to_a_key_set_twice_leaves_one_line() {
    let store = write_file("twice", "TabWidth=4\r\n# between\r\nTabWidth=5\r\n");
    assert!(store.load_problem().unwrap().contains("set twice"));
    store.set_integer(SettingKey::TabWidth, 6).unwrap();
    assert_eq!(on_disk(&store), "TabWidth=6\r\n# between\r\n");
}
#[test]
fn a_bad_value_stays_on_its_line_until_that_setting_changes() {
    let text = "TabWidth=abc\r\nWindowHeightLines=40\r\n";
    let store = write_file("bad-stays", text);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(on_disk(&store), format!("{text}SmartQuotes=true\r\n"));
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    assert_eq!(
        on_disk(&store),
        "TabWidth=5\r\nWindowHeightLines=40\r\nSmartQuotes=true\r\n"
    );
}
#[test]
fn a_shortcut_changes_its_own_line_and_clearing_writes_none() {
    let store = write_file("key-line", "KeyboardShortcut.Find=Ctrl+G\r\n# keep\r\n");
    store.clear_key(CommandId::Find);
    assert_eq!(on_disk(&store), "KeyboardShortcut.Find=none\r\n# keep\r\n");
    store.reset_key(CommandId::Find).unwrap();
    assert_eq!(on_disk(&store), "# keep\r\n");
}

// Finding 12: a healthy copy does not stop a save.
#[test]
fn a_read_only_copy_gets_a_new_flushed_copy_and_the_save_goes_on() {
    // The save must flush the copy, and a read-only file cannot be flushed. So this also
    // fails if the flush is taken out: the read-only copy would then count.
    let junk = [0xff_u8, 0x41];
    let path = settings_path("read-only-copy");
    std::fs::write(&path, junk).unwrap();
    let store = SettingsStore::open(&path);
    set_read_only(&broken_copy(&store), true);
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    store.flush().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "SmartQuotes=true\r\n"
    );
    assert_eq!(
        std::fs::read(path.with_file_name("settings.broken-2.txt")).unwrap(),
        junk
    );
    set_read_only(&broken_copy(&store), false);
}
#[test]
fn a_file_fixed_by_hand_needs_no_copy_at_all() {
    let store = write_file("fixed-by-hand", "junk\r\n");
    std::fs::remove_file(broken_copy(&store)).unwrap();
    std::fs::write(store.path(), "TabWidth=4\r\n").unwrap();
    store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert_eq!(on_disk(&store), "TabWidth=4\r\nSmartQuotes=true\r\n");
    assert!(!broken_copy(&store).exists(), "a good file needs no copy");
    assert_eq!(store.load_problem(), None);
}

// Finding 13: any free name will do.
#[test]
fn the_highest_copy_number_does_not_stop_a_copy() {
    let path = settings_path("top-number");
    let top = path.with_file_name("settings.broken-4294967295.txt");
    std::fs::write(&top, "old").unwrap();
    std::fs::write(&path, "junk\r\n").unwrap();
    let store = SettingsStore::open(&path);
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("A copy of the file"), "{problem}");
    assert_eq!(std::fs::read_to_string(&top).unwrap(), "old");
}
#[test]
fn a_folder_with_the_name_of_the_copy_does_not_stop_a_copy() {
    let path = settings_path("folder-name");
    std::fs::create_dir(path.with_file_name("settings.broken.txt")).unwrap();
    std::fs::write(&path, "junk\r\n").unwrap();
    let store = SettingsStore::open(&path);
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("settings.broken-2.txt"), "{problem}");
}

// Finding 14: a long path saves.
#[test]
fn a_path_longer_than_the_old_limit_saves() {
    let mut folder = test_folder("long-path");
    while folder.as_os_str().len() < 300 {
        folder.push("a-long-folder-name-for-the-path-limit");
    }
    let path = folder.join("settings.txt");
    let store = SettingsStore::open(&path);
    store.set_integer(SettingKey::TabWidth, 5).unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "TabWidth=5\r\n");
    assert_eq!(names_beside(&path), ["settings.txt"]);
}
