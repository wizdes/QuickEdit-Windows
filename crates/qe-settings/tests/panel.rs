//! The Settings content with real event order: the recorder, keyboard-only use of every
//! control, Tab order, focus marks and the widget tree. No window opens.
mod support;

use qe_api::{
    CommandId, DrawCommand, Handled, InputKind, KeyBinding, SettingKey, TextEncoding, TextMeasurer,
    View,
};
use qe_settings::*;
use qe_ui::{duplicate_ids, focused_nodes};
use support::*;

fn ctrl(code: u16) -> KeyBinding {
    KeyBinding::new(code, true, false, false)
}
fn row_y(rig: &Rig, command: CommandId) -> f32 {
    let tree = rig.panel.widgets().unwrap();
    let id = format!("Shortcut for {}", row_name(command));
    let found = find(&tree, &id).unwrap_or_else(|| panic!("no node {id}"));
    found.bounds.y.0 + 4.0
}
fn row_name(command: CommandId) -> String {
    menu_place(command).name
}
fn find<'a>(node: &'a qe_api::Widget, name: &str) -> Option<&'a qe_api::Widget> {
    if node.name == name {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, name))
}
fn walk<'a>(node: &'a qe_api::Widget, out: &mut Vec<&'a qe_api::Widget>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

// The recorder.

