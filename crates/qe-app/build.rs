//! Puts the application manifest and the icon inside `QuickEdit.exe`, using `rc.exe` from the
//! Windows SDK. The link arguments reach the bin only, so test executables stay as they were.
//!
//! A release build with no `rc.exe` stops with a named error, because it would make an exe with
//! no icon and no manifest. Any other build prints a warning and goes on.
//!
//! `QE_RC` names an `rc.exe` to use, and then no other is tried.
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let root = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let packaging = root.join("..").join("..").join("packaging");
    let manifest = packaging.join("QuickEdit.exe.manifest");
    let icon = packaging.join("QuickEdit.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-env-changed=QE_RC");
    if env("CARGO_CFG_TARGET_OS") != "windows" {
        return;
    }
    // UI Automation and the shell are slow to load, and most runs reach for them late or never.
    // The linker loads them at their first call, so the first frame does not wait for them.
    if env("CARGO_CFG_TARGET_ENV") == "msvc" {
        for library in ["UIAutomationCore.dll", "SHELL32.dll", "PROPSYS.dll"] {
            println!("cargo:rustc-link-arg-bin=QuickEdit=/DELAYLOAD:{library}");
        }
        println!("cargo:rustc-link-arg-bin=QuickEdit=delayimp.lib");
    }
    let release = env("PROFILE") == "release";
    let compiler = match find_rc() {
        Ok(path) => path,
        Err(problem) if release => stop(&format!(
            "QE_RC_MISSING: a release build needs rc.exe from the Windows SDK. {problem}"
        )),
        Err(problem) => {
            println!(
                "cargo:warning=QuickEdit.exe has no manifest and no icon in this build: {problem}"
            );
            return;
        }
    };
    let out = PathBuf::from(env("OUT_DIR"));
    let script = out.join("QuickEdit.rc");
    let resources = out.join("QuickEdit.res");
    // 24 is RT_MANIFEST, and 1 is the id Windows reads for an exe's manifest. The lowest icon
    // id is the one the shell shows.
    let text = format!(
        "1 24 \"{}\"\n1 ICON \"{}\"\n",
        rc_path(&manifest),
        rc_path(&icon)
    );
    if let Err(error) = std::fs::write(&script, text) {
        stop(&format!(
            "QE_RC_FAILED: writing {}: {error}",
            script.display()
        ));
    }
    let output = Command::new(&compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resources)
        .arg(&script)
        .output();
    match output {
        Ok(output) if output.status.success() => {}
        Ok(output) => stop(&format!(
            "QE_RC_FAILED: {} said {}{}",
            compiler.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) => stop(&format!(
            "QE_RC_FAILED: {} did not run: {error}",
            compiler.display()
        )),
    }
    // The linker's own manifest would be a second copy of resource 24.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
    println!("cargo:rustc-link-arg-bins={}", resources.display());
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| stop(&format!("cargo did not set {name}")))
}

fn stop(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// An `.rc` string treats a backslash as an escape.
fn rc_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

/// `QE_RC`, then the newest SDK's x64 folder, then the `PATH`.
fn find_rc() -> Result<PathBuf, String> {
    if let Some(named) = std::env::var_os("QE_RC") {
        let named = PathBuf::from(named);
        return if named.is_file() {
            Ok(named)
        } else {
            Err(format!(
                "QE_RC names {}, which is not a file",
                named.display()
            ))
        };
    }
    let kits = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .map(|folder| folder.join("Windows Kits").join("10").join("bin"));
    if let Some(found) = kits.as_deref().and_then(newest_in_kits) {
        return Ok(found);
    }
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|folder| folder.join("rc.exe"))
            .find(|file| file.is_file())
    });
    on_path.ok_or_else(|| {
        "It is not in \"Windows Kits\\10\\bin\" or on PATH. Install the SDK or set QE_RC.".into()
    })
}

fn newest_in_kits(bin: &Path) -> Option<PathBuf> {
    let mut found: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(bin)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = name
                .split('.')
                .map(|part| part.parse().ok())
                .collect::<Option<Vec<u32>>>()?;
            let file = entry.path().join("x64").join("rc.exe");
            file.is_file().then_some((version, file))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, file)| file)
}
