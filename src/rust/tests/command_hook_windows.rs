//! Real Windows command-hook launch from an executable path containing spaces.
#![cfg(windows)]

use whisper_dictate_app::command_hook::run_command_hook;

#[test]
fn quoted_windows_hook_executable_path_launches_without_losing_backslashes() {
    let dir = tempfile::tempdir().unwrap();
    let tool_dir = dir.path().join("hook tools with spaces");
    std::fs::create_dir(&tool_dir).unwrap();
    let tool = tool_dir.join("hook processor.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_wd"), &tool).unwrap();
    // This test binary holds exactly one test, so process-wide env is private.
    std::env::set_var(
        "VOICEPI_COMMAND_HOOK",
        format!("\"{}\" --help", tool.display()),
    );
    // First launch of a copied executable can include Windows runner/AV
    // startup work. This test checks argv fidelity, not deadline behavior.
    std::env::set_var("VOICEPI_COMMAND_HOOK_TIMEOUT_MS", "30000");
    std::env::set_var("VOICEPI_CONFIG", dir.path().join("config.json"));

    let report = run_command_hook(&serde_json::json!({"text": "test"}));

    assert!(report.enabled, "{report:?}");
    assert_eq!(report.returncode, Some(0), "{report:?}");
    assert!(!report.timeout, "{report:?}");
    assert!(report.error.is_none(), "{report:?}");
}