#[test]
fn a_chord_in_real_order_records_and_saves() {
    let mut rig = Rig::new("record");
    rig.record(CommandId::Rename);
    let target = KeyBinding::new(0x47, true, false, true);
    assert_eq!(rig.down(VK_CTRL, CTRL), Handled::Yes);
    assert_eq!(rig.down(VK_SHIFT, CTRL_SHIFT), Handled::Yes);
    let painted = painted_text(&rig.light());
    assert!(painted.iter().any(|t| t == "Ctrl+Shift+"), "{painted:?}");
    assert_eq!(rig.down(0x47, CTRL_SHIFT), Handled::Yes);
    assert!(!rig.recording());
    assert_eq!(rig.store.key_of(CommandId::Rename), Some(target));
    // The rest of the chord lands on the table with nothing to do.
    rig.up(0x47, CTRL_SHIFT);
    rig.up(VK_SHIFT, CTRL);
    rig.up(VK_CTRL, NONE);
    assert_eq!(rig.store.key_of(CommandId::Rename), Some(target));
    rig.store.flush().unwrap();
    let again = SettingsStore::open(rig.store.path());
    assert_eq!(again.key_of(CommandId::Rename), Some(target));
}
#[test]
fn modifiers_held_then_released_show_and_clear() {
    let mut rig = Rig::new("held");
    rig.record(CommandId::Rename);
    assert!(
        painted_text(&rig.light())
            .iter()
            .any(|t| t == TYPE_SHORTCUT)
    );
    rig.down(VK_ALT, ALT);
    assert!(painted_text(&rig.light()).iter().any(|t| t == "Alt+"));
    rig.up(VK_ALT, NONE);
    assert!(
        painted_text(&rig.light())
            .iter()
            .any(|t| t == TYPE_SHORTCUT)
    );
    assert!(rig.recording(), "a modifier alone records nothing");
}
#[test]
fn a_held_enter_does_not_become_the_new_key() {
    let mut rig = Rig::new("repeat");
    rig.go_to_keyboard_table();
    rig.down(VK_ENTER, NONE);
    assert!(rig.recording());
    assert_eq!(rig.panel.key_message(), HINT_WHILE_RECORDING);
    rig.held(VK_ENTER, NONE);
    rig.held(VK_ENTER, NONE);
    assert!(rig.recording());
    assert_eq!(rig.panel.key_message(), HINT_WHILE_RECORDING, "no refusal");
    rig.up(VK_ENTER, NONE);
    assert!(rig.recording());
}
#[test]
fn space_also_starts_recording_and_its_character_is_dropped() {
    let mut rig = Rig::new("space");
    rig.go_to_keyboard_table();
    rig.down(VK_SPACE, NONE);
    rig.send(InputKind::Char(' '));
    rig.up(VK_SPACE, NONE);
    assert!(rig.recording());
}
#[test]
fn esc_cancels_with_no_change_and_a_held_esc_does_not_close() {
    let mut rig = Rig::new("esc");
    rig.record(CommandId::Find);
    let before = rig.store.key_of(CommandId::Find);
    assert_eq!(rig.down(VK_ESC, NONE), Handled::Yes);
    assert!(!rig.recording());
    assert_eq!(rig.store.key_of(CommandId::Find), before);
    assert!(!rig.store.is_key_changed(CommandId::Find));
    assert_eq!(rig.panel.key_message(), "");
    rig.held(VK_ESC, NONE);
    rig.held(VK_ESC, NONE);
    assert!(
        !rig.panel.closed(),
        "the same press must not close Settings"
    );
    rig.up(VK_ESC, NONE);
    rig.down(VK_ESC, NONE);
    assert!(rig.panel.closed(), "a new Esc closes Settings");
}
#[test]
fn delete_while_recording_removes_the_key_and_delete_on_a_row_resets_it() {
    let mut rig = Rig::new("delete");
    rig.record(CommandId::Find);
    rig.down(VK_DELETE, NONE);
    assert!(!rig.recording());
    assert_eq!(rig.store.key_of(CommandId::Find), None);
    assert!(rig.store.is_key_changed(CommandId::Find));
    rig.up(VK_DELETE, NONE);
    rig.down(VK_DELETE, NONE);
    assert_eq!(rig.store.key_of(CommandId::Find), Some(ctrl(0x46)));
    assert!(!rig.store.is_key_changed(CommandId::Find));
    rig.up(VK_DELETE, NONE);
    // Backspace does the same in both places, as on the Mac.
    rig.tap(VK_ENTER);
    rig.down(VK_BACK, NONE);
    assert_eq!(rig.store.key_of(CommandId::Find), None);
    rig.up(VK_BACK, NONE);
    rig.tap(VK_BACK);
    assert_eq!(rig.store.key_of(CommandId::Find), Some(ctrl(0x46)));
}
#[test]
fn a_clash_is_refused_with_a_message_and_recording_goes_on() {
    let mut rig = Rig::new("clash");
    rig.record(CommandId::Rename);
    rig.chord(0x46, CTRL);
    assert!(rig.recording());
    assert_eq!(rig.store.key_of(CommandId::Rename), None);
    assert_eq!(rig.store.key_of(CommandId::Find), Some(ctrl(0x46)));
    let words = "Ctrl+F is already used by \u{201c}Find\u{201d} in the Edit > Find menu.";
    assert_eq!(rig.panel.key_message(), words);
    assert!(
        painted_text(&rig.light())
            .iter()
            .any(|t| t.contains("already used"))
    );
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(find(&tree, words).unwrap().role, qe_api::WidgetRole::Label);
    assert_eq!(rig.panel.take_announcement().as_deref(), Some(words));
    // A good key after the refusal still works, and the message goes.
    rig.chord(0x47, CTRL);
    assert!(!rig.recording());
    assert_eq!(rig.store.key_of(CommandId::Rename), Some(ctrl(0x47)));
    assert_eq!(rig.panel.key_message(), "");
}
#[test]
fn reserved_keys_are_refused_and_alt_f4_stays_a_normal_close() {
    let mut rig = Rig::new("reserved");
    rig.record(CommandId::Rename);
    // While recording, Alt+F4 is refused and does not reach Windows.
    assert_eq!(rig.down(VK_ALT, ALT), Handled::Yes);
    assert_eq!(rig.down(VK_F4, ALT), Handled::Yes);
    assert_eq!(rig.panel.key_message(), "Alt+F4 is kept by Windows.");
    rig.up(VK_F4, ALT);
    rig.up(VK_ALT, NONE);
    rig.down(0x4b, WIN);
    assert!(rig.panel.key_message().contains("kept by Windows"));
    rig.up(0x4b, WIN);
    rig.tap(VK_ESC);
    // With no recording, Alt+F4 passes on, so Windows closes the window.
    assert_eq!(rig.down(VK_ALT, ALT), Handled::No);
    assert_eq!(rig.down(VK_F4, ALT), Handled::No);
    assert!(!rig.panel.closed());
}
#[test]
fn no_row_edits_a_key_that_windows_owns() {
    let store = SettingsStore::open(settings_path("no-alt-f4-row"));
    let rows = keyboard_rows();
    for row in rows {
        if let KeyRow::Command { command, .. } = row {
            for owned in WINDOWS_KEYS {
                if command.default_key() != Some(*owned) {
                    assert!(store.refusal_for(*owned, command).is_some(), "{command:?}");
                }
            }
        }
    }
}
#[test]
fn changed_rows_show_the_marker_and_the_arrow_resets() {
    let mut rig = Rig::new("marks");
    rig.go_to_keyboard_table();
    let tree = rig.panel.widgets().unwrap();
    assert!(find(&tree, "Reset New to its default shortcut").is_none());
    rig.store.clear_key(CommandId::New);
    let painted = painted_text(&rig.light());
    assert!(painted.iter().any(|t| t == CHANGED_MARK), "{painted:?}");
    let icons = rig
        .light()
        .commands
        .iter()
        .filter(|c| matches!(c, DrawCommand::Icon { icon_id, .. } if icon_id == "E7A7"))
        .count();
    assert_eq!(icons, 1);
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(
        find(&tree, "New").and_then(|row| find(row, "New").map(|_| ())),
        Some(())
    );
    let reset = find(&tree, "Reset New to its default shortcut").expect("the arrow's node");
    assert_eq!(reset.role, qe_api::WidgetRole::Button);
    let (x, y) = (reset.bounds.x.0 + 5.0, reset.bounds.y.0 + 5.0);
    rig.click(x, y);
    assert_eq!(rig.store.key_of(CommandId::New), Some(ctrl(0x4e)));
    assert!(!rig.store.is_key_changed(CommandId::New));
    assert!(!painted_text(&rig.light()).iter().any(|t| t == CHANGED_MARK));
}
#[test]
fn a_click_on_a_key_records_and_a_click_elsewhere_ends_it() {
    let mut rig = Rig::new("clicks");
    rig.go_to_tab(TAB_KEYBOARD);
    let tree = rig.panel.widgets().unwrap();
    let node = find(&tree, "Shortcut for New").unwrap();
    let (x, y) = (node.bounds.x.0 + 10.0, node.bounds.y.0 + 5.0);
    rig.click(x, y);
    assert_eq!(
        rig.panel.table().unwrap().recording_command(),
        Some(CommandId::New)
    );
    assert_eq!(rig.focused().as_deref(), Some("Keyboard shortcuts"));
    // A click on the button below ends the recording and still works.
    rig.store.clear_key(CommandId::Save);
    let tree = rig.panel.widgets().unwrap();
    let button = find(&tree, "Restore Keyboard Defaults").unwrap();
    let (x, y) = (button.bounds.x.0 + 10.0, button.bounds.y.0 + 10.0);
    rig.click(x, y);
    assert!(!rig.recording());
    assert!(!rig.store.is_key_changed(CommandId::Save));
}
#[test]
fn a_click_on_nothing_ends_a_recording_with_no_focus_change() {
    let mut rig = Rig::new("click-nothing");
    rig.record(CommandId::Rename);
    // The empty space above the tabs' right end takes no focus, so no Blur ends it.
    rig.click(420.0, 10.0);
    assert!(!rig.recording());
    assert_eq!(rig.focused().as_deref(), Some("Keyboard shortcuts"));
    assert_eq!(rig.store.key_of(CommandId::Rename), None);
}
#[test]
fn a_double_click_on_a_name_records_and_one_click_selects() {
    let mut rig = Rig::new("double");
    rig.go_to_tab(TAB_KEYBOARD);
    let y = row_y(&rig, CommandId::Open);
    let at = qe_api::Point {
        x: qe_api::Dip(60.0),
        y: qe_api::Dip(y),
    };
    let press = InputKind::MouseDown {
        at,
        button: qe_api::MouseButton::Left,
        modifiers: NONE,
    };
    rig.send(press.clone());
    assert!(!rig.recording());
    assert_eq!(
        rig.panel.table().unwrap().selected_command(),
        Some(CommandId::Open)
    );
    rig.time_us -= 900_000;
    rig.send(press);
    assert!(rig.recording());
}
#[test]
fn recording_ends_when_the_tab_changes_or_the_window_loses_the_keyboard() {
    let mut rig = Rig::new("ends");
    rig.record(CommandId::Rename);
    rig.send(InputKind::Blur);
    assert!(!rig.recording());
    rig.send(InputKind::Focus);
    rig.record(CommandId::Rename);
    rig.go_to_tab(TAB_NEW_DOCUMENT);
    assert!(!rig.recording());
    assert_eq!(rig.store.key_of(CommandId::Rename), None);
}
#[test]
fn restore_keyboard_defaults_and_restore_all_work_from_the_keyboard() {
    let mut rig = Rig::new("restore");
    rig.store.clear_key(CommandId::New);
    rig.store.set_integer(SettingKey::TabWidth, 2).unwrap();
    rig.go_to_tab(TAB_KEYBOARD);
    rig.tab_to("Restore Keyboard Defaults");
    rig.tap(VK_SPACE);
    assert!(!rig.store.is_key_changed(CommandId::New));
    assert_eq!(
        rig.store.get_integer(SettingKey::TabWidth),
        Some(2),
        "only keys"
    );
    rig.store.clear_key(CommandId::New);
    rig.tab_to("Restore All Defaults");
    rig.tap(VK_SPACE);
    assert!(!rig.store.is_key_changed(CommandId::New));
    assert_eq!(rig.store.get_integer(SettingKey::TabWidth), Some(8));
}
#[test]
fn restore_all_drops_a_half_typed_size() {
    let mut rig = Rig::new("half-typed");
    rig.typed("77");
    rig.tab_to("Restore All Defaults");
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowWidthCharacters),
        Some(77)
    );
    rig.store
        .set_integer(SettingKey::WindowHeightLines, 5)
        .unwrap();
    rig.tap(VK_SPACE);
    rig.tab_to("Window width in characters");
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(
        find(&tree, "Window width in characters")
            .unwrap()
            .value
            .as_deref(),
        Some("90")
    );
    assert_eq!(
        find(&tree, "Window height in lines")
            .unwrap()
            .value
            .as_deref(),
        Some("30")
    );
}

