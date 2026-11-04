//! Tests that force a failure at one exact step of a save, through `SaveEnv`.
use super::*;
use std::cell::RefCell;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qe-unit-{name}-{}", std::process::id()));
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

type ReplaceStep = Box<dyn FnMut(&Path, &Path, &Path) -> Result<(), u32>>;
type PathHook = Option<Box<dyn FnMut(&Path)>>;
#[derive(Default)]
struct FakeEnv {
    replace: Option<ReplaceStep>,
    move_fails_with: Option<u32>,
    write_fails_with: Option<i32>,
    flush_fails_with: Option<i32>,
    randoms: Vec<u64>,
    on_temp_created: PathHook,
    on_before_commit: PathHook,
    sleeps: u32,
    replace_calls: u32,
}
impl SaveEnv for FakeEnv {
    fn replace(&mut self, target: &Path, temp: &Path, backup: &Path) -> Result<(), u32> {
        self.replace_calls += 1;
        match &mut self.replace {
            Some(step) => step(target, temp, backup),
            None => RealEnv.replace(target, temp, backup),
        }
    }
    fn move_no_replace(&mut self, from: &Path, to: &Path) -> Result<(), u32> {
        match self.move_fails_with {
            Some(code) => Err(code),
            None => RealEnv.move_no_replace(from, to),
        }
    }
    fn write_temp(&mut self, file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
        match self.write_fails_with {
            Some(code) => Err(std::io::Error::from_raw_os_error(code)),
            None => RealEnv.write_temp(file, bytes),
        }
    }
    fn flush_temp(&mut self, file: &mut File) -> std::io::Result<()> {
        match self.flush_fails_with {
            Some(code) => Err(std::io::Error::from_raw_os_error(code)),
            None => RealEnv.flush_temp(file),
        }
    }
    fn random(&mut self) -> u64 {
        if self.randoms.is_empty() {
            RealEnv.random()
        } else {
            self.randoms.remove(0)
        }
    }
    fn temp_created(&mut self, temp: &Path) {
        if let Some(hook) = &mut self.on_temp_created {
            hook(temp);
        }
    }
    fn before_commit(&mut self, temp: &Path) {
        if let Some(hook) = &mut self.on_before_commit {
            hook(temp);
        }
    }
    fn sleep(&mut self, _time: Duration) {
        self.sleeps += 1;
    }
}

/// A file's access rules as SDDL, the text form of those rules.
fn rules_of(path: &Path) -> String {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
    };
    let mut rules = read_access_rules(path).unwrap().unwrap();
    let mut text = windows::core::PWSTR::null();
    // SAFETY: the descriptor is live, the string is read before it is freed, and it is
    // freed once.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            rules.pointer(),
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
/// The rules alone. ReplaceFileW adds an "auto-inherited" mark (AI) to a protected
/// list, which changes no access. The list must stay protected (P).
fn protected_rules(sddl: &str) -> &str {
    let (flags, rules) = sddl.split_at(sddl.find('(').unwrap());
    assert!(flags.contains('P'), "not protected: {sddl}");
    rules
}
fn set_rules(path: &Path, sddl: &str) {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::SetFileSecurityW;
    use windows::core::HSTRING;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the strings outlive the calls, and the descriptor is freed once.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .unwrap();
        assert!(
            SetFileSecurityW(
                &HSTRING::from(path.to_str().unwrap()),
                DACL_SECURITY_INFORMATION,
                descriptor
            )
            .as_bool()
        );
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
}

/// Item 4: the moment that matters is before the first byte, in an open folder.
#[test]
fn temp_file_has_the_private_rules_before_any_byte_is_written() {
    let dir = scratch("rules");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    // The folder gives everyone access, and this file gives only its owner access.
    set_rules(&dir, "D:(A;OICI;FA;;;WD)");
    fs::write(dir.join("other.txt"), b"x").unwrap();
    let open_rules = rules_of(&dir.join("other.txt"));
    set_rules(&path, "D:P(A;;FA;;;OW)");
    let private = rules_of(&path);
    assert_ne!(open_rules, private, "the fixture must differ");
    let checks = std::rc::Rc::new(RefCell::new(0));
    let seen = checks.clone();
    let wanted = private.clone();
    let mut env = FakeEnv {
        on_temp_created: Some(Box::new(move |temp| {
            assert_eq!(fs::metadata(temp).unwrap().len(), 0, "bytes before rules");
            assert_eq!(rules_of(temp), wanted, "temp is wider than the target");
            *seen.borrow_mut() += 1;
        })),
        ..Default::default()
    };
    atomic_write_with(&path, b"after", &mut env).unwrap();
    assert_eq!(*checks.borrow(), 1);
    assert_eq!(protected_rules(&rules_of(&path)), protected_rules(&private));
    fs::remove_dir_all(dir).unwrap();
}

