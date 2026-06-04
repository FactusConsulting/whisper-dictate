use super::*;
use std::env;
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_app(settings: AppSettings) -> WhisperDictateApp {
    WhisperDictateApp {
        selected_tab: Tab::Runtime,
        runtime_state: RuntimeState::Stopped,
        runtime_log: String::new(),
        runtime_log_scroll_to_bottom: false,
        config_path: String::new(),
        saved_settings: settings.clone(),
        settings,
        settings_status: String::new(),
        stt_api_key_input: String::new(),
        saved_stt_api_key_input: String::new(),
        stt_api_key_reveal_until: None,
        stt_api_key_status: String::new(),
        post_api_key_input: String::new(),
        saved_post_api_key_input: String::new(),
        post_api_key_reveal_until: None,
        post_api_key_status: String::new(),
        dictionary_preview: String::new(),
        history_preview: String::new(),
        metrics_preview: String::new(),
        supervisor: RuntimeSupervisor::new(),
        background_task: None,
        background_task_label: None,
    }
}

#[test]
fn stt_backend_mode_maps_only_active_backend() {
    assert_eq!(SttBackendMode::from_raw("whisper"), SttBackendMode::Whisper);
    assert_eq!(
        SttBackendMode::from_raw("parakeet"),
        SttBackendMode::Parakeet
    );
    assert_eq!(SttBackendMode::from_raw("openai"), SttBackendMode::Cloud);
    assert_eq!(SttBackendMode::from_raw(""), SttBackendMode::Whisper);
}

#[test]
fn combo_labels_keep_cloud_backend_distinct_from_openai_provider() {
    assert_eq!(
        selected_option_label("openai", STT_BACKEND_OPTIONS),
        "Cloud STT (Groq/OpenAI)"
    );
    assert_eq!(
        selected_option_label("openai", CLOUD_PROVIDER_OPTIONS),
        "OpenAI"
    );
    assert_eq!(
        selected_option_label("groq", CLOUD_PROVIDER_OPTIONS),
        "Groq"
    );
    assert_eq!(
        selected_option_label("custom", CLOUD_PROVIDER_OPTIONS),
        "custom"
    );
}

#[test]
fn provider_api_key_can_load_from_environment_fallback() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-test-key");

    assert_eq!(
        load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
        Some("groq-test-key")
    );
    assert_eq!(load_stt_api_key_from_env(CloudProvider::OpenAi), None);

    unsafe {
        env::set_var("VOICEPI_STT_API_KEY", "shared-test-key");
    }

    assert_eq!(
        load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
        Some("shared-test-key")
    );
}

#[test]
fn post_api_key_can_load_from_environment_fallback() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let _post = EnvVarGuard::remove("VOICEPI_POST_API_KEY");
    let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-post-key");

    assert_eq!(
        load_post_api_key_from_env(PostProvider::Groq).as_deref(),
        Some("groq-post-key")
    );
    assert_eq!(load_post_api_key_from_env(PostProvider::OpenAi), None);

    unsafe {
        env::set_var("VOICEPI_POST_API_KEY", "post-override-key");
    }

    assert_eq!(
        load_post_api_key_from_env(PostProvider::Groq).as_deref(),
        Some("post-override-key")
    );
}

#[test]
fn file_api_key_store_round_trips_and_deletes_stt_keys() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);

    save_file_secret(CloudProvider::Groq.credential_user(), " groq-secret ").unwrap();

    assert_eq!(
        load_file_secret(CloudProvider::Groq.credential_user()).unwrap(),
        "groq-secret"
    );
    let raw = std::fs::read_to_string(&store).unwrap();
    assert!(raw.contains("stt-api-key:groq"));
    assert!(raw.contains("groq-secret"));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o600
    );

    save_file_secret(CloudProvider::Groq.credential_user(), "").unwrap();

    assert!(!store.exists());
}

#[test]
fn file_api_key_store_keeps_post_and_stt_keys_separate() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);

    save_file_secret(CloudProvider::OpenAi.credential_user(), "stt-secret").unwrap();
    save_file_secret(PostProvider::OpenAi.credential_user(), "post-secret").unwrap();

    assert_eq!(
        load_file_secret(CloudProvider::OpenAi.credential_user()).unwrap(),
        "stt-secret"
    );
    assert_eq!(
        load_file_secret(PostProvider::OpenAi.credential_user()).unwrap(),
        "post-secret"
    );
}

