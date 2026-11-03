//! Saves against the real file system. Tests that need to force a Windows failure
//! at one exact step are in `src/save.rs`.
use qe_codec::{
    AtomicWriteResult, CodecError, LeftoverKind, atomic_write, copy_large_original, find_leftovers,
    prepare_large_copy,
};
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetFileSecurityW, PSECURITY_DESCRIPTOR, SetFileSecurityW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qe-save-{name}-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
/// The rules alone. ReplaceFileW adds an "auto-inherited" mark (AI) to a protected
/// list, which changes no access. The list must stay protected (P).
fn protected_rules(sddl: &str) -> &str {
    let (flags, rules) = sddl.split_at(sddl.find('(').unwrap());
    assert!(flags.contains('P'), "not protected: {sddl}");
    rules
}
/// Sets a file's access rules from SDDL, the text form of those rules.
fn set_rules(path: &Path, sddl: &str) {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let name = HSTRING::from(path.to_str().unwrap());
    // SAFETY: the strings outlive the calls, and the descriptor is freed once.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .unwrap();
        assert!(SetFileSecurityW(&name, DACL_SECURITY_INFORMATION, descriptor).as_bool());
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
}
/// A file's access rules as SDDL.
fn rules_of(path: &Path) -> String {
    let name = HSTRING::from(path.to_str().unwrap());
    let mut needed = 0;
    // SAFETY: the buffer is at least as long as the call is told, the string the last
    // call returns is read before it is freed, and it is freed once.
    unsafe {
        let _ = GetFileSecurityW(&name, DACL_SECURITY_INFORMATION.0, None, 0, &mut needed);
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
        assert!(
            GetFileSecurityW(
                &name,
                DACL_SECURITY_INFORMATION.0,
                Some(descriptor),
                needed,
                &mut needed
            )
            .as_bool()
        );
        let mut text = PWSTR::null();
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
        .unwrap();
        let out = PCWSTR(text.0).to_string().unwrap();
        LocalFree(Some(HLOCAL(text.0.cast())));
        out
    }
}

