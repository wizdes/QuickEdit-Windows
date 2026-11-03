//! The settings file: one UTF-8 text file with a line for each setting that differs
//! from its default, such as `WindowWidthCharacters=120`. Shortcuts are lines such as
//! `KeyboardShortcut.Find=Ctrl+G`, with `none` for no key. Comments, blank lines and
//! keys this version does not know stay as they are when the file is saved.
use crate::keys::{command_id_text, kind_refusal, opens_submenu, parse_key};
use qe_api::{CommandId, KeyBinding, SettingKey, SettingValue, TextEncoding};
use qe_ui::key_text;

pub(crate) const SHORTCUT_PREFIX: &str = "KeyboardShortcut.";
/// A larger file is not one this app wrote. It counts as broken and is not read in.
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
    /// Lines kept word for word: comments, blanks, unknown keys, unknown commands.
    pub kept: Vec<String>,
    /// What is wrong with lines that have a known key and an unusable value.
    pub bad_values: Vec<String>,
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
/// Reads the text. `Err` means the file as a whole cannot be read as settings.
pub(crate) fn parse(text: &str) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            parsed.kept.push(line.to_owned());
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("line {} has no \"=\"", number + 1));
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            return Err(format!("line {} has no name before \"=\"", number + 1));
        }
        if let Some(command) = name.strip_prefix(SHORTCUT_PREFIX) {
            parse_shortcut(line, command, value, &mut parsed);
            continue;
        }
        let Some(key) = SettingKey::ALL
            .iter()
            .copied()
            .find(|key| key.name() == name && *key != SettingKey::KeyboardShortcuts)
        else {
            parsed.kept.push(line.to_owned());
            continue;
        };
        if parsed.values.iter().any(|(seen, _)| *seen == key) {
            parsed.bad_values.push(format!("{name} is set twice"));
            continue;
        }
        match parse_value(key, value) {
            Ok(value) => parsed.values.push((key, value)),
            Err(problem) => parsed.bad_values.push(problem),
        }
    }
    Ok(parsed)
}
fn parse_shortcut(line: &str, command: &str, value: &str, parsed: &mut Parsed) {
    let Some(id) = CommandId::ALL
        .iter()
        .copied()
        .find(|id| command_id_text(*id) == command)
    else {
        parsed.kept.push(line.to_owned());
        return;
    };
    if opens_submenu(id) {
        parsed
            .bad_values
            .push(format!("{command} opens a submenu, so it takes no key"));
        return;
    }
    if parsed.keys.iter().any(|(seen, _)| *seen == id) {
        parsed.bad_values.push(format!("{command} has two keys"));
        return;
    }
    if value == "none" {
        parsed.keys.push((id, None));
        return;
    }
    let Some(key) = parse_key(value) else {
        parsed
            .bad_values
            .push(format!("{command}: {value:?} is not a key"));
        return;
    };
    // A command's own default is always allowed. Windows rules cover only a change.
    if id.default_key() != Some(key)
        && let Some(refused) = kind_refusal(key)
    {
        parsed
            .bad_values
            .push(format!("{command}: {}", refused.message()));
        return;
    }
    if let Some((other, _)) = parsed.keys.iter().find(|(_, seen)| *seen == Some(key)) {
        parsed.bad_values.push(format!(
            "{command} and {} both name {}",
            command_id_text(*other),
            key_text(key)
        ));
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
/// The file's text. Known settings keep the table's order, so a save is stable.
pub(crate) fn encode(
    values: &[(SettingKey, StoredValue)],
    keys: &[(CommandId, Option<KeyBinding>)],
    kept: &[String],
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
            match key {
                Some(key) => out.push_str(&key_text(*key)),
                None => out.push_str("none"),
            }
            out.push_str("\r\n");
        }
    }
    for line in kept {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}
