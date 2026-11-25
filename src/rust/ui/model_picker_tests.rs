use super::render_test_support::{contains_text, rendered_texts_for_tab};
use super::test_support::{test_app, EnvVarGuard};
use super::{
    model_download_status, model_download_warning, whisper_model_choices, whisper_model_hint,
    AppSettings, RuntimeState, Tab,
};
use crate::ui::app::WHISPER_MODEL_PATH_ENV;
use crate::ui::whisper_models_state::ModelAvailability;

const CONFIG_ENV: &str = "VOICEPI_CONFIG";

const CACHE_ENV_VAR: &str = if cfg!(windows) {
    "LOCALAPPDATA"
} else if cfg!(target_os = "macos") {
    "HOME"
} else {
    "XDG_CACHE_HOME"
};

fn with_empty_model_cache(run: impl FnOnce()) {
    let _lock = crate::test_env_lock::lock_env();
    let cache = tempfile::tempdir().unwrap();
    let _cache = EnvVarGuard::set(CACHE_ENV_VAR, cache.path().to_str().unwrap());
    let config_path = cache.path().join("config.json");
    std::fs::write(
        &config_path,
        r#"{"stt_backend":"whisper","model":"large-v3"}"#,
    )
    .unwrap();
    let _config = EnvVarGuard::set(CONFIG_ENV, config_path.to_str().unwrap());
    let _model_path = EnvVarGuard::remove(WHISPER_MODEL_PATH_ENV);
    let _model = EnvVarGuard::remove("VOICEPI_MODEL");
    let _local_only = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    run();
}

#[test]
fn every_whisper_model_has_a_nonempty_hint() {
    // Adding a visible catalog model without metadata would silently show it
    // with no accuracy note and a 0 MB estimate (so it never greys out).
    for model in whisper_model_choices() {
        let (note, mb) = whisper_model_hint(model);
        assert!(!note.is_empty(), "missing accuracy note for {model}");
        assert!(mb > 0, "missing VRAM estimate for {model}");
    }
}

#[test]
fn unknown_model_has_empty_hint() {
    assert_eq!(whisper_model_hint("nonexistent"), ("", 0));
}

#[test]
fn model_download_status_distinguishes_verification_from_missing() {
    assert_eq!(
        model_download_status(ModelAvailability::Available),
        "downloaded"
    );
    assert_eq!(
        model_download_status(ModelAvailability::Checking),
        "checking download"
    );
    assert_eq!(
        model_download_status(ModelAvailability::Missing),
        "not downloaded"
    );
}

#[test]
fn unavailable_model_warns_before_recording() {
    assert_eq!(
        model_download_warning("large-v3", ModelAvailability::Missing, true, false).as_deref(),
        Some("large-v3 is not downloaded. Download it below before recording.")
    );
    assert_eq!(
        model_download_warning("large-v3", ModelAvailability::Checking, true, false).as_deref(),
        Some("large-v3 is still being verified. Wait for verification before recording.")
    );
    assert_eq!(
        model_download_warning("large-v3", ModelAvailability::Available, true, false),
        None
    );
}

#[test]
fn unavailable_retained_model_has_a_download_command() {
    assert_eq!(
        model_download_warning("small", ModelAvailability::Missing, false, false).as_deref(),
        Some("small is not downloaded. Choose a listed model or run `wd models download small`.")
    );
}

#[test]
fn local_only_warning_explains_manual_installation() {
    assert_eq!(
        model_download_warning("large-v3", ModelAvailability::Missing, true, true).as_deref(),
        Some(
            "large-v3 is not downloaded. Install it manually or disable local-only mode before recording."
        )
    );
    assert_eq!(
        model_download_warning("small", ModelAvailability::Missing, false, true).as_deref(),
        Some(
            "small is not downloaded. Choose a listed model, install it manually, or disable local-only mode before recording."
        )
    );
}

#[test]
fn local_only_status_uses_loaded_settings_and_fast_environment_override() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let app = test_app(AppSettings {
        local_only: true,
        ..Default::default()
    });
    assert!(app.local_only_enabled());

    let _env = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
    let app = test_app(AppSettings::default());
    assert!(app.local_only_enabled());
}

#[test]
fn start_is_blocked_when_the_selected_local_model_is_missing() {
    with_empty_model_cache(|| {
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);

        app.start_runtime();

        assert_eq!(app.runtime_state, RuntimeState::Stopped);
        assert!(app.settings_status.contains("large-v3 is not downloaded"));
        assert!(app
            .last_runtime_error
            .as_deref()
            .is_some_and(|message| message.contains("large-v3 is not downloaded")));
        assert!(app.runtime_log.contains("[ui] start blocked:"));
    });
}

#[test]
fn start_checks_the_saved_model_not_an_unsaved_picker_change() {
    with_empty_model_cache(|| {
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);
        app.settings.model = "large-v3-turbo".to_owned();

        app.start_runtime();

        assert!(app.settings_status.contains("large-v3 is not downloaded"));
    });
}