/// Item 4: rules inherited from an older folder must not widen to this folder's.
#[test]
fn temp_file_keeps_rules_that_are_not_protected() {
    let dir = scratch("inherited");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    // Not protected, and narrower than what this folder would give a new file.
    set_rules(&dir, "D:(A;OICI;FA;;;WD)");
    set_rules(&path, "D:(A;;FA;;;OW)");
    let narrow = rules_of(&path);
    assert!(
        narrow.contains(";OW)") && !narrow.contains(";WD)"),
        "{narrow}"
    );
    let mut env = FakeEnv {
        on_temp_created: Some(Box::new(move |temp| {
            let rules = rules_of(temp);
            assert!(
                !rules.contains(";WD)"),
                "open rule on the temp file: {rules}"
            );
            assert!(rules.contains(";OW)"), "{rules}");
        })),
        ..Default::default()
    };
    atomic_write_with(&path, b"after", &mut env).unwrap();
    // The document itself must not have picked up the folder's open rule.
    assert_eq!(fs::read(&path).unwrap(), b"after");
    let after = rules_of(&path);
    assert!(
        !after.contains(";WD)"),
        "the save widened the file: {after}"
    );
    assert!(
        after.contains(";OW)"),
        "the save lost the owner rule: {after}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// The temp file carries the target's rules as they are. ReplaceFileW puts right a temp file marked
/// protected over a target that inherits, but a temp file kept after a failed swap would stay so.
#[test]
fn temp_file_of_a_target_that_inherits_is_not_protected() {
    let dir = scratch("temp-inherits");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let wanted = rules_of(&path);
    assert!(wanted.contains("ID;"), "the fixture must inherit: {wanted}");
    let checks = std::rc::Rc::new(RefCell::new(0));
    let seen = checks.clone();
    let mut env = FakeEnv {
        on_temp_created: Some(Box::new(move |temp| {
            // Windows leaves off the "auto-inherited" mark (AI), which changes no access.
            let rules = rules_of(temp);
            let (flags, list) = rules.split_at(rules.find('(').unwrap());
            assert!(!flags.contains('P'), "protected: {rules}");
            assert_eq!(list, &wanted[wanted.find('(').unwrap()..]);
            *seen.borrow_mut() += 1;
        })),
        ..Default::default()
    };
    atomic_write_with(&path, b"after", &mut env).unwrap();
    assert_eq!(*checks.borrow(), 1);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 5: a name that is taken stays as it was, for temp and backup.
#[test]
fn stale_files_with_the_same_name_are_kept_and_do_not_block_a_save() {
    let dir = scratch("stale");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let hash = name_hash(path.file_name().unwrap());
    let pid = std::process::id();
    let stale_temp = dir.join(leftover_name(hash, pid, 1, "tmp"));
    let stale_backup = dir.join(leftover_name(hash, pid, 1, "bak"));
    fs::write(&stale_temp, b"older recovery text").unwrap();
    fs::write(&stale_backup, b"older recovery bytes").unwrap();
    let mut env = FakeEnv {
        // Each new name first tries the taken value, then a free one.
        randoms: vec![1, 2, 1, 3],
        ..Default::default()
    };
    assert_eq!(
        atomic_write_with(&path, b"after", &mut env).unwrap(),
        AtomicWriteResult::Replaced
    );
    assert_eq!(fs::read(&path).unwrap(), b"after");
    assert_eq!(fs::read(&stale_temp).unwrap(), b"older recovery text");
    assert_eq!(fs::read(&stale_backup).unwrap(), b"older recovery bytes");
    assert_eq!(names_in(&dir).len(), 3);
    fs::remove_dir_all(dir).unwrap();
}

/// A failure with nothing moved: the error is honest, the temp file goes. This
/// replaces the old test of the same start, which passed with no check at all.
#[test]
fn replace_failure_keeps_original_and_checks_it_before_saying_so() {
    let dir = scratch("intact");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    for code in [1176, 5, 32, 33, 87] {
        let mut env = FakeEnv {
            replace: Some(Box::new(move |_, _, _| Err(code))),
            ..Default::default()
        };
        let error = atomic_write_with(&path, b"after", &mut env).unwrap_err();
        let ok = match (&error, code) {
            (CodecError::AccessDenied { code: got, .. }, 5) => *got == 5,
            (CodecError::SharingViolation { code: got, .. }, 32 | 33) => *got == code,
            (
                CodecError::SaveFailed {
                    code: got,
                    original_intact: true,
                    leftover: None,
                    ..
                },
                1176 | 87,
            ) => *got == code,
            _ => false,
        };
        assert!(ok, "code {code}: {error:?}");
        assert!(error.to_string().contains(&code.to_string()), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"before");
        assert_eq!(names_in(&dir), ["doc.txt"], "code {code}");
    }
    fs::remove_dir_all(dir).unwrap();
}

/// If the check that the original is intact were removed, this would fail: the
/// failed replace changes the target and the error must not claim it is intact.
#[test]
fn replace_failure_does_not_claim_intact_when_the_target_changed() {
    let dir = scratch("changed");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, _, _| {
            fs::write(target, b"damaged by the failed call").unwrap();
            Err(1176)
        })),
        ..Default::default()
    };
    let error = atomic_write_with(&path, b"after", &mut env).unwrap_err();
    assert!(error.to_string().contains("may have changed"), "{error}");
    match error {
        CodecError::SaveFailed {
            code,
            original_intact,
            leftover,
            ..
        } => {
            assert_eq!(code, 1176);
            assert!(!original_intact);
            // The new text stays on disk, because the old text is not safe.
            let temp = leftover.expect("temp must be kept");
            assert_eq!(fs::read(temp).unwrap(), b"after");
        }
        other => panic!("{other:?}"),
    }
    fs::remove_dir_all(dir).unwrap();
}

