use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn help_uses_public_binary_name_even_when_binary_path_differs() {
    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("--help")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("Usage: whisper-dictate [COMMAND]"));
    assert!(!stdout.contains("Usage: whisper-dictate-app"));
}

#[test]
fn version_flag_prints_public_version_line() {
    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("--version")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.starts_with("whisper-dictate "));
}

#[test]
fn worker_failure_does_not_print_rust_backtrace() {
    let Some(python) = test_python() else {
        eprintln!("skipping: no Python launcher found on PATH");
        return;
    };
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
        .env("VOICEPI_PYTHON", python)
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

fn test_python() -> Option<PathBuf> {
    for candidate in python_candidates() {
        if let Some(path) = find_on_path(candidate) {
            return Some(path);
        }
    }
    None
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    if name.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(name);
        return path.exists().then_some(path);
    }

    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|path| path.exists())
    })
}

fn python_candidates() -> &'static [&'static str] {
    if cfg!(windows) {
        &["py.exe", "py", "python.exe", "python"]
    } else {
        &["python3", "python"]
    }
}
