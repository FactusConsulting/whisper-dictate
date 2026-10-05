#![cfg(windows)]

use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

#[cfg(feature = "rust-hotkeys")]
#[test]
fn windows_copy_last_shortcut_validation_blocks_bad_save_and_accepts_valid_chord() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let original = r#"{"key":"pause"}"#;
    std::fs::write(&path, original).unwrap();
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let mut app = test_app(config::load_settings().unwrap());

    for invalid in ["ctrl+shift+f13", "pause", "ctrl+f12"] {
        app.settings.copy_last_hotkey = invalid.to_owned();
        app.save_settings();
        assert!(app
            .settings_status
            .starts_with("Copy last shortcut is invalid:"));
        assert!(app.has_unsaved_settings());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    app.settings.copy_last_hotkey = "ctrl+shift+f8".to_owned();
    app.save_settings();
    assert!(app.settings_status.starts_with("Saved settings:"));
    assert_eq!(
        config::load_settings().unwrap().copy_last_hotkey,
        "ctrl+shift+f8"
    );
}

#[test]
fn windows_history_retention_settings_validate_persist_reset_and_report_required_restart() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("indstillinger rødgrød");
    std::fs::create_dir(&parent).unwrap();
    let path = parent.join("config med mellemrum.json");
    let original =
        r#"{"history_max_entries":"0","ui_settings_mode":"advanced","model":"large-v3"}"#;
    std::fs::write(&path, original).unwrap();
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _cache = EnvVarGuard::set("LOCALAPPDATA", &dir.path().to_string_lossy());
    let _model = EnvVarGuard::remove(super::app::WHISPER_MODEL_PATH_ENV);
    let mut app = test_app(config::load_settings().unwrap());
    app.selected_tab = Tab::Output;
    for invalid in ["1.5", "100001"] {
        app.settings.history_max_entries = invalid.to_owned();
        app.save_settings();
        assert!(app.settings_status.starts_with("Save failed:"));
        assert!(app.has_unsaved_settings());
        assert_eq!(app.saved_settings.history_max_entries, "0");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
    app.settings.history_max_entries = "25".to_owned();
    app.save_settings();
    assert!(app.settings_status.starts_with("Saved settings:"));
    assert!(!app.has_unsaved_settings());
    assert_eq!(config::load_settings().unwrap().history_max_entries, "25");
    app.settings.history_max_entries = "50".to_owned();
    assert!(app.has_unsaved_settings());
    app.reload_settings();
    assert_eq!(app.settings.history_max_entries, "25");
    assert!(!app.has_unsaved_settings());
    app.reset_current_tab_settings();
    assert_eq!(app.settings.history_max_entries, "0");
    assert!(app.has_unsaved_settings());
    app.supervisor.set_running_for_tests();
    app.save_settings();
    assert_eq!(config::load_settings().unwrap().history_max_entries, "0");
    assert!(!app.has_unsaved_settings());
    assert!(app.runtime_log.lines().any(|line| {
        line.contains("restart required after settings change:")
            && line.contains("history_max_entries")
    }));
    // The isolated empty model cache prevents any real runtime from starting.
    assert!(app.runtime_log.contains("[ui] restart blocked:"));
    assert!(!app.runtime_log.contains("[ui] restarting:"));
    assert!(app.supervisor.is_running());
}

#[test]
fn windows_settings_save_reload_keeps_auto_language_authoritative() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"lang":"en"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _lang_guard = EnvVarGuard::set("VOICEPI_LANG", "da");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.settings.lang.clear();

    app.save_settings();
    app.reload_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("lang"), Some(&serde_json::Value::Null));
    assert!(app.settings.lang.is_empty());
    assert!(!config::effective_runtime_env().contains_key("VOICEPI_LANG"));
}

#[test]
fn windows_settings_unrelated_save_preserves_explicit_metrics_clear() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"metrics_jsonl":null,"log_level":"info"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.config_path = path.display().to_string();
    app.reload_settings();
    assert!(app.settings.metrics_jsonl.is_empty());

    app.settings.log_level = "debug".to_owned();
    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("metrics_jsonl"), Some(&serde_json::Value::Null));
    assert_eq!(
        raw.get("log_level").and_then(|value| value.as_str()),
        Some("debug")
    );
}