/// The call moved the original aside, and putting it back works.
#[test]
fn original_moved_aside_is_put_back_and_only_then_called_intact() {
    let dir = scratch("restore");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, _, backup| {
            fs::rename(target, backup).unwrap();
            Err(1177)
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::SaveFailed {
            code,
            original_intact,
            ..
        } => {
            assert_eq!(code, 1177);
            assert!(original_intact);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"before");
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// RecoveryRequired, through a stand-in for ReplaceFileW and for the rename. A real
/// ReplaceFileW was not made to fail at this exact step.
#[test]
fn original_moved_aside_that_cannot_return_gives_recovery_required() {
    let dir = scratch("recovery");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, _, backup| {
            fs::rename(target, backup).unwrap();
            Err(1177)
        })),
        move_fails_with: Some(5),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::RecoveryRequired {
            original,
            backup,
            new_text,
            code,
        } => {
            assert_eq!(code, 1177);
            assert!(original.ends_with("doc.txt"));
            assert_eq!(fs::read(&backup).unwrap(), b"before", "where the bytes are");
            let new_text = new_text.expect("the new text is kept");
            assert_eq!(fs::read(&new_text).unwrap(), b"after");
            assert!(!path.exists());
            // M14 can find both files. This process made them, so they wait until it ends.
            let found = find_leftovers(&dir).unwrap();
            assert_eq!(found.len(), 2, "{found:#?}");
            assert!(found.iter().all(|l| l.target_missing && l.owner_running));
            assert!(found.iter().all(|l| !l.offer_recovery));
        }
        other => panic!("{other:?}"),
    }
    fs::remove_dir_all(dir).unwrap();
}

/// Item 7: the number of tries and the wait are bounded.
#[test]
fn sharing_violation_tries_ten_times_then_reports() {
    let dir = scratch("tries");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|_, _, _| Err(32))),
        ..Default::default()
    };
    let error = atomic_write_with(&path, b"after", &mut env).unwrap_err();
    assert!(matches!(
        error,
        CodecError::SharingViolation { code: 32, .. }
    ));
    assert_eq!(env.replace_calls, 10);
    assert_eq!(env.sleeps, 9);
    assert_eq!(names_in(&dir), ["doc.txt"]);
    // A code that will not pass with time gets one try.
    let mut env = FakeEnv {
        replace: Some(Box::new(|_, _, _| Err(87))),
        ..Default::default()
    };
    assert!(atomic_write_with(&path, b"after", &mut env).is_err());
    assert_eq!(env.replace_calls, 1);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 12: a file made during a save to a new name is left alone.
