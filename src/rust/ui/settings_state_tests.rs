#![cfg(windows)]

use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

#[test]
fn windows_settings_save_reload_keeps_auto_language_authoritative() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"lang":"en"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _lang_guard = EnvVarGuard::set("VOICEPI_LANG", "da");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.settings.lang.clear();

    app.save_settings();
    app.reload_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("lang"), Some(&serde_json::Value::Null));
    assert!(app.settings.lang.is_empty());
    assert!(!config::effective_runtime_env().contains_key("VOICEPI_LANG"));
}

#[test]
fn windows_settings_unrelated_save_preserves_explicit_metrics_clear() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"metrics_jsonl":null,"log_level":"info"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.config_path = path.display().to_string();
    app.reload_settings();
    assert!(app.settings.metrics_jsonl.is_empty());

    app.settings.log_level = "debug".to_owned();
    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("metrics_jsonl"), Some(&serde_json::Value::Null));
    assert_eq!(
        raw.get("log_level").and_then(|value| value.as_str()),
        Some("debug")
    );
}

#[test]
fn windows_settings_auto_selection_clears_absent_language_key() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"log_level":"info"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _lang_guard = EnvVarGuard::set("VOICEPI_LANG", "da");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.record_nullable_selection("lang", "");
    assert!(app.has_unsaved_settings());

    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("lang"), Some(&serde_json::Value::Null));
    assert!(!config::effective_runtime_env().contains_key("VOICEPI_LANG"));
}