#[test]
fn windows_unrelated_save_preserves_hosted_stt_model_clear() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"stt_backend":"openai","stt_provider":"groq","stt_base_url":"https://api.groq.com/openai/v1","stt_model":null,"log_level":"info"}"#,
    )
    .unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _model_guard = EnvVarGuard::remove("VOICEPI_STT_MODEL");

    let loaded = config::load_settings().unwrap();
    assert!(loaded.stt_model.is_empty());
    let mut app = test_app(loaded);
    app.settings.log_level = "debug".to_owned();

    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("stt_model"), Some(&serde_json::Value::Null));
    assert_eq!(
        raw.get("log_level").and_then(|value| value.as_str()),
        Some("debug")
    );
    assert!(app.settings.stt_model.is_empty());

    app.settings.profiles_json = "{".to_owned();
    app.save_settings();
    assert!(app.settings_status.starts_with("Profiles JSON is invalid:"));
    assert!(app.settings.stt_model.is_empty());
    assert_eq!(
        config::load_raw_config().unwrap().get("stt_model"),
        Some(&serde_json::Value::Null)
    );
}

#[test]
fn windows_settings_auto_selection_clears_absent_language_key() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"log_level":"info"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _lang_guard = EnvVarGuard::set("VOICEPI_LANG", "da");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.record_nullable_selection("lang", "");
    assert!(app.has_unsaved_settings());

    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("lang"), Some(&serde_json::Value::Null));
    assert!(!config::effective_runtime_env().contains_key("VOICEPI_LANG"));
}

#[test]
fn windows_failed_nullable_save_keeps_clear_intent_dirty_for_retry() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let original = r#"{"log_level":"info"}"#;
    std::fs::write(&path, original).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.record_nullable_selection("lang", "");
    app.settings.stt_timeout_ms = "not-a-number".to_owned();

    app.save_settings();

    assert!(app.settings_status.starts_with("Save failed:"));
    assert!(app.explicit_nullable_clears.contains("lang"));
    assert!(app.has_unsaved_settings());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn windows_nullable_text_edit_can_clear_an_absent_ambient_hook() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"log_level":"info"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _hook_guard = EnvVarGuard::set("VOICEPI_COMMAND_HOOK", "ambient-hook.exe");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.settings.command_hook = "temporary-hook.exe".to_owned();
    app.record_nullable_text_edit("command_hook", "", "temporary-hook.exe");
    app.settings.command_hook.clear();
    app.record_nullable_text_edit("command_hook", "temporary-hook.exe", "");

    app.save_settings();

    let raw = config::load_raw_config().unwrap();
    assert_eq!(raw.get("command_hook"), Some(&serde_json::Value::Null));
    assert!(!config::effective_runtime_env().contains_key("VOICEPI_COMMAND_HOOK"));
}

#[test]
fn windows_explicit_ambient_microphone_clear_restarts_running_runtime() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"log_level":"info","stt_backend":"openai","stt_provider":"custom","stt_model":"test-model","stt_base_url":"http://127.0.0.1:9000/v1"}"#,
    )
    .unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _device_guard = EnvVarGuard::set("VOICEPI_AUDIO_DEVICE", "Yeti Classic");

    let loaded = config::load_settings().unwrap();
    let mut app = test_app(loaded);
    app.record_nullable_selection("audio_device", "");
    app.supervisor.set_running_for_tests();

    app.save_settings();

    assert!(app
        .runtime_log
        .contains("restart required after settings change: audio_device"));
    assert!(app.runtime_log.contains("[ui] restarting:"));
}

#[test]
fn enabling_local_only_cancels_an_active_nemotron_model_probe() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path.to_string_lossy());
    let mut app = test_app(AppSettings::default());
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    app.nemotron_probe_active = Some(active.clone());
    app.settings.local_only = true;

    app.save_settings();

    assert!(!active.load(std::sync::atomic::Ordering::Acquire));
    assert!(app.nemotron_probe_active.is_some());
    assert!(app.runtime_log.contains("Cancelling 1 model download"));
}

