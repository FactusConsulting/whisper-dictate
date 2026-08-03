use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::{
    model_download_status, model_download_warning, whisper_model_hint, AppSettings, RuntimeState,
    WHISPER_MODELS,
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
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _local_only = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    run();
}

#[test]
fn every_whisper_model_has_a_nonempty_hint() {
    // Adding a model to WHISPER_MODELS without metadata would silently show it
    // with no accuracy note and a 0 MB estimate (so it never greys out).
    for model in WHISPER_MODELS {
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
        model_download_warning("large-v3", ModelAvailability::Checking, true, false),
        None
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
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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

        app.start_runtime();

        assert_eq!(app.runtime_state, RuntimeState::Stopped);
        assert!(app.settings_status.contains("large-v3 is not downloaded"));
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
        app.settings.model = "large-v3-turbo".to_owned();

        app.start_runtime();

        assert!(app.settings_status.contains("large-v3 is not downloaded"));
    });
}

#[test]
fn start_checks_the_effective_worker_model_from_environment() {
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
fn pending_local_only_disable_allows_model_recovery_downloads() {
    with_empty_model_cache(|| {
        let _ambient_local_only = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let active_command = crate::runtime::WorkerCommand {
            program: std::path::PathBuf::from("wd"),
            args: Vec::new(),
            working_dir: std::path::PathBuf::new(),
            env: vec![("VOICEPI_LOCAL_ONLY".to_owned(), "1".to_owned())],
        };
        crate::runtime::in_process::apply_worker_command_env(&active_command);
        let mut app = test_app(AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            local_only: false,
            ..Default::default()
        });
        app.supervisor.set_running_for_tests();

        let active = app.local_only_enabled();
        let pending = app.local_only_change_pending();
        let blocked = app.local_only_downloads_blocked();
        let warning = app.selected_whisper_model_warning();
        crate::runtime::in_process::restore_session_scoped_env();

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
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _model_path = EnvVarGuard::remove(WHISPER_MODEL_PATH_ENV);
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
fn picker_offers_only_the_two_real_choices() {
    // The offer list is deliberately minimal (see WHISPER_MODELS): a machine
    // that cannot run these should use a cloud STT backend rather than a tiny
    // local model. Keep it in sync with the GGML download catalog so Settings
    // never offers a model the downloader won't fetch.
    assert_eq!(WHISPER_MODELS, &["large-v3-turbo", "large-v3"]);
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
