#[cfg(windows)]
#[test]
fn painted_text_and_selection_round_trip_through_uia() {
    let exe = env!("CARGO_BIN_EXE_spike-trials");
    let output = std::process::Command::new(exe)
        .args(["--trial", "uia", "--self-check"])
        .output()
        .expect("start UI Automation self-check");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS: UIA text and selection"));
}