// Sizes.

#[test]
fn width_field_takes_good_numbers_and_rejects_junk_with_a_message() {
    let mut rig = Rig::new("sizes");
    assert_eq!(
        rig.focused().as_deref(),
        Some("Window width in characters"),
        "the Width field has the keyboard first, as on the Mac"
    );
    // The first typed digit replaces the selected value.
    rig.typed("120");
    rig.tap(VK_ENTER);
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowWidthCharacters),
        Some(120)
    );
    assert_eq!(rig.panel.size_message(), "");

    // A letter is refused as it is typed, with a visible message.
    rig.typed("a");
    assert_eq!(
        rig.panel.size_message(),
        "Only the digits 0 to 9 can go here."
    );
    assert!(
        painted_text(&rig.light())
            .iter()
            .any(|t| t.contains("digits"))
    );

    for junk in ["0", "10001", "99999999999999999999"] {
        rig.tab_to("Window width in characters");
        rig.typed(junk);
        rig.tap(VK_TAB);
        assert_eq!(
            rig.panel.size_message(),
            "Width must be a whole number from 1 to 10000.",
            "{junk}"
        );
        assert_eq!(
            rig.store.get_integer(SettingKey::WindowWidthCharacters),
            Some(120)
        );
    }
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(
        find(&tree, "Window width in characters")
            .unwrap()
            .value
            .as_deref(),
        Some("120"),
        "the last good value comes back"
    );

    // Typing in that field again clears the message. 1 and 10000 are good.
    rig.tab_to("Window width in characters");
    rig.typed("1");
    assert_eq!(rig.panel.size_message(), "");
    rig.tap(VK_ENTER);
    rig.tab_to("Window height in lines");
    rig.typed("10000");
    rig.tap(VK_ENTER);
    assert_eq!(rig.panel.size_message(), "");
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowHeightLines),
        Some(10_000)
    );
    rig.typed("1");
    rig.tap(VK_ENTER);
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowHeightLines),
        Some(1)
    );
}
#[test]
fn an_emptied_field_puts_the_value_back_with_a_message() {
    let mut rig = Rig::new("emptied");
    rig.tap(VK_DELETE);
    rig.tap(VK_TAB);
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowWidthCharacters),
        Some(90)
    );
    assert!(rig.panel.size_message().starts_with("Width must be"));
}
#[test]
fn a_typed_size_is_kept_when_the_window_closes() {
    let mut rig = Rig::new("close-keeps");
    rig.typed("55");
    rig.panel.commit_typed();
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowWidthCharacters),
        Some(55)
    );
}
#[test]
fn tab_width_autosave_and_paper_have_no_control() {
    let mut rig = Rig::new("no-control");
    for tab in [TAB_NEW_DOCUMENT, TAB_OPEN_AND_SAVE, TAB_KEYBOARD] {
        rig.go_to_tab(tab);
        let painted = painted_text(&rig.light()).join(" ").to_lowercase();
        for word in [
            "tab width",
            "autosave",
            "paper",
            "grammar",
            "text replacement",
        ] {
            assert!(!painted.contains(word), "{word} on tab {tab}: {painted}");
        }
    }
}

// Keyboard-only use of every control.

