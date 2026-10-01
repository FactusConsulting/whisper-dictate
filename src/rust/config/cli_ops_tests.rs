use super::{get_value, set_value};

#[test]
fn single_key_updates_preserve_sparse_config_and_environment_privacy_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let before = serde_json::json!({
        "lang": "da", "audio_device": null, "history_enabled": true,
        "release_tail_ms": 250, "foreign": {"nested": [1, null, false]}
    });
    for (key, value) in [
        ("lang", "en"),
        ("ui_theme", "light"),
        ("post_timeout_ms", "500"),
    ] {
        std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
        set_value(key, value, &path).unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut expected = before.clone();
        expected[key] = serde_json::json!(value);
        assert_eq!(stored, expected, "only {key} may change");
    }
}

#[test]
fn single_key_update_on_missing_file_does_not_materialize_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/config.json");
    set_value("history_enabled", "true", &path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, serde_json::json!({"history_enabled": "1"}));
}

#[test]
fn single_key_nullable_reset_preserves_other_explicit_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let before = serde_json::json!({"lang": "da", "initial_prompt": null, "local_only": true});
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    set_value("lang", "", &path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({"lang": null, "initial_prompt": null, "local_only": true})
    );
}

#[test]
fn empty_and_whitespace_numeric_updates_reset_to_schema_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    for key in [
        "release_tail_ms",
        "post_timeout_ms",
        "command_hook_timeout_ms",
        "min_snr_db",
    ] {
        let default = crate::config::runtime_settings()
            .iter()
            .find(|setting| setting.key == key)
            .unwrap()
            .default
            .as_ref()
            .unwrap();
        for reset in ["", " \t\n "] {
            let before = format!(r#"{{"{key}":"500","unknown":{{"kept":true}},"lang":null}}"#);
            std::fs::write(&path, before).unwrap();
            set_value(key, reset, &path).unwrap();
            assert_eq!(get_value(key, &path).unwrap(), *default, "{key}");
            let stored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(stored["unknown"]["kept"], true);
            assert!(stored["lang"].is_null());
        }
    }
}
