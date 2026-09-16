//! Tests for the launch-time auto-start one-shot (#894): the stage machine,
//! the skip policy for an unconfigured install, the "exactly once, never
//! retried" wiring into `start_runtime`, and the single-key persistence of
//! the `ui_autostart_runtime` preference.

use super::app::WHISPER_MODEL_PATH_ENV;
use super::tasks::RUN_BENCHMARK_LABEL;
use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

// --- the one-shot stage machine -------------------------------------------

#[test]
fn autostart_fires_on_the_pass_after_the_first_frame_and_never_again() {
    let mut stage = AutostartStage::default();
    let mut fired = Vec::new();
    for _ in 0..6 {
        let (next, due) = stage.advance();
        stage = next;
        fired.push(due);
    }
    // Pass 1 only records that a frame was painted; pass 2 fires; every
    // later pass is inert, so a failure can never spin.
    assert_eq!(fired, vec![false, true, false, false, false, false]);
    assert_eq!(stage, AutostartStage::Done);
}

#[test]
fn a_fresh_app_has_not_consumed_the_autostart_one_shot() {
    let app = test_app(AppSettings::default());
    assert_eq!(app.autostart_stage, AutostartStage::BeforeFirstFrame);
}

// --- the skip policy ------------------------------------------------------

#[test]
fn a_usable_configuration_has_no_skip_reason() {
    assert_eq!(autostart_skip_reason(None, None, None), None);
}

#[test]
fn a_missing_cloud_key_or_local_model_is_a_skip_reason() {
    assert_eq!(
        autostart_skip_reason(Some("no key"), None, None).as_deref(),
        Some("no key")
    );
    assert_eq!(
        autostart_skip_reason(None, Some("model missing"), None).as_deref(),
        Some("model missing")
    );
    // Both broken: report the cloud key, the one the user can act on from
    // the tab auto-start is reachable from.
    assert_eq!(
        autostart_skip_reason(Some("no key"), Some("model missing"), None).as_deref(),
        Some("no key")
    );
}

#[test]
fn a_nemotron_profile_language_mismatch_is_a_skip_reason() {
    assert_eq!(
        autostart_skip_reason(None, None, Some("English profile mismatch")).as_deref(),
        Some("English profile mismatch")
    );
    // The cloud key and local-model reasons still take precedence when
    // present — the Nemotron mismatch is the last fallback, not a special
    // case, matching `autostart_skip_reason`'s stable, documented order.
    assert_eq!(
        autostart_skip_reason(Some("no key"), None, Some("English profile mismatch")).as_deref(),
        Some("no key")
    );
}

// --- wiring into the real `start_runtime` ---------------------------------

/// The model-cache root, so `is_downloaded` can be pointed at an empty
/// temp dir and "the local model is missing" becomes deterministic instead
/// of depending on what the developer happens to have downloaded. Same
/// mechanism as `model_picker_tests::with_empty_model_cache`.
const CACHE_ENV_VAR: &str = if cfg!(windows) {
    "LOCALAPPDATA"
} else if cfg!(target_os = "macos") {
    "HOME"
} else {
    "XDG_CACHE_HOME"
};

/// Run `body` with a fully pinned runtime environment: an empty model cache,
/// a scratch config.json holding exactly `config_json`, no API-key store, no
/// OS keyring and no ambient `VOICEPI_*` overrides.
///
/// `runtime_whisper_model_warning` (the same helper the Start button uses)
/// resolves the EFFECTIVE worker settings from config.json + the ambient
/// environment, not from `app.settings`, so nothing less than this is
/// deterministic.
fn with_pinned_runtime_env(config_json: &str, body: impl FnOnce()) {
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let _cache = EnvVarGuard::set(CACHE_ENV_VAR, dir.path().to_str().unwrap());
    let config = dir.path().join("config.json");
    std::fs::write(&config, config_json).unwrap();
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", config.to_str().unwrap());
    let _store = EnvVarGuard::set(
        SECRET_STORE_ENV,
        dir.path().join("api-keys.json").to_str().unwrap(),
    );
    let _keyring = EnvVarGuard::set(DISABLE_OS_KEYRING_ENV, "1");
    let _model_path = EnvVarGuard::remove(WHISPER_MODEL_PATH_ENV);
    let _backend = EnvVarGuard::remove("VOICEPI_STT_BACKEND");
    let _model = EnvVarGuard::remove("VOICEPI_MODEL");
    let _ambient_key = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _local_only = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    body();
}

