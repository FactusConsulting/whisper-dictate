//! Exercise the real CLI without touching the user's configuration.
use serde_json::json;
use std::process::Command;

#[test]
fn unrelated_cli_write_keeps_environment_owned_settings_absent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let before = json!({"lang": "da", "initial_prompt": null, "foreign": {"kept": true}});
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wd"))
        .args(["config", "set", "lang", "en", "--config"])
        .arg(&path)
        .env("VOICEPI_LOCAL_ONLY", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored,
        json!({"lang": "en", "initial_prompt": null, "foreign": {"kept": true}})
    );
}