#[test]
fn start_checks_the_effective_worker_model_from_environment() {
    let _lock = crate::test_env_lock::lock_env();
    let cache = tempfile::tempdir().unwrap();
    let _cache = EnvVarGuard::set(CACHE_ENV_VAR, cache.path().to_str().unwrap());
    let config_path = cache.path().join("config.json");
    std::fs::write(&config_path, "{}").unwrap();
    let _config = EnvVarGuard::set(CONFIG_ENV, config_path.to_str().unwrap());
    let _model_path = EnvVarGuard::remove(WHISPER_MODEL_PATH_ENV);
    let _backend = EnvVarGuard::set("VOICEPI_STT_BACKEND", "whisper");
    let _model = EnvVarGuard::set("VOICEPI_MODEL", "small");
    let _local_only = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let mut app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        model: "large-v3".to_owned(),
        ..Default::default()
    });
    app.gpu_total_mb = Some(8192);

    app.start_runtime();

    assert!(app.settings_status.contains("small is not downloaded"));
    assert!(!app.settings_status.contains("large-v3 is not downloaded"));
}

#[test]
fn credential_restart_is_blocked_when_the_saved_local_model_is_missing() {
    with_empty_model_cache(|| {
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);
        app.supervisor.set_running_for_tests();

        app.restart_after_credential_change("post-processing", true);

        assert!(app.supervisor.is_running());
        assert!(app.runtime_log.contains("[ui] restart blocked:"));
        assert!(app
            .settings_status
            .contains("Restart pending: credential change is not active"));
        assert!(app.settings_status.contains("large-v3 is not downloaded"));
    });
}

#[test]
fn restart_verifies_an_uncached_model_before_replacing_a_running_session() {
    with_empty_model_cache(|| {
        let entry = crate::whisper::model_manager::find("large-v3").unwrap();
        let model_path = crate::whisper::model_manager::model_path(entry).unwrap();
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"corrupt model bytes").unwrap();

        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);
        app.supervisor.set_running_for_tests();

        app.restart_runtime();

        assert!(app.supervisor.is_running());
        assert!(app.runtime_log.contains("[ui] restart blocked:"));
        assert!(app.settings_status.contains("large-v3 is not downloaded"));
    });
}

#[test]
fn restart_keeps_the_running_session_when_the_saved_model_is_missing() {
    with_empty_model_cache(|| {
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ..Default::default()
        });
        app.supervisor.set_running_for_tests();

        app.restart_runtime();

        assert!(app.supervisor.is_running());
        assert!(app.runtime_log.contains("[ui] restart blocked:"));
    });
}

#[test]
fn local_whisper_start_on_a_cpu_only_host_requires_a_gpu_not_a_download() {
    with_empty_model_cache(|| {
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ..Default::default()
        });

        app.start_runtime();

        assert_eq!(app.runtime_state, RuntimeState::Stopped);
        assert!(app.settings_status.contains("compatible GPU"));
        assert!(!app.settings_status.contains("Download"));
        assert_eq!(app.settings.stt_backend, "whisper");
    });
}

#[test]
fn pending_local_only_disable_allows_model_recovery_downloads() {
    with_empty_model_cache(|| {
        let _ambient_local_only = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            local_only: false,
            ..Default::default()
        });
        app.supervisor.set_active_local_only_for_tests(true);

        let active = app.local_only_enabled();
        let pending = app.local_only_change_pending();
        let blocked = app.local_only_downloads_blocked();
        let warning = app.selected_whisper_model_warning();
        assert!(active);
        assert!(pending);
        assert!(!blocked);
        assert_eq!(
            warning.as_deref(),
            Some("large-v3 is not downloaded. Download it below before recording.")
        );
    });
}

#[test]
fn pending_local_only_enable_keeps_downloads_blocked_until_restart() {
    with_empty_model_cache(|| {
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            local_only: true,
            ..Default::default()
        });
        app.supervisor.set_running_for_tests();

        assert!(!app.local_only_enabled());
        assert!(app.local_only_change_pending());
        assert!(app.local_only_downloads_blocked());
        assert_eq!(
            app.selected_whisper_model_warning().as_deref(),
            Some(
                "large-v3 is not downloaded. Install it manually or disable local-only mode before recording."
            )
        );
    });
}

#[test]
fn existing_external_model_path_skips_cache_warning() {
    let _lock = crate::test_env_lock::lock_env();
    let model = tempfile::NamedTempFile::new().unwrap();
    let model_path = model.path().to_str().unwrap();
    let _model_path = EnvVarGuard::set(WHISPER_MODEL_PATH_ENV, model_path);
    let app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        model: "large-v3".to_owned(),
        ..Default::default()
    });

    assert!(app.has_external_whisper_model_path());
    assert_eq!(app.selected_whisper_model_warning(), None);
}

