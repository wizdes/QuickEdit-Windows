//! The Open Recent list: `recent.txt`, next to the settings file. Newest first.
use crate::escape::{escape_path, unescape_path};
use std::path::{Path, PathBuf};

/// AppKit's list holds 10 unless the Mac's own setting says otherwise.
pub const RECENT_LIMIT: usize = 10;
pub(crate) const RECENT_FILE: &str = "recent.txt";

pub(crate) fn add_recent(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|old| !same_path_text(old, path));
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT_LIMIT);
}

pub(crate) fn same_path_text(a: &Path, b: &Path) -> bool {
    a == b || a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

pub(crate) fn encode_recent(list: &[PathBuf]) -> String {
    let mut out = String::from("QuickEditRecent 1\n");
    for path in list {
        out.push_str(&escape_path(path));
        out.push('\n');
    }
    out
}

/// A line that cannot be read is dropped and counted, so the caller can say so.
pub(crate) fn decode_recent(text: &str) -> (Vec<PathBuf>, usize) {
    let mut lines = text.lines();
    if lines.next() != Some("QuickEditRecent 1") {
        return (Vec::new(), usize::from(!text.is_empty()));
    }
    let (mut list, mut bad) = (Vec::new(), 0);
    for line in lines {
        match unescape_path(line) {
            Some(path) if list.len() < RECENT_LIMIT => list.push(path),
            Some(_) => {}
            None => bad += 1,
        }
    }
    (list, bad)
}

/// The rows for `qe_ui::set_recent_files`: label and path. A name that appears twice shows
/// its folder too, as AppKit does.
pub fn recent_menu_rows(list: &[PathBuf]) -> Vec<(String, String)> {
    let name = |path: &Path| {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    list.iter()
        .map(|path| {
            let shown = name(path);
            let twice = list.iter().filter(|other| name(other) == shown).count() > 1;
            let label = match path.parent().filter(|_| twice).and_then(Path::file_name) {
                Some(folder) => format!("{shown} \u{2014} {}", folder.to_string_lossy()),
                None => shown,
            };
            (label, path.to_string_lossy().into_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newest_first_no_repeats_capped_and_it_survives_a_round_trip() {
        let mut list = Vec::new();
        for i in 0..14 {
            add_recent(&mut list, Path::new(&format!(r"C:\f\{i}.txt")));
        }
        add_recent(&mut list, Path::new(r"C:\F\10.TXT"));
        assert_eq!(list.len(), RECENT_LIMIT);
        assert_eq!(list[0], Path::new(r"C:\F\10.TXT"));
        let tens = list
            .iter()
            .filter(|p| p.to_string_lossy().to_lowercase().ends_with("10.txt"))
            .count();
        assert_eq!(tens, 1);
        let (back, bad) = decode_recent(&encode_recent(&list));
        assert_eq!((back, bad), (list, 0));
    }
    #[test]
    fn a_damaged_list_is_counted_and_not_silently_dropped() {
        assert_eq!(decode_recent("nonsense").1, 1);
        assert_eq!(decode_recent("").1, 0);
        let (list, bad) = decode_recent("QuickEditRecent 1\nC:\\a.txt\n%zz\n");
        assert_eq!((list.len(), bad), (1, 1));
    }
    #[test]
    fn the_same_name_in_two_folders_shows_its_folder() {
        let rows = recent_menu_rows(&[
            PathBuf::from(r"C:\one\a.txt"),
            PathBuf::from(r"C:\two\a.txt"),
            PathBuf::from(r"C:\two\b.txt"),
        ]);
        assert_eq!(rows[0].0, "a.txt \u{2014} one");
        assert_eq!(rows[1].0, "a.txt \u{2014} two");
        assert_eq!(rows[2].0, "b.txt");
    }
}
