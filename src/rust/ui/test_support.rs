use super::*;
use std::env;
use std::ffi::OsString;
use std::sync::Mutex;

pub(super) static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn test_app(settings: AppSettings) -> WhisperDictateApp {
    WhisperDictateApp {
        app_version: "test".to_owned(),
        selected_tab: Tab::Log,
        runtime_state: RuntimeState::Stopped,
        runtime_log: String::new(),
        runtime_log_scroll_to_bottom: false,
        runtime_log_view: LogViewMode::from_raw(&settings.ui_log_view),
        audio_capture_opening: false,
        audio_capture_active: false,
        audio_meter_level: 0.0,
        audio_meter_raw_dbfs: None,
        audio_meter_peak: None,
        active_audio_device: String::new(),
        audio_device_options: Vec::new(),
        audio_devices_loaded: false,
        window_options: Vec::new(),
        device_error: None,
        device_test_result: None,
        corpus_items: Vec::new(),
        corpus_loaded: false,
        corpus_selected_id: None,
        corpus_recorded_ids: std::collections::HashSet::new(),
        corpus_record_result: None,
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
        scroll_to_history_preview: false,
        scroll_to_metrics_preview: false,
        supervisor: RuntimeSupervisor::new(),
        background_task: None,
        background_task_label: None,
        gpu_total_mb: None,
        gpu_probe: None,
        last_worker_status_state: String::new(),
        pipeline_stage: None,
        pipeline_preview: None,
        worker_ready: false,
        worker_start_time: None,
        fast_crash_count: 0,
        compact_mode: false,
        update_available: None,
        last_update_check: None,
        update_check_rx: None,
        update_command_copied_until: None,
        tray: TrayManager::new(),
    }
}

pub(super) struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    pub(super) fn remove(key: &'static str) -> Self {
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
