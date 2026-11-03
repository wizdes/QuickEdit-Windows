//! What may be a shortcut, and the rows of the Keyboard tab. Both come from
//! `CommandId`'s table, so a command added there shows up with no change here.
use qe_api::{CommandId, KeyBinding};
use qe_ui::{MENU_LABELS, MenuBar, key_text};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const BACK: u16 = 0x08;
const TAB: u16 = 0x09;
const ENTER: u16 = 0x0d;
const ESC: u16 = 0x1b;
const SPACE: u16 = 0x20;
const END: u16 = 0x23;
const HOME: u16 = 0x24;
const LEFT: u16 = 0x25;
const UP: u16 = 0x26;
const RIGHT: u16 = 0x27;
const DOWN: u16 = 0x28;
const PRINT_SCREEN: u16 = 0x2c;
const INSERT: u16 = 0x2d;
pub(crate) const DELETE: u16 = 0x2e;
const F4: u16 = 0x73;
const F10: u16 = 0x79;
const F24: u16 = 0x87;

/// Why a key cannot be given to a command. The message says so in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Another command has it: its name and where it is in the menus.
    Clash {
        key: KeyBinding,
        name: String,
        menu_path: String,
    },
    /// Windows keeps it, or uses it before the app can.
    Windows(KeyBinding),
    /// A text field or the editor uses it. The action it does there.
    TextEditing { key: KeyBinding, action: String },
    /// It opens a menu of the menu bar.
    MenuKey { key: KeyBinding, menu: String },
    /// Ctrl+Alt is AltGr on many keyboards, so this key types a character there.
    AltGr(KeyBinding),
    /// Needs Ctrl or Alt, unless it is a function key.
    NeedsModifier,
    /// This command only opens a submenu, so it holds no key. Its name.
    OpensSubmenu(String),
}
impl Refusal {
    /// The line under the table.
    pub fn message(&self) -> String {
        match self {
            Self::Clash {
                key,
                name,
                menu_path,
            } => format!(
                "{} is already used by \u{201c}{name}\u{201d} in the {menu_path} menu.",
                key_text(*key)
            ),
            Self::Windows(key) => format!("{} is kept by Windows.", key_text(*key)),
            Self::TextEditing { key, action } => format!(
                "{} is the text-editing key for \u{201c}{action}\u{201d}.",
                key_text(*key)
            ),
            Self::MenuKey { key, menu } => {
                format!("{} opens the {menu} menu.", key_text(*key))
            }
            Self::AltGr(key) => format!(
                "{} types a character on many keyboards, because Ctrl+Alt is the same as AltGr.",
                key_text(*key)
            ),
            Self::NeedsModifier => {
                "A shortcut needs Ctrl or Alt, unless it is a function key.".into()
            }
            Self::OpensSubmenu(name) => {
                format!("\u{201c}{name}\u{201d} opens a submenu, so it takes no key.")
            }
        }
    }
}

const fn binding(virtual_key: u16, ctrl: bool, alt: bool, shift: bool) -> KeyBinding {
    KeyBinding::new(virtual_key, ctrl, alt, shift)
}

/// Keys that belong to Windows. Some reach the app, such as Alt+F4 and F10. Others
/// never do, such as Alt+Tab and Ctrl+Alt+Delete; they are listed so that a settings
/// file that names one is refused with a reason.
pub const WINDOWS_KEYS: &[KeyBinding] = &[
    binding(F4, false, true, false),
    binding(TAB, false, true, false),
    binding(TAB, false, true, true),
    binding(ESC, false, true, false),
    binding(ESC, false, true, true),
    binding(ESC, true, false, false),
    binding(ESC, true, false, true),
    binding(SPACE, false, true, false),
    binding(DELETE, true, true, false),
    binding(F10, false, false, false),
    binding(F10, false, false, true),
    binding(PRINT_SCREEN, false, false, false),
    binding(PRINT_SCREEN, false, true, false),
];

/// True for the keys that may stand alone: F1 to F24.
pub fn is_function_key(virtual_key: u16) -> bool {
    (0x70..=F24).contains(&virtual_key)
}

