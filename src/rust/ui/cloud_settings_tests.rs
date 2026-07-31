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
fn ui_worker_command_stamps_post_api_key_endpoint_marker_for_cloud_processor() {
    // Codex P1 #666 #1 (`PRRT_kwDOSfNjQs6UXpn-`) regression pin.
    // Un-fixed shape: `App::worker_command` pushed VOICEPI_POST_API_KEY
    // directly without stamping the marker, so the P1 #642 revalidation
    // check saw an empty marker and permitted the leak on the primary
    // Windows tray path. The FIXED shape stamps the marker via the shared
    // `runtime::cloud_api_keys::stamp_post_api_key_endpoint_marker` shim.
    let settings = AppSettings {
        post_processor: "groq".to_owned(),
        post_base_url: "https://api.groq.com/openai/v1".to_owned(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.post_api_key_input = "groq-key".to_owned();

    let command = app.worker_command();

    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "UI worker_command must stamp the endpoint marker alongside the \
         post key -- without it, `postprocess::require_endpoint_matches_marker` \
         sees an empty marker and permits the P1 #642 leak on the primary \
         Windows launcher path. command.env = {:?}",
        command.env
    );
}

#[test]
fn ui_worker_command_binds_mirrored_stt_key_to_stt_endpoint_not_post_endpoint() {
    // Codex P1 round-2 #1 (`PRRT_kwDOSfNjQs6UXpn-` cmt 3665199618)
    // UI-side regression pin. Scenario: Groq STT configured, OpenAI
    // post-processing selected, NO post-specific key. `App::worker_command`
    // mirrors the Groq STT key into VOICEPI_POST_API_KEY. Un-fixed shape
    // stamped the OpenAI post endpoint as the marker -- so a subsequent
    // live change was approved as "same provider = OpenAI" and the Groq
    // key was sent to OpenAI (cross-provider leak). Fixed shape stamps
    // the GROQ STT endpoint because that is where the mirrored key is
    // actually valid.
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

    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "SttMirror provenance MUST bind the marker to the STT endpoint. \
         Binding it to the OpenAI post endpoint would let the revalidation \
         check approve sending the Groq STT key to OpenAI -- exactly the \
         cross-provider leak the P1 round-2 finding calls out. \
         command.env = {:?}",
        command.env
    );
}

#[test]
fn ui_worker_command_treats_post_field_equal_to_stt_field_as_stt_mirror() {
    // Codex P1 round-3 (`PRRT_kwDOSfNjQs6UZdNL` cmt 3665509647)
    // regression pin. `load_post_api_key_state` in `ui/api_keys.rs`
    // populates `post_api_key_input` from the `VOICEPI_STT_API_KEY`
    // fallback when no post-specific credential is saved. In that
    // shape, the post field is NON-EMPTY but its value is a copy of
    // the STT key. Un-fixed shape: `App::worker_command` classified
    // any non-empty post field as `PostSpecific` and stamped the
    // post endpoint -- so a Groq STT / OpenAI post setup with the
    // fallback-loaded field got an OpenAI marker for a Groq key,
    // approving a cross-provider send. Fixed shape: when the post
    // field's VALUE equals the STT field's value, provenance falls
    // through to `SttMirror` regardless of how the field got loaded.
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

    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "post field == STT field must classify as SttMirror -- the key IS \
         the STT key regardless of how it got loaded into the post field. \
         Stamping the OpenAI post endpoint here (as the un-fixed shape \
         did) would approve sending the Groq STT key to OpenAI. \
         command.env = {:?}",
        command.env
    );
}

#[test]
fn ui_worker_command_preserves_ambient_env_key_ownership() {
    // Codex P2 round-4 (`PRRT_kwDOSfNjQs6UZxN2` cmt 3665701506)
    // regression pin. Scenario: Windows tray has no stored post
    // credential, so `load_post_api_key_state` (ui/api_keys.rs:204-209)
    // copies an explicit `VOICEPI_POST_API_KEY` from the parent env
    // into `post_api_key_input`. The user configured that env var
    // themselves and expects the "explicit env keys own their
    // resolution" compatibility contract to apply.
    //
    // Un-fixed shape: the UI classified any non-empty post field as
    // `PostSpecific`, pushed the value into `command.env`, and the
    // shim stamped the current endpoint. A live provider/profile
    // change was then REJECTED for the user's own env key, and
    // restarting produced the same rejection because the same env
    // value loaded and stamped again -- unbreakable state loop.
    //
    // Fixed shape: when the pushed value equals the ambient
    // `VOICEPI_POST_API_KEY`, `App::worker_command` skips the push
    // (child inherits from parent env) AND keeps provenance = None,
    // so the shim's ambient-ownership rule fires and no marker is
    // stamped.
    // Kept as a pure-helper pin instead of an env-touching integration
    // test: `App::worker_command` reads `std::env::var` for the
    // ambient-ownership check, and other UI tests (e.g.
    // `ui_worker_command_stamps_stt_endpoint_marker_for_stt_only_injection`)
    // do NOT acquire ENV_TEST_LOCK -- so touching process env here
    // would flake those tests when cargo runs the suite in parallel.
    // The decision itself lives in
    // `super::app::post_key_is_ambient_env_owned`, exercised
    // exhaustively below.
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
    // Codex P1 round-4 (`PRRT_kwDOSfNjQs6UZxA5` cmt 3665625004)
    // regression pin. Scenario: user switches STT backend to local
    // Whisper but `stt_api_key_input` retains the previously-loaded
    // Groq key. Cloud post-processing is still selected (OpenAI) with
    // no post-specific key.
    //
    // Un-fixed shape: `App::worker_command` mirrors the stale Groq key
    // into `VOICEPI_POST_API_KEY` (SttMirror provenance) but does NOT
    // push `VOICEPI_STT_API_KEY` because `stt_backend != "openai"`.
    // The shim's endpoint selection was gated on `has_stt &&
    // stt_backend == "openai"` for the mirror branch, so both
    // conditions failed and no marker was stamped -- leaving the
    // stale Groq key unguarded across the OpenAI post send.
    //
    // Fixed shape: SttMirror provenance binds to the STT endpoint
    // REGARDLESS of the current STT backend / whether we pushed the
    // STT env var, because the KEY itself came from the STT input
    // field and was resolved for that endpoint.
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

    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "SttMirror provenance MUST bind to the STT endpoint even when \
         stt_backend has switched to local Whisper. A missing marker \
         (the un-fixed shape) would let the revalidation check approve \
         sending the stale Groq key to OpenAI. command.env = {:?}",
        command.env
    );
}

#[test]
fn ui_worker_command_stamps_stt_endpoint_marker_for_stt_only_injection() {
    // Codex P1 #666 #2 (`PRRT_kwDOSfNjQs6UXpnu`) UI-side regression pin.
    // When only an STT key is pushed (post_processor local at spawn), the
    // STT key can still serve as a post-key fallback via the settings
    // loader. The marker must record the STT endpoint so a later live
    // change to cloud post-processing hits the revalidation check.
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

    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        marker,
        Some("https://api.groq.com/openai/v1"),
        "STT-only injection must still stamp the marker so the \
         STT-as-post fallback is guarded after a live change to a cloud \
         post-processor. command.env = {:?}",
        command.env
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
fn successful_credential_change_restarts_a_running_native_session() {
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