/// Item 7: a program holds the target through all the tries, with no sharing.
#[test]
fn a_held_target_gives_a_sharing_violation_after_bounded_retries() {
    let dir = scratch("held");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let start = Instant::now();
    let result = atomic_write(&path, b"after");
    let took = start.elapsed();
    drop(holder);
    match result {
        Err(CodecError::SharingViolation {
            path: named,
            code,
            leftover,
        }) => {
            assert_ne!(code, 0);
            assert!(named.ends_with("doc.txt"), "{named:?}");
            assert_eq!(leftover, None);
        }
        other => panic!("{other:?}"),
    }
    assert!(took >= Duration::from_millis(400), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(fs::read(&path).unwrap(), b"before");
    assert_eq!(names_in(&dir), ["doc.txt"], "temp or backup left behind");
    fs::remove_dir_all(dir).unwrap();
}

/// Item 7: a scanner that lets go during the tries does not fail the save.
#[test]
fn a_target_held_for_a_moment_saves_after_retries() {
    let dir = scratch("moment");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(holder);
    });
    let result = atomic_write(&path, b"after");
    release.join().unwrap();
    assert_eq!(result.unwrap(), AtomicWriteResult::Replaced);
    assert_eq!(fs::read(&path).unwrap(), b"after");
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 6: a target that is a folder.
#[test]
fn a_folder_as_target_is_an_error_with_a_code() {
    let dir = scratch("folder");
    let target = dir.join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep.txt"), b"original").unwrap();
    match atomic_write(&target, b"new") {
        Err(CodecError::SaveFailed {
            code,
            original_intact,
            ..
        }) => {
            assert_eq!(code, 267);
            assert!(original_intact);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"original");
    assert_eq!(names_in(&dir), ["target"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 13: a rule that denies writing stops the save, though ReplaceFileW could
/// still swap the file.
#[test]
fn an_access_rule_that_denies_writing_stops_the_save() {
    let dir = scratch("deny");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    // Deny file-write-data (2) to everyone; allow all else.
    set_rules(&path, "D:P(D;;0x2;;;WD)(A;;FA;;;WD)");
    let start = Instant::now();
    let result = atomic_write(&path, b"after");
    let took = start.elapsed();
    let verdict = match &result {
        Err(CodecError::AccessDenied { code, .. }) => *code == 5,
        _ => false,
    };
    set_rules(&path, "D:P(A;;FA;;;WD)");
    assert!(verdict, "{result:?}");
    // Stopped by the check before the save starts, not by the retries of a later step.
    assert!(took < Duration::from_millis(400), "{took:?}");
    assert_eq!(fs::read(&path).unwrap(), b"before");
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 4, end state. `src/save.rs` checks the temp file before any byte is written.
#[test]
fn a_private_target_stays_private_after_a_save() {
    let dir = scratch("private");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    set_rules(&path, "D:P(A;;FA;;;OW)");
    let rules = rules_of(&path);
    assert_eq!(
        atomic_write(&path, b"after").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"after");
    assert_eq!(protected_rules(&rules_of(&path)), protected_rules(&rules));
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 8.
#[test]
fn saving_through_a_link_writes_the_target_and_keeps_the_link() {
    let dir = scratch("link");
    let real_dir = dir.join("real");
    fs::create_dir(&real_dir).unwrap();
    let real = real_dir.join("doc.txt");
    fs::write(&real, b"before").unwrap();
    let link = dir.join("link.txt");
    std::os::windows::fs::symlink_file(&real, &link).expect("symlinks need Developer Mode");
    assert_eq!(
        atomic_write(&link, b"after").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&real).unwrap(), b"after");
    assert_eq!(fs::read(&link).unwrap(), b"after");
    assert_eq!(names_in(&real_dir), ["doc.txt"]);

    // A relative link, and a link to a link.
    let relative = dir.join("relative.txt");
    std::os::windows::fs::symlink_file(Path::new("real").join("doc.txt"), &relative).unwrap();
    let chain = dir.join("chain.txt");
    std::os::windows::fs::symlink_file("relative.txt", &chain).unwrap();
    atomic_write(&chain, b"third").unwrap();
    assert_eq!(fs::read(&real).unwrap(), b"third");
    assert!(
        fs::symlink_metadata(&chain)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(&relative)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(dir).unwrap();
}

/// Item 8: a dangling link.
#[test]
fn a_dangling_link_writes_its_target_or_names_the_missing_folder() {
    let dir = scratch("dangling");
    let wanted = dir.join("wanted.txt");
    let link = dir.join("link.txt");
    std::os::windows::fs::symlink_file(&wanted, &link).unwrap();
    assert_eq!(
        atomic_write(&link, b"made").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&wanted).unwrap(), b"made");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let lost = dir.join("no-such-folder").join("lost.txt");
    let lost_link = dir.join("lost-link.txt");
    std::os::windows::fs::symlink_file(&lost, &lost_link).unwrap();
    match atomic_write(&lost_link, b"x") {
        Err(CodecError::DanglingLink { link, target }) => {
            assert!(link.ends_with("lost-link.txt"), "{link:?}");
            assert!(target.ends_with("lost.txt"), "{target:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        fs::symlink_metadata(&lost_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(dir).unwrap();
}

/// Item 9: exactly 431 characters, and a new file past the old limit.
#[test]
fn long_paths_save() {
    let root = scratch("long");
    let mut folder = root.clone();
    while folder.as_os_str().len() < 395 {
        folder = folder.join("a".repeat(30));
    }
    fs::create_dir_all(&folder).unwrap();
    let name = "x".repeat(431 - folder.as_os_str().len() - 1);
    let path = folder.join(name);
    assert_eq!(path.as_os_str().len(), 431);
    fs::write(&path, b"before").unwrap();
    assert_eq!(
        atomic_write(&path, b"after").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"after");
    let new = folder.join("new.txt");
    assert_eq!(
        atomic_write(&new, b"made").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&new).unwrap(), b"made");
    assert_eq!(names_in(&folder).len(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn relative_paths_and_empty_files_save() {
    let dir = scratch("relative");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    assert_eq!(
        atomic_write(&path, b"").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"");
    assert_eq!(
        atomic_write(&path, b"x").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(names_in(&dir), ["doc.txt"]);
    assert!(matches!(
        atomic_write(&dir.join("no-folder").join("x"), b"x"),
        Err(CodecError::SaveFailed { .. })
    ));
    fs::remove_dir_all(dir).unwrap();
}

/// Item 10: a large copy over an existing file, all at once.
#[test]
fn a_prepared_copy_replaces_a_target_only_at_commit() {
    let dir = scratch("copy");
    let input = dir.join("input");
    let dest = dir.join("dest");
    let bytes = vec![0xa5; 2_200_000];
    fs::write(&input, &bytes).unwrap();
    fs::write(&dest, b"old").unwrap();
    set_rules(&dest, "D:P(A;;FA;;;OW)");
    let rules = rules_of(&dest);
    let mut during = Vec::new();
    let prepared = prepare_large_copy(
        &input,
        &dest,
        |_, _| during.push(fs::read(&dest).unwrap()),
        || false,
    )
    .unwrap();
    assert!(during.len() >= 3);
    assert!(during.iter().all(|seen| seen == b"old"));
    assert_eq!(fs::read(&dest).unwrap(), b"old");
    assert_eq!(rules_of(prepared.temp_path()), rules, "temp was wider");
    assert_eq!(fs::read(prepared.temp_path()).unwrap(), bytes);
    assert_eq!(prepared.commit().unwrap(), AtomicWriteResult::Replaced);
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert_eq!(protected_rules(&rules_of(&dest)), protected_rules(&rules));
    assert_eq!(names_in(&dir), ["dest", "input"]);

    // Dropped without a commit: the target stays and the temp file goes.
    fs::write(&input, b"second").unwrap();
    let prepared = prepare_large_copy(&input, &dest, |_, _| {}, || false).unwrap();
    drop(prepared);
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert_eq!(names_in(&dir), ["dest", "input"]);

    // A cancel leaves the same state.
    let result = prepare_large_copy(&input, &dest, |_, _| {}, || true);
    assert!(matches!(result, Err(CodecError::Cancelled)));
    assert_eq!(names_in(&dir), ["dest", "input"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 10: nothing partial under the final name while the copy runs.
#[test]
fn a_large_copy_never_shows_a_partial_file_under_the_final_name() {
    let dir = scratch("partial");
    let input = dir.join("input");
    let dest = dir.join("dest");
    fs::write(&input, vec![7u8; 3_300_000]).unwrap();
    let mut temp_seen = 0;
    copy_large_original(
        &input,
        &dest,
        |_, _| {
            assert!(!dest.exists(), "partial file under the final name");
            let names = names_in(&dir);
            temp_seen = names.iter().filter(|n| n.ends_with(".tmp")).count();
        },
        || false,
    )
    .unwrap();
    assert_eq!(temp_seen, 1);
    assert_eq!(fs::read(&dest).unwrap().len(), 3_300_000);
    assert_eq!(names_in(&dir), ["dest", "input"]);
    // A file at the final name: refused before any copying.
    let result = copy_large_original(&input, &dest, |_, _| panic!("copied"), || false);
    assert!(matches!(result, Err(CodecError::TargetExists { .. })));
    fs::remove_dir_all(dir).unwrap();
}

fn leftover(hash: &str, pid: u32, ext: &str) -> String {
    format!(".qe-{hash}-{pid}-0123456789abcdef.{ext}")
}

/// Item 11.
#[test]
fn find_leftovers_lists_our_files_and_deletes_nothing() {
    let dir = scratch("leftovers");
    // No process has this number; Windows process numbers are multiples of 4.
    const DEAD: u32 = 4_294_967_293;
    let doc = dir.join("doc.txt");
    fs::write(&doc, b"current").unwrap();
    // Find the hash this document uses from a real save's temp name.
    let mut temp_name = String::new();
    let prepared = prepare_large_copy(&doc, &doc, |_, _| {}, || false).unwrap();
    temp_name.push_str(&prepared.temp_path().file_name().unwrap().to_string_lossy());
    drop(prepared);
    let hash = &temp_name[4..12];
    let backup_of_doc = dir.join(leftover(hash, DEAD, "bak"));
    let temp_of_doc = dir.join(leftover(hash, DEAD, "tmp"));
    let reserved = dir.join(format!(".qe-{hash}-{DEAD}-1123456789abcdef.bak"));
    let running = dir.join(format!(
        ".qe-{hash}-{}-2123456789abcdef.bak",
        std::process::id()
    ));
    let gone_backup = dir.join(leftover("00000000", DEAD, "bak"));
    let gone_temp = dir.join(leftover("00000001", DEAD, "tmp"));
    for path in [
        &backup_of_doc,
        &temp_of_doc,
        &running,
        &gone_backup,
        &gone_temp,
    ] {
        fs::write(path, b"text").unwrap();
    }
    fs::write(&reserved, b"").unwrap();
    for other in [".qe-notes.txt", ".qe-1-2-3.tmp", "doc.txt.bak"] {
        fs::write(dir.join(other), b"not ours").unwrap();
    }
    let before = names_in(&dir);

    let all = find_leftovers(&dir).unwrap();
    assert_eq!(all.len(), 6, "{all:#?}");
    let row = |path: &Path| {
        let name = path.file_name().unwrap();
        all.iter()
            .find(|l| l.path.file_name() == Some(name))
            .unwrap_or_else(|| panic!("{path:?} not listed"))
    };
    assert!(row(&backup_of_doc).offer_recovery);
    assert_eq!(row(&backup_of_doc).kind, LeftoverKind::Backup);
    assert!(!row(&backup_of_doc).target_missing);
    assert_eq!(row(&backup_of_doc).size, 4);
    assert!(
        row(&backup_of_doc)
            .likely_target
            .as_ref()
            .unwrap()
            .ends_with("doc.txt")
    );
    // A temp file beside an intact document is not offered.
    assert_eq!(row(&temp_of_doc).kind, LeftoverKind::Temp);
    assert!(!row(&temp_of_doc).offer_recovery);
    // An empty reservation holds nothing.
    assert!(!row(&reserved).offer_recovery);
    // A process that still runs may still be saving.
    assert!(row(&running).owner_running);
    assert!(!row(&running).offer_recovery);
    // The document is gone: both kinds are offered.
    assert!(row(&gone_backup).offer_recovery && row(&gone_backup).target_missing);
    assert!(row(&gone_temp).offer_recovery && row(&gone_temp).target_missing);

    // Named by document: only that document's files.
    let one = find_leftovers(&doc).unwrap();
    assert_eq!(one.len(), 4, "{one:#?}");
    assert!(one.iter().all(|l| !l.target_missing));
    // A document that has gone can still be named.
    let missing = dir.join("no-such-doc.txt");
    assert_eq!(find_leftovers(&missing).unwrap(), []);

    assert_eq!(names_in(&dir), before, "find_leftovers changed the folder");
    fs::remove_dir_all(dir).unwrap();
}

/// The built-in Guests group by its id, which reads the same in every language of Windows.
const GUESTS_ADD: &str = "*S-1-5-32-546:(OI)(CI)(R)";
const GUESTS_ID: &str = "*S-1-5-32-546";
/// The same group as SDDL writes it.
const GUESTS_RULE: &str = ";BG)";

/// Changes a folder's rules the way Explorer does, which also carries them to its files.
fn icacls(path: &Path, what: &[&str]) {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(what)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// A file whose rules come from its folder keeps following that folder after a Save. The plain
/// file beside it shows what the folder's change does to a file that was never saved.
#[test]
fn a_file_that_inherits_keeps_following_its_folder_after_a_save() {
    let dir = scratch("follows");
    let path = dir.join("doc.txt");
    let plain = dir.join("plain.txt");
    fs::write(&path, b"before").unwrap();
    fs::write(&plain, b"x").unwrap();
    assert!(!rules_of(&path).contains(GUESTS_RULE), "the fixture");

    assert_eq!(
        atomic_write(&path, b"after").unwrap(),
        AtomicWriteResult::Replaced
    );

    assert_eq!(
        rules_of(&path),
        rules_of(&plain),
        "the Save changed the rules"
    );
    icacls(&dir, &["/grant", GUESTS_ADD]);
    assert!(
        rules_of(&plain).contains(GUESTS_RULE),
        "the folder's rule reached no file"
    );
    let got = rules_of(&path);
    assert!(
        got.contains(GUESTS_RULE),
        "the file stopped following: {got}"
    );

    // A rule taken away from the folder leaves too, so the file's rules are truly inherited.
    icacls(&dir, &["/remove", GUESTS_ID]);
    assert!(!rules_of(&plain).contains(GUESTS_RULE));
    let got = rules_of(&path);
    assert!(!got.contains(GUESTS_RULE), "the old rule stayed: {got}");
    fs::remove_dir_all(dir).unwrap();
}

/// A file with its inheritance removed stays out of the folder's reach after a Save.
#[test]
fn a_file_with_inheritance_removed_stays_protected_after_a_save() {
    let dir = scratch("stays-protected");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    set_rules(&path, "D:P(A;;FA;;;OW)");

    atomic_write(&path, b"after").unwrap();

    icacls(&dir, &["/grant", GUESTS_ADD]);
    let got = rules_of(&path);
    assert!(!got.contains(GUESTS_RULE), "the folder reached it: {got}");
    assert!(got.contains("P"), "no longer protected: {got}");
    fs::remove_dir_all(dir).unwrap();
}