#[test]
fn disabled_os_keyring_uses_file_store_through_ui_secret_api() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);
    let _disable_guard = EnvVarGuard::set(DISABLE_OS_KEYRING_ENV, "1");

    let report = save_stt_api_key(CloudProvider::Groq, " groq-secret ").unwrap();
    assert_eq!(report.location, SecretSaveLocation::File);
    assert_eq!(report.fallback_path, store);
    assert!(report.fallback_reason.contains(DISABLE_OS_KEYRING_ENV));
    assert!(report.status_label().contains("fallback key file"));
    assert_eq!(
        load_stt_api_key_state(CloudProvider::Groq).unwrap().0,
        "groq-secret"
    );

    save_stt_api_key(CloudProvider::Groq, "").unwrap();

    assert!(!store.exists());
}

#[test]
fn successful_keyring_save_keeps_file_fallback() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);

    save_file_fallback_after_keyring_success(
        CloudProvider::Groq.credential_user(),
        " groq-secret ",
    )
    .unwrap();

    assert_eq!(
        load_file_secret(CloudProvider::Groq.credential_user()).unwrap(),
        "groq-secret"
    );
}

#[test]
fn credential_store_report_status_does_not_claim_fallback_file() {
    let report = SecretSaveReport {
        location: SecretSaveLocation::CredentialStore,
        credential_service: "whisper-dictate",
        credential_user: "stt-api-key:groq".to_owned(),
        credential_target: "stt-api-key:groq.whisper-dictate".to_owned(),
        fallback_path: std::path::PathBuf::from("api-keys.json"),
        fallback_reason: "Windows Credential Manager read-back verified".to_owned(),
    };

    let status = report.status_label();
    let log_details = report.log_details();

    assert!(status.contains("Credential") || status.contains("credential"));
    assert!(!status.contains("fallback file"));
    assert!(log_details.contains("stored=credential_store"));
    assert!(log_details.contains("credential_target=stt-api-key:groq.whisper-dictate"));
    assert!(!log_details.contains("fallback_file="));
}

#[test]
fn fallback_report_names_credential_failure_and_fallback_path() {
    let report = SecretSaveReport {
        location: SecretSaveLocation::File,
        credential_service: "whisper-dictate",
        credential_user: "stt-api-key:groq".to_owned(),
        credential_target: "stt-api-key:groq.whisper-dictate".to_owned(),
        fallback_path: std::path::PathBuf::from("api-keys.json"),
        fallback_reason: "OS credential store failed: unavailable".to_owned(),
    };

    let status = report.status_label();
    let log_details = report.log_details();

    assert!(status.contains("fallback key file"));
    assert!(status.contains("OS credential store failed"));
    assert!(log_details.contains("stored=fallback_file"));
    assert!(log_details.contains("fallback_file=api-keys.json"));
    assert!(log_details.contains("reason=OS credential store failed"));
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
fn saving_api_key_persists_selected_cloud_provider_settings() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let config_env = config.to_string_lossy().to_string();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_env);

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
    let saved = config::load_settings().unwrap();

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
fn worker_command_passes_wayland_keyboard_layout() {
    let settings = AppSettings {
        xkb_layout: "dk".to_owned(),
        ..Default::default()
    };
    let app = test_app(settings);

    let command = app.worker_command();
    assert_eq!(
        command
            .env
            .iter()
            .find(|(key, _)| key == XKB_LAYOUT_ENV)
            .map(|(_, value)| value.as_str()),
        Some("dk")
    );
}

#[test]
fn configured_keyboard_layout_beats_gnome_detection() {
    let settings = AppSettings {
        xkb_layout: " no ".to_owned(),
        ..Default::default()
    };

    assert_eq!(effective_xkb_layout(&settings).as_deref(), Some("no"));
}

#[test]
fn keyboard_layout_accepts_language_aliases_but_not_en() {
    assert_eq!(normalize_xkb_layout("da").as_deref(), Some("dk"));
    assert_eq!(normalize_xkb_layout("nb").as_deref(), Some("no"));
    assert_eq!(normalize_xkb_layout("uk").as_deref(), Some("ua"));
    assert_eq!(normalize_xkb_layout("en"), None);
}

#[test]
fn parses_gnome_danish_keyboard_layout() {
    assert_eq!(
        parse_gnome_xkb_sources("[('xkb', 'dk')]").as_deref(),
        Some("dk")
    );
    assert_eq!(
        parse_gnome_xkb_sources("[('ibus', 'mozc-jp'), ('xkb', 'dk')]").as_deref(),
        Some("dk")
    );
}

#[test]
fn gnome_keyboard_layout_parser_ignores_us_fallback() {
    assert_eq!(parse_gnome_xkb_sources("[('xkb', 'us')]"), None);
    assert_eq!(
        parse_gnome_xkb_sources("[('xkb', 'us'), ('xkb', 'dk')]").as_deref(),
        Some("dk")
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

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.original {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }
}
