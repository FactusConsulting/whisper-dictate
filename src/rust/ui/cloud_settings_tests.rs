use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

fn runtime_value<'a>(command: &'a crate::runtime::WorkerCommand, name: &str) -> Option<&'a str> {
    command.runtime_value(name)
}

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
fn programmatic_cloud_provider_selection_clears_stale_model_null_intent() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "custom".to_owned(),
        stt_base_url: CUSTOM_STT_BASE_URL.to_owned(),
        stt_model: String::new(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.record_nullable_selection("stt_model", "");

    app.set_cloud_provider(CloudProvider::Groq);

    assert_eq!(app.settings.stt_model, GROQ_STT_MODEL);
    assert!(!app.explicit_nullable_clears.contains("stt_model"));
}

#[test]
fn nemotron_model_picker_offers_english_and_multilingual_profiles() {
    let options = CloudProvider::Nemotron.model_options();
    assert_eq!(
        options,
        &[NEMOTRON_ENGLISH_STT_MODEL, NEMOTRON_MULTI_STT_MODEL]
    );
    let labels = CloudProvider::Nemotron.labeled_model_options();
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0].0, NEMOTRON_ENGLISH_STT_MODEL);
    assert!(labels[0].1.to_ascii_lowercase().contains("english"));
    assert_eq!(labels[1].0, NEMOTRON_MULTI_STT_MODEL);
    assert!(labels[1].1.to_ascii_lowercase().contains("multilingual"));
}

#[test]
fn switching_to_nemotron_defaults_to_multilingual_profile() {
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        stt_model: OPENAI_STT_MODEL.to_owned(),
        ..Default::default()
    });

    app.set_cloud_provider(CloudProvider::Nemotron);

    assert_eq!(app.settings.stt_model, NEMOTRON_MULTI_STT_MODEL);
}

#[test]
fn selecting_english_nemotron_profile_makes_language_explicit() {
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: NEMOTRON_STT_BASE_URL.to_owned(),
        stt_model: NEMOTRON_ENGLISH_STT_MODEL.to_owned(),
        lang: String::new(),
        ..Default::default()
    });

    let message = app
        .normalize_nemotron_profile_language()
        .expect("English profile should normalize Auto language");

    assert_eq!(app.settings.lang, "en");
    assert!(message.contains("Language set to English"));
    assert!(!app.explicit_nullable_clears.contains("lang"));
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
fn saving_english_nemotron_profile_persists_normalized_language() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let config_env = config.to_string_lossy().to_string();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_env);
    let _stt_model_guard = EnvVarGuard::remove("VOICEPI_STT_MODEL");

    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: NEMOTRON_STT_BASE_URL.to_owned(),
        stt_model: NEMOTRON_ENGLISH_STT_MODEL.to_owned(),
        lang: "en".to_owned(),
        ..Default::default()
    });
    app.saved_settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        stt_base_url: OPENAI_STT_BASE_URL.to_owned(),
        stt_model: OPENAI_STT_MODEL.to_owned(),
        ..Default::default()
    };

    let path = app
        .persist_cloud_provider_selection()
        .expect("provider settings save")
        .expect("English profile language should be persisted");
    let saved = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap(),
    )
    .unwrap();

    assert_eq!(saved.stt_provider, "nemotron");
    assert_eq!(saved.stt_model, NEMOTRON_ENGLISH_STT_MODEL);
    assert_eq!(saved.lang, "en");
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
    assert_eq!(runtime_value(&command, POST_API_KEY_ENV), Some("stt-key"));

    app.post_api_key_input = "post-key".to_owned();
    let command = app.worker_command();
    assert_eq!(runtime_value(&command, POST_API_KEY_ENV), Some("post-key"));
}