#[test]
fn changing_stt_identity_cancels_but_retains_an_active_nemotron_probe_gate() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path.to_string_lossy());
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "inproc://nemotron".to_owned(),
        stt_model: NEMOTRON_MULTI_STT_MODEL.to_owned(),
        ..AppSettings::default()
    };
    let mut app = test_app(settings);
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    app.nemotron_probe_active = Some(active.clone());
    app.nemotron_probe_settings = Some(local_nemotron_probe_settings(&app.settings));
    app.settings.stt_model = NEMOTRON_ENGLISH_STT_MODEL.to_owned();
    app.settings.lang = "en".to_owned();

    app.save_settings();

    assert!(!active.load(std::sync::atomic::Ordering::Acquire));
    assert!(app.nemotron_probe_active.is_some());
    assert!(app.nemotron_probe_settings.is_some());
    assert!(app.runtime_log.contains("Cancelling stale local Nemotron"));
}

#[test]
fn changing_device_cancels_but_retains_an_active_nemotron_probe_gate() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path.to_string_lossy());
    let mut app = test_app(AppSettings::default());
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    app.nemotron_probe_active = Some(active.clone());
    app.nemotron_probe_settings = Some(local_nemotron_probe_settings(&app.settings));
    app.settings.device = "cpu".to_owned();

    app.save_settings();

    assert!(!active.load(std::sync::atomic::Ordering::Acquire));
    assert!(app.nemotron_probe_active.is_some());
    assert!(app.nemotron_probe_settings.is_some());
    assert!(app.runtime_log.contains("Cancelling stale local Nemotron"));
}

#[test]
fn disabling_local_only_cancels_but_retains_an_active_nemotron_probe_gate() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path.to_string_lossy());
    let settings = AppSettings {
        local_only: true,
        ..AppSettings::default()
    };
    let mut app = test_app(settings);
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    app.nemotron_probe_active = Some(active.clone());
    app.nemotron_probe_settings = Some(local_nemotron_probe_settings(&app.settings));
    app.settings.local_only = false;

    app.save_settings();

    assert!(!active.load(std::sync::atomic::Ordering::Acquire));
    assert!(app.nemotron_probe_active.is_some());
    assert!(app.nemotron_probe_settings.is_some());
    assert!(app.runtime_log.contains("Cancelling stale local Nemotron"));
}

#[test]
fn in_process_save_preserves_an_explicit_official_nemotron_gguf_path() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let model_path = directory
        .path()
        .join("nemotron-speech-streaming-en-0.6b.q8_0.gguf");
    std::fs::write(&model_path, b"local model fixture").unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path.to_string_lossy());
    let mut settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "inproc://nemotron".to_owned(),
        stt_model: model_path.display().to_string(),
        lang: "en".to_owned(),
        ..AppSettings::default()
    };
    let expected = settings.stt_model.clone();
    let mut app = test_app(settings.clone());

    app.save_settings();

    assert_eq!(app.settings.stt_model, expected);
    settings = config::load_settings().unwrap();
    assert_eq!(settings.stt_model, expected);
}

#[cfg(all(
    target_os = "windows",
    feature = "rust-hotkeys",
    feature = "rust-injection"
))]
#[test]
fn save_merges_the_worker_persisted_post_mode() {
    // Codex P2 worker_events.rs:105: a mode press persists post_mode on
    // the worker thread and the structured event lands on a later frame;
    // a Save in between must adopt the file's latest value instead of
    // writing the stale snapshot and undoing the press.
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"post_mode":"bullets"}"#).unwrap();
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let mut app = test_app(config::load_settings().unwrap());
    // The worker pressed cycle after the UI loaded: the file now holds
    // bullets while the app's snapshots still say raw.
    app.settings.post_mode = "raw".to_owned();
    app.saved_settings.post_mode = "raw".to_owned();
    app.save_settings();
    assert_eq!(config::load_settings().unwrap().post_mode, "bullets");
    assert_eq!(app.settings.post_mode, "bullets");
    assert_eq!(app.saved_settings.post_mode, "bullets");
    // A dirty user edit wins over the file value.
    let mut app = test_app(config::load_settings().unwrap());
    app.settings.post_mode = "terminal".to_owned();
    app.save_settings();
    assert_eq!(config::load_settings().unwrap().post_mode, "terminal");
    assert_eq!(app.settings.post_mode, "terminal");
}