#[test]
fn every_checkbox_toggles_from_the_keyboard_and_writes_its_setting() {
    let mut rig = Rig::new("checks");
    let boxes = [
        ("Wrap to page", SettingKey::ShowPageBreaks, TAB_NEW_DOCUMENT),
        (
            "Check spelling as you type",
            SettingKey::CheckSpellingWhileTyping,
            TAB_NEW_DOCUMENT,
        ),
        (
            "Correct spelling automatically",
            SettingKey::CorrectSpellingAutomatically,
            TAB_NEW_DOCUMENT,
        ),
        (
            "Smart copy/paste",
            SettingKey::SmartCopyPaste,
            TAB_NEW_DOCUMENT,
        ),
        ("Smart quotes", SettingKey::SmartQuotes, TAB_NEW_DOCUMENT),
        ("Smart dashes", SettingKey::SmartDashes, TAB_NEW_DOCUMENT),
        (
            "Show line numbers",
            SettingKey::ShowLineNumbers,
            TAB_NEW_DOCUMENT,
        ),
        (
            "Add \".txt\" extension to plain text files",
            SettingKey::AddTxtExtension,
            TAB_OPEN_AND_SAVE,
        ),
    ];
    for (name, key, tab) in boxes {
        rig.go_to_tab(tab);
        rig.tab_to(name);
        let before = rig.store.get_bool(key).unwrap();
        rig.tap(VK_SPACE);
        assert_eq!(rig.store.get_bool(key), Some(!before), "{name}");
        let tree = rig.panel.widgets().unwrap();
        assert_eq!(find(&tree, name).unwrap().states.checked, !before, "{name}");
    }
    rig.store.flush().unwrap();
    let text = std::fs::read_to_string(rig.store.path()).unwrap();
    assert!(text.contains("ShowLineNumbers=true"), "{text}");
    assert!(text.contains("AddTxtExtension=false"), "{text}");
}
#[test]
fn line_numbers_send_an_event_at_once() {
    let mut rig = Rig::new("line-numbers");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = seen.clone();
    rig.store
        .subscribe(move |change| log.borrow_mut().push(change.clone()));
    rig.tab_to("Show line numbers");
    rig.tap(VK_SPACE);
    assert_eq!(
        *seen.borrow(),
        [SettingChange::Value(SettingKey::ShowLineNumbers)]
    );
}
#[test]
fn the_change_button_asks_for_the_font_dialog_from_the_keyboard() {
    let mut rig = Rig::new("font-button");
    rig.tab_to("Change\u{2026}");
    assert_eq!(rig.panel.take_request(), None);
    rig.tap(VK_SPACE);
    assert_eq!(rig.panel.take_request(), Some(PanelRequest::ChooseFont));
    assert_eq!(rig.panel.font_text(), "Cascadia Mono 10 pt");
    assert!(
        painted_text(&rig.light())
            .iter()
            .any(|t| t == "Cascadia Mono 10 pt")
    );
    rig.store
        .set_font(&FontChoice {
            family: "Consolas".into(),
            points: 12.0,
        })
        .unwrap();
    rig.panel.refresh();
    assert_eq!(rig.panel.font_text(), "Consolas 12 pt");
}
#[test]
fn encoding_lists_work_from_the_keyboard_and_customize_runs_only_from_the_open_list() {
    let mut rig = Rig::new("encodings");
    rig.go_to_tab(TAB_OPEN_AND_SAVE);
    rig.tab_to("Encoding for opening files");
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(
        find(&tree, "Encoding for opening files")
            .unwrap()
            .value
            .as_deref(),
        Some("Automatic")
    );
    // A closed list: Down moves to the first enabled encoding, and it is saved at once.
    rig.tap(VK_DOWN);
    assert_eq!(
        rig.store.get_text(SettingKey::OpenEncoding).as_deref(),
        Some(TextEncoding::DEFAULT_LIST[0].name())
    );
    // Up, Up again wraps to the Customize row. On a closed list that row does nothing.
    rig.tap(VK_UP);
    assert_eq!(
        rig.store.get_text(SettingKey::OpenEncoding).as_deref(),
        Some("Automatic")
    );
    rig.tap(VK_UP);
    assert_eq!(rig.panel.take_request(), None);
    assert_eq!(
        rig.store.get_text(SettingKey::OpenEncoding).as_deref(),
        Some("Automatic")
    );
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(
        find(&tree, "Encoding for opening files")
            .unwrap()
            .value
            .as_deref(),
        Some("Automatic")
    );
    // An open list: Up wraps to Customize, and Enter runs it. The value stays.
    rig.tap(VK_SPACE);
    rig.tap(VK_UP);
    rig.tap(VK_ENTER);
    assert_eq!(
        rig.panel.take_request(),
        Some(PanelRequest::CustomizeEncodings)
    );
    assert_eq!(
        rig.store.get_text(SettingKey::OpenEncoding).as_deref(),
        Some("Automatic")
    );
}
#[test]
fn a_saved_encoding_off_the_list_shows_at_the_top_and_the_list_follows_the_store() {
    let mut rig = Rig::new("off-list");
    rig.store
        .set_encoding_choice(
            SettingKey::SaveEncoding,
            qe_api::EncodingChoice::Specific(TextEncoding::Koi8R),
        )
        .unwrap();
    rig.panel.refresh();
    rig.go_to_tab(TAB_OPEN_AND_SAVE);
    let tree = rig.panel.widgets().unwrap();
    let combo = find(&tree, "Encoding for saving files").unwrap();
    assert_eq!(combo.value.as_deref(), Some("KOI8-R"));
    rig.store
        .set_enabled_encodings(&[TextEncoding::Utf8])
        .unwrap();
    rig.panel.refresh();
    rig.tab_to("Encoding for opening files");
    rig.tap(VK_DOWN);
    assert_eq!(
        rig.store.get_text(SettingKey::OpenEncoding).as_deref(),
        Some("UTF-8")
    );
}
#[test]
fn the_tab_bar_takes_arrows_and_ctrl_tab_from_anywhere() {
    let mut rig = Rig::new("tabs");
    assert_eq!(rig.panel.current_tab(), TAB_NEW_DOCUMENT);
    rig.chord(VK_TAB, CTRL);
    assert_eq!(rig.panel.current_tab(), TAB_OPEN_AND_SAVE);
    // The Width field just went away, so the tab bar has the keyboard.
    assert_eq!(rig.focused().as_deref(), Some("Settings tabs"));
    rig.tap(VK_RIGHT);
    assert_eq!(rig.panel.current_tab(), TAB_KEYBOARD);
    assert_eq!(rig.panel.wanted_height(), KEYBOARD_HEIGHT);
    rig.tap(VK_LEFT);
    assert_eq!(rig.panel.current_tab(), TAB_OPEN_AND_SAVE);
    assert_eq!(rig.panel.wanted_height(), TEXT_TABS_HEIGHT);
    rig.chord(VK_TAB, CTRL_SHIFT);
    assert_eq!(rig.panel.current_tab(), TAB_NEW_DOCUMENT);
}
#[test]
fn the_keyboard_table_moves_with_every_key() {
    let mut rig = Rig::new("table-keys");
    rig.go_to_keyboard_table();
    let selected = |rig: &Rig| rig.panel.table().unwrap().selected_command().unwrap();
    let first = selected(&rig);
    rig.tap(VK_DOWN);
    assert_ne!(selected(&rig), first);
    rig.tap(VK_UP);
    assert_eq!(selected(&rig), first);
    rig.tap(0x23);
    assert_eq!(selected(&rig), *CommandId::ALL.last().unwrap());
    assert!(
        rig.panel.table().unwrap().first_visible() > 0,
        "End scrolls"
    );
    rig.tap(0x24);
    assert_eq!(selected(&rig), first);
    assert_eq!(
        rig.panel.table().unwrap().first_visible(),
        0,
        "Home shows the title"
    );
    rig.tap(0x22);
    let paged = selected(&rig);
    assert_ne!(paged, first);
    // Page Down can land on a section title and pass over it, so one Page Up may stop a
    // row short. It still moves up, and the next one reaches the top.
    rig.tap(0x21);
    assert_ne!(selected(&rig), paged);
    rig.tap(0x21);
    assert_eq!(selected(&rig), first);
    // Arrows pass over section titles.
    for _ in 0..100 {
        rig.tap(VK_DOWN);
        assert!(rig.panel.table().unwrap().selected_command().is_some());
    }
}
#[test]
fn the_wheel_scrolls_the_table_and_the_bar_drags_it() {
    let mut rig = Rig::new("wheel");
    rig.go_to_tab(TAB_KEYBOARD);
    let at = qe_api::Point {
        x: qe_api::Dip(100.0),
        y: qe_api::Dip(200.0),
    };
    rig.send(InputKind::Wheel {
        at,
        delta_x: 0.0,
        delta_y: -120.0,
        modifiers: NONE,
    });
    assert_eq!(rig.panel.table().unwrap().first_visible(), 3);
    rig.send(InputKind::Wheel {
        at,
        delta_x: 0.0,
        delta_y: 240.0,
        modifiers: NONE,
    });
    assert_eq!(rig.panel.table().unwrap().first_visible(), 0);
    let table = rig.panel.table().unwrap().bounds;
    let x = table.x.0 + table.width.0 - 4.0;
    let y = table.y.0 + 30.0;
    let point = |y: f32| qe_api::Point {
        x: qe_api::Dip(x),
        y: qe_api::Dip(y),
    };
    rig.send(InputKind::MouseDown {
        at: point(y),
        button: qe_api::MouseButton::Left,
        modifiers: NONE,
    });
    rig.send(InputKind::MouseMove {
        at: point(y + 100.0),
    });
    rig.send(InputKind::MouseUp {
        at: point(y + 100.0),
        button: qe_api::MouseButton::Left,
        modifiers: NONE,
    });
    assert!(
        rig.panel.table().unwrap().first_visible() > 0,
        "the bar moved it"
    );
    assert!(
        !rig.recording(),
        "a press on the bar never starts a recording"
    );
}

