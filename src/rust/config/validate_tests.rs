use crate::config::AppSettings;

#[test]
fn persistence_validation_rejects_nonfinite_and_out_of_schema_numeric_values() {
    for (key, value) in [
        ("release_tail_ms", "60000"),
        ("release_tail_ms", "NaN"),
        ("min_record_seconds", "-1"),
        ("target_dbfs", "inf"),
        ("command_hook_timeout_ms", "600001"),
        ("update_check_interval_minutes", "1"),
    ] {
        let mut fields = serde_json::to_value(AppSettings::default()).unwrap();
        fields[key] = serde_json::json!(value);
        let settings: AppSettings = serde_json::from_value(fields).unwrap();
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains(key), "{error}");
    }
}

fn with_field(key: &str, value: &str) -> AppSettings {
    let mut fields = serde_json::to_value(AppSettings::default()).unwrap();
    fields[key] = serde_json::json!(value);
    serde_json::from_value(fields).unwrap()
}

#[test]
fn validation_rejects_values_outside_schema_choices() {
    assert_eq!(
        with_field("format_commands", "bogus")
            .validate()
            .unwrap_err()
            .to_string(),
        "format_commands must be one of off, en, da, both; got \"bogus\""
    );
    assert_eq!(
        with_field("log_level", "bogus")
            .validate()
            .unwrap_err()
            .to_string(),
        "log_level must be one of off, info, debug, trace; got \"bogus\""
    );
}

#[test]
fn validation_accepts_every_schema_choice() {
    for setting in crate::config::schema::runtime_settings() {
        if setting.choices.is_empty() || setting.key == "device" {
            continue;
        }
        for choice in &setting.choices {
            let settings = with_field(&setting.key, choice);
            settings
                .validate_choices()
                .unwrap_or_else(|e| panic!("{}={choice}: {e}", setting.key));
        }
    }
}

#[test]
fn legacy_parakeet_backend_still_loads_after_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"stt_backend":"parakeet"}"#).unwrap();
    let settings = crate::config::load_settings_from_path(&path).unwrap();
    assert_eq!(settings.stt_backend, "whisper");
    settings.validate().unwrap();
}
