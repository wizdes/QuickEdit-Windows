//! A PDF saved over an existing file keeps that file's access rules, as a normal save does.
use qe_api::testing::StringStore;
use qe_pdf::{PdfOptions, export_to_file};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

const NO_WINDOW: u32 = 0x0800_0000;

struct Folder(PathBuf);
impl Folder {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("qe-pdf-{name}-{}", std::process::id()));
        // A folder left by an earlier stopped run must not change this one's result.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        // A failed removal only leaves a folder in temp, and must not hide the result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn icacls(path: &Path, what: &[&str]) -> String {
    let output = Command::new("icacls")
        .arg(path)
        .args(what)
        .creation_flags(NO_WINDOW)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The rule lines for one file, with its own name taken off the first.
fn rules(path: &Path) -> Vec<String> {
    let text = icacls(path, &[]);
    let name = path.display().to_string();
    text.lines()
        .map(|line| line.replace(&name, "").trim().to_owned())
        .filter(|line| !line.is_empty() && !line.starts_with("Successfully"))
        .collect()
}

fn export(path: &Path) {
    export_to_file(
        &StringStore::new("words"),
        &PdfOptions::default(),
        path,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
}

#[test]
fn a_pdf_over_a_restricted_file_keeps_its_rules() {
    let folder = Folder::new("rules");
    let target = folder.0.join("private.pdf");
    std::fs::write(&target, b"the old file").unwrap();
    let user = std::env::var("USERNAME").unwrap();
    icacls(
        &target,
        &["/inheritance:r", "/grant:r", &format!("{user}:(F)")],
    );
    let before = rules(&target);
    assert_eq!(before.len(), 1, "one rule for this test: {before:?}");

    export(&target);

    assert!(std::fs::read(&target).unwrap().starts_with(b"%PDF"));
    assert_eq!(rules(&target), before, "the PDF took the folder's rules");
    let left: Vec<_> = std::fs::read_dir(&folder.0).unwrap().collect();
    assert_eq!(left.len(), 1, "a temp file stayed: {left:?}");
}

#[test]
fn a_new_pdf_takes_the_rules_of_its_folder() {
    let folder = Folder::new("new");
    let target = folder.0.join("new.pdf");
    export(&target);
    let beside = folder.0.join("plain.txt");
    std::fs::write(&beside, b"x").unwrap();
    assert_eq!(rules(&target), rules(&beside));
}

#[test]
fn a_pdf_with_a_very_long_path_still_saves() {
    let folder = Folder::new("long");
    let mut deep = folder.0.clone();
    for _ in 0..6 {
        deep = deep.join("a-folder-name-of-fifty-characters-for-a-long-path0");
    }
    std::fs::create_dir_all(&deep).unwrap();
    let target = deep.join("out.pdf");
    assert!(target.as_os_str().len() > 260);
    export(&target);
    assert!(std::fs::read(&target).unwrap().starts_with(b"%PDF"));
    export(&target);
}

/// The built-in Guests group by its id, which reads the same in every language of Windows.
const GUESTS_ADD: &str = "*S-1-5-32-546:(OI)(CI)(R)";
const GUESTS_ID: &str = "*S-1-5-32-546";

fn names_guests(path: &Path) -> bool {
    rules(path).iter().any(|rule| rule.contains("Guests"))
}

/// A file whose rules come from its folder keeps following that folder after a PDF replaces it.
/// The plain file beside it shows what the folder's change does to a file that never saw a PDF.
#[test]
fn a_pdf_over_a_file_that_inherits_keeps_following_its_folder() {
    let folder = Folder::new("follows");
    let target = folder.0.join("inherits.pdf");
    let plain = folder.0.join("plain.txt");
    std::fs::write(&target, b"the old file").unwrap();
    std::fs::write(&plain, b"x").unwrap();
    assert!(!names_guests(&target), "the fixture must start without it");

    export(&target);

    assert_eq!(
        rules(&target),
        rules(&plain),
        "the export changed the rules"
    );
    icacls(&folder.0, &["/grant", GUESTS_ADD]);
    assert!(
        names_guests(&plain),
        "the folder's new rule reached no file"
    );
    assert!(
        names_guests(&target),
        "the PDF stopped following: {:?}",
        rules(&target)
    );

    // A rule taken away from the folder leaves too, so the PDF's rules are truly inherited.
    icacls(&folder.0, &["/remove", GUESTS_ID]);
    assert!(!names_guests(&plain));
    assert!(
        !names_guests(&target),
        "the old rule stayed: {:?}",
        rules(&target)
    );
}

/// A file with its inheritance removed stays out of the folder's reach after a PDF replaces it.
#[test]
fn a_pdf_over_a_file_with_inheritance_removed_stays_protected() {
    let folder = Folder::new("protected");
    let target = folder.0.join("private.pdf");
    std::fs::write(&target, b"the old file").unwrap();
    let user = std::env::var("USERNAME").unwrap();
    icacls(
        &target,
        &["/inheritance:r", "/grant:r", &format!("{user}:(F)")],
    );

    export(&target);

    icacls(&folder.0, &["/grant", GUESTS_ADD]);
    assert!(
        !names_guests(&target),
        "the folder reached it: {:?}",
        rules(&target)
    );
}