// Tab order and focus marks.

fn tab_stops(rig: &mut Rig) -> Vec<String> {
    let start = rig.focused().unwrap();
    let mut stops = vec![start.clone()];
    for _ in 0..64 {
        rig.tap(VK_TAB);
        let now = rig.focused().unwrap();
        if now == start {
            return stops;
        }
        stops.push(now);
    }
    panic!("the Tab order is not a loop: {stops:?}");
}
#[test]
fn tab_order_is_reading_order_in_a_closed_loop_on_each_tab() {
    let mut rig = Rig::new("tab-order");
    rig.tab_to("Settings tabs");
    assert_eq!(
        tab_stops(&mut rig),
        [
            "Settings tabs",
            "Wrap to page",
            "Window width in characters",
            "Window height in lines",
            "Change\u{2026}",
            "Check spelling as you type",
            "Correct spelling automatically",
            "Smart copy/paste",
            "Smart quotes",
            "Smart dashes",
            "Show line numbers",
            "Restore All Defaults",
        ]
    );
    rig.go_to_tab(TAB_OPEN_AND_SAVE);
    rig.tab_to("Settings tabs");
    assert_eq!(
        tab_stops(&mut rig),
        [
            "Settings tabs",
            "Add \".txt\" extension to plain text files",
            "Encoding for opening files",
            "Encoding for saving files",
            "Restore All Defaults",
        ]
    );
    rig.go_to_tab(TAB_KEYBOARD);
    rig.tab_to("Settings tabs");
    assert_eq!(
        tab_stops(&mut rig),
        [
            "Settings tabs",
            "Keyboard shortcuts",
            "Restore Keyboard Defaults",
            "Restore All Defaults",
        ]
    );
    // Shift+Tab goes the other way.
    rig.tab_to("Keyboard shortcuts");
    rig.chord(VK_TAB, SHIFT);
    assert_eq!(rig.focused().as_deref(), Some("Settings tabs"));
}
#[test]
fn each_stop_has_one_focused_node_and_paints_a_focus_mark() {
    let mut rig = Rig::new("focus-marks");
    for tab in [TAB_NEW_DOCUMENT, TAB_OPEN_AND_SAVE, TAB_KEYBOARD] {
        rig.go_to_tab(tab);
        rig.tab_to("Settings tabs");
        let stops = tab_stops(&mut rig);
        for name in stops {
            rig.tab_to(&name);
            let tree = rig.panel.widgets().unwrap();
            let focused = focused_nodes(&tree);
            assert_eq!(focused.len(), 1, "{name}: {focused:?}");
            let with_focus = rig.light();
            rig.send(InputKind::Blur);
            let without = rig.light();
            rig.send(InputKind::Focus);
            let difference = with_focus
                .commands
                .iter()
                .filter(|c| !without.commands.contains(c))
                .any(|c| {
                    matches!(
                        c,
                        DrawCommand::StrokeRect { .. } | DrawCommand::FillRect { .. }
                    )
                });
            assert!(difference, "{name} shows no focus mark");
        }
    }
}

