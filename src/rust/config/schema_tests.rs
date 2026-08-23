use std::env;

use serde_json::Value;

use super::{effective_runtime_config, effective_runtime_env, runtime_settings, RuntimeSetting};
use crate::config::io::{load_settings_from_path, save_settings_to_path, CONFIG_ENV};
use crate::config::test_support::{restore_env, ENV_LOCK};

#[test]
fn public_setup_metadata_api_is_populated() {
    let settings: &[RuntimeSetting] = runtime_settings();
    assert!(!settings.is_empty());
    assert!(settings
        .iter()
        .any(|setting| setting.key == "stt_backend" && !setting.choices.is_empty()));

    let effective = effective_runtime_config();
    assert!(effective.contains_key("model"));
    assert!(effective.contains_key("stt_backend"));
}

#[test]
fn model_choices_match_the_visible_download_catalog() {
    let model = runtime_settings()
        .iter()
        .find(|setting| setting.key == "model")
        .expect("model setting");
    let visible = crate::whisper::model_manager::visible_catalog()
        .map(|entry| entry.name.to_owned())
        .collect::<Vec<_>>();

    assert_eq!(model.choices, visible);
}

#[test]
fn model_description_enumerates_the_hidden_download_catalog() {
    let model = runtime_settings()
        .iter()
        .find(|setting| setting.key == "model")
        .expect("model setting");
    let documented = model
        .description
        .split_once("Hidden legacy ")
        .and_then(|(_, names)| names.split_once(" values remain loadable"))
        .map(|(names, _)| names.replace(", and ", ", "))
        .expect("model description must delimit its hidden legacy names")
        .split(", ")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let hidden = crate::whisper::model_manager::CATALOG
        .iter()
        .filter(|entry| entry.hidden)
        .map(|entry| entry.name.to_owned())
        .collect::<Vec<_>>();

    assert_eq!(documented, hidden);
}

#[test]
fn schema_marks_the_optional_string_settings_as_nullable() {
    let nullable = runtime_settings()
        .iter()
        .filter(|setting| setting.nullable)
        .map(|setting| setting.key.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        nullable,
        [
            "stt_model",
            "audio_device",
            "lang",
            "xkb_layout",
            "initial_prompt",
            "dictionary",
            "metrics_jsonl",
            "command_hook",
            "history_jsonl",
            "post_redact_terms",
        ]
    );
}

#[test]
fn explicit_null_restart_setting_suppresses_ambient_environment() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"audio_device":null}"#).unwrap();

    let old_config = env::var_os(CONFIG_ENV);
    let old_device = env::var_os("VOICEPI_AUDIO_DEVICE");
    env::set_var(CONFIG_ENV, &path);
    env::set_var("VOICEPI_AUDIO_DEVICE", "Yeti Classic");

    let resolved = effective_runtime_env();

    assert!(!resolved.contains_key("VOICEPI_AUDIO_DEVICE"));
    restore_env(CONFIG_ENV, old_config);
    restore_env("VOICEPI_AUDIO_DEVICE", old_device);
}

#[test]
fn worker_overrides_carry_explicit_clear_markers() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"lang":null}"#).unwrap();

    let old_config = env::var_os(CONFIG_ENV);
    let old_lang = env::var_os("VOICEPI_LANG");
    env::set_var(CONFIG_ENV, &path);
    env::set_var("VOICEPI_LANG", "da");

    let overrides = super::worker_env_overrides();

    assert_eq!(
        overrides
            .iter()
            .find(|(name, _)| name == "VOICEPI_LANG")
            .map(|(_, value)| value.as_str()),
        Some("")
    );
    restore_env(CONFIG_ENV, old_config);
    restore_env("VOICEPI_LANG", old_lang);
}

#[test]
fn non_nullable_null_uses_ambient_then_schema_default() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"key":null,"model":null}"#).unwrap();

    let old_config = env::var_os(CONFIG_ENV);
    let old_key = env::var_os("VOICEPI_KEY");
    let old_model = env::var_os("VOICEPI_MODEL");
    env::set_var(CONFIG_ENV, &path);
    env::set_var("VOICEPI_KEY", "f9");
    env::remove_var("VOICEPI_MODEL");

    let resolved = effective_runtime_env();

    assert_eq!(resolved.get("VOICEPI_KEY").map(String::as_str), Some("f9"));
    assert_eq!(
        resolved.get("VOICEPI_MODEL").map(String::as_str),
        Some("large-v3-turbo")
    );
    restore_env(CONFIG_ENV, old_config);
    restore_env("VOICEPI_KEY", old_key);
    restore_env("VOICEPI_MODEL", old_model);
}

#[test]
fn unrelated_save_keeps_ambient_language_effective() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"ui_theme":"dark"}"#).unwrap();

    let old_config = env::var_os(CONFIG_ENV);
    let old_lang = env::var_os("VOICEPI_LANG");
    env::set_var(CONFIG_ENV, &path);
    env::set_var("VOICEPI_LANG", "da");

    let mut settings = load_settings_from_path(&path).unwrap();
    settings.ui_theme = "light".to_owned();
    save_settings_to_path(&settings, &path).unwrap();
    let resolved = effective_runtime_env();

    assert_eq!(resolved.get("VOICEPI_LANG").map(String::as_str), Some("da"));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(!saved.as_object().unwrap().contains_key("lang"));
    restore_env(CONFIG_ENV, old_config);
    restore_env("VOICEPI_LANG", old_lang);
}
