use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

#[test]
fn cloud_provider_prefers_saved_provider_over_stale_url() {
    let settings = AppSettings {
        stt_provider: "groq".to_owned(),
        stt_base_url: OPENAI_STT_BASE_URL.to_owned(),
        ..Default::default()
    };

    assert_eq!(CloudProvider::from_settings(&settings), CloudProvider::Groq);

    let app = test_app(settings);
    assert_eq!(app.current_cloud_provider(), CloudProvider::Groq);
}

#[test]
fn saving_api_key_persists_selected_cloud_provider_settings() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let config_env = config.to_string_lossy().to_string();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_env);
    let _stt_model_guard = EnvVarGuard::remove("VOICEPI_STT_MODEL");

    let saved_settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        stt_base_url: OPENAI_STT_BASE_URL.to_owned(),
        stt_model: OPENAI_STT_MODEL.to_owned(),
        ..Default::default()
    };
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: GROQ_STT_BASE_URL.to_owned(),
        stt_model: GROQ_STT_MODEL.to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.saved_settings = saved_settings;

    let path = app.persist_cloud_provider_selection().unwrap().unwrap();
    let saved = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap(),
    )
    .unwrap();

    assert_eq!(path, config);
    assert_eq!(saved.stt_backend, "openai");
    assert_eq!(saved.stt_provider, "groq");
    assert_eq!(saved.stt_base_url, GROQ_STT_BASE_URL);
    assert_eq!(saved.stt_model, GROQ_STT_MODEL);
}

#[test]
fn environment_api_keys_do_not_make_settings_dirty_at_startup() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: GROQ_STT_BASE_URL.to_owned(),
        post_processor: "groq".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.stt_api_key_input = "env-stt-key".to_owned();
    app.saved_stt_api_key_input = "env-stt-key".to_owned();
    app.post_api_key_input = "env-post-key".to_owned();
    app.saved_post_api_key_input = "env-post-key".to_owned();

    assert!(!app.has_unsaved_settings());
}

#[test]
fn edited_api_key_still_makes_settings_dirty() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: GROQ_STT_BASE_URL.to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.stt_api_key_input = "edited-key".to_owned();
    app.saved_stt_api_key_input = "original-key".to_owned();

    assert!(app.has_unsaved_settings());
}

#[test]
fn worker_command_uses_post_key_with_stt_key_fallback() {
    let settings = AppSettings {
        post_processor: "groq".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.stt_api_key_input = "stt-key".to_owned();

    let command = app.worker_command();
    assert_eq!(
        command
            .env
            .iter()
            .find(|(key, _)| key == POST_API_KEY_ENV)
            .map(|(_, value)| value.as_str()),
        Some("stt-key")
    );

    app.post_api_key_input = "post-key".to_owned();
    let command = app.worker_command();
    assert_eq!(
        command
            .env
            .iter()
            .find(|(key, _)| key == POST_API_KEY_ENV)
            .map(|(_, value)| value.as_str()),
        Some("post-key")
    );
}

#[test]
fn custom_provider_keeps_user_endpoint_and_needs_no_api_key() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "custom".to_owned(),
        stt_base_url: "http://localhost:9000/v1".to_owned(),
        stt_model: "Systran/faster-whisper-large-v3".to_owned(),
        ..Default::default()
    });
    assert_eq!(app.current_cloud_provider(), CloudProvider::Custom);
    // A self-hosted endpoint needs no key, so start is not blocked.
    assert!(!app.cloud_stt_missing_api_key());

    // Saving must NOT normalize the user's base URL/model back to a hosted default.
    app.save_settings();
    assert_eq!(app.settings.stt_provider, "custom");
    assert_eq!(app.settings.stt_base_url, "http://localhost:9000/v1");
    assert_eq!(app.settings.stt_model, "Systran/faster-whisper-large-v3");
}

#[test]
fn switching_to_custom_seeds_localhost_from_a_hosted_url() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    // Provider just flipped to custom while the URL is still the hosted one.
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "custom".to_owned(),
        stt_base_url: OPENAI_STT_BASE_URL.to_owned(),
        ..Default::default()
    });
    // Save runs provider normalization, which seeds a localhost starting point.
    app.save_settings();
    assert_eq!(app.settings.stt_base_url, CUSTOM_STT_BASE_URL);
}

#[test]
fn effective_post_api_key_uses_post_key_then_stt_fallback() {
    let settings = AppSettings {
        post_processor: "groq".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);

    app.stt_api_key_input = "stt-key".to_owned();
    assert_eq!(app.effective_post_api_key(), "stt-key");

    app.post_api_key_input = "post-key".to_owned();
    assert_eq!(app.effective_post_api_key(), "post-key");
}

#[test]
fn cloud_stt_runtime_requires_api_key_before_worker_start() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: GROQ_STT_BASE_URL.to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);

    assert!(app.cloud_stt_missing_api_key());

    app.stt_api_key_input = "test-key".to_owned();

    assert!(!app.cloud_stt_missing_api_key());
}
