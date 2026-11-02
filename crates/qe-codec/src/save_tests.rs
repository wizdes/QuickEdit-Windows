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
    let wanted = narrow.clone();
    let mut env = FakeEnv {
        on_temp_created: Some(Box::new(move |temp| {
            let rules = rules_of(temp);
            assert!(
                !rules.contains(";WD)"),
                "open rule on the temp file: {rules}"
            );
            assert!(wanted.contains("OW"), "{wanted}");
            assert!(rules.contains(";OW)"), "{rules}");
        })),
        ..Default::default()
    };
    atomic_write_with(&path, b"after", &mut env).unwrap();
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
