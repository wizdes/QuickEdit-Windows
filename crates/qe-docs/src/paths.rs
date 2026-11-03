//! Which paths may become documents. A path from another launch, a drop or a dialog is only
//! text until it passes here, and nothing here touches a disk or a network.
use crate::error::{DocError, DocErrorKind};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf, Prefix};

/// The names Windows keeps for devices in every folder, whatever the extension.
const DEVICE_NAMES: [&str; 8] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$", "CONFIG$",
];

/// The shares that hold pipes and mailslots, not files.
const DEVICE_SHARES: [&str; 3] = ["PIPE", "MAILSLOT", "IPC$"];

fn is_port_name(stem: &str) -> bool {
    let upper = stem.to_uppercase();
    let Some(rest) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    else {
        return false;
    };
    // Windows also takes the superscript digits 1, 2 and 3 here.
    let mut digits = rest.chars();
    matches!(
        digits.next(),
        Some('1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}')
    ) && digits.next().is_none()
}

fn is_device_name(part: &OsStr) -> bool {
    let text = part.to_string_lossy();
    let stem = text.split('.').next().unwrap_or(&text).trim_end();
    DEVICE_NAMES.iter().any(|d| stem.eq_ignore_ascii_case(d)) || is_port_name(stem)
}

fn refused(path: &Path, why: &str) -> DocError {
    DocError::new(
        DocErrorKind::PathRefused,
        Some(path),
        format!("QuickEdit cannot open {}: {why}.", path.display()),
    )
}

/// True for a path on a network share. Its reads can wait for a long time on the network.
pub(crate) fn is_network_path(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}

/// The absolute form of a path that names a file on a local drive or a network share.
/// Anything else gets a `PathRefused` error: a device, a `\\.\` or `\\?\GLOBALROOT` name,
/// an alternate data stream, a pipe share. It reads no disk and no network.
pub fn check_open_path(raw: &Path) -> Result<PathBuf, DocError> {
    let path =
        std::path::absolute(raw).map_err(|_| refused(raw, "this is not a complete file path"))?;
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Err(refused(&path, "this is not a path on a drive or a share"));
    };
    let share = match prefix.kind() {
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) => None,
        Prefix::UNC(_, share) | Prefix::VerbatimUNC(_, share) => Some(share),
        Prefix::DeviceNS(_) | Prefix::Verbatim(_) => {
            return Err(refused(&path, "this names a device, not a file"));
        }
    };
    if let Some(share) = share {
        let share = share.to_string_lossy();
        if DEVICE_SHARES.iter().any(|d| share.eq_ignore_ascii_case(d)) {
            return Err(refused(&path, "this share holds pipes, not files"));
        }
    }
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(refused(&path, "this is not a complete file path"));
    }
    let mut last = None;
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(refused(&path, "this is not a plain file path"));
        };
        if name.to_string_lossy().contains(':') {
            return Err(refused(&path, "this names a stream inside a file"));
        }
        if is_device_name(name) {
            return Err(refused(&path, "this names a device, not a file"));
        }
        last = Some(name);
    }
    if last.is_none() {
        return Err(refused(&path, "this names no file"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_network_files_pass_and_stay_as_they_were() {
        for good in [
            r"C:\a\b.txt",
            r"D:\console\nul-not.txt",
            r"\\host\share\a.txt",
            r"\\?\C:\long\a.txt",
            r"\\?\UNC\host\share\a.txt",
            r"C:\com10.txt",
            r"C:\a\console.txt",
        ] {
            assert_eq!(check_open_path(Path::new(good)).unwrap(), Path::new(good));
        }
    }

    #[test]
    fn a_relative_path_is_made_absolute_from_this_folder() {
        let made = check_open_path(Path::new("a.txt")).unwrap();
        assert!(made.is_absolute(), "{made:?}");
    }

    #[test]
    fn devices_streams_and_odd_roots_are_refused_by_name() {
        for bad in [
            "",
            r"\\.\NUL",
            r"\\.\pipe\QuickEdit",
            r"\\.\C:\a.txt",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\a.txt",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\a.txt",
            r"C:\a.txt:hidden",
            r"C:\a.txt::$DATA",
            r"C:\folder\NUL",
            r"C:\folder\nul.txt",
            r"C:\folder\COM1.txt",
            r"C:\folder\lpt\u{b9}",
            r"C:\folder\CON .txt",
            r"C:\folder\CONIN$",
            r"\\localhost\pipe\QuickEdit",
            r"\\localhost\IPC$\x",
            r"\\host\mailslot\x",
            r"C:\",
        ] {
            let bad = bad.replace(r"\u{b9}", "\u{b9}");
            let error = check_open_path(Path::new(&bad)).unwrap_err();
            assert_eq!(error.kind, DocErrorKind::PathRefused, "{bad}: {error}");
            assert!(error.message.contains("cannot open"), "{}", error.message);
        }
    }

    #[test]
    fn a_network_path_is_told_apart() {
        assert!(is_network_path(Path::new(r"\\host\share\a.txt")));
        assert!(is_network_path(Path::new(r"\\?\UNC\host\share\a.txt")));
        assert!(!is_network_path(Path::new(r"C:\a.txt")));
        assert!(!is_network_path(Path::new(r"\\?\C:\a.txt")));
    }
}