// The widget tree.

#[test]
fn every_control_has_a_name_and_role_on_every_tab_and_ids_are_unique() {
    let mut rig = Rig::new("a11y");
    for tab in [TAB_NEW_DOCUMENT, TAB_OPEN_AND_SAVE, TAB_KEYBOARD] {
        rig.go_to_tab(tab);
        let tree = rig.panel.widgets().unwrap();
        assert_eq!(tree.role, qe_api::WidgetRole::Dialog);
        assert_eq!(tree.name, "Settings");
        let mut nodes = Vec::new();
        walk(&tree, &mut nodes);
        for node in &nodes {
            assert!(
                !node.name.is_empty(),
                "{:?} has no name on tab {tab}",
                node.role
            );
        }
        assert_eq!(duplicate_ids(&tree), vec![], "tab {tab}");
    }
}
#[test]
fn the_tree_has_the_expected_roles() {
    let mut rig = Rig::new("roles");
    let tree = rig.panel.widgets().unwrap();
    let role = |tree: &qe_api::Widget, name: &str| find(tree, name).unwrap().role;
    use qe_api::WidgetRole::*;
    assert_eq!(role(&tree, "Settings tabs"), TabList);
    assert_eq!(role(&tree, "New Document"), Tab);
    assert_eq!(role(&tree, "Wrap to page"), Checkbox);
    assert_eq!(role(&tree, "Window width in characters"), TextField);
    assert_eq!(role(&tree, "Change\u{2026}"), Button);
    assert_eq!(role(&tree, "Restore All Defaults"), Button);
    assert!(find(&tree, "Format").unwrap().states.section);
    assert!(
        find(&tree, "Smart quotes").unwrap().description.is_some(),
        "the Mac's tooltip"
    );
    rig.go_to_tab(TAB_OPEN_AND_SAVE);
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(role(&tree, "Encoding for opening files"), ComboBox);
    rig.go_to_tab(TAB_KEYBOARD);
    let tree = rig.panel.widgets().unwrap();
    assert_eq!(role(&tree, "Keyboard shortcuts"), Table);
    assert_eq!(role(&tree, "Shortcut for New"), Button);
    assert_eq!(
        find(&tree, "Shortcut for New").unwrap().value.as_deref(),
        Some("Ctrl+N")
    );
    assert!(find(&tree, "Edit").unwrap().states.section);
    assert!(
        find(&tree, "Open Recent").is_none(),
        "a submenu's own command has no row"
    );
    assert!(find(&tree, "Open Recent \u{203a} Clear Menu").is_some());
    rig.tab_to("Keyboard shortcuts");
    rig.tap(VK_ENTER);
    let tree = rig.panel.widgets().unwrap();
    let selected = focused_nodes(&tree);
    assert_eq!(selected[0].value.as_deref(), Some(TYPE_SHORTCUT));
}

// The design.

