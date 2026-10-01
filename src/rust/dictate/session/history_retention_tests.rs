use super::history_sink::{HistorySink, JsonlHistorySink};
use serde_json::json;

#[test]
fn explicit_sink_retention_keeps_filtered_tail_and_recovery_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let sink = JsonlHistorySink::new(path.clone()).with_retention(1, None);
    sink.append(&json!({"text":"first", "api_key":"must not persist"}));
    sink.append(&json!({"text":"second", "api_key":"must not persist"}));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"text\":\"second\"}\n");
    let backup = crate::jsonl_file::sibling(&path, ".retention-backup").unwrap();
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "{\"text\":\"first\"}\n");
}

#[test]
fn retention_failure_is_nonfatal_and_preserves_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    std::fs::write(&path, b"broken final row").unwrap();
    JsonlHistorySink::new(path.clone()).with_retention(1, None).append(&json!({"text":"second"}));
    assert_eq!(std::fs::read(path).unwrap(), b"broken final row");
}

#[test]
fn live_retention_config_wins_over_environment_and_invalid_values_disable_pruning() {
    use crate::config::test_support::{restore_env, ENV_LOCK};
    use super::history_sink::effective_history_settings;
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let old_config = std::env::var_os("VOICEPI_CONFIG");
    let old_limit = std::env::var_os("VOICEPI_HISTORY_MAX_ENTRIES");
    std::env::set_var("VOICEPI_CONFIG", &config_path);
    std::env::set_var("VOICEPI_HISTORY_MAX_ENTRIES", "3");
    std::fs::write(&config_path, "{}").unwrap();
    assert_eq!(effective_history_settings().max_entries, 3);
    std::fs::write(&config_path, r#"{"history_max_entries":"1"}"#).unwrap();
    assert_eq!(effective_history_settings().max_entries, 1);
    std::fs::write(&config_path, r#"{"history_max_entries":"broken"}"#).unwrap();
    assert_eq!(effective_history_settings().max_entries, 0);
    restore_env("VOICEPI_CONFIG", old_config);
    restore_env("VOICEPI_HISTORY_MAX_ENTRIES", old_limit);
}
