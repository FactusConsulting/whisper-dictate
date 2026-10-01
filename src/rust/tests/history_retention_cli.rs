use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::json;

fn append_history(config: &Path, history: &Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wd"))
        .arg("append-history")
        .arg("--path")
        .arg(history)
        .env("VOICEPI_CONFIG", config)
        .env("VOICEPI_HISTORY_MAX_ENTRIES", "1")
        .env("VOICEPI_METRICS_JSONL", history)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"text\":\"new\"}")
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn hidden_history_retention_protects_disabled_metrics_and_honors_explicit_clear() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let history = dir.path().join("history.jsonl");
    let original = b"{\"text\":\"keep\"}\n";
    for json_output in [false, true] {
        fs::write(&history, original).unwrap();
        fs::write(
            &config,
            json!({"json_output":json_output,"metrics_jsonl":history}).to_string(),
        )
        .unwrap();
        let output = append_history(&config, &history);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("separate file from metrics"));
        assert_eq!(fs::read(&history).unwrap(), original);
    }
    fs::write(&config, r#"{"metrics_jsonl":null}"#).unwrap();
    let output = append_history(&config, &history);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "{\"text\":\"new\"}\n"
    );
}

#[test]
fn hidden_history_retention_fails_closed_on_invalid_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let history = dir.path().join("history.jsonl");
    let original = b"{\"text\":\"keep\"}\n";
    fs::write(&history, original).unwrap();
    fs::write(&config, "broken json").unwrap();
    let output = append_history(&config, &history);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("history config read failed"));
    assert_eq!(fs::read(&history).unwrap(), original);
}

#[test]
fn hidden_history_retention_unlimited_allows_shared_metrics_without_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let history = dir.path().join("history.jsonl");
    fs::write(&history, b"{\"text\":\"keep\"}\n").unwrap();
    fs::write(
        &config,
        json!({"history_max_entries":"0","metrics_jsonl":history,"json_output":false}).to_string(),
    )
    .unwrap();
    let output = append_history(&config, &history);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "{\"text\":\"keep\"}\n{\"text\":\"new\"}\n"
    );
    assert!(!history
        .with_file_name("history.jsonl.retention-backup")
        .exists());
}