#[test]
fn the_keyboard_tab_has_the_design_sizes() {
    let mut rig = Rig::new("design");
    rig.go_to_tab(TAB_KEYBOARD);
    assert_eq!(ROW_HEIGHT, 22.0);
    assert_eq!(SHORTCUT_COLUMN, 132.0);
    assert_eq!(FOOTER_HEIGHT, 44.0);
    assert_eq!(rig.panel.table().unwrap().bounds.width.0, 394.0);
    let tree = rig.panel.widgets().unwrap();
    let row = find(&tree, "Shortcut for New").unwrap();
    assert_eq!(row.bounds.height.0, 22.0);
    let restore = find(&tree, "Restore All Defaults").unwrap();
    assert_eq!(restore.bounds.x.0, 14.0, "lower left");
    assert!(restore.bounds.y.0 >= KEYBOARD_HEIGHT - FOOTER_HEIGHT);
    assert!(restore.bounds.y.0 + restore.bounds.height.0 <= KEYBOARD_HEIGHT);
}
#[test]
fn a_short_window_shortens_the_table_and_keeps_the_footer_whole() {
    let mut rig = Rig::new("short");
    rig.go_to_tab(TAB_KEYBOARD);
    let bounds = |height: f32| qe_api::Rect {
        x: qe_api::Dip(0.0),
        y: qe_api::Dip(0.0),
        width: qe_api::Dip(440.0),
        height: qe_api::Dip(height),
    };
    let tall = rig.panel.table().unwrap().bounds.height.0;
    rig.panel.layout(bounds(480.0), &FixedMeasure);
    let short = rig.panel.table().unwrap().bounds.height.0;
    assert!(short < tall && short >= 22.0 * 3.0, "{tall} to {short}");
    assert_eq!((short - 2.0) % 22.0, 0.0, "whole rows only");
    let tree = rig.panel.widgets().unwrap();
    let restore = find(&tree, "Restore All Defaults").unwrap();
    assert!(restore.bounds.y.0 + restore.bounds.height.0 <= 480.0);
    let keys = find(&tree, "Restore Keyboard Defaults").unwrap();
    let table = rig.panel.table().unwrap().bounds;
    assert!(table.y.0 + table.height.0 <= keys.bounds.y.0);
}
#[test]
fn heading_rows_can_be_read_in_high_contrast() {
    use qe_ui::{SystemPalette, UiTokens, contrast_ratio, over};
    for palette in [
        SystemPalette::HIGH_CONTRAST_BLACK,
        SystemPalette::HIGH_CONTRAST_WHITE,
    ] {
        let theme = palette.theme();
        let t = UiTokens::from_theme(&theme);
        let mut rig = Rig::new("contrast-rows");
        rig.go_to_tab(TAB_KEYBOARD);
        let list = rig.list(&theme);
        // Under each piece of text, find the last fill that covers it.
        let mut under = Vec::new();
        for command in &list.commands {
            match command {
                DrawCommand::FillRect { rect, color }
                | DrawCommand::RoundedRect { rect, color, .. } => {
                    under.push((*rect, *color));
                }
                DrawCommand::TextRun {
                    at, color, text, ..
                } => {
                    let fill = under
                        .iter()
                        .rev()
                        .find(|(r, _)| {
                            at.x.0 >= r.x.0
                                && at.x.0 < r.x.0 + r.width.0
                                && at.y.0 >= r.y.0
                                && at.y.0 < r.y.0 + r.height.0
                        })
                        .map_or(t.background, |(_, c)| *c);
                    let back = over(fill, t.background);
                    let ratio = contrast_ratio(over(*color, back), back);
                    assert!(ratio >= 4.5, "{text:?}: {ratio}");
                }
                _ => {}
            }
        }
    }
}
#[test]
fn a_write_error_and_a_load_problem_show_in_the_footer() {
    let path = settings_path("status");
    std::fs::write(&path, "junk\r\n").unwrap();
    let mut rig = Rig::with_store(SettingsStore::open(&path));
    assert!(
        rig.panel.status_text().contains("problem"),
        "load problem shows"
    );
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    rig.store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert!(rig.store.flush().is_err());
    rig.panel.refresh();
    assert!(
        rig.panel.status_text().contains("read-only"),
        "write error shows"
    );
    let tree = rig.panel.widgets().unwrap();
    let shown = rig.panel.status_text();
    assert_eq!(find(&tree, &shown).unwrap().role, qe_api::WidgetRole::Label);
    assert!(
        painted_text(&rig.light())
            .iter()
            .any(|t| t.contains("Settings"))
    );
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
}
#[test]
fn no_list_names_a_group_by_hand() {
    // A rule of this round: every list comes from the command table.
    for file in [
        include_str!("../src/keys.rs"),
        include_str!("../src/table.rs"),
        include_str!("../src/panel.rs"),
        include_str!("../src/store.rs"),
    ] {
        for word in [
            "CommandId::About",
            "CommandId::Quit",
            "\"QuickEdit/",
            "Menu(\"QuickEdit",
        ] {
            assert!(!file.contains(word), "{word}");
        }
    }
    for source in [
        include_str!("../src/window.rs"),
        include_str!("../src/panel.rs"),
        include_str!("../src/table.rs"),
        include_str!("../src/store.rs"),
    ] {
        assert!(!source.contains("SetTimer"), "no timers at idle");
        assert!(!source.contains("request_timer"), "no timers at idle");
    }
}
#[test]
fn a_refused_size_stays_for_its_own_field_and_goes_when_it_stops_being_true() {
    const WIDTH_ERROR: &str = "Width must be a whole number from 1 to 10000.";
    let mut rig = Rig::new("size-message");
    rig.typed("0");
    rig.tap(VK_ENTER);
    assert_eq!(rig.panel.size_message(), WIDTH_ERROR);
    // Leaving the field, and a good commit in the other field, do not answer it.
    rig.tab_to("Window height in lines");
    assert_eq!(rig.panel.size_message(), WIDTH_ERROR);
    rig.typed("44");
    rig.tap(VK_ENTER);
    assert_eq!(
        rig.store.get_integer(SettingKey::WindowHeightLines),
        Some(44)
    );
    assert_eq!(rig.panel.size_message(), WIDTH_ERROR);
    // Typing the value it already has does.
    rig.tab_to("Window width in characters");
    rig.typed("90");
    rig.tap(VK_ENTER);
    assert_eq!(rig.panel.size_message(), "");

    // It does not wait across tabs.
    rig.typed("0");
    rig.tap(VK_ENTER);
    assert_eq!(rig.panel.size_message(), WIDTH_ERROR);
    rig.go_to_tab(TAB_KEYBOARD);
    rig.go_to_tab(TAB_NEW_DOCUMENT);
    assert_eq!(rig.panel.size_message(), "");
}
fn folder_in_the_files_place(name: &str) -> SettingsStore {
    let path = settings_path(name);
    std::fs::create_dir(&path).unwrap();
    SettingsStore::open(&path)
}
/// A blocked save reports while the window's state is in use, so the listener skips it.
/// The panel must still show it after the event that caused it.
#[test]
fn a_write_failure_raised_by_a_checkbox_reaches_the_footer() {
    let store = folder_in_the_files_place("checkbox-error");
    let mut rig = Rig::with_store(store);
    rig.tab_to("Smart quotes");
    rig.tap(VK_SPACE);
    assert!(rig.store.write_error().is_some());
    let shown = rig.panel.status_text();
    assert!(shown.contains("Settings were not saved"), "{shown}");
    assert_eq!(
        shown.matches("could not be read").count(),
        1,
        "the same words only once: {shown}"
    );
}
#[test]
fn a_problem_the_window_met_clears_at_the_next_action_and_hides_nothing() {
    let path = settings_path("window-problem");
    std::fs::write(&path, "junk\r\n").unwrap();
    let mut rig = Rig::with_store(SettingsStore::open(&path));
    rig.panel.show_problem("The font dialog could not open.");
    let shown = rig.panel.status_text();
    assert!(shown.contains("font dialog"), "{shown}");
    assert!(
        shown.contains("has a problem"),
        "the load problem stays: {shown}"
    );
    rig.tab_to("Smart quotes");
    rig.tap(VK_SPACE);
    let shown = rig.panel.status_text();
    assert!(!shown.contains("font dialog"), "{shown}");
    assert!(shown.contains("has a problem"), "{shown}");
    rig.panel.show_problem("Again.");
    rig.tab_to("Restore All Defaults");
    rig.tap(VK_SPACE);
    assert!(!rig.panel.status_text().contains("Again."));
}
/// Text runs that stick out of the clip they are painted in, so part of them never shows.
fn cut_text(list: &qe_api::DisplayList) -> Vec<String> {
    let mut clips: Vec<qe_api::Rect> = Vec::new();
    let mut cut = Vec::new();
    for command in &list.commands {
        match command {
            DrawCommand::PushClip(rect) => clips.push(*rect),
            DrawCommand::PopClip => {
                clips.pop();
            }
            DrawCommand::TextRun { at, text, .. } => {
                let width = FixedMeasure.measure(text, "").width.0;
                let sticks_out = clips.iter().any(|clip| {
                    at.x.0 < clip.x.0 - 0.5
                        || at.x.0 + width > clip.x.0 + clip.width.0 + 0.5
                        || at.y.0 > clip.y.0 + clip.height.0
                });
                // The fixed measurer is wider than Segoe UI, and these two fit in the real
                // font, as the saved pictures show.
                let fixed_label = ["Plain text font:", "Correct spelling automatically"];
                if sticks_out && !fixed_label.contains(&text.as_str()) {
                    cut.push(text.clone());
                }
            }
            _ => {}
        }
    }
    cut
}
#[test]
fn the_footer_shows_the_whole_load_problem() {
    let path = settings_path("long-problem");
    std::fs::write(&path, "this line is junk\r\n").unwrap();
    let store = SettingsStore::open(&path);
    let problem = store.load_problem().unwrap();
    let mut rig = Rig::with_store(store);
    assert!(
        rig.panel.wanted_height() > TEXT_TABS_HEIGHT,
        "the footer grows"
    );
    let list = rig.light();
    assert_eq!(cut_text(&list), Vec::<String>::new());
    let squeezed = |text: &str| text.split_whitespace().collect::<String>();
    let painted = painted_text(&list).concat();
    assert!(
        squeezed(&painted).contains(&squeezed(&problem)),
        "all of it, in order: {painted}"
    );
    let tree = rig.panel.widgets().unwrap();
    assert!(
        find(&tree, &problem).is_some(),
        "a screen reader gets it too"
    );
    // Every control keeps its place, and the keyboard table keeps its rows.
    let restore = find(&tree, "Restore All Defaults").unwrap().bounds;
    assert!(restore.y.0 >= TEXT_TABS_HEIGHT - FOOTER_HEIGHT);
    let tall = {
        rig.go_to_tab(TAB_KEYBOARD);
        rig.panel.table().unwrap().bounds.height.0
    };
    let plain = {
        let mut other = Rig::new("short-problem");
        other.go_to_tab(TAB_KEYBOARD);
        other.panel.table().unwrap().bounds.height.0
    };
    assert_eq!(tall, plain);
}
#[test]
fn a_short_message_keeps_the_footer_small() {
    let path = settings_path("short-footer");
    let mut rig = Rig::with_store(SettingsStore::open(&path));
    rig.panel.show_problem("Short.");
    rig.lay_out();
    assert_eq!(rig.panel.wanted_height(), TEXT_TABS_HEIGHT);
    rig.panel.show_problem("");
    rig.lay_out();
    assert_eq!(rig.panel.wanted_height(), TEXT_TABS_HEIGHT);
}
#[test]
fn long_keys_show_whole_so_two_rows_never_look_the_same() {
    let store = SettingsStore::open(settings_path("long-keys"));
    let page_up = KeyBinding::new(0x21, true, true, true);
    let page_down = KeyBinding::new(0x22, true, true, true);
    store.set_key(CommandId::New, page_up).unwrap();
    store.set_key(CommandId::Open, page_down).unwrap();
    let mut rig = Rig::with_store(store);
    rig.go_to_tab(TAB_KEYBOARD);
    let list = rig.light();
    let painted = painted_text(&list);
    for key in ["Ctrl+Alt+Shift+Page Up", "Ctrl+Alt+Shift+Page Down"] {
        assert!(painted.iter().any(|t| t == key), "{key} in {painted:?}");
    }
    assert_eq!(cut_text(&list), Vec::<String>::new());
}
#[test]
fn a_long_font_name_shows_its_size_and_is_not_cut() {
    let store = SettingsStore::open(settings_path("long-font"));
    store
        .set_font(&FontChoice {
            family: "Cascadia Mono ExtraLight".into(),
            points: 10.5,
        })
        .unwrap();
    let rig = Rig::with_store(store);
    let list = rig.light();
    assert_eq!(cut_text(&list), Vec::<String>::new());
    let painted = painted_text(&list).join(" ");
    assert!(painted.contains("Cascadia Mono ExtraLight"), "{painted}");
    assert!(painted.contains("10.5 pt"), "{painted}");
    // A name no font dialog gives, from a file edited by hand: an ellipsis says so.
    let store = SettingsStore::open(settings_path("very-long-font"));
    let family = "Very Long Family ".repeat(12);
    store
        .set_font(&FontChoice {
            family: family.trim().to_owned(),
            points: 10.5,
        })
        .unwrap();
    let rig = Rig::with_store(store);
    let list = rig.light();
    assert_eq!(cut_text(&list), Vec::<String>::new());
    let painted = painted_text(&list).join(" ");
    assert!(painted.contains('\u{2026}'), "{painted}");
    assert!(painted.contains("10.5 pt"), "{painted}");
}
/// Concern 5: with Num Lock on, Windows sends Ctrl+Shift+Num 1 as Ctrl+End. That is what
/// the app would get at every later press too, so the refusal is the honest answer.
#[test]
fn what_windows_sends_for_ctrl_shift_num_1_is_refused_with_a_reason() {
    let mut rig = Rig::new("numpad");
    rig.record(CommandId::Rename);
    rig.chord(0x23, CTRL);
    assert!(rig.recording(), "the recording goes on");
    assert!(
        rig.panel.key_message().contains("Ctrl+End"),
        "{}",
        rig.panel.key_message()
    );
    // Num Lock with Ctrl alone keeps the number, and that key works.
    rig.chord(0x61, CTRL);
    assert_eq!(
        rig.store.key_of(CommandId::Rename),
        Some(KeyBinding::new(0x61, true, false, false))
    );
}
