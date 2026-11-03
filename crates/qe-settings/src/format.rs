//! The settings file: one UTF-8 text file with a line for each setting that differs
//! from its default, such as `WindowWidthCharacters=120`. Shortcuts are lines such as
//! `KeyboardShortcut.Find=Ctrl+G`, with `none` for no key. A save edits the file in place:
//! a setting changes its own line, and every other line stays where it is, word for word.
use crate::keys::{command_by_text, command_id_text, kind_refusal, parse_key};
use qe_api::{CommandId, KeyBinding, SettingKey, SettingValue, TextEncoding};
use qe_ui::key_text;
use std::collections::HashMap;

pub(crate) const SHORTCUT_PREFIX: &str = "KeyboardShortcut.";
/// A larger file is not read in, and no save makes one: the save is refused instead.
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// A setting's value, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredValue {
    Bool(bool),
    Integer(i64),
    Text(String),
}
pub(crate) fn default_of(key: SettingKey) -> StoredValue {
    match key.default_value() {
        SettingValue::Bool(value) => StoredValue::Bool(value),
        SettingValue::Integer(value) => StoredValue::Integer(value),
        SettingValue::Text(value) => StoredValue::Text(value.to_owned()),
    }
}
/// The whole numbers each integer setting takes. The size fields' 1 to 10000 is the
/// Mac's. Tab width and the autosave delay have no control on either system, so their
/// limits only keep a hand-edited file from breaking the app.
pub fn integer_range(key: SettingKey) -> Option<(i64, i64)> {
    match key {
        SettingKey::WindowWidthCharacters | SettingKey::WindowHeightLines => Some((1, 10_000)),
        SettingKey::TabWidth => Some((1, 64)),
        SettingKey::AutosavingDelaySeconds => Some((0, 86_400)),
        _ => None,
    }
}
/// The name of the plain font and its size in points, from `Cascadia Mono 10`.
pub fn parse_font(text: &str) -> Option<(&str, f32)> {
    let (family, size) = text.rsplit_once(' ')?;
    let points = size.parse::<f32>().ok()?;
    (!family.trim().is_empty() && points.is_finite() && (1.0..=400.0).contains(&points))
        .then_some((family, points))
}
pub(crate) fn encoding_by_name(name: &str) -> Option<TextEncoding> {
    TextEncoding::ALL.iter().copied().find(|e| e.name() == name)
}
/// Why this value cannot go in this setting, or `None`.
pub(crate) fn value_problem(key: SettingKey, value: &StoredValue) -> Option<String> {
    if std::mem::discriminant(value) != std::mem::discriminant(&default_of(key)) {
        return Some(format!("{} takes another type of value", key.name()));
    }
    match (key, value) {
        (_, StoredValue::Integer(number)) => integer_range(key).and_then(|(low, high)| {
            (*number < low || *number > high)
                .then(|| format!("{} must be a whole number from {low} to {high}", key.name()))
        }),
        (_, StoredValue::Text(text)) if text.chars().any(char::is_control) => {
            Some(format!("{} cannot hold a line break", key.name()))
        }
        (SettingKey::PlainTextFont, StoredValue::Text(text)) => parse_font(text)
            .is_none()
            .then(|| format!("{text:?} is not a font name followed by a size from 1 to 400")),
        (SettingKey::OpenEncoding | SettingKey::SaveEncoding, StoredValue::Text(text)) => {
            (text != "Automatic" && encoding_by_name(text).is_none())
                .then(|| format!("{text:?} is not an encoding"))
        }
        (SettingKey::EnabledEncodings, StoredValue::Text(text)) => text
            .split(',')
            .find(|name| !name.is_empty() && encoding_by_name(name).is_none())
            .map(|name| format!("{name:?} is not an encoding")),
        (SettingKey::PaperSize, StoredValue::Text(text)) => {
            (text != "Letter" && text != "A4").then(|| format!("{text:?} is not Letter or A4"))
        }
        _ => None,
    }
}

