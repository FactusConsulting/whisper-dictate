#![cfg(windows)]

use super::super::test_support::{test_app, EnvVarGuard};
use super::*;

#[test]
fn windows_speech_reset_records_and_saves_ambient_microphone_clear() {
    let _lock = crate::test_env_lock::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"log_level":"info"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _device_guard = EnvVarGuard::set("VOICEPI_AUDIO_DEVICE", "Yeti Classic");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.selected_tab = Tab::Speech;
    app.reset_current_tab_settings();

    assert!(app.explicit_nullable_clears.contains("audio_device"));
    assert!(app.has_unsaved_settings());
    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("audio_device"), Some(&serde_json::Value::Null));
    assert!(!config::effective_runtime_env().contains_key("VOICEPI_AUDIO_DEVICE"));
}
