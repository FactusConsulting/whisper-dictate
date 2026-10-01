use super::{runtime_value, validate_numeric};
use crate::config::{runtime_settings, AppSettings};

#[test]
fn schema_numeric_min_and_max_are_inclusive_and_zero_disables_remain_valid() {
    for setting in runtime_settings() {
        if let (Some(min), Some(max)) = (setting.min, setting.max) {
            for value in [min, max] {
                validate_numeric(&setting.key, &value.to_string()).unwrap();
            }
            assert!(validate_numeric(&setting.key, &(min - 1.0).to_string()).is_err());
            assert!(validate_numeric(&setting.key, &(max + 1.0).to_string()).is_err());
        }
    }
    for key in [
        "release_tail_ms",
        "preview_seconds",
        "max_record_s",
        "max_chars_per_second",
        "dictionary_max_terms",
        "dictionary_prompt_chars",
    ] {
        validate_numeric(key, "0").unwrap();
    }
    for (key, value) in [
        ("release_tail_ms", "250.9"),
        ("preview_seconds", "3.5"),
        ("max_record_s", "30.5"),
        ("max_chars_per_second", "30.5"),
    ] {
        validate_numeric(key, value).unwrap();
    }
    validate_numeric("command_hook_timeout_ms", "100.5").unwrap();
}

#[test]
fn every_numeric_setting_rejects_nonfinite_and_unrepresentable_values() {
    let defaults = serde_json::to_value(AppSettings::default()).unwrap();
    for key in defaults.as_object().unwrap().keys() {
        if super::float_setting(key) {
            for bad in ["NaN", "inf", "-inf", "1e300", "not a number"] {
                assert!(validate_numeric(key, bad).is_err(), "{key}: {bad}");
                let fallback = runtime_value(key, bad.to_owned());
                assert!(
                    validate_numeric(key, &fallback).is_ok(),
                    "{key}: {fallback}"
                );
            }
        }
    }
}

#[test]
fn explicit_config_set_rejects_invalid_numbers_without_touching_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let before = r#"{"release_tail_ms":"200","unknown":{"kept":true},"lang":null}"#;
    std::fs::write(&path, before).unwrap();
    for (key, bad) in [
        ("release_tail_ms", "60000"),
        ("release_tail_ms", "-1"),
        ("release_tail_ms", "NaN"),
        ("target_dbfs", "inf"),
        ("command_hook_timeout_ms", "600001"),
        ("post_timeout_ms", "0"),
        ("history_max_entries", "1.5"),
        ("history_max_entries", "100001"),
    ] {
        let error = crate::config::set_value(key, bad, &path)
            .unwrap_err()
            .to_string();
        assert!(error.contains(key), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}

#[test]
fn history_retention_default_roundtrip_and_restart_contract() {
    let defaults = AppSettings::default();
    assert_eq!(defaults.history_max_entries, "0");
    let setting = runtime_settings().iter().find(|setting| setting.key == "history_max_entries").unwrap();
    assert_eq!(setting.default.as_deref(), Some("0"));
    assert!(!setting.live);
    assert!(setting.advanced);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, "{}").unwrap();
    crate::config::set_value("history_max_entries", "25", &path).unwrap();
    let after = crate::config::load_settings_from_path(&path).unwrap();
    assert_eq!(after.history_max_entries, "25");
    assert_eq!(crate::config::restart_required_keys(&defaults, &after), vec!["history_max_entries"]);
    let invalid = AppSettings::from_value(serde_json::json!({"history_max_entries":"-1"})).unwrap();
    assert_eq!(invalid.history_max_entries, "0");
}

#[test]
fn hand_edited_numeric_values_fall_back_in_memory_without_changing_other_settings() {
    let settings = AppSettings::from_value(serde_json::json!({
        "release_tail_ms":"60000", "target_dbfs":"NaN", "local_only":"1", "lang":"da"
    }))
    .unwrap();
    assert_eq!(settings.release_tail_ms, "200");
    assert_eq!(settings.target_dbfs, "-20");
    assert!(settings.local_only);
    assert_eq!(settings.lang, "da");
    settings.validate().unwrap();
}
