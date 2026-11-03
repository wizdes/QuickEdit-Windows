//! Writes any Windows path or name on one line of a UTF-8 file. A name that is not valid
//! Unicode survives, because each unpaired unit becomes `%uXXXX`.
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub(crate) fn escape_units(units: impl IntoIterator<Item = u16>) -> String {
    let mut out = String::new();
    for item in char::decode_utf16(units) {
        match item {
            Ok('%') => out.push_str("%25"),
            Ok('\n') => out.push_str("%0A"),
            Ok('\r') => out.push_str("%0D"),
            Ok(' ') => out.push_str("%20"),
            Ok('=') => out.push_str("%3D"),
            Ok(c) => out.push(c),
            Err(bad) => out.push_str(&format!("%u{:04X}", bad.unpaired_surrogate())),
        }
    }
    out
}
pub(crate) fn escape_text(text: &str) -> String {
    escape_units(text.encode_utf16())
}
pub(crate) fn escape_path(path: &Path) -> String {
    escape_units(path.as_os_str().encode_wide())
}
pub(crate) fn unescape_units(text: &str) -> Option<Vec<u16>> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.extend(rest.get(..at)?.encode_utf16());
        let tail = rest.get(at + 1..)?;
        if let Some(hex) = tail.strip_prefix('u') {
            out.push(u16::from_str_radix(hex.get(..4)?, 16).ok()?);
            rest = hex.get(4..)?;
        } else {
            let byte = u8::from_str_radix(tail.get(..2)?, 16).ok()?;
            out.push(u16::from(byte));
            rest = tail.get(2..)?;
        }
    }
    out.extend(rest.encode_utf16());
    Some(out)
}
pub(crate) fn unescape_text(text: &str) -> Option<String> {
    String::from_utf16(&unescape_units(text)?).ok()
}
pub(crate) fn unescape_path(text: &str) -> Option<PathBuf> {
    Some(PathBuf::from(OsString::from_wide(&unescape_units(text)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_name_that_is_not_unicode_and_odd_characters_survive() {
        let units = [0x43, 0x3a, 0x5c, 0xd800, 0x25, 0x20, 0x3d, 0x0a, 0x4e2d];
        let path = PathBuf::from(OsString::from_wide(&units));
        let line = escape_path(&path);
        assert!(!line.contains(' ') && !line.contains('\n') && !line.contains('='));
        assert_eq!(unescape_path(&line).unwrap(), path);
        assert_eq!(
            unescape_text(&escape_text("a b=c%\u{1f600}")).unwrap(),
            "a b=c%\u{1f600}"
        );
        assert!(unescape_units("%u12").is_none());
        assert!(unescape_units("%zz").is_none());
    }
}
