//! Real Windows command-hook launch from an executable path containing spaces.
#![cfg(windows)]

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn quoted_windows_hook_executable_path_launches_without_losing_backslashes() {
    let dir = tempfile::tempdir().unwrap();
    let tool_dir = dir.path().join("hook tools with spaces");
    std::fs::create_dir(&tool_dir).unwrap();
    let tool = tool_dir.join("hook processor.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_wd"), &tool).unwrap();
    let hook = format!("\"{}\" --help", tool.display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_wd"))
        .arg("command-hook")
        .env("VOICEPI_COMMAND_HOOK", hook)
        // First launch of a copied executable can include Windows runner/AV
        // startup work. This test checks argv fidelity, not deadline behavior.
        .env("VOICEPI_COMMAND_HOOK_TIMEOUT_MS", "30000")
        .env("VOICEPI_CONFIG", dir.path().join("config.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"text":"test"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["returncode"], 0, "{report}");
    assert_eq!(report["timeout"], false, "{report}");
    assert!(report["error"].is_null(), "{report}");
}
