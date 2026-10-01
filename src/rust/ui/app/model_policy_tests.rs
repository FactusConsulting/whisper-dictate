use super::*;

#[test]
fn local_whisper_skips_cloud_profile_language_guard() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let config_env = config.to_string_lossy().to_string();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_env);
    crate::config::save_settings(&AppSettings {
        stt_backend: "whisper".to_owned(),
        ..Default::default()
    })
    .unwrap();
    let mut app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_model: crate::dictate::backends::cloud_transcribe::NEMOTRON_ENGLISH_MODEL.to_owned(),
        lang: String::new(),
        ..Default::default()
    });

    assert!(app.validate_nemotron_profile_language_for_runtime("start"));
    assert!(app.settings_status.is_empty());
    assert!(app.last_runtime_error.is_none());
}

#[test]
fn lifecycle_guard_uses_persisted_runtime_snapshot_for_pending_language() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let config_env = config.to_string_lossy().to_string();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_env);
    let persisted = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: crate::ui::NEMOTRON_STT_BASE_URL.to_owned(),
        stt_model: crate::ui::NEMOTRON_ENGLISH_STT_MODEL.to_owned(),
        lang: "en".to_owned(),
        ..Default::default()
    };
    crate::config::save_settings(&persisted).unwrap();

    let mut app = test_app(AppSettings {
        lang: "da".to_owned(),
        ..persisted.clone()
    });
    app.saved_settings = persisted;

    assert!(app.validate_nemotron_profile_language_for_runtime("start"));
    assert!(app.settings_status.is_empty());
    assert_eq!(
        app.runtime_worker_command().runtime_value("VOICEPI_LANG"),
        Some("en")
    );
}
