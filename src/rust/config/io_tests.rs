use crate::config::{load_settings_from_path, save_settings_to_path, set_raw_string_key};

#[test]
fn atomic_config_save_and_focused_mutation_preserve_unknown_keys_and_explicit_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"lang":null,"initial_prompt":null,"custom":{"kept":true}}"#,
    )
    .unwrap();
    let mut settings = load_settings_from_path(&path).unwrap();
    settings.ui_theme = "light".to_owned();
    save_settings_to_path(&settings, &path).unwrap();
    set_raw_string_key("ui_settings_mode", "simple", &path).unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(raw["lang"], serde_json::Value::Null);
    assert_eq!(raw["initial_prompt"], serde_json::Value::Null);
    assert_eq!(raw["custom"]["kept"], true);
    assert_eq!(raw["ui_theme"], "light");
    assert_eq!(raw["ui_settings_mode"], "simple");
}

#[cfg(windows)]
#[test]
fn locked_config_replacement_is_rejected_without_altering_last_good_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let previous = r#"{"lang":null,"unknown":true}"#;
    std::fs::write(&path, previous).unwrap();
    let settings = load_settings_from_path(&path).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(save_settings_to_path(&settings, &path).is_err());
    assert!(set_raw_string_key("ui_settings_mode", "simple", &path).is_err());
    drop(lock);
    assert_eq!(std::fs::read_to_string(path).unwrap(), previous);
}