/// What the key does in the editor and in text fields, if it does anything.
pub fn text_editing_action(key: KeyBinding) -> Option<String> {
    let base = |text: &str| {
        if key.shift {
            text.replace("Move", "Select")
        } else {
            text.to_owned()
        }
    };
    if key.alt {
        return None;
    }
    if key.ctrl {
        return match key.virtual_key {
            LEFT => Some(base("Move Word Left")),
            RIGHT => Some(base("Move Word Right")),
            UP => Some(base("Move to Beginning of Paragraph")),
            DOWN => Some(base("Move to End of Paragraph")),
            HOME => Some(base("Move to Beginning of Document")),
            END => Some(base("Move to End of Document")),
            BACK if !key.shift => Some("Delete Word Backward".into()),
            DELETE if !key.shift => Some("Delete Word Forward".into()),
            INSERT if !key.shift => Some("Copy".into()),
            _ => None,
        };
    }
    if key.shift {
        return match key.virtual_key {
            INSERT => Some("Paste".into()),
            DELETE => Some("Cut".into()),
            _ => None,
        };
    }
    None
}

/// The rules for a key on its own, with no regard to other commands. In the Mac's
/// order: the system's keys, then editing keys, then the modifier rule.
pub fn kind_refusal(key: KeyBinding) -> Option<Refusal> {
    if WINDOWS_KEYS.contains(&key) {
        return Some(Refusal::Windows(key));
    }
    if let Some(action) = text_editing_action(key) {
        return Some(Refusal::TextEditing { key, action });
    }
    if key.alt
        && !key.ctrl
        && !key.shift
        && let Some(label) = MenuBar::mnemonic_label(key.virtual_key)
    {
        return Some(Refusal::MenuKey {
            key,
            menu: MENU_LABELS[label].to_owned(),
        });
    }
    if key.ctrl && key.alt && types_a_character(key.virtual_key) {
        return Some(Refusal::AltGr(key));
    }
    if !key.ctrl && !key.alt && !is_function_key(key.virtual_key) {
        return Some(Refusal::NeedsModifier);
    }
    None
}

/// Letters, digits and punctuation. AltGr can make any of them type a character.
fn types_a_character(virtual_key: u16) -> bool {
    matches!(virtual_key, 0x30..=0x39 | 0x41..=0x5a | 0xba..=0xc0 | 0xdb..=0xde | 0xe2)
}

