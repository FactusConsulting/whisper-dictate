use std::fs;
use std::process::Command;

#[test]
fn worker_failure_does_not_print_rust_backtrace() {
    let dir = tempfile::tempdir().unwrap();
    let worker = dir.path().join("voice_pi.py");
    fs::write(
        &worker,
        "import sys\nprint('fake doctor failed')\nsys.exit(7)\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("doctor")
        .env("VOICEPI_APP_ROOT", dir.path())
        .env("VOICEPI_PYTHON", "python3")
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success());
    assert!(stdout.contains("fake doctor failed"));
    assert!(stderr.contains("worker exited with status"));
    assert!(!stderr.contains("Stack backtrace"));
}
