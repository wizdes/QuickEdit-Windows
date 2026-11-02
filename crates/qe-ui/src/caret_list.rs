//! The contents of the caret list. Every Mac menu that Windows keeps lives here
//! (parity rows I01 to I10 and W06). Each row's command is in `CommandId`.
use crate::{MenuItem, MenuModel};
use qe_api::{CommandId, KeyBinding};

/// A key as a row shows it, such as `Ctrl+Shift+S`.
pub fn key_text(key: KeyBinding) -> String {
    let mut out = String::new();
    if key.ctrl {
        out.push_str("Ctrl+");
    }
    if key.alt {
        out.push_str("Alt+");
    }
    if key.shift {
        out.push_str("Shift+");
    }
    let code = key.virtual_key;
    match code {
        0x30..=0x39 | 0x41..=0x5a => out.push(char::from(code as u8)),
        0x70..=0x87 => out.push_str(&format!("F{}", code - 0x6f)),
        0x09 => out.push_str("Tab"),
        0x20 => out.push_str("Space"),
        0xba => out.push(';'),
        0xbb => out.push('='),
        0xbc => out.push(','),
        0xbd => out.push('-'),
        0xbe => out.push('.'),
        other => out.push_str(&format!("Key {other:#04x}")),
    }
    out
}

fn row(
    label: &str,
    command: CommandId,
    key_of: &dyn Fn(CommandId) -> Option<KeyBinding>,
) -> MenuItem {
    let mut item = MenuItem::command(label, command);
    if let Some(key) = key_of(command) {
        item.key_text = key_text(key);
    }
    item
}
fn group(label: &str, mnemonic: char, items: Vec<MenuItem>) -> MenuItem {
    let mut item = MenuItem::submenu(label, MenuModel { items });
    item.mnemonic = Some(mnemonic);
    item
}

/// The caret list with each command's default key.
pub fn caret_menu() -> MenuModel {
    caret_menu_with_keys(&CommandId::default_key)
}

/// The caret list with the keys that the settings give.
pub fn caret_menu_with_keys(key_of: &dyn Fn(CommandId) -> Option<KeyBinding>) -> MenuModel {
    use CommandId::*;
    let r = |label: &str, command: CommandId| row(label, command, key_of);
    let sep = MenuItem::separator;
    let mut open_recent = group("Open Recent", 'r', vec![r("Clear Menu", ClearRecent)]);
    open_recent.command = Some(OpenRecent);
    let file = group(
        "File",
        'f',
        vec![
            r("New", New),
            r("New Tab", NewTab),
            r("Open…", Open),
            open_recent,
            r("Close", Close),
            sep(),
            r("Save", Save),
            r("Duplicate", Duplicate),
            r("Save As…", SaveAs),
            r("Rename…", Rename),
            r("Move To…", MoveTo),
            r("Save All", SaveAll),
            r("Revert to Saved", RevertToSaved),
            sep(),
            r("Export as PDF…", ExportPdf),
            r("Print…", Print),
        ],
    );
    let find = group(
        "Find",
        'f',
        vec![
            r("Find…", Find),
            r("Find and Replace…", FindReplace),
            r("Find Next", FindNext),
            r("Find Previous", FindPrevious),
            r("Use Selection for Find", UseSelectionForFind),
            r("Jump to Selection", JumpToSelection),
            r("Select Line…", SelectLine),
        ],
    );
    let spelling = group(
        "Spelling",
        's',
        vec![
            r("Show Spelling", ShowSpelling),
            r("Check Document Now", CheckDocumentSpelling),
            r("Check Spelling While Typing", CheckSpellingWhileTyping),
            r(
                "Correct Spelling Automatically",
                CorrectSpellingAutomatically,
            ),
        ],
    );
    let substitutions = group(
        "Substitutions",
        'b',
        vec![
            r("Show Substitutions", ShowSubstitutions),
            r("Smart Copy/Paste", SmartCopyPaste),
            r("Smart Quotes", SmartQuotes),
            r("Smart Dashes", SmartDashes),
        ],
    );
    let transformations = group(
        "Transformations",
        't',
        vec![
            r("Make Upper Case", UpperCase),
            r("Make Lower Case", LowerCase),
            r("Capitalize", Capitalize),
        ],
    );
    let edit = group(
        "Edit",
        'e',
        vec![
            r("Undo", Undo),
            r("Redo", Redo),
            sep(),
            r("Cut", Cut),
            r("Copy", Copy),
            r("Paste", Paste),
            r("Delete", Delete),
            sep(),
            r("Complete", Complete),
            r("Select All", SelectAll),
            sep(),
            find,
            spelling,
            substitutions,
            transformations,
        ],
    );
    let format = group(
        "Format",
        'o',
        vec![
            r("Prevent Editing", PreventEditing),
            r("Wrap to Page", WrapToPage),
        ],
    );
    let view = group(
        "View",
        'v',
        vec![
            r("Actual Size", ActualSize),
            r("Zoom In", ZoomIn),
            r("Zoom Out", ZoomOut),
            sep(),
            r("Show Line Numbers", ShowLineNumbers),
            r("Show Document Map", ShowDocumentMap),
            sep(),
            r("Enter Full Screen", FullScreen),
        ],
    );
    let window = group(
        "Window",
        'w',
        vec![
            r("Minimize", Minimize),
            r("Zoom", ZoomWindow),
            sep(),
            r("Show Previous Tab", PreviousTab),
            r("Show Next Tab", NextTab),
            sep(),
            r("Tab 1", Tab1),
            r("Tab 2", Tab2),
            r("Tab 3", Tab3),
            r("Tab 4", Tab4),
            r("Tab 5", Tab5),
            r("Tab 6", Tab6),
            r("Tab 7", Tab7),
            r("Tab 8", Tab8),
            r("Tab 9", Tab9),
            r("Last Tab", LastTab),
            sep(),
            r("Bring All to Front", BringAllToFront),
            r("Move Tab to New Window", MoveTabToNewWindow),
            r("Merge All Windows", MergeAllWindows),
        ],
    );
    MenuModel {
        items: vec![
            file,
            edit,
            format,
            view,
            window,
            sep(),
            r("About QuickEdit", About),
            r("Settings…", Settings),
            r("Quit QuickEdit", Quit),
        ],
    }
}

/// Sets a command's row in a model, at any depth. Returns false if no row has it.
pub fn set_command_state(
    model: &mut MenuModel,
    command: CommandId,
    enabled: bool,
    checked: bool,
) -> bool {
    let mut found = false;
    for item in &mut model.items {
        if item.command == Some(command) {
            item.enabled = enabled;
            item.checked = checked;
            found = true;
        }
        if let Some(sub) = &mut item.submenu {
            found |= set_command_state(sub, command, enabled, checked);
        }
    }
    found
}