/// A command's place in the menus, from its path in the command table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuPlace {
    pub menu: String,
    pub submenu: Option<String>,
    pub name: String,
}
/// Reads a path such as `Edit/Find/Find Next`. A name can hold a slash, as
/// `Smart Copy/Paste` does, so only the first two slashes split the path.
pub fn menu_place(command: CommandId) -> MenuPlace {
    let mut parts = command.menu_path().splitn(3, '/');
    let first = parts.next().unwrap_or_default().to_owned();
    let second = parts.next().unwrap_or_default().to_owned();
    match parts.next() {
        Some(rest) => MenuPlace {
            menu: first,
            submenu: Some(second),
            name: rest.to_owned(),
        },
        None => MenuPlace {
            menu: first,
            submenu: None,
            name: second,
        },
    }
}
/// The menus a clash message names, such as `Edit > Find`.
pub fn menu_path_text(command: CommandId) -> String {
    let place = menu_place(command);
    match place.submenu {
        Some(submenu) => format!("{} > {submenu}", place.menu),
        None => place.menu,
    }
}
/// A command that opens a submenu holds no key of its own: its path starts others.
pub fn opens_submenu(command: CommandId) -> bool {
    let path = command.menu_path();
    CommandId::ALL.iter().any(|other| {
        other
            .menu_path()
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}
/// One row of the Keyboard tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyRow {
    /// A top-level menu.
    Menu(String),
    /// A submenu with more than one command.
    Submenu(String),
    /// A command. `indented` when it is under a submenu row.
    Command {
        command: CommandId,
        name: String,
        indented: bool,
    },
}
/// Every command with a header row per menu and per submenu of more than one command,
/// as on the Mac. A command alone in its submenu names that submenu in its own row.
pub fn keyboard_rows() -> Vec<KeyRow> {
    let places: Vec<(CommandId, MenuPlace)> = CommandId::ALL
        .iter()
        .filter(|command| !opens_submenu(**command))
        .map(|command| (*command, menu_place(*command)))
        .collect();
    let mut menus: Vec<&str> = Vec::new();
    for (_, place) in &places {
        if !menus.contains(&place.menu.as_str()) {
            menus.push(&place.menu);
        }
    }
    let mut rows = Vec::with_capacity(places.len() + 16);
    for menu in menus {
        rows.push(KeyRow::Menu(menu.to_owned()));
        let mut submenu_done: Vec<&str> = Vec::new();
        for (command, place) in places.iter().filter(|(_, p)| p.menu == menu) {
            let Some(submenu) = &place.submenu else {
                rows.push(KeyRow::Command {
                    command: *command,
                    name: place.name.clone(),
                    indented: false,
                });
                continue;
            };
            if submenu_done.contains(&submenu.as_str()) {
                continue;
            }
            submenu_done.push(submenu);
            let inside: Vec<_> = places
                .iter()
                .filter(|(_, p)| p.menu == menu && p.submenu.as_ref() == Some(submenu))
                .collect();
            if let [(only, only_place)] = inside.as_slice() {
                rows.push(KeyRow::Command {
                    command: *only,
                    name: format!("{submenu} \u{203a} {}", only_place.name),
                    indented: false,
                });
                continue;
            }
            rows.push(KeyRow::Submenu(submenu.clone()));
            for (inner, inner_place) in inside {
                rows.push(KeyRow::Command {
                    command: *inner,
                    name: inner_place.name.clone(),
                    indented: true,
                });
            }
        }
    }
    rows
}

/// A command's stable name in the settings file. A menu can move, so its path is not
/// used here.
pub(crate) fn command_id_text(command: CommandId) -> String {
    format!("{command:?}")
}
/// The command with this stable name, and whether it only opens a submenu. One table, made
/// once, because a settings file can hold 1 MB of these lines and a lookup that formats
/// each command's name for each line took 200 ms at every launch.
pub(crate) fn command_by_text(text: &str) -> Option<(CommandId, bool)> {
    static TABLE: OnceLock<HashMap<String, (CommandId, bool)>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        // Every path that starts another path is a submenu.
        let mut submenus = HashSet::new();
        for command in CommandId::ALL {
            let path = command.menu_path();
            for (end, _) in path.match_indices('/') {
                submenus.insert(&path[..end]);
            }
        }
        CommandId::ALL
            .iter()
            .map(|command| {
                let opens = submenus.contains(command.menu_path());
                (command_id_text(*command), (*command, opens))
            })
            .collect()
    });
    table.get(text).copied()
}
/// Reads the saved form of a key: `key_text`'s spelling, such as `Ctrl+Shift+Page Up`.
pub fn parse_key(text: &str) -> Option<KeyBinding> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = text;
    loop {
        if let Some(after) = rest.strip_prefix("Ctrl+") {
            ctrl = true;
            rest = after;
        } else if let Some(after) = rest.strip_prefix("Alt+") {
            alt = true;
            rest = after;
        } else if let Some(after) = rest.strip_prefix("Shift+") {
            shift = true;
            rest = after;
        } else {
            break;
        }
    }
    named_key(rest).map(|code| KeyBinding::new(code, ctrl, alt, shift))
}
fn named_key(name: &str) -> Option<u16> {
    if let [only] = name.as_bytes() {
        return match *only {
            b'0'..=b'9' | b'A'..=b'Z' => Some(u16::from(*only)),
            b';' => Some(0xba),
            b'=' => Some(0xbb),
            b',' => Some(0xbc),
            b'-' => Some(0xbd),
            b'.' => Some(0xbe),
            b'/' => Some(0xbf),
            b'`' => Some(0xc0),
            b'[' => Some(0xdb),
            b'\\' => Some(0xdc),
            b']' => Some(0xdd),
            b'\'' => Some(0xde),
            _ => None,
        };
    }
    if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&number).then(|| 0x6f + number);
    }
    if let Some(number) = name.strip_prefix("Num ") {
        return match number {
            "*" => Some(0x6a),
            "+" => Some(0x6b),
            "-" => Some(0x6d),
            "." => Some(0x6e),
            "/" => Some(0x6f),
            "Lock" => Some(0x90),
            digit => digit
                .parse::<u16>()
                .ok()
                .filter(|d| *d < 10)
                .map(|d| 0x60 + d),
        };
    }
    if let Some(code) = name.strip_prefix("Key 0x") {
        return u16::from_str_radix(code, 16).ok().filter(|c| *c < 0x100);
    }
    Some(match name {
        "Backspace" => BACK,
        "Tab" => TAB,
        "Enter" => ENTER,
        "Pause" => 0x13,
        "Caps Lock" => 0x14,
        "Esc" => ESC,
        "Space" => SPACE,
        "Page Up" => 0x21,
        "Page Down" => 0x22,
        "End" => END,
        "Home" => HOME,
        "Left" => LEFT,
        "Up" => UP,
        "Right" => RIGHT,
        "Down" => DOWN,
        "Print Screen" => PRINT_SCREEN,
        "Insert" => INSERT,
        "Delete" => DELETE,
        "Menu" => 0x5d,
        "Scroll Lock" => 0x91,
        "OEM 102" => 0xe2,
        _ => return None,
    })
}
