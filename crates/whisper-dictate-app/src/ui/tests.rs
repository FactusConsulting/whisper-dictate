use super::*;
use std::env;
use std::ffi::OsString;
use std::sync::Mutex;

static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_app(settings: AppSettings) -> WhisperDictateApp {
    WhisperDictateApp {
        selected_tab: Tab::Runtime,
        runtime_state: RuntimeState::Stopped,
        runtime_log: String::new(),
        config_path: String::new(),
        saved_settings: settings.clone(),
        settings,
        settings_status: String::new(),
        stt_api_key_input: String::new(),
        saved_stt_api_key_input: String::new(),
        stt_api_key_status: String::new(),
        post_api_key_input: String::new(),
        saved_post_api_key_input: String::new(),
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
