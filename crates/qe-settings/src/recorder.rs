//! What one key event means while a shortcut is being recorded.
use qe_api::{KeyBinding, Modifiers};

const ESC: u16 = 0x1b;
const BACK: u16 = 0x08;

/// What a key press means to the recorder, as `ShortcutRecorder.Outcome` on the Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordStep {
    /// Esc with no modifiers: stop, change nothing.
    Cancel,
    /// Delete or Backspace with no modifiers: the command gets no shortcut.
    Remove,
    /// A key with its modifiers, to be checked and saved.
    Candidate(KeyBinding),
    /// A Windows key is down. Nothing can take that chord.
    WindowsKey(KeyBinding),
    /// Only modifiers are down. They show in the row while held.
    Held(Modifiers),
    /// A key that cannot name a shortcut, or the repeat of a held key.
    Ignore,
}
fn is_modifier(key: u16) -> bool {
    matches!(key, 0x10 | 0x11 | 0x12 | 0x5b | 0x5c | 0xa0..=0xa5)
}
/// Lock keys, input method keys, media keys and the key code that carries pasted text.
fn cannot_name_a_shortcut(key: u16) -> bool {
    matches!(key, 0x14 | 0x90 | 0x91 | 0x15..=0x1a | 0x1c..=0x1f | 0xa6..=0xb7 | 0xe5 | 0xe7 | 0xff)
}
/// A key's meaning. A repeat means nothing: the Enter that began the recording may
/// still be held, and it must not become the new shortcut.
pub fn record_step(virtual_key: u16, modifiers: Modifiers, repeat: bool) -> RecordStep {
    if is_modifier(virtual_key) {
        return RecordStep::Held(modifiers);
    }
    if repeat || cannot_name_a_shortcut(virtual_key) {
        return RecordStep::Ignore;
    }
    let key = KeyBinding::new(virtual_key, modifiers.ctrl, modifiers.alt, modifiers.shift);
    if modifiers.win {
        return RecordStep::WindowsKey(key);
    }
    if !modifiers.ctrl && !modifiers.alt && !modifiers.shift {
        match virtual_key {
            ESC => return RecordStep::Cancel,
            BACK | crate::keys::DELETE => return RecordStep::Remove,
            _ => {}
        }
    }
    RecordStep::Candidate(key)
}
/// The modifiers held, as the row shows them while they are down.
pub fn held_text(modifiers: Modifiers) -> String {
    let mut text = String::new();
    if modifiers.ctrl {
        text.push_str("Ctrl+");
    }
    if modifiers.alt {
        text.push_str("Alt+");
    }
    if modifiers.shift {
        text.push_str("Shift+");
    }
    if modifiers.win {
        text.push_str("Win+");
    }
    text
}