#[test]
fn invalid_external_model_path_blocks_recording() {
    let _lock = crate::test_env_lock::lock_env();
    let _model_path = EnvVarGuard::set(WHISPER_MODEL_PATH_ENV, "missing-model.ggml");
    let app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        ..Default::default()
    });

    assert_eq!(
        app.selected_whisper_model_warning().as_deref(),
        Some("VOICEPI_WHISPER_MODEL_PATH must point to an existing GGML model file.")
    );
}

#[test]
fn unknown_local_model_requires_a_new_selection() {
    let _lock = crate::test_env_lock::lock_env();
    let _model_path = EnvVarGuard::remove(WHISPER_MODEL_PATH_ENV);
    let _model = EnvVarGuard::remove("VOICEPI_MODEL");
    let app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        model: "not-a-whisper-model".to_owned(),
        ..Default::default()
    });

    assert_eq!(
        app.selected_whisper_model_warning().as_deref(),
        Some("not-a-whisper-model is not supported. Choose a listed model before recording.")
    );
}

#[test]
fn picker_offers_visible_stock_and_danish_models() {
    let choices = whisper_model_choices();
    assert_eq!(choices.len(), 4);
    for model in ["large-v3-turbo", "large-v3", "edda-v0.2", "edda-v0.2-large"] {
        assert!(choices.contains(&model), "{model} missing from picker");
    }
}

fn speech_test_app(settings: AppSettings) -> super::WhisperDictateApp {
    let mut app = test_app(settings);
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app
}

#[test]
fn speech_shows_danish_for_pending_edda_without_changing_the_language() {
    with_empty_model_cache(|| {
        let mut app = speech_test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            lang: "de".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);
        app.settings.model = "edda-v0.2".to_owned();

        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(texts.contains("Danish (fixed by model)"));
        assert!(contains_text(&texts, "Danish Turbo —"));
        assert!(!texts.contains("German"));
        assert_eq!(app.settings.lang, "de");
        assert_eq!(app.saved_settings.lang, "de");

        app.settings.model = "large-v3".to_owned();
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(!texts.contains("Danish (fixed by model)"));
        assert!(texts.contains("German"));
    });
}

#[test]
fn speech_keeps_language_picker_for_custom_path_and_hosted_with_saved_edda() {
    with_empty_model_cache(|| {
        let mut app = speech_test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "edda-v0.2-large".to_owned(),
            lang: "de".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);
        {
            let model = tempfile::NamedTempFile::new().unwrap();
            model.as_file().set_len(1).unwrap();
            let _path = EnvVarGuard::set(WHISPER_MODEL_PATH_ENV, model.path().to_str().unwrap());
            let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
            assert!(contains_text(&texts, "Custom GGML file"));
            assert!(!texts.contains("Danish (fixed by model)"));
            assert!(texts.contains("German"));
        }

        app.settings.stt_backend = "openai".to_owned();
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(!texts.contains("Danish (fixed by model)"));
        assert!(texts.contains("German"));
        assert_eq!(app.settings.lang, "de");
    });
}

#[test]
fn speech_shows_only_fitting_download_rows() {
    with_empty_model_cache(|| {
        let mut app = speech_test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3-turbo".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(2815);
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(!texts.contains("Whisper model downloads"));
        assert!(!texts.contains("Danish Turbo"));

        app.gpu_total_mb = Some(2816);
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(texts.contains("large-v3-turbo"));
        assert!(texts.contains("Danish Turbo"));
        assert!(!texts.contains("large-v3"));
        assert!(!texts.contains("Danish Quality"));

        app.gpu_total_mb = Some(4352);
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(texts.contains("large-v3"));
        assert!(texts.contains("Danish Quality"));
    });
}

#[test]
fn speech_uses_model_env_only_when_config_has_no_model() {
    with_empty_model_cache(|| {
        let _model = EnvVarGuard::set("VOICEPI_MODEL", "edda-v0.2-large");
        let mut app = speech_test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            lang: "de".to_owned(),
            ..Default::default()
        });
        app.gpu_total_mb = Some(8192);

        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(!texts.contains("Danish (fixed by model)"));

        std::fs::write(crate::config::config_path(), r#"{"stt_backend":"whisper"}"#).unwrap();
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(texts.contains("Danish (fixed by model)"));
        assert!(contains_text(&texts, "Danish Quality —"));

        app.settings.model = "large-v3-turbo".to_owned();
        let texts = rendered_texts_for_tab(&mut app, Tab::Speech);
        assert!(!texts.contains("Danish (fixed by model)"));
        assert!(texts.contains("German"));
    });
}

#[test]
fn retired_models_keep_their_hints_for_saved_settings() {
    // Removing a model from the OFFER list must not blank out the picker for
    // someone who already had it selected — nothing resets `settings.model`,
    // so the hint lookup has to keep resolving the retired names.
    for retired in ["medium", "small", "base", "tiny"] {
        let (note, mb) = whisper_model_hint(retired);
        assert!(!note.is_empty(), "retired model {retired} lost its label");
        assert!(mb > 0, "retired model {retired} lost its VRAM estimate");
    }
}
