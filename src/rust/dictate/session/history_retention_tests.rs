use super::history_sink::{HistorySink, JsonlHistorySink};
use serde_json::json;

#[test]
fn explicit_sink_retention_keeps_filtered_tail_and_recovery_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let sink = JsonlHistorySink::new(path.clone()).with_retention(1, None);
    sink.append(&json!({"text":"first", "api_key":"must not persist"}));
    sink.append(&json!({"text":"second", "api_key":"must not persist"}));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"text\":\"second\"}\n"
    );
    let backup = crate::jsonl_file::sibling(&path, ".retention-backup").unwrap();
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "{\"text\":\"first\"}\n"
    );
}

#[test]
fn invalid_history_keeps_recording_without_pruning_old_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    std::fs::write(&path, b"broken final row").unwrap();
    JsonlHistorySink::new(path.clone())
        .with_retention(1, None)
        .append(&json!({"text":"second"}));
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"broken final row\n{\"text\":\"second\"}\n"
    );
}

#[test]
fn live_retention_config_wins_over_environment_and_invalid_values_disable_pruning() {
    use super::history_sink::effective_history_settings;
    use crate::config::test_support::{restore_env, ENV_LOCK};
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
    for invalid in [
        json!(true),
        json!(false),
        json!(null),
        json!([]),
        json!({}),
        json!(1.5),
        json!(1.0),
        json!(-1),
        json!(100001),
        json!(""),
        json!("broken"),
    ] {
        std::fs::write(
            &config_path,
            json!({"history_max_entries":invalid}).to_string(),
        )
        .unwrap();
        assert_eq!(effective_history_settings().max_entries, 0);
    }
    for valid in [json!(1), json!("1")] {
        std::fs::write(
            &config_path,
            json!({"history_max_entries":valid}).to_string(),
        )
        .unwrap();
        assert_eq!(effective_history_settings().max_entries, 1);
    }
    std::fs::write(&config_path, "{}").unwrap();
    std::env::set_var("VOICEPI_HISTORY_MAX_ENTRIES", "invalid");
    assert_eq!(effective_history_settings().max_entries, 0);
    restore_env("VOICEPI_CONFIG", old_config);
    restore_env("VOICEPI_HISTORY_MAX_ENTRIES", old_limit);
}

#[test]
fn live_history_retention_protects_configured_metrics_even_when_disabled() {
    use super::history_sink::ReloadingHistorySink;
    use crate::config::test_support::{restore_env, ENV_LOCK};
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let path = dir.path().join("shared.jsonl");
    let original = b"{\"text\":\"keep\"}\n";
    std::fs::write(&path, original).unwrap();
    let old_config = std::env::var_os("VOICEPI_CONFIG");
    std::env::set_var("VOICEPI_CONFIG", &config_path);
    for json_output in [false, true] {
        std::fs::write(
            &config_path,
            json!({
                "history_enabled": true,
                "history_jsonl": path,
                "history_max_entries": "1",
                "metrics_jsonl": path,
                "json_output": json_output,
            })
            .to_string(),
        )
        .unwrap();
        let error = ReloadingHistorySink::new()
            .append_with_result(&json!({"text":"second"}))
            .unwrap_err();
        assert!(error.to_string().contains("separate file from metrics"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!crate::jsonl_file::sibling(&path, ".retention-backup")
            .unwrap()
            .exists());
    }
    restore_env("VOICEPI_CONFIG", old_config);
}

#[test]
fn history_retention_metrics_guard_uses_snapshot_and_config_env_precedence() {
    use super::history_sink::{
        effective_history_settings_with_metrics_path, history_settings_from_snapshot,
    };
    use crate::config::test_support::{restore_env, ENV_LOCK};
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let history = dir.path().join("history.jsonl");
    let metrics = dir.path().join("config-metrics.jsonl");
    let env_metrics = dir.path().join("env-metrics.jsonl");
    let old_config = std::env::var_os("VOICEPI_CONFIG");
    let old_metrics = std::env::var_os("VOICEPI_METRICS_JSONL");
    std::env::set_var("VOICEPI_CONFIG", &config_path);
    std::env::set_var("VOICEPI_METRICS_JSONL", &env_metrics);
    let snapshot = json!({
        "history_enabled": true,
        "history_jsonl": history,
        "history_max_entries": "1",
        "metrics_jsonl": metrics,
        "json_output": false,
    });
    std::fs::write(&config_path, snapshot.to_string()).unwrap();
    let initial = effective_history_settings_with_metrics_path();
    assert_eq!(initial.0.path, history);
    assert_eq!(initial.0.max_entries, 1);
    assert_eq!(initial.1, Some(metrics));

    // A later file version cannot change either half of an already-read snapshot.
    std::fs::write(
        &config_path,
        r#"{"history_max_entries":"2","metrics_jsonl":null}"#,
    )
    .unwrap();
    assert_eq!(history_settings_from_snapshot(&snapshot), initial);
    assert_eq!(effective_history_settings_with_metrics_path().1, None);
    for clear in [json!(null), json!(""), json!("   ")] {
        assert_eq!(
            history_settings_from_snapshot(&json!({"metrics_jsonl":clear})).1,
            None
        );
    }
    assert_eq!(
        history_settings_from_snapshot(&json!({})).1,
        Some(env_metrics)
    );
    restore_env("VOICEPI_CONFIG", old_config);
    restore_env("VOICEPI_METRICS_JSONL", old_metrics);
}

#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
#[test]
fn static_history_retention_protects_metrics_with_json_output_disabled() {
    use super::history_sink::history_sink_from_app_settings;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.jsonl");
    let original = b"{\"text\":\"keep\"}\n";
    std::fs::write(&path, original).unwrap();
    let settings = crate::config::AppSettings {
        history_enabled: true,
        history_jsonl: path.to_string_lossy().into_owned(),
        history_max_entries: "1".to_owned(),
        metrics_jsonl: path.to_string_lossy().into_owned(),
        inject_json: false,
        ..Default::default()
    };
    history_sink_from_app_settings(&settings)
        .unwrap()
        .append(&json!({"text":"second"}));
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
