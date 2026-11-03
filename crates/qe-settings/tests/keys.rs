//! The key table's rules, with no window: each comes from `Shortcuts.swift`, or from the
//! Windows changes in the parity rows.
mod support;

use qe_api::{CommandId, KeyBinding, Modifiers};
use qe_settings::*;
use qe_ui::{key_text, menu_bar_groups_with_keys};
use support::*;

fn key(virtual_key: u16, modifiers: Modifiers) -> KeyBinding {
    KeyBinding::new(virtual_key, modifiers.ctrl, modifiers.alt, modifiers.shift)
}
fn fresh(name: &str) -> SettingsStore {
    SettingsStore::open(settings_path(name))
}

#[test]
fn every_command_has_one_row_or_opens_a_submenu() {
    let rows = keyboard_rows();
    for command in CommandId::ALL {
        let count = rows
            .iter()
            .filter(|row| matches!(row, KeyRow::Command { command: held, .. } if held == command))
            .count();
        let want = usize::from(!opens_submenu(*command));
        assert_eq!(count, want, "{command:?}");
    }
}
#[test]
fn rows_follow_the_table_with_headers() {
    let rows = keyboard_rows();
    assert!(matches!(rows[0], KeyRow::Menu(_)));
    assert!(rows.contains(&KeyRow::Menu("File".into())));
    assert!(rows.contains(&KeyRow::Submenu("Find".into())));
    // The design's change: Substitutions rows are in the table.
    assert!(rows.contains(&KeyRow::Submenu("Substitutions".into())));
    assert!(rows.contains(&KeyRow::Command {
        command: CommandId::SmartCopyPaste,
        name: "Smart Copy/Paste".into(),
        indented: true
    }));
    // A command alone in its submenu names the submenu in its own row.
    assert!(rows.contains(&KeyRow::Command {
        command: CommandId::ClearRecent,
        name: "Open Recent \u{203a} Clear Menu".into(),
        indented: false
    }));
    assert!(!rows.iter().any(|row| match row {
        KeyRow::Command { name, .. } => name.contains("Text Replacement"),
        _ => false,
    }));
}
#[test]
fn key_of_fits_the_menu_builder_and_shows_a_change() {
    let store = fresh("menus");
    let new_key = key(0x47, CTRL_SHIFT);
    store.set_key(CommandId::Find, new_key).unwrap();
    let groups = menu_bar_groups_with_keys(&|command| store.key_of(command));
    let debug = format!("{groups:?}");
    assert!(debug.contains("Ctrl+Shift+G"), "{debug}");
    assert_eq!(store.command_for_key(new_key), Some(CommandId::Find));
    assert_eq!(
        store.command_for_press(0x47, CTRL_SHIFT),
        Some(CommandId::Find)
    );
    assert_eq!(
        store.command_for_press(0x46, CTRL),
        None,
        "the old key is free"
    );
}
#[test]
fn lookup_finds_each_default_and_ignores_a_windows_key() {
    let store = fresh("lookup");
    for command in CommandId::ALL {
        if let Some(default) = command.default_key() {
            assert_eq!(
                store.command_for_key(default),
                Some(*command),
                "{command:?}"
            );
        }
    }
    assert_eq!(store.command_for_press(0x53, CTRL), Some(CommandId::Save));
    let mut win_ctrl = CTRL;
    win_ctrl.win = true;
    assert_eq!(store.command_for_press(0x53, win_ctrl), None);
}
#[test]
fn windows_defaults_of_w08_and_i62() {
    let store = fresh("windows-defaults");
    let checks = [
        (CommandId::FindReplace, key(0x48, CTRL)),
        (CommandId::FindNext, key(0x72, NONE)),
        (CommandId::FindPrevious, key(0x72, SHIFT)),
        (CommandId::Complete, key(0x20, CTRL)),
        (CommandId::FullScreen, key(0x7a, NONE)),
        (CommandId::NextTab, key(0x09, CTRL)),
        (CommandId::PreviousTab, key(0x09, CTRL_SHIFT)),
        (CommandId::Tab1, key(0x31, CTRL)),
        (CommandId::Tab9, key(0x39, CTRL)),
        (CommandId::LastTab, key(0x30, CTRL)),
    ];
    for (command, want) in checks {
        assert_eq!(store.key_of(command), Some(want), "{command:?}");
    }
}
#[test]
fn refusals_each_say_why() {
    let store = fresh("refusals");
    let command = CommandId::Rename;
    let cases: [(KeyBinding, &str); 12] = [
        (key(VK_F4, ALT), "Alt+F4 is kept by Windows."),
        (key(VK_TAB, ALT), "Alt+Tab is kept by Windows."),
        (key(VK_SPACE, ALT), "Alt+Space is kept by Windows."),
        (key(VK_ESC, CTRL), "Ctrl+Esc is kept by Windows."),
        (key(VK_F10, NONE), "F10 is kept by Windows."),
        (key(VK_F10, SHIFT), "Shift+F10 is kept by Windows."),
        (
            key(VK_LEFT, CTRL),
            "Ctrl+Left is the text-editing key for \u{201c}Move Word Left\u{201d}.",
        ),
        (
            key(VK_RIGHT, CTRL_SHIFT),
            "Ctrl+Shift+Right is the text-editing key for \u{201c}Select Word Right\u{201d}.",
        ),
        (key(0x46, ALT), "Alt+F opens the File menu."),
        (key(0x57, ALT), "Alt+W opens the Window menu."),
        (
            key(0x45, CTRL_ALT),
            "Ctrl+Alt+E types a character on many keyboards, because Ctrl+Alt is the same as AltGr.",
        ),
        (
            key(0x4b, SHIFT),
            "A shortcut needs Ctrl or Alt, unless it is a function key.",
        ),
    ];
    for (pressed, words) in cases {
        let refused = store.set_key(command, pressed).unwrap_err();
        assert_eq!(refused.message(), words);
        assert_eq!(store.key_of(command), None, "{}", key_text(pressed));
    }
}
#[test]
fn a_clash_names_the_command_and_its_menu_and_changes_nothing() {
    let store = fresh("clash");
    let refused = store
        .set_key(CommandId::Rename, key(0x46, CTRL))
        .unwrap_err();
    assert_eq!(
        refused.message(),
        "Ctrl+F is already used by \u{201c}Find\u{201d} in the Edit > Find menu."
    );
    assert_eq!(store.key_of(CommandId::Rename), None);
    assert_eq!(store.key_of(CommandId::Find), Some(key(0x46, CTRL)));
    let refused = store
        .set_key(CommandId::Rename, key(0x4e, CTRL))
        .unwrap_err();
    assert!(refused.message().contains("in the File menu"));
}
#[test]
fn function_keys_and_new_chords_are_allowed() {
    let store = fresh("allowed");
    for (index, pressed) in [
        key(VK_F5, NONE),
        key(VK_F5, SHIFT),
        key(0x51, ALT),
        key(VK_F5, CTRL_ALT),
        key(0x47, CTRL_SHIFT),
        key(0x47, CTRL),
    ]
    .into_iter()
    .enumerate()
    {
        let command = [
            CommandId::Rename,
            CommandId::MoveTo,
            CommandId::SaveAll,
            CommandId::RevertToSaved,
            CommandId::ExportPdf,
            CommandId::Delete,
        ][index];
        store.set_key(command, pressed).unwrap();
        assert_eq!(store.key_of(command), Some(pressed));
    }
}
#[test]
fn own_default_is_never_refused_and_saves_no_change() {
    let store = fresh("own");
    // F3 has no Ctrl or Alt. It is still Find Next's own key.
    store.set_key(CommandId::FindNext, key(0x72, NONE)).unwrap();
    assert!(!store.is_key_changed(CommandId::FindNext));
    store.flush().unwrap();
    assert!(!store.path().exists() || std::fs::read_to_string(store.path()).unwrap().is_empty());
}
#[test]
fn a_default_taken_by_another_command_stays_with_it() {
    let store = fresh("taken");
    // Free Ctrl+F, give it to Rename. Find now has no key and shows as changed.
    store.clear_key(CommandId::Find);
    store.set_key(CommandId::Rename, key(0x46, CTRL)).unwrap();
    assert_eq!(store.key_of(CommandId::Find), None);
    assert!(store.is_key_changed(CommandId::Find));
    // Resetting Find is refused while Rename holds the key, and both keys stay.
    let refused = store.reset_key(CommandId::Find).unwrap_err();
    assert!(
        refused.message().contains("Rename"),
        "{}",
        refused.message()
    );
    assert_eq!(store.key_of(CommandId::Find), None);
    assert_eq!(store.key_of(CommandId::Rename), Some(key(0x46, CTRL)));
    store.reset_key(CommandId::Rename).unwrap();
    store.reset_key(CommandId::Find).unwrap();
    assert_eq!(store.key_of(CommandId::Find), Some(key(0x46, CTRL)));
    assert!(!store.is_key_changed(CommandId::Find));
}
#[test]
fn a_future_default_does_not_take_a_key_the_user_set() {
    // The Mac's upgrade rule: a saved change wins over a default that came later.
    let path = settings_path("upgrade");
    std::fs::write(&path, "KeyboardShortcut.Rename=Ctrl+N\r\n").unwrap();
    let store = SettingsStore::open(&path);
    assert_eq!(store.load_problem(), None);
    assert_eq!(store.key_of(CommandId::Rename), Some(key(0x4e, CTRL)));
    assert_eq!(store.key_of(CommandId::New), None);
    assert!(store.is_key_changed(CommandId::New));
}
#[test]
fn every_key_name_reads_back() {
    for code in 1..=0xfe_u16 {
        let plain = KeyBinding::new(code, false, false, false);
        let chord = KeyBinding::new(code, true, true, true);
        assert_eq!(parse_key(&key_text(plain)), Some(plain), "{code:#x}");
        assert_eq!(parse_key(&key_text(chord)), Some(chord), "{code:#x}");
    }
    assert_eq!(parse_key("Ctrl+Nonsense"), None);
    assert_eq!(parse_key("F25"), None);
    assert_eq!(parse_key(""), None);
}
#[test]
fn recorder_steps_are_the_macs() {
    assert_eq!(record_step(VK_ESC, NONE, false), RecordStep::Cancel);
    assert_eq!(record_step(VK_DELETE, NONE, false), RecordStep::Remove);
    assert_eq!(record_step(VK_BACK, NONE, false), RecordStep::Remove);
    assert_eq!(record_step(VK_CTRL, CTRL, false), RecordStep::Held(CTRL));
    assert_eq!(
        record_step(0xa2, CTRL, false),
        RecordStep::Held(CTRL),
        "the left Ctrl key's own code"
    );
    assert_eq!(
        record_step(0x4b, CTRL, true),
        RecordStep::Ignore,
        "a repeat"
    );
    assert_eq!(
        record_step(0x14, NONE, false),
        RecordStep::Ignore,
        "Caps Lock"
    );
    assert_eq!(
        record_step(0xe5, NONE, false),
        RecordStep::Ignore,
        "input method"
    );
    assert_eq!(
        record_step(VK_ESC, CTRL, false),
        RecordStep::Candidate(key(VK_ESC, CTRL)),
        "only a plain Esc cancels"
    );
    assert_eq!(
        record_step(VK_DELETE, CTRL, false),
        RecordStep::Candidate(key(VK_DELETE, CTRL)),
        "only a plain Delete removes"
    );
    assert_eq!(
        record_step(0x4b, WIN, false),
        RecordStep::WindowsKey(key(0x4b, NONE))
    );
    assert_eq!(held_text(CTRL_SHIFT), "Ctrl+Shift+");
}
#[test]
fn font_dialog_start_and_result() {
    let font = FontChoice {
        family: "Cascadia Mono".into(),
        points: 10.0,
    };
    let start = font_dialog_start(&font, 96);
    assert_eq!(start.lfHeight, -13);
    let name: String = String::from_utf16_lossy(&start.lfFaceName[..13]);
    assert_eq!(name, "Cascadia Mono");
    assert_eq!(start.lfFaceName[13], 0);
    assert_eq!(font_dialog_result(&start, 100).unwrap(), font);
    assert_eq!(font_dialog_result(&start, 115).unwrap().points, 11.5);
    assert!(font_dialog_result(&start, 0).is_err());
    assert!(
        font_dialog_result(&Default::default(), 100).is_err(),
        "no name"
    );
    let long = FontChoice {
        family: "x".repeat(200),
        points: 9.0,
    };
    let cut = font_dialog_start(&long, 96);
    assert_eq!(*cut.lfFaceName.last().unwrap(), 0, "always ends in zero");
}