#[test]
fn new_file_save_does_not_overwrite_a_file_made_during_the_save() {
    let dir = scratch("race");
    let path = dir.join("doc.txt");
    let other = path.clone();
    let mut env = FakeEnv {
        on_before_commit: Some(Box::new(move |_| {
            fs::write(&other, b"made by another program").unwrap();
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"mine", &mut env).unwrap_err() {
        CodecError::TargetExists {
            path: named,
            leftover,
        } => {
            assert!(named.ends_with("doc.txt"));
            assert_eq!(leftover, None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"made by another program");
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Holds a temp or backup file so that it cannot be deleted while the test runs.
fn hold(path: &Path) -> File {
    use std::os::windows::fs::OpenOptionsExt;
    // Share read and write, not delete: the save's own handle stays legal.
    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .open(path)
        .unwrap()
}
type Held = std::rc::Rc<RefCell<Vec<File>>>;
fn holding_temp() -> (Held, PathHook) {
    let held: Held = Default::default();
    let keep = held.clone();
    (
        held,
        Some(Box::new(move |temp: &Path| {
            keep.borrow_mut().push(hold(temp));
        })),
    )
}

/// The old bytes are safe to delete, but something holds the backup.
#[test]
fn a_backup_that_cannot_go_is_reported_after_a_good_save() {
    let dir = scratch("kept");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let held: Held = Default::default();
    let keep = held.clone();
    let mut env = FakeEnv {
        replace: Some(Box::new(move |target, temp, backup| {
            RealEnv.replace(target, temp, backup)?;
            keep.borrow_mut().push(hold(backup));
            Ok(())
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap() {
        AtomicWriteResult::ReplacedBackupKept { backup } => {
            assert_eq!(fs::read(&backup).unwrap(), b"before");
            assert_eq!(fs::read(&path).unwrap(), b"after");
            held.borrow_mut().clear();
            fs::remove_file(backup).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// A full disk during the temp write, and during the flush. Windows error 112.
#[test]
fn a_failed_write_keeps_the_original_and_removes_the_temp() {
    let dir = scratch("full");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    for (write, flush) in [(Some(112), None), (None, Some(112))] {
        let mut env = FakeEnv {
            write_fails_with: write,
            flush_fails_with: flush,
            ..Default::default()
        };
        match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
            CodecError::SaveFailed {
                code: 112,
                original_intact: true,
                leftover: None,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read(&path).unwrap(), b"before");
        assert_eq!(names_in(&dir), ["doc.txt"]);
    }
    fs::remove_dir_all(dir).unwrap();
}

fn copy_with(input: &Path, dest: &Path, env: &mut FakeEnv) -> Result<PreparedCopy, CodecError> {
    prepare_copy(input, dest, false, &mut |_, _| {}, &mut || false, env)
}

/// `copy_failure` for a large copy, with the temp file going away.
#[test]
fn a_failed_copy_keeps_the_target_and_removes_the_temp() {
    let dir = scratch("copyfail");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, vec![1u8; 1000]).unwrap();
    fs::write(&dest, b"old").unwrap();
    for (write, flush) in [(Some(112), None), (None, Some(112))] {
        let mut env = FakeEnv {
            write_fails_with: write,
            flush_fails_with: flush,
            ..Default::default()
        };
        match copy_with(&input, &dest, &mut env).unwrap_err() {
            CodecError::SaveFailed {
                code: 112,
                original_intact: true,
                leftover: None,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        assert_eq!(names_in(&dir), ["dest", "input"]);
    }
    fs::remove_dir_all(dir).unwrap();
}

/// Item 10: a temp file that cannot be deleted is named, on every failing route.
#[test]
fn a_temp_file_that_cannot_be_deleted_is_named_in_the_error() {
    let dir = scratch("stuck");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, vec![1u8; 1000]).unwrap();
    fs::write(&dest, b"old").unwrap();
    let expect_stuck = |error: CodecError, held: &Held| {
        let CodecError::SaveFailed {
            code: 112,
            original_intact: true,
            leftover: Some(stuck),
            ..
        } = error
        else {
            panic!("{error:?}");
        };
        assert!(stuck.exists(), "named a file that is gone: {stuck:?}");
        held.borrow_mut().clear();
        fs::remove_file(stuck).unwrap();
    };

    // The copy route.
    let (held, hook) = holding_temp();
    let mut env = FakeEnv {
        write_fails_with: Some(112),
        on_temp_created: hook,
        ..Default::default()
    };
    expect_stuck(copy_with(&input, &dest, &mut env).unwrap_err(), &held);

    // The atomic_write route.
    let (held, hook) = holding_temp();
    let mut env = FakeEnv {
        write_fails_with: Some(112),
        on_temp_created: hook,
        ..Default::default()
    };
    expect_stuck(
        atomic_write_with(&dest, b"new", &mut env).unwrap_err(),
        &held,
    );

    // A cancel has no leftover field of its own.
    let (held, hook) = holding_temp();
    let mut env = FakeEnv {
        on_temp_created: hook,
        ..Default::default()
    };
    let error =
        prepare_copy(&input, &dest, false, &mut |_, _| {}, &mut || true, &mut env).unwrap_err();
    match error {
        CodecError::CleanupFailed { cause, leftover } => {
            assert!(matches!(*cause, CodecError::Cancelled), "{cause:?}");
            assert!(leftover.exists());
            held.borrow_mut().clear();
            fs::remove_file(leftover).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(names_in(&dir), ["dest", "input"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 10: `discard` reports what `Drop` cannot.
#[test]
fn discard_reports_a_copy_that_cannot_go() {
    let dir = scratch("discard");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, b"new").unwrap();
    let prepared = prepare_large_copy(&input, &dest, |_, _| {}, || false).unwrap();
    let temp = prepared.temp_path().to_path_buf();
    let holder = hold(&temp);
    match prepared.discard().unwrap_err() {
        CodecError::SaveFailed {
            leftover: Some(stuck),
            ..
        } => assert_eq!(
            fs::canonicalize(stuck).unwrap(),
            fs::canonicalize(&temp).unwrap()
        ),
        other => panic!("{other:?}"),
    }
    drop(holder);
    fs::remove_file(&temp).unwrap();
    let prepared = prepare_large_copy(&input, &dest, |_, _| {}, || false).unwrap();
    prepared.discard().unwrap();
    assert_eq!(names_in(&dir), ["input"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 2: another program writes after the temp is ready and before the swap.
#[test]
fn a_change_just_before_the_swap_is_an_error_and_keeps_both_versions() {
    let dir = scratch("before-swap");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let other = path.clone();
    let mut env = FakeEnv {
        on_before_commit: Some(Box::new(move |_| {
            fs::write(&other, b"other program").unwrap();
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"mine", &mut env).unwrap_err() {
        CodecError::TargetChanged {
            new_text: Some(new_text),
            other_version: None,
            ..
        } => {
            assert_eq!(fs::read(&new_text).unwrap(), b"mine");
            fs::remove_file(new_text).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(env.replace_calls, 0, "the swap ran anyway");
    assert_eq!(fs::read(&path).unwrap(), b"other program");
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 2: the last instant, after the check. The swap has happened, so the other
/// program's bytes are in the backup, and the backup must not be deleted.
#[test]
fn a_change_in_the_last_instant_keeps_the_other_programs_bytes_in_the_backup() {
    let dir = scratch("in-swap");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, temp, backup| {
            fs::write(target, b"other program").unwrap();
            RealEnv.replace(target, temp, backup)
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"mine", &mut env).unwrap_err() {
        CodecError::TargetChanged {
            new_text: None,
            other_version: Some(other),
            ..
        } => {
            assert_eq!(fs::read(&other).unwrap(), b"other program");
            assert_eq!(fs::read(&path).unwrap(), b"mine");
            fs::remove_file(other).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

/// Item 2: the normal swap must not look like a change. `before` must match the
/// original as ReplaceFileW leaves it under the backup name.
#[test]
fn a_real_swap_leaves_the_original_recognisable_under_the_backup_name() {
    let dir = scratch("recognise");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let start = file_state(&path).unwrap().unwrap();
    let seen = std::rc::Rc::new(RefCell::new(None));
    let keep = seen.clone();
    let mut env = FakeEnv {
        replace: Some(Box::new(move |target, temp, backup| {
            RealEnv.replace(target, temp, backup)?;
            *keep.borrow_mut() = file_state(backup).unwrap();
            Ok(())
        })),
        ..Default::default()
    };
    assert_eq!(
        atomic_write_with(&path, b"after", &mut env).unwrap(),
        AtomicWriteResult::Replaced
    );
    let aside = seen.borrow().unwrap();
    assert_eq!((aside.size, aside.written), (start.size, start.written));
    fs::remove_dir_all(dir).unwrap();
}

/// Item 9: a failure names only files that are there.
#[test]
fn errors_do_not_name_a_temp_file_that_is_gone() {
    let dir = scratch("gone");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, temp, _| {
            fs::write(target, b"damaged").unwrap();
            fs::remove_file(temp).unwrap();
            Err(87)
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::SaveFailed {
            original_intact: false,
            leftover: None,
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, temp, backup| {
            fs::rename(target, backup).unwrap();
            fs::remove_file(temp).unwrap();
            Err(1177)
        })),
        move_fails_with: Some(5),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::RecoveryRequired {
            new_text: None,
            backup,
            ..
        } => assert_eq!(fs::read(backup).unwrap(), b"before"),
        other => panic!("{other:?}"),
    }
    fs::remove_dir_all(dir).unwrap();
}

/// Item 13. On FAT a file's number may come from its folder slot, so a moved
/// original can share the empty reservation's number. This forces that on NTFS by
/// writing into the reservation itself. FAT itself was not tested.
#[test]
fn bytes_in_the_reservation_are_never_deleted_as_a_placeholder() {
    let dir = scratch("fat-intact");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|_, _, backup| {
            fs::write(backup, b"bytes").unwrap();
            Err(87)
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::SaveFailed {
            original_intact: true,
            leftover: Some(kept),
            ..
        } => assert_eq!(fs::read(kept).unwrap(), b"bytes"),
        other => panic!("{other:?}"),
    }
    fs::remove_dir_all(&dir).unwrap();

    // The target is gone and the bytes sit in the reservation's slot.
    let dir = scratch("fat-aside");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, _, backup| {
            fs::write(backup, b"before").unwrap();
            fs::remove_file(target).unwrap();
            Err(1177)
        })),
        move_fails_with: Some(5),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::RecoveryRequired { backup, .. } => {
            assert_eq!(fs::read(backup).unwrap(), b"before");
        }
        other => panic!("{other:?}"),
    }
    fs::remove_dir_all(dir).unwrap();
}

/// Item 10: an empty reservation that cannot go is named when nothing else is.
#[test]
fn a_reservation_that_cannot_go_is_named() {
    let dir = scratch("reservation");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let held: Held = Default::default();
    let keep = held.clone();
    let mut env = FakeEnv {
        replace: Some(Box::new(move |target, temp, backup| {
            fs::write(target, b"damaged").unwrap();
            fs::remove_file(temp).unwrap();
            keep.borrow_mut().push(hold(backup));
            Err(87)
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::SaveFailed {
            original_intact: false,
            leftover: Some(kept),
            ..
        } => {
            assert_eq!(fs::read(&kept).unwrap(), b"");
        }
        other => panic!("{other:?}"),
    }
    drop(env);
    held.borrow_mut().clear();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn process_is_running_tells_a_live_process_from_an_ended_one() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    assert!(process_is_running(std::process::id()));
    // Not a real process number: real ones are multiples of 4.
    assert!(!process_is_running(4_294_967_293));
    // The System process exists but cannot be opened: denied still means running.
    assert!(process_is_running(4));
    let mut child = std::process::Command::new("cmd")
        .args(["/c", "ping -n 30 127.0.0.1 >NUL"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(process_is_running(pid), "live child not seen");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        !process_is_running(pid),
        "ended child still seen as running"
    );
}

/// Round 3, item 1: the temp file is checked just before the swap, on both routes.
#[test]
fn a_temp_file_changed_before_the_swap_is_refused() {
    let dir = scratch("temp-changed");
    let (input, dest) = (dir.join("input"), dir.join("dest"));
    fs::write(&input, b"whole text").unwrap();
    fs::write(&dest, b"good file").unwrap();
    let overwrite = || -> PathHook {
        Some(Box::new(|temp: &Path| {
            std::thread::sleep(Duration::from_millis(40));
            fs::write(temp, b"partial").unwrap();
        }))
    };
    let check = |error: CodecError, what: &str| {
        let CodecError::TempChanged {
            temp: Some(temp), ..
        } = &error
        else {
            panic!("{what}: {error:?}");
        };
        assert_eq!(fs::read(temp).unwrap(), b"partial", "{what}");
        assert_eq!(error.left_on_disk(), [temp.as_path()], "{what}");
        fs::remove_file(temp).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"good file", "{what}");
        assert_eq!(names_in(&dir), ["dest", "input"], "{what}");
    };

    let mut env = FakeEnv {
        on_before_commit: overwrite(),
        ..Default::default()
    };
    check(
        atomic_write_with(&dest, b"mine", &mut env).unwrap_err(),
        "atomic_write",
    );
    assert_eq!(env.replace_calls, 0, "the swap ran anyway");

    let mut env = FakeEnv {
        on_before_commit: overwrite(),
        ..Default::default()
    };
    let prepared = copy_with(&input, &dest, &mut FakeEnv::default()).unwrap();
    let failure = prepared.attempt(None, &mut env).unwrap_err();
    assert!(failure.copy.is_none(), "a changed copy must not be retried");
    check(failure.error, "commit");
    assert_eq!(env.replace_calls, 0, "the swap ran anyway");

    // A new name is checked too.
    let fresh = dir.join("fresh");
    let mut env = FakeEnv {
        on_before_commit: overwrite(),
        ..Default::default()
    };
    match atomic_write_with(&fresh, b"mine", &mut env).unwrap_err() {
        CodecError::TempChanged {
            temp: Some(temp), ..
        } => fs::remove_file(temp).unwrap(),
        other => panic!("{other:?}"),
    }
    assert!(!fresh.exists());
    fs::remove_dir_all(dir).unwrap();
}

/// Round 3, item 5: the file came back, but it is not the one the save started from.
#[test]
fn a_restored_file_that_differs_is_reported_as_it_is() {
    let dir = scratch("restored-other");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let mut env = FakeEnv {
        replace: Some(Box::new(|target, _, backup| {
            fs::write(target, b"other program").unwrap();
            fs::rename(target, backup).unwrap();
            Err(1177)
        })),
        ..Default::default()
    };
    match atomic_write_with(&path, b"after", &mut env).unwrap_err() {
        CodecError::TargetChanged {
            new_text: Some(new_text),
            other_version: None,
            ..
        } => {
            assert_eq!(fs::read(&new_text).unwrap(), b"after");
            fs::remove_file(new_text).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"other program");
    assert_eq!(names_in(&dir), ["doc.txt"], "a backup name would be false");
    fs::remove_dir_all(dir).unwrap();
}

/// Round 3, item 6: a Windows name need not be valid Unicode. Only display may change it.
#[test]
fn friendly_keeps_a_name_that_is_not_valid_unicode() {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<u16> = r"C:\a"
        .encode_utf16()
        .chain([0xd800])
        .chain([0x62])
        .collect();
    let exact = PathBuf::from(OsString::from_wide(&units));
    let wide = |p: &Path| p.as_os_str().encode_wide().collect::<Vec<u16>>();
    assert_eq!(wide(&friendly(&verbatim(&exact))), units);
    assert_eq!(wide(&friendly(&exact)), units);
    let unc: Vec<u16> = r"\\host\share\".encode_utf16().chain([0xdc00]).collect();
    let exact = PathBuf::from(OsString::from_wide(&unc));
    assert_eq!(wide(&friendly(&verbatim(&exact))), unc);
    // A path that a plain Windows call could not open keeps its long form.
    let long = PathBuf::from(format!(r"\\?\C:\{}", "a".repeat(300)));
    assert_eq!(friendly(&long), long);
}

/// Round 3, item 7: every file that stayed is named, on both failing routes.
#[test]
fn two_files_that_could_not_go_are_both_named() {
    let dir = scratch("two-left");
    let path = dir.join("doc.txt");
    fs::write(&path, b"before").unwrap();
    let names = |error: &CodecError| -> Vec<String> {
        let mut names: Vec<String> = error
            .left_on_disk()
            .iter()
            .map(|p| p.extension().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    // The target is intact, and something holds the temp file and the reservation.
    for damaged in [false, true] {
        let held: Held = Default::default();
        let keep = held.clone();
        let mut env = FakeEnv {
            replace: Some(Box::new(move |target, temp, backup| {
                if damaged {
                    fs::write(target, b"damaged").unwrap();
                }
                keep.borrow_mut().push(hold(temp));
                keep.borrow_mut().push(hold(backup));
                Err(87)
            })),
            ..Default::default()
        };
        let error = atomic_write_with(&path, b"after", &mut env).unwrap_err();
        assert_eq!(names(&error), ["bak", "tmp"], "{error:?}");
        assert!(
            error.to_string().contains(".bak") || error.to_string().contains(".tmp"),
            "{error}"
        );
        drop(env);
        held.borrow_mut().clear();
        for left in error.left_on_disk() {
            fs::remove_file(left).unwrap();
        }
        fs::write(&path, b"before").unwrap();
    }
    assert_eq!(names_in(&dir), ["doc.txt"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn leftover_names_parse_strictly() {
    let name = leftover_name(0xabcd_0123, 77, 0xfeed, "bak");
    let parsed = parse_leftover_name(&name).unwrap();
    assert_eq!((parsed.hash, parsed.pid), (0xabcd_0123, 77));
    assert_eq!(parsed.kind, LeftoverKind::Backup);
    for other in [
        ".qe-1-2-3.tmp",
        ".qe-notes.txt",
        "x.tmp",
        ".qe-abcd0123-7-feed.tmp",
    ] {
        assert!(parse_leftover_name(other).is_none(), "{other}");
    }
}

#[test]
fn verbatim_and_friendly_forms() {
    let plain = Path::new(r"C:\a\b.txt");
    assert_eq!(verbatim(plain), Path::new(r"\\?\C:\a\b.txt"));
    assert_eq!(friendly(&verbatim(plain)), plain);
    let share = Path::new(r"\\host\share\b.txt");
    assert_eq!(verbatim(share), Path::new(r"\\?\UNC\host\share\b.txt"));
    assert_eq!(friendly(&verbatim(share)), share);
}

#[test]
fn committed_temp_stamp_matches_replaced_file_on_this_volume() {
    let dir = scratch("committed-stamp");
    let path = dir.join("doc.txt");
    fs::write(&path, b"old").unwrap();
    let expected = file_stamp(&path).unwrap().unwrap();
    let (_, committed) = atomic_write_guarded(&path, b"mine", Some(expected), false).unwrap();
    assert_eq!(file_stamp(&path).unwrap(), Some(committed));
    // A same-size write inside one step of the file time, about 16 ms on NTFS, leaves the
    // stamp as it was, and the next save then overwrites it unseen. This wait does not
    // close that limit, which `FileStamp` also states. It only tests a write past it.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&path, b"THEM").unwrap();
    assert!(matches!(
        atomic_write_guarded(&path, b"mine", Some(committed), false),
        Err(CodecError::TargetChanged { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), b"THEM");
}

#[test]
fn absent_guard_refuses_a_recreated_file() {
    let dir = scratch("absent-guard");
    let path = dir.join("doc.txt");
    fs::write(&path, b"theirs").unwrap();
    assert!(matches!(
        atomic_write_guarded(&path, b"mine", None, true),
        Err(CodecError::TargetChanged { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), b"theirs");
}

fn state(index: u64, size: u64, written: u64) -> FileStamp {
    FileStamp(FileState {
        volume: 7,
        index,
        size,
        written,
    })
}

#[test]
fn only_a_new_file_index_after_the_swap_keeps_the_stamp_seen_on_disk() {
    let committed = state(1, 10, 100);
    let renamed = state(2, 10, 100);
    assert_eq!(stamp_after_swap(committed, Some(renamed)), Some(renamed));
    assert_eq!(
        stamp_after_swap(committed, Some(committed)),
        Some(committed)
    );
}

#[test]
fn a_new_size_or_time_after_the_swap_keeps_no_stamp() {
    let committed = state(1, 10, 100);
    assert_eq!(stamp_after_swap(committed, Some(state(1, 10, 101))), None);
    assert_eq!(stamp_after_swap(committed, Some(state(2, 10, 101))), None);
    assert_eq!(stamp_after_swap(committed, Some(state(1, 11, 100))), None);
    assert_eq!(stamp_after_swap(committed, None), None);
}

#[test]
fn the_stamp_a_save_returns_is_of_its_temp_file_not_of_the_name_after_the_swap() {
    let dir = scratch("stamp-not-by-name");
    let path = dir.join("doc.txt");
    fs::write(&path, b"old").unwrap();
    let expected = file_stamp(&path).unwrap().unwrap();
    let target = path.clone();
    let (_, committed) = atomic_write_guarded_with(
        &path,
        b"mine",
        Some(expected),
        false,
        SaveHooks {
            before_commit: &|| {},
            before_replace: &|| {},
            after_commit: &|| fs::write(&target, b"a longer outside text").unwrap(),
        },
    )
    .unwrap();
    assert_eq!(committed.0.size, 4, "the stamp is of what this save wrote");
    assert_ne!(file_stamp(&path).unwrap(), Some(committed));
}

#[test]
fn a_file_holding_the_bytes_is_told_from_one_that_does_not() {
    let dir = scratch("holds-bytes");
    let path = dir.join("doc.txt");
    fs::write(&path, b"mine").unwrap();
    let stamp = file_stamp(&path).unwrap().unwrap();
    assert!(file_holds_bytes(&path, b"mine", stamp));
    assert!(!file_holds_bytes(&path, b"THEM", stamp));
    assert!(!file_holds_bytes(&path, b"mine and more", stamp));
    assert!(!file_holds_bytes(&dir.join("gone.txt"), b"mine", stamp));
    let old = stamp;
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&path, b"mine").unwrap();
    assert!(
        !file_holds_bytes(&path, b"mine", old),
        "a stamp that no longer holds says nothing about the bytes"
    );
    let large = vec![7u8; 300_000];
    fs::write(&path, &large).unwrap();
    let stamp = file_stamp(&path).unwrap().unwrap();
    assert!(file_holds_bytes(&path, &large, stamp));
    let mut differs = large.clone();
    differs[299_999] = 8;
    assert!(!file_holds_bytes(&path, &differs, stamp));
}
