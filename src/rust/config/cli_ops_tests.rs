use super::{get_value, set_value};

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