/// A config whose engine is usable: a cloud backend (so no local model is
/// needed) whose key the app already holds in memory.
const USABLE_CLOUD_CONFIG: &str = r#"{"stt_backend":"openai"}"#;

/// A runtime start whose ONLY blocker is a running benchmark: `start_runtime`
/// refuses it with a known message and bumps `runtime_error_revision`, all
/// without touching the supervisor. That makes "was a start requested?"
/// observable on every feature set, exactly the way the existing
/// `app_tests.rs` start-gate tests do it.
fn app_with_a_startable_but_blocked_runtime(autostart: bool) -> WhisperDictateApp {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        ui_autostart_runtime: autostart,
        ..AppSettings::default()
    };
    let mut app = test_app(settings);
    // A loaded cloud key, so the precheck sees a usable configuration and
    // `ensure_stt_api_key_loaded_for_runtime` returns without touching any
    // credential store.
    app.stt_api_key_input = "sk-test".to_owned();
    // `start_runtime`'s very first guard reads only the label, so no live
    // background-task channel is needed to trip it.
    app.background_task_label = Some(RUN_BENCHMARK_LABEL);
    app
}

const BENCHMARK_BLOCK: &str = "Cannot start the runtime while a benchmark is running.";

#[test]
fn autostart_on_requests_the_runtime_start_exactly_once() {
    with_pinned_runtime_env(USABLE_CLOUD_CONFIG, || {
        let mut app = app_with_a_startable_but_blocked_runtime(true);
        let baseline = app.runtime_error_revision;

        // The first pass must not start anything — the window is not up yet.
        app.poll_autostart_runtime();
        assert_eq!(app.runtime_error_revision, baseline);
        assert!(!app.runtime_log.contains("auto-start"));

        app.poll_autostart_runtime();
        assert_eq!(
            app.runtime_error_revision,
            baseline.wrapping_add(1),
            "the pass after the first frame must request a start, log: {}",
            app.runtime_log
        );
        assert_eq!(app.last_runtime_error.as_deref(), Some(BENCHMARK_BLOCK));

        // No retry, ever: further passes leave the failure exactly as it was,
        // and the Start button stays the way back in.
        for _ in 0..8 {
            app.poll_autostart_runtime();
        }
        assert_eq!(app.runtime_error_revision, baseline.wrapping_add(1));
        assert_eq!(app.runtime_log.matches("[ui] auto-start:").count(), 1);
        assert_eq!(app.runtime_log.matches("[ui] start blocked:").count(), 1);
        assert_eq!(app.runtime_state, RuntimeState::Stopped);
    });
}

#[test]
fn autostart_off_never_requests_a_runtime_start() {
    with_pinned_runtime_env(USABLE_CLOUD_CONFIG, || {
        let mut app = app_with_a_startable_but_blocked_runtime(false);
        let baseline = app.runtime_error_revision;

        for _ in 0..10 {
            app.poll_autostart_runtime();
        }

        assert_eq!(app.runtime_error_revision, baseline);
        assert!(!app.runtime_log.contains("auto-start"));
        assert_eq!(app.runtime_state, RuntimeState::Stopped);
    });
}

/// Turning the toggle ON mid-session must not retroactively fire the
/// launch-time one-shot: the preference applies at the NEXT launch.
#[test]
fn enabling_autostart_after_launch_does_not_fire_it_this_session() {
    with_pinned_runtime_env(USABLE_CLOUD_CONFIG, || {
        let mut app = app_with_a_startable_but_blocked_runtime(false);
        let baseline = app.runtime_error_revision;
        app.poll_autostart_runtime();
        app.poll_autostart_runtime();

        app.saved_settings.ui_autostart_runtime = true;
        for _ in 0..5 {
            app.poll_autostart_runtime();
        }

        assert_eq!(app.runtime_error_revision, baseline);
        assert!(!app.runtime_log.contains("auto-start"));
    });
}