#[test]
fn restart_command_does_not_write_scoped_credentials_to_process_env() {
    let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _post_key = EnvVarGuard::remove("VOICEPI_POST_API_KEY");
    let _marker = EnvVarGuard::remove("VOICEPI_POST_API_KEY_ENDPOINT");

    let settings = AppSettings {
        post_processor: "groq".to_owned(),
        post_base_url: GROQ_STT_BASE_URL.to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.post_api_key_input = "saved-post-key".to_owned();

    let replacement = app.runtime_worker_command();

    assert_eq!(
        runtime_value(&replacement, POST_API_KEY_ENV),
        Some("saved-post-key")
    );
    assert_eq!(
        runtime_value(&replacement, "VOICEPI_POST_API_KEY_ENDPOINT"),
        Some(GROQ_STT_BASE_URL)
    );
    assert!(std::env::var(POST_API_KEY_ENV).is_err());
    assert!(std::env::var("VOICEPI_POST_API_KEY_ENDPOINT").is_err());
}

#[test]
fn ui_worker_command_stamps_post_api_key_endpoint_marker_for_cloud_processor() {
    // A cloud post key must carry the endpoint it was resolved for.
    let settings = AppSettings {
        post_processor: "groq".to_owned(),
        post_base_url: "https://api.groq.com/openai/v1".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.post_api_key_input = "groq-key".to_owned();

    let command = app.worker_command();

    let marker = runtime_value(&command, "VOICEPI_POST_API_KEY_ENDPOINT");
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "UI worker_command must stamp the endpoint marker alongside the \
         post key -- without it, `postprocess::require_endpoint_matches_marker` \
         sees an empty marker and permits a cross-provider key leak on the primary \
         Windows launcher path. runtime snapshot = {:?}",
        command
    );
}

#[test]
fn ui_worker_command_binds_mirrored_stt_key_to_stt_endpoint_not_post_endpoint() {
    // A mirrored STT key must be bound to the STT endpoint, not the post
    // endpoint, so provider changes cannot leak the credential.
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
        post_processor: "openai".to_owned(), // OpenAI post + Groq STT
        post_base_url: "https://api.openai.com/v1".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.stt_api_key_input = "groq-stt-key".to_owned();
    // NO post_api_key_input -> UI mirrors STT into POST env.

    let command = app.worker_command();

    let marker = runtime_value(&command, "VOICEPI_POST_API_KEY_ENDPOINT");
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "SttMirror provenance MUST bind the marker to the STT endpoint. \
         Binding it to the OpenAI post endpoint would let the revalidation \
         check approve sending the Groq STT key to OpenAI -- exactly the \
         cross-provider credential leak. \
         runtime snapshot = {:?}",
        command
    );
}

#[test]
fn ui_worker_command_treats_post_field_equal_to_stt_field_as_stt_mirror() {
    // A post field copied from the STT key remains an STT mirror, even
    // when the copied value is non-empty.
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
        post_processor: "openai".to_owned(),
        post_base_url: "https://api.openai.com/v1".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    // Both fields hold the same value -- exactly what happens after
    // load_post_api_key_state falls back to the STT env var. The user
    // has NOT typed a post-specific key.
    app.stt_api_key_input = "groq-stt-key".to_owned();
    app.post_api_key_input = "groq-stt-key".to_owned();

    let command = app.worker_command();

    let marker = runtime_value(&command, "VOICEPI_POST_API_KEY_ENDPOINT");
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "post field == STT field must classify as SttMirror -- the key IS \
         the STT key regardless of how it got loaded into the post field. \
         Stamping the OpenAI post endpoint here (as the behavior \
         did) would approve sending the Groq STT key to OpenAI. \
         runtime snapshot = {:?}",
        command
    );
}

#[test]
fn ui_worker_command_preserves_ambient_env_key_ownership() {
    // A key already owned by the ambient environment must remain ambient;
    // the UI should not stamp it as a child-process credential.
    assert!(
        super::app::post_key_is_ambient_env_owned(
            "ambient-user-post-key",
            Some("ambient-user-post-key"),
        ),
        "matching values under an ambient VOICEPI_POST_API_KEY must be \
         classified as ambient-owned"
    );
    // Trims: whitespace surrounding the ambient value still matches.
    assert!(super::app::post_key_is_ambient_env_owned(
        "ambient-user-post-key",
        Some("  ambient-user-post-key  "),
    ));
    // No ambient: user has NOT declared ownership via env, so the UI
    // must push + stamp normally.
    assert!(!super::app::post_key_is_ambient_env_owned("some-key", None,));
    // Blank / whitespace-only ambient: `export VOICEPI_POST_API_KEY=`
    // is a leftover, not an ownership declaration.
    assert!(!super::app::post_key_is_ambient_env_owned(
        "some-key",
        Some(""),
    ));
    assert!(!super::app::post_key_is_ambient_env_owned(
        "some-key",
        Some("   "),
    ));
    // Different values: the pushed key is NOT the env-declared one
    // (e.g. it came from the credential store while ambient holds a
    // different explicit override), so the UI's push carries through
    // and the marker still applies.
    assert!(!super::app::post_key_is_ambient_env_owned(
        "store-post-key",
        Some("ambient-different-key"),
    ));
}

