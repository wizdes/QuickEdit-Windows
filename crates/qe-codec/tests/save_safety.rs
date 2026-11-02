//! Saves and large copies while other programs act on the same files, and the
//! properties the README promises, against the real file system.
use qe_codec::{
    AtomicWriteResult, CodecError, LeftoverDelete, atomic_write, atomic_write_if_unchanged,
    copy_large_original, delete_leftover, file_stamp, find_leftovers, prepare_large_copy,
};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_FLAGS_AND_ATTRIBUTES, GetFileAttributesW, SetFileAttributesW,
};
use windows::core::HSTRING;

const MIB: usize = 1 << 20;

/// Waits out one step of the file time, which moves about every 16 ms. A write of the
/// same size inside one step looks like no change at all. The README lists this limit.
fn next_time_step() {
    std::thread::sleep(std::time::Duration::from_millis(40));
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qe-safety-{name}-{}", std::process::id()));
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

/// Runs a copy that fails because `change` acted on the source at the first report.
fn copy_while_changing(name: &str, change: impl FnOnce(&Path)) {
    let dir = scratch(name);
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, vec![0xa5u8; 5 * MIB]).unwrap();
    fs::write(&dest, b"good file").unwrap();
    let mut change = Some(change);
    let mut over = false;
    let result = prepare_large_copy(
        &input,
        &dest,
        |done, total| {
            over |= done > total;
            if let Some(change) = change.take() {
                change(&input);
            }
        },
        || false,
    );
    assert!(!over, "{name}: done passed total");
    match result {
        Err(CodecError::SourceChanged { path, leftover }) => {
            assert!(path.ends_with("input"), "{name}: {path:?}");
            assert_eq!(leftover, None, "{name}");
        }
        other => panic!("{name}: {other:?}"),
    }
    assert_eq!(fs::read(&dest).unwrap(), b"good file", "{name}");
    let names = names_in(&dir);
    assert!(
        names.iter().all(|n| !n.ends_with(".tmp")),
        "{name}: partial copy left: {names:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// Item 1: the exact case the review proved.
#[test]
fn a_copy_refuses_a_source_that_shrinks() {
    copy_while_changing("shrink", |input| {
        fs::OpenOptions::new()
            .write(true)
            .open(input)
            .unwrap()
            .set_len(MIB as u64)
            .unwrap();
    });
}
#[test]
fn a_copy_refuses_a_source_that_grows_and_done_stays_within_total() {
    copy_while_changing("grow", |input| {
        fs::OpenOptions::new()
            .append(true)
            .open(input)
            .unwrap()
            .write_all(&vec![1u8; 5 * MIB])
            .unwrap();
    });
}
#[test]
fn a_copy_refuses_a_source_rewritten_at_the_same_size() {
    copy_while_changing("rewrite", |input| {
        next_time_step();
        let mut file = fs::OpenOptions::new().write(true).open(input).unwrap();
        file.seek(SeekFrom::Start(4 * MIB as u64)).unwrap();
        file.write_all(b"other").unwrap();
    });
}
#[test]
fn a_copy_refuses_a_source_replaced_under_its_name() {
    copy_while_changing("swap", |input| {
        let old = input.with_extension("old");
        fs::rename(input, &old).unwrap();
        fs::write(input, vec![0xa5u8; 5 * MIB]).unwrap();
    });
}
#[test]
fn a_copy_to_a_new_name_refuses_a_changed_source_and_leaves_nothing() {
    let dir = scratch("new-name");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, vec![1u8; 5 * MIB]).unwrap();
    let mut cut = false;
    let result = copy_large_original(
        &input,
        &dest,
        |_, _| {
            if !cut {
                cut = true;
                fs::OpenOptions::new()
                    .write(true)
                    .open(&input)
                    .unwrap()
                    .set_len(MIB as u64)
                    .unwrap();
            }
        },
        || false,
    );
    assert!(
        matches!(result, Err(CodecError::SourceChanged { .. })),
        "{result:?}"
    );
    assert_eq!(names_in(&dir), ["input"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 11: an empty source reports (0, 0) as its finish.
#[test]
fn a_copy_of_an_empty_file_reports_zero_of_zero_once() {
    let dir = scratch("empty-copy");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, b"").unwrap();
    let mut seen = Vec::new();
    copy_large_original(&input, &dest, |d, t| seen.push((d, t)), || false).unwrap();
    assert_eq!(seen, [(0, 0)]);
    assert_eq!(fs::read(&dest).unwrap(), b"");
    fs::remove_dir_all(dir).unwrap();
}

/// Item 2: the exact case the review proved.
#[test]
fn a_commit_refuses_a_target_changed_after_prepare_and_keeps_both_versions() {
    let dir = scratch("commit-changed");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, b"copied text").unwrap();
    fs::write(&dest, b"old").unwrap();
    let prepared = prepare_large_copy(&input, &dest, |_, _| {}, || false).unwrap();
    fs::write(&dest, b"other program").unwrap();
    match prepared.commit().unwrap_err() {
        CodecError::TargetChanged {
            path,
            new_text: Some(new_text),
            other_version: None,
        } => {
            assert!(path.ends_with("dest"), "{path:?}");
            assert_eq!(fs::read(&new_text).unwrap(), b"copied text");
            fs::remove_file(new_text).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&dest).unwrap(), b"other program");
    assert_eq!(names_in(&dir), ["dest", "input"]);
    fs::remove_dir_all(dir).unwrap();
}

/// A change since open, seen through the stamp M14 keeps with the document.
#[test]
fn a_stamp_taken_at_open_catches_a_change_made_since() {
    let dir = scratch("stamp");
    let path = dir.join("doc.txt");
    assert_eq!(file_stamp(&path).unwrap(), None);
    fs::write(&path, b"opened").unwrap();
    let opened = file_stamp(&path).unwrap().unwrap();
    assert_eq!(file_stamp(&path).unwrap(), Some(opened));

    // Nothing changed: the save works, and the old stamp is then out of date.
    assert_eq!(
        atomic_write_if_unchanged(&path, b"saved", opened).unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"saved");
    let saved = file_stamp(&path).unwrap().unwrap();
    assert_ne!(saved, opened);
    let refused = atomic_write_if_unchanged(&path, b"late", opened);
    assert!(
        matches!(
            refused,
            Err(CodecError::TargetChanged {
                new_text: None,
                other_version: None,
                ..
            })
        ),
        "{refused:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"saved");
    assert_eq!(names_in(&dir), ["doc.txt"], "a refused save left a file");

    // Another program wrote.
    fs::write(&path, b"other program").unwrap();
    assert!(matches!(
        atomic_write_if_unchanged(&path, b"mine", saved),
        Err(CodecError::TargetChanged { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), b"other program");

    // Another program deleted it. The save must not silently bring it back.
    let current = file_stamp(&path).unwrap().unwrap();
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        atomic_write_if_unchanged(&path, b"mine", current),
        Err(CodecError::TargetChanged { .. })
    ));
    assert!(!path.exists());

    // The large-copy form.
    let input = dir.join("input");
    fs::write(&input, b"copied").unwrap();
    fs::write(&path, b"one").unwrap();
    let stamp = file_stamp(&path).unwrap().unwrap();
    // Older than what prepare_large_copy sees, so the plain check would not notice.
    next_time_step();
    fs::write(&path, b"two").unwrap();
    let prepared = prepare_large_copy(&input, &path, |_, _| {}, || false).unwrap();
    match prepared.commit_if_unchanged(stamp).unwrap_err() {
        CodecError::TargetChanged {
            new_text: Some(new_text),
            ..
        } => fs::remove_file(new_text).unwrap(),
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"two");
    let stamp = file_stamp(&path).unwrap().unwrap();
    let prepared = prepare_large_copy(&input, &path, |_, _| {}, || false).unwrap();
    assert_eq!(
        prepared.commit_if_unchanged(stamp).unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"copied");
    fs::remove_dir_all(dir).unwrap();
}

/// Item 3: the exact case the review proved. `..` belongs to the real parent.
#[test]
fn a_relative_dangling_link_in_a_linked_folder_writes_beside_the_real_folder() {
    let dir = scratch("parent-link");
    let sub = dir.join("real").join("sub");
    fs::create_dir_all(&sub).unwrap();
    let elsewhere = dir.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::windows::fs::symlink_dir(&sub, elsewhere.join("linkdir"))
        .expect("symlinks need Developer Mode");
    std::os::windows::fs::symlink_file(r"..\wanted.txt", sub.join("doc.txt")).unwrap();
    assert_eq!(
        atomic_write(&elsewhere.join("linkdir").join("doc.txt"), b"made").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(
        fs::read(dir.join("real").join("wanted.txt")).unwrap(),
        b"made"
    );
    assert_eq!(
        fs::read(elsewhere.join("linkdir").join("doc.txt")).unwrap(),
        b"made"
    );
    assert!(
        !elsewhere.join("wanted.txt").exists(),
        "wrote the wrong folder"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// Item 3: every case is checked by reading back through the link, because the right
/// answer is whatever Windows itself opens.
#[test]
fn a_dangling_link_writes_where_windows_opens_it() {
    let dir = scratch("inner-link");
    let deep = dir.join("real").join("deep").join("er");
    fs::create_dir_all(&deep).unwrap();
    std::os::windows::fs::symlink_dir(&deep, dir.join("hop")).unwrap();
    // Windows removes the `..` in a link's own text as text, so this is the top folder.
    std::os::windows::fs::symlink_file(r"hop\..\a.txt", dir.join("one.txt")).unwrap();
    atomic_write(&dir.join("one.txt"), b"one").unwrap();
    assert_eq!(fs::read(dir.join("one.txt")).unwrap(), b"one");
    assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"one");
    assert!(!dir.join("real").join("deep").join("a.txt").exists());

    // A link to a dangling link, whose second step starts from a linked folder.
    std::os::windows::fs::symlink_file(r"..\b.txt", deep.join("step.txt")).unwrap();
    std::os::windows::fs::symlink_file(r"hop\step.txt", dir.join("two.txt")).unwrap();
    atomic_write(&dir.join("two.txt"), b"two").unwrap();
    assert_eq!(fs::read(dir.join("two.txt")).unwrap(), b"two");
    assert_eq!(
        fs::read(dir.join("real").join("deep").join("b.txt")).unwrap(),
        b"two"
    );
    assert!(!dir.join("b.txt").exists(), "wrote the wrong folder");

    // A folder inside the link's text that does not exist is refused.
    std::os::windows::fs::symlink_file(r"nope\c.txt", dir.join("three.txt")).unwrap();
    let result = atomic_write(&dir.join("three.txt"), b"three");
    assert!(
        matches!(result, Err(CodecError::DanglingLink { .. })),
        "{result:?}"
    );
    // A link that points at itself never ends, and is refused too.
    std::os::windows::fs::symlink_file("loop.txt", dir.join("loop.txt")).unwrap();
    let result = atomic_write(&dir.join("loop.txt"), b"x");
    assert!(result.is_err(), "{result:?}");
    fs::remove_dir_all(dir).unwrap();
}

/// A folder of Modify rights: this user may make and edit files there, and may not
/// delete a file whose own rules do not allow it.
const FOLDER_MODIFY: &str = "D:P(A;OICI;0x1301BF;;;WD)";
/// Read and write, no delete.
const FILE_NO_DELETE: &str = "D:P(A;;0x12019F;;;WD)";
const OPEN: &str = "D:(A;OICI;FA;;;WD)";

/// Item 6: the exact case the review proved.
#[test]
fn a_file_that_can_be_edited_but_not_replaced_gives_a_clear_error_and_no_leftover() {
    let dir = scratch("no-delete");
    let folder = dir.join("folder");
    fs::create_dir(&folder).unwrap();
    let path = folder.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let input = dir.join("input");
    fs::write(&input, b"copied").unwrap();
    set_rules(&path, FILE_NO_DELETE);
    set_rules(&folder, FOLDER_MODIFY);

    let first = atomic_write(&path, b"after");
    let second = atomic_write(&path, b"after");
    let copy = prepare_large_copy(&input, &path, |_, _| {}, || false).map(drop);
    let names = names_in(&folder);
    let read = fs::read(&path);

    // Open the fixture again before any assert, so a failure can still clean up.
    let rules_back = |path: &Path| set_rules(path, OPEN);
    rules_back(&folder);
    rules_back(&path);
    // Whatever a failed run left must be removable too.
    for name in &names {
        rules_back(&folder.join(name));
    }
    for result in [first, second, copy.map(|()| AtomicWriteResult::Replaced)] {
        match result {
            Err(error @ CodecError::NotReplaceable { code: 5, .. }) => {
                let text = error.to_string();
                assert!(text.contains("Save As"), "{text}");
                assert!(text.contains("not changed"), "{text}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(names, ["doc.txt"], "a failed save left a file");
    assert_eq!(read.unwrap(), b"before");
    fs::remove_dir_all(dir).unwrap();
}

const DEAD: u32 = 4_294_967_293;
fn leftover_name(hash: &str, pid: u32, ext: &str) -> String {
    format!(".qe-{hash}-{pid}-0123456789abcdef.{ext}")
}
/// The hash in the leftover names of `doc`, from a real save's temp name.
fn hash_of(doc: &Path) -> String {
    let prepared = prepare_large_copy(doc, doc, |_, _| {}, || false).unwrap();
    let name = prepared
        .temp_path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    drop(prepared);
    name[4..12].to_string()
}

/// Item 6: known leftovers go on request, and only known ones.
#[test]
fn delete_leftover_removes_what_was_listed_and_refuses_what_is_still_useful() {
    let dir = scratch("delete-leftover");
    let doc = dir.join("doc.txt");
    fs::write(&doc, b"kept").unwrap();
    let hash = hash_of(&doc);
    let temp_of_doc = dir.join(leftover_name(&hash, DEAD, "tmp"));
    let gone_backup = dir.join(leftover_name("00000000", DEAD, "bak"));
    let running = dir.join(leftover_name("00000001", 4, "tmp"));
    let changed = dir.join(leftover_name(&hash, DEAD, "bak"));
    for path in [&temp_of_doc, &gone_backup, &running, &changed] {
        fs::write(path, b"text").unwrap();
    }
    let all = find_leftovers(&dir).unwrap();
    let row = |path: &Path| {
        all.iter()
            .find(|l| l.path.file_name() == path.file_name())
            .unwrap()
    };

    // Not useful, and its process has ended: it goes.
    assert!(!row(&temp_of_doc).offer_recovery);
    delete_leftover(row(&temp_of_doc), LeftoverDelete::KeepRecoverable).unwrap();
    assert!(!temp_of_doc.exists());
    // Already gone is success: the wish is met.
    delete_leftover(row(&temp_of_doc), LeftoverDelete::KeepRecoverable).unwrap();

    // The only copy of a gone document: it stays unless the user chose that.
    assert!(row(&gone_backup).offer_recovery);
    let kept = delete_leftover(row(&gone_backup), LeftoverDelete::KeepRecoverable);
    assert!(
        matches!(kept, Err(CodecError::LeftoverKept { .. })),
        "{kept:?}"
    );
    assert_eq!(fs::read(&gone_backup).unwrap(), b"text");
    delete_leftover(row(&gone_backup), LeftoverDelete::IncludeRecoverable).unwrap();
    assert!(!gone_backup.exists());

    // Another process may still be saving.
    assert!(row(&running).owner_running);
    let kept = delete_leftover(row(&running), LeftoverDelete::IncludeRecoverable);
    assert!(
        matches!(kept, Err(CodecError::LeftoverKept { .. })),
        "{kept:?}"
    );
    assert!(running.exists());

    // Changed since the list was made: the list no longer describes it.
    fs::write(&changed, b"newer and longer").unwrap();
    let kept = delete_leftover(row(&changed), LeftoverDelete::IncludeRecoverable);
    assert!(
        matches!(kept, Err(CodecError::LeftoverKept { .. })),
        "{kept:?}"
    );
    assert!(changed.exists());

    // A file that is not one of ours, whatever the list claims.
    let mut forged = row(&changed).clone();
    forged.path = doc.clone();
    // It matches the real file, so only the name can stop this.
    let meta = fs::metadata(&doc).unwrap();
    forged.size = meta.len();
    forged.modified = meta.modified().unwrap();
    let kept = delete_leftover(&forged, LeftoverDelete::IncludeRecoverable);
    assert!(
        matches!(kept, Err(CodecError::LeftoverKept { .. })),
        "{kept:?}"
    );
    assert_eq!(fs::read(&doc).unwrap(), b"kept");
    fs::remove_dir_all(dir).unwrap();
}

/// Item 6: a leftover whose copied rules stop this user deleting it still goes.
#[test]
fn delete_leftover_removes_a_temp_whose_rules_deny_deleting_it() {
    let dir = scratch("denied-leftover");
    let folder = dir.join("folder");
    fs::create_dir(&folder).unwrap();
    let doc = folder.join("doc.txt");
    fs::write(&doc, b"kept").unwrap();
    let temp = folder.join(leftover_name(&hash_of(&doc), DEAD, "tmp"));
    fs::write(&temp, b"text").unwrap();
    set_rules(&temp, FILE_NO_DELETE);
    set_rules(&folder, FOLDER_MODIFY);
    let denied_first = fs::remove_file(&temp).is_err();
    let list = find_leftovers(&folder).unwrap();
    let row = list
        .iter()
        .find(|l| l.path.ends_with(temp.file_name().unwrap()))
        .unwrap();
    let result = delete_leftover(row, LeftoverDelete::KeepRecoverable);
    let gone = !temp.exists();
    set_rules(&folder, OPEN);
    if temp.exists() {
        set_rules(&temp, OPEN);
    }
    assert!(denied_first, "the fixture must really deny the delete");
    result.unwrap();
    assert!(gone);
    fs::remove_dir_all(dir).unwrap();
}

fn attributes(path: &Path) -> u32 {
    // SAFETY: the string outlives the call.
    unsafe { GetFileAttributesW(&HSTRING::from(path.to_str().unwrap())) }
}

/// README: a save keeps the creation date, alternate streams and the hidden mark.
#[test]
fn a_save_keeps_alternate_streams_and_the_hidden_mark() {
    let dir = scratch("kept-properties");
    let path = dir.join("doc.txt");
    let stream = dir.join("doc.txt:side");
    fs::write(&path, b"before").unwrap();
    fs::write(&stream, b"stream text").unwrap();
    let created = fs::metadata(&path).unwrap().created().unwrap();
    let name = HSTRING::from(path.to_str().unwrap());
    let hidden = attributes(&path) | FILE_ATTRIBUTE_HIDDEN.0;
    // SAFETY: the string outlives the call.
    unsafe { SetFileAttributesW(&name, FILE_FLAGS_AND_ATTRIBUTES(hidden)) }.unwrap();
    assert_eq!(
        atomic_write(&path, b"after").unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"after");
    assert_eq!(fs::read(&stream).unwrap(), b"stream text", "stream lost");
    assert_ne!(
        attributes(&path) & FILE_ATTRIBUTE_HIDDEN.0,
        0,
        "hidden mark lost"
    );
    assert_eq!(fs::metadata(&path).unwrap().created().unwrap(), created);
    // The large-copy route takes the same steps.
    let input = dir.join("input");
    fs::write(&input, b"copied").unwrap();
    let prepared = prepare_large_copy(&input, &path, |_, _| {}, || false).unwrap();
    prepared.commit().unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"copied");
    assert_eq!(fs::read(&stream).unwrap(), b"stream text", "stream lost");
    assert_ne!(
        attributes(&path) & FILE_ATTRIBUTE_HIDDEN.0,
        0,
        "hidden mark lost"
    );
    assert_eq!(names_in(&dir), ["doc.txt", "input"]);
    fs::remove_dir_all(dir).unwrap();
}