/// An install whose local model is not downloaded must be SKIPPED with a
/// logged reason, not started — a first-run user must not be met with an
/// error they never asked for.
#[test]
fn an_unconfigured_local_install_is_skipped_with_a_logged_reason() {
    with_pinned_runtime_env(r#"{"stt_backend":"whisper","model":"large-v3"}"#, || {
        let settings = AppSettings {
            stt_backend: "whisper".to_owned(),
            model: "large-v3".to_owned(),
            ui_autostart_runtime: true,
            ..AppSettings::default()
        };
        let mut app = test_app(settings);
        // A start that would also be blocked, so a regressed skip policy
        // still shows up as a refusal rather than reaching the supervisor.
        app.background_task_label = Some(RUN_BENCHMARK_LABEL);
        let baseline = app.runtime_error_revision;

        app.poll_autostart_runtime();
        app.poll_autostart_runtime();

        assert!(
            app.runtime_log.contains("[ui] auto-start skipped"),
            "expected a logged skip reason, got: {}",
            app.runtime_log
        );
        assert!(app.runtime_log.contains("large-v3 is not downloaded"));
        // No start was requested, and no error banner was raised at the user.
        assert_eq!(app.runtime_error_revision, baseline);
        assert_eq!(app.last_runtime_error, None);
        assert!(app.settings_status.is_empty());
        assert_eq!(app.runtime_state, RuntimeState::Stopped);
    });
}

/// The cloud half of the same rule: a cloud provider with no API key
/// anywhere (no keyring, no file store, no ambient env) is a skip.
#[test]
fn a_cloud_install_with_no_api_key_is_skipped_with_a_logged_reason() {
    with_pinned_runtime_env(USABLE_CLOUD_CONFIG, || {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            ui_autostart_runtime: true,
            ..AppSettings::default()
        };
        let mut app = test_app(settings);
        app.background_task_label = Some(RUN_BENCHMARK_LABEL);
        let baseline = app.runtime_error_revision;

        app.poll_autostart_runtime();
        app.poll_autostart_runtime();

        assert!(
            app.runtime_log.contains("[ui] auto-start skipped"),
            "expected a logged skip reason, got: {}",
            app.runtime_log
        );
        assert!(app.runtime_log.contains("API key"));
        assert_eq!(app.runtime_error_revision, baseline);
        assert_eq!(app.last_runtime_error, None);
        assert_eq!(app.runtime_state, RuntimeState::Stopped);
    });
}

/// A saved Nemotron English profile paired with a non-English language is
/// exactly as unusable as a missing key or model: without this precheck,
/// `start_runtime`'s own `validate_nemotron_profile_language_for_runtime`
/// gate would raise a full red error banner on EVERY launch for an install
/// saved in this state — precisely the unrequested-error experience #894's
/// "must not auto-start an unconfigured install" forbids.
#[test]
fn a_mismatched_nemotron_profile_language_is_skipped_with_a_logged_reason() {
    let config = format!(
        r#"{{"stt_backend":"openai","stt_provider":"nemotron","stt_base_url":"inproc://nemotron","stt_model":"{}","lang":"da"}}"#,
        crate::dictate::backends::cloud_transcribe::NEMOTRON_ENGLISH_MODEL
    );
    with_pinned_runtime_env(&config, || {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            // In-process, so `cloud_stt_missing_api_key` (which reads the
            // live form, not the pinned config) does not fire first and
            // mask the mismatch this test targets.
            stt_base_url: "inproc://nemotron".to_owned(),
            stt_model: crate::dictate::backends::cloud_transcribe::NEMOTRON_ENGLISH_MODEL
                .to_owned(),
            lang: "da".to_owned(),
            ui_autostart_runtime: true,
            ..AppSettings::default()
        };
        let mut app = test_app(settings);
        app.background_task_label = Some(RUN_BENCHMARK_LABEL);
        let baseline = app.runtime_error_revision;

        app.poll_autostart_runtime();
        app.poll_autostart_runtime();

        assert!(
            app.runtime_log.contains("[ui] auto-start skipped"),
            "expected a logged skip reason, got: {}",
            app.runtime_log
        );
        assert!(
            app.runtime_log.contains("Nemotron English profile"),
            "expected the profile/language mismatch message, got: {}",
            app.runtime_log
        );
        // No start was requested, and — the whole point of this test — no
        // error banner was raised at the user.
        assert_eq!(app.runtime_error_revision, baseline);
        assert_eq!(app.last_runtime_error, None);
        assert!(app.settings_status.is_empty());
        assert_eq!(app.runtime_state, RuntimeState::Stopped);
    });
}