#[test]
fn ui_worker_command_binds_stale_stt_key_to_stt_endpoint_after_switch_to_local_whisper() {
    // A stale cloud STT key remains bound to its STT endpoint after the
    // user switches the active STT backend to local Whisper.
    let settings = AppSettings {
        stt_backend: "whisper".to_owned(), // switched to local
        stt_provider: "custom".to_owned(),
        // Retained from a previous cloud-STT config (user did not
        // clear it when switching to local).
        stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
        post_processor: "openai".to_owned(),
        post_base_url: "https://api.openai.com/v1".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.stt_api_key_input = "stale-groq-key".to_owned();

    let command = app.worker_command();

    let marker = runtime_value(&command, "VOICEPI_POST_API_KEY_ENDPOINT");
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "SttMirror provenance MUST bind to the STT endpoint even when \
         stt_backend has switched to local Whisper. A missing marker \
         (the behavior) would let the revalidation check approve \
         sending the stale Groq key to OpenAI. runtime snapshot = {:?}",
        command
    );
}

#[test]
fn ui_worker_command_stamps_stt_endpoint_marker_for_stt_only_injection() {
    // STT-only injection can later serve as a post fallback, so retain
    // the STT endpoint marker for subsequent provider changes.
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
        post_processor: "none".to_owned(), // local at spawn
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.stt_api_key_input = "groq-stt-key".to_owned();

    let command = app.worker_command();

    let marker = runtime_value(&command, "VOICEPI_POST_API_KEY_ENDPOINT");
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "STT-only injection must still stamp the marker so the \
         STT-as-post fallback is guarded after a live change to a cloud \
         post-processor. runtime snapshot = {:?}",
        command
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

#[cfg(windows)]
#[test]
fn windows_custom_provider_save_persists_and_restarts_native_session() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let saved_settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        stt_base_url: OPENAI_STT_BASE_URL.to_owned(),
        stt_model: OPENAI_STT_MODEL.to_owned(),
        ..Default::default()
    };
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "custom".to_owned(),
        stt_base_url: "http://localhost:9000/v1".to_owned(),
        stt_model: "my-transcription-model".to_owned(),
        ..Default::default()
    });
    app.saved_settings = saved_settings;
    app.supervisor.set_running_for_tests();

    app.save_settings();

    let saved = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(saved.stt_provider, "custom");
    assert_eq!(saved.stt_base_url, "http://localhost:9000/v1");
    assert_eq!(saved.stt_model, "my-transcription-model");
    assert!(
        app.runtime_log
            .contains("restart required after settings change"),
        "custom provider changes must restart the Windows managed runtime: {}",
        app.runtime_log
    );
    assert!(
        app.runtime_log.contains("[ui] restarting:"),
        "custom provider changes must enter the native restart path: {}",
        app.runtime_log
    );
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
fn successful_credential_change_restarts_a_running_native_session() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, r#"{"stt_backend":"openai"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());
    let _backend_guard = EnvVarGuard::remove("VOICEPI_STT_BACKEND");
    let mut app = test_app(AppSettings::default());
    app.supervisor.set_running_for_tests();

    app.restart_after_credential_change("cloud STT", true);

    assert!(
        app.runtime_log
            .contains("restart required after cloud STT credential change"),
        "credential replacement/clear must not leave the old backend snapshot running: {}",
        app.runtime_log
    );
    assert!(
        app.runtime_log.contains("[ui] restarting:"),
        "credential change must enter the real restart path: {}",
        app.runtime_log
    );
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

#[test]
fn local_nemotron_runtime_does_not_require_api_key() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "http://localhost:9000/v1".to_owned(),
        ..Default::default()
    };
    let app = test_app(settings);
    assert!(!app.cloud_stt_missing_api_key());
}