/// A file read into its parts.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub values: Vec<(SettingKey, StoredValue)>,
    pub keys: Vec<(CommandId, Option<KeyBinding>)>,
    /// A shortcut line for a command this version does not know is in the file.
    pub unknown_shortcut: bool,
    /// What is wrong with lines: no `=`, or a known key and an unusable value.
    bad_values: Vec<String>,
    /// How many more lines are wrong. The message has room for only a few.
    more_bad: usize,
}
/// A 1 MB file can have hundreds of thousands of bad lines. The message holds this many.
const MAX_BAD_VALUES: usize = 16;
impl Parsed {
    fn bad(&mut self, problem: impl FnOnce() -> String) {
        if self.bad_values.len() < MAX_BAD_VALUES {
            self.bad_values.push(problem());
        } else {
            self.more_bad += 1;
        }
    }
    /// What is wrong with the file's lines, or `None`.
    pub fn problem(&self) -> Option<String> {
        if self.bad_values.is_empty() {
            return None;
        }
        let mut text = self.bad_values.join("; ");
        if self.more_bad > 0 {
            text.push_str(&format!("; and {} more", self.more_bad));
        }
        Some(text)
    }
    /// Nothing in the file could be used.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.keys.is_empty()
    }
}
fn parse_value(key: SettingKey, raw: &str) -> Result<StoredValue, String> {
    let value = match default_of(key) {
        StoredValue::Bool(_) => match raw {
            "true" => StoredValue::Bool(true),
            "false" => StoredValue::Bool(false),
            _ => return Err(format!("{}: {raw:?} is not true or false", key.name())),
        },
        StoredValue::Integer(_) => match raw.parse::<i64>() {
            Ok(number) => StoredValue::Integer(number),
            Err(_) => return Err(format!("{}: {raw:?} is not a whole number", key.name())),
        },
        StoredValue::Text(_) => StoredValue::Text(raw.to_owned()),
    };
    match value_problem(key, &value) {
        Some(problem) => Err(problem),
        None => Ok(value),
    }
}
/// The name and the value on a line that sets something. A blank line, a comment or a
/// line with no name sets nothing.
fn line_parts(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (name, value) = trimmed.split_once('=')?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name, value.trim()))
}
/// Reads the text. A line that cannot be read costs only itself, because a save leaves it
/// where it is and the lines around it still count.
pub(crate) fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line_parts(line) else {
            parsed.bad(|| {
                if trimmed.contains('=') {
                    format!("line {} has no name before \"=\"", number + 1)
                } else {
                    format!("line {} has no \"=\"", number + 1)
                }
            });
            continue;
        };
        if let Some(command) = name.strip_prefix(SHORTCUT_PREFIX) {
            parse_shortcut(command, value, &mut parsed);
            continue;
        }
        let Some(key) = SettingKey::ALL
            .iter()
            .copied()
            .find(|key| key.name() == name && *key != SettingKey::KeyboardShortcuts)
        else {
            continue;
        };
        if parsed.values.iter().any(|(seen, _)| *seen == key) {
            parsed.bad(|| format!("{name} is set twice"));
            continue;
        }
        match parse_value(key, value) {
            Ok(value) => parsed.values.push((key, value)),
            Err(problem) => parsed.bad(|| problem),
        }
    }
    parsed
}
fn parse_shortcut(command: &str, value: &str, parsed: &mut Parsed) {
    let Some((id, opens_submenu)) = command_by_text(command) else {
        parsed.unknown_shortcut = true;
        return;
    };
    if opens_submenu {
        parsed.bad(|| format!("{command} opens a submenu, so it takes no key"));
        return;
    }
    if parsed.keys.iter().any(|(seen, _)| *seen == id) {
        parsed.bad(|| format!("{command} has two keys"));
        return;
    }
    if value == "none" {
        parsed.keys.push((id, None));
        return;
    }
    let Some(key) = parse_key(value) else {
        parsed.bad(|| format!("{command}: {value:?} is not a key"));
        return;
    };
    // A command's own default is always allowed. Windows rules cover only a change.
    if id.default_key() != Some(key)
        && let Some(refused) = kind_refusal(key)
    {
        parsed.bad(|| format!("{command}: {}", refused.message()));
        return;
    }
    if let Some((other, _)) = parsed.keys.iter().find(|(_, seen)| *seen == Some(key)) {
        let other = *other;
        parsed.bad(|| {
            format!(
                "{command} and {} both name {}",
                command_id_text(other),
                key_text(key)
            )
        });
        return;
    }
    parsed.keys.push((id, Some(key)));
}
fn value_text(value: &StoredValue) -> String {
    match value {
        StoredValue::Bool(value) => value.to_string(),
        StoredValue::Integer(value) => value.to_string(),
        StoredValue::Text(value) => value.clone(),
    }
}
/// One change this store has made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    Setting(SettingKey, StoredValue),
    Key(CommandId, Option<KeyBinding>),
}
impl Edit {
    fn line_name(&self) -> String {
        match self {
            Self::Setting(key, _) => key.name().to_owned(),
            Self::Key(command, _) => format!("{SHORTCUT_PREFIX}{}", command_id_text(*command)),
        }
    }
    /// The value for the file, or `None` when it is the default and the line must go.
    fn line_value(&self) -> Option<String> {
        match self {
            Self::Setting(key, value) => (*value != default_of(*key)).then(|| value_text(value)),
            Self::Key(command, key) => (*key != command.default_key()).then(|| key_line_text(*key)),
        }
    }
    fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Setting(a, _), Self::Setting(b, _)) => a == b,
            (Self::Key(a, _), Self::Key(b, _)) => a == b,
            _ => false,
        }
    }
}
fn key_line_text(key: Option<KeyBinding>) -> String {
    key.map_or_else(|| "none".to_owned(), key_text)
}
/// The changes that have not reached the disk. Each has a number, so a save that took a
/// snapshot clears only what it held, and a newer change stays.
#[derive(Clone, Debug, Default)]
pub(crate) struct Unsaved {
    edits: Vec<(u64, Edit)>,
    /// Restore Keyboard Defaults, not saved yet: every shortcut line goes.
    drop_shortcuts: Option<u64>,
    last: u64,
}
impl Unsaved {
    pub fn add(&mut self, edit: Edit) {
        self.last += 1;
        self.edits.retain(|(_, seen)| !seen.same_target(&edit));
        self.edits.push((self.last, edit));
    }
    pub fn drop_all_shortcuts(&mut self) {
        self.last += 1;
        self.edits
            .retain(|(_, seen)| !matches!(seen, Edit::Key(..)));
        self.drop_shortcuts = Some(self.last);
    }
    pub fn last(&self) -> u64 {
        self.last
    }
    pub fn clear_through(&mut self, last: u64) {
        self.edits.retain(|(number, _)| *number > last);
        if self.drop_shortcuts.is_some_and(|number| number <= last) {
            self.drop_shortcuts = None;
        }
    }
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.drop_shortcuts.is_none()
    }
    pub fn holds_setting(&self, key: SettingKey) -> bool {
        self.edits
            .iter()
            .any(|(_, edit)| matches!(edit, Edit::Setting(seen, _) if *seen == key))
    }
    pub fn holds_key(&self, command: CommandId) -> bool {
        self.edits
            .iter()
            .any(|(_, edit)| matches!(edit, Edit::Key(seen, _) if *seen == command))
    }
    /// Puts these changes over what a file holds, for memory after a save found new lines.
    pub fn apply_to(&self, parsed: &mut Parsed) {
        if self.drop_shortcuts.is_some() {
            parsed.keys.clear();
            parsed.unknown_shortcut = false;
        }
        for (_, edit) in &self.edits {
            match edit {
                Edit::Setting(key, value) => {
                    parsed.values.retain(|(seen, _)| seen != key);
                    parsed.values.push((*key, value.clone()));
                }
                Edit::Key(command, key) => {
                    parsed.keys.retain(|(seen, _)| seen != command);
                    parsed.keys.push((*command, *key));
                }
            }
        }
    }
}
/// The text with these changes made. A setting changes its own line and keeps that line's
/// end. A setting back at its default loses its line. A new one goes at the end. Any other
/// line stays where it is, word for word.
pub(crate) fn apply_edits(text: &str, unsaved: &Unsaved) -> String {
    /// One setting's change, and whether its line has been met yet.
    struct Wanted {
        value: Option<String>,
        met: bool,
    }
    let mut wanted: HashMap<String, Wanted> = unsaved
        .edits
        .iter()
        .map(|(_, edit)| {
            let value = edit.line_value();
            (edit.line_name(), Wanted { value, met: false })
        })
        .collect();
    let drop_shortcuts = unsaved.drop_shortcuts.is_some();
    let mut out = String::with_capacity(text.len() + 64);
    for whole in text.split_inclusive('\n') {
        let line = whole.trim_end_matches(['\r', '\n']);
        let Some((name, value)) = line_parts(line) else {
            out.push_str(whole);
            continue;
        };
        let Some(change) = wanted.get_mut(name) else {
            if !(drop_shortcuts && name.starts_with(SHORTCUT_PREFIX)) {
                out.push_str(whole);
            }
            continue;
        };
        // A second line for the same setting would be a problem at the next launch.
        if std::mem::replace(&mut change.met, true) {
            continue;
        }
        match &change.value {
            None => {}
            Some(new) if new == value => out.push_str(whole),
            Some(new) => {
                out.push_str(name);
                out.push('=');
                out.push_str(new);
                let end = &whole[line.len()..];
                out.push_str(if end.is_empty() { "\r\n" } else { end });
            }
        }
    }
    for (_, edit) in &unsaved.edits {
        let name = edit.line_name();
        let (Some(change), Some(value)) = (wanted.get(&name), edit.line_value()) else {
            continue;
        };
        if !change.met {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str("\r\n");
            }
            out.push_str(&name);
            out.push('=');
            out.push_str(&value);
            out.push_str("\r\n");
        }
    }
    out
}
/// The whole text of a file made from memory alone, for a file that is gone or that holds
/// no lines to edit. Known settings keep the table's order.
pub(crate) fn encode(
    values: &[(SettingKey, StoredValue)],
    keys: &[(CommandId, Option<KeyBinding>)],
) -> String {
    let mut out = String::with_capacity(256);
    for key in SettingKey::ALL {
        if let Some((_, value)) = values.iter().find(|(seen, _)| seen == key) {
            out.push_str(key.name());
            out.push('=');
            out.push_str(&value_text(value));
            out.push_str("\r\n");
        }
    }
    for command in CommandId::ALL {
        if let Some((_, key)) = keys.iter().find(|(seen, _)| seen == command) {
            out.push_str(SHORTCUT_PREFIX);
            out.push_str(&command_id_text(*command));
            out.push('=');
            out.push_str(&key_line_text(*key));
            out.push_str("\r\n");
        }
    }
    out
}