/// Defensive guard: if the runtime is somehow already running or restarting
/// by the time the one-shot fires (unreachable today — nothing can Start
/// before the window is even up — but cheap to guarantee), auto-start must
/// not call `start_runtime` at all, so a healthy running worker never gets
/// hit with a spurious error banner.
#[test]
fn autostart_never_calls_start_runtime_while_already_running_or_restarting() {
    with_pinned_runtime_env(USABLE_CLOUD_CONFIG, || {
        let mut app = app_with_a_startable_but_blocked_runtime(true);
        // Simulate "already running" without a real worker process: the
        // guard reads `supervisor.is_running_or_restarting()`, not
        // `app.runtime_state`, so the supervisor itself must report it.
        app.supervisor.set_running_for_tests();
        let baseline = app.runtime_error_revision;

        app.poll_autostart_runtime();
        app.poll_autostart_runtime();

        assert_eq!(app.runtime_error_revision, baseline);
        assert!(
            !app.runtime_log.contains("auto-start"),
            "must not even attempt a start while already running, log: {}",
            app.runtime_log
        );
    });
}

// --- persistence ----------------------------------------------------------

#[test]
fn toggling_autostart_persists_the_choice_for_the_next_launch() {
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());
    let mut app = test_app(AppSettings::default());

    app.set_autostart_runtime(true);

    assert!(app.settings.ui_autostart_runtime);
    // Applied AND recorded as saved, so the form never looks dirty.
    assert!(app.saved_settings.ui_autostart_runtime);
    let reloaded = config::load_settings().unwrap();
    assert!(
        reloaded.ui_autostart_runtime,
        "the toggle must survive a restart"
    );

    app.set_autostart_runtime(false);
    assert!(!config::load_settings().unwrap().ui_autostart_runtime);
}

/// #890's class of bug: a casual UI click must NOT route through
/// `config::set_value`, which materializes every other known setting's
/// schema default into a sparse config.json. Config beats environment at
/// load time, so a written `"local_only":"0"` would silently and
/// permanently clobber a `VOICEPI_LOCAL_ONLY=1` override. The file must end
/// up with exactly its original keys plus `ui_autostart_runtime`.
#[test]
fn toggling_autostart_does_not_materialize_defaults_into_a_sparse_config() {
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, r#"{"lang":"da"}"#).unwrap();
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());
    let _local_only = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
    let mut app = test_app(AppSettings::default());

    app.set_autostart_runtime(true);

    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    let mut keys: Vec<&str> = raw
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["lang", "ui_autostart_runtime"],
        "the toggle must write ONLY its own key -- got {keys:?}"
    );
    // And the value is the "1"/"0" encoding a full save would have used.
    assert_eq!(raw["ui_autostart_runtime"], serde_json::json!("1"));
}

/// Mirrors `settings_mode_persistence_tests::set_settings_mode_leaves_the_saved_snapshot_stale_when_the_write_fails`:
/// a failed persist must not silently mark the toggle as saved.
#[test]
fn set_autostart_runtime_leaves_the_saved_snapshot_stale_when_the_write_fails() {
    let _lock = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // A directory where the config file is expected forces the read/write to
    // fail with an IO error instead of succeeding.
    let config_as_dir = dir.path().join("config.json");
    std::fs::create_dir(&config_as_dir).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_as_dir.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    assert!(!app.settings.ui_autostart_runtime);
    assert!(!app.saved_settings.ui_autostart_runtime);

    app.set_autostart_runtime(true);

    // The choice still applies instantly in the UI...
    assert!(app.settings.ui_autostart_runtime);
    // ...but the on-disk snapshot was never advanced, so a subsequent
    // launch-time read of `saved_settings` (which is exactly what
    // `poll_autostart_runtime` consults) correctly still sees it off,
    // rather than the write silently reporting success.
    assert!(!app.saved_settings.ui_autostart_runtime);
    assert!(
        app.settings_status.contains("Could not save"),
        "expected a save-failure hint, got: {:?}",
        app.settings_status
    );
    assert!(
        app.runtime_log
            .contains("[ui] could not persist the auto-start preference"),
        "expected a logged persistence failure, got: {}",
        app.runtime_log
    );
}

#[test]
fn an_absent_key_loads_as_off_so_an_existing_install_is_unchanged() {
    let value = serde_json::json!({"lang": "da"});
    let settings = AppSettings::from_value(value).unwrap();
    assert!(!settings.ui_autostart_runtime);
    assert!(!AppSettings::default().ui_autostart_runtime);
}

#[test]
fn the_toggle_is_visible_in_simple_mode() {
    assert!(setting_visible(
        SettingsMode::Simple,
        "ui_autostart_runtime"
    ));
    assert!(setting_visible(
        SettingsMode::Advanced,
        "ui_autostart_runtime"
    ));
}
