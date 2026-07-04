//! Config-key catalogs and restart-impact comparison.
//!
//! [`SETTINGS_KEYS`] lists every config.json key the typed [`AppSettings`]
//! owns (used to wipe stale entries before re-serializing). [`RESTART_KEYS`] is
//! the subset whose change requires a worker restart; it must stay consistent
//! with the schema's `live` flag (guarded by a test in the parent module).
//!
//! [`DEPRECATED_KEYS`] is the subset of legacy keys we ACTIVELY strip on save
//! so they fade out of users' config.json after one save round-trip. The
//! Parakeet/NeMo backend removal in Wave 8 of #348 added the parakeet_*
//! entries here; migration code in [`crate::config::load`] also logs a one-
//! line warning and switches `stt_backend = "parakeet"` to the default.

use crate::config::AppSettings;

/// Every config.json key managed by [`AppSettings`]. Used by the serializer to
/// remove stale keys before writing the current typed values back.
pub(crate) const SETTINGS_KEYS: &[&str] = &[
    "key",
    "model",
    "stt_backend",
    "stt_provider",
    "stt_model",
    "stt_base_url",
    "stt_timeout_ms",
    "device",
    "compute_type",
    "audio_device",
    "lang",
    "xkb_layout",
    "initial_prompt",
    "inject_mode",
    "format_commands",
    "beam_size",
    "temperature",
    "context_min_seconds",
    "hallucination_guard",
    "max_chars_per_second",
    "min_record_seconds",
    "release_tail_ms",
    "preview_seconds",
    "max_record_s",
    "vad_threshold",
    "vad_min_silence_ms",
    "vad_speech_pad_ms",
    "target_dbfs",
    "min_input_dbfs",
    "min_snr_db",
    "audio_ducking",
    "audio_ducking_level",
    "dictionary",
    "dictionary_enabled",
    "dictionary_max_terms",
    "dictionary_prompt_chars",
    "json_output",
    "metrics_jsonl",
    "command_hook",
    "command_hook_timeout_ms",
    "history_enabled",
    "history_jsonl",
    "local_only",
    "post_processor",
    "post_mode",
    "post_model",
    "post_base_url",
    "post_timeout_ms",
    "post_max_input_chars",
    "post_max_output_chars",
    "post_redact",
    "post_redact_terms",
    "postprocess_hotkey",
    "postprocess_profiles",
    "postprocess_profile_index",
    "feedback_sounds",
    "feedback_notify",
    "debug",
    "stt_debug",
    "trace",
    "toggle_mode",
    "quit_key",
    "quit_count",
    "quit_window_ms",
    "update_check",
    "update_check_interval_minutes",
    "update_include_prereleases",
    "ui_language",
    "ui_log_view",
    "ui_theme",
    "ui_text_scale",
    "overlay_enabled",
    "overlay_position",
    "overlay_show_on_idle",
    // Issue #328: onboarding wizard state.
    "onboarding_completed",
    "onboarding_seen_at",
];

/// Keys whose change forces a worker restart (everything else is live-reloaded).
pub(crate) const RESTART_KEYS: &[&str] = &[
    "key",
    "model",
    "stt_backend",
    "stt_provider",
    "stt_model",
    "stt_base_url",
    "stt_timeout_ms",
    "device",
    "compute_type",
    "local_only",
    "toggle_mode",
    "quit_key",
    "quit_count",
    "quit_window_ms",
    "postprocess_hotkey",
];

/// Legacy config.json keys we now strip on save so they fade out of users'
/// config.json after one save round-trip. The Parakeet/NeMo backend removal
/// (Wave 8 of #348) added the parakeet_* entries here. Independent of
/// [`SETTINGS_KEYS`] so the typed [`AppSettings`] does NOT have to keep
/// (now-unused) fields for them.
pub(crate) const DEPRECATED_KEYS: &[&str] = &[
    "parakeet_model",
    "parakeet_min_seconds",
    "parakeet_force_pc",
];

/// Report which [`RESTART_KEYS`] differ between two settings snapshots, so the
/// UI can warn that a restart is required.
///
/// Iteration-3 review finding #3: when the Rust audio backend is in
/// play, `audio_device` is treated as restart-required because the
/// supervisor opens the CPAL stream at worker start and does not
/// listen for live device changes (the Python sounddevice path does).
/// Without this, saving a different mic mid-session keeps the Rust
/// pipeline bound to the old device until the next worker restart,
/// silently overriding the user's selection. We do NOT add the key to
/// the static [`RESTART_KEYS`] table because the Python paths
/// (sounddevice/arecord) DO honour live changes and forcing a restart
/// there would be a UX regression.
pub fn restart_required_keys(before: &AppSettings, after: &AppSettings) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = RESTART_KEYS
        .iter()
        .copied()
        .filter(|key| before.setting_value(key) != after.setting_value(key))
        .collect();
    if crate::runtime::audio_pipeline_requested()
        && crate::runtime::audio_pipeline_available()
        && before.setting_value("audio_device") != after.setting_value("audio_device")
        && !keys.contains(&"audio_device")
    {
        keys.push("audio_device");
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Iteration-3 review finding #3: changing the microphone while
    /// the Rust audio backend is active must require a restart, since
    /// the supervisor opens the CPAL stream at worker start and does
    /// not currently honour live device changes. Under the Python
    /// sounddevice path the setting stays live.
    #[test]
    fn restart_required_keys_marks_audio_device_under_rust_backend() {
        use crate::runtime::AUDIO_BACKEND_ENV;
        use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = AppSettings::default();
        let after = AppSettings {
            audio_device: "Yeti X".to_owned(),
            ..AppSettings::default()
        };

        // Sounddevice path (env unset) → audio_device stays live.
        {
            let _env = EnvVarGuard::remove(AUDIO_BACKEND_ENV);
            assert!(
                restart_required_keys(&before, &after).is_empty(),
                "audio_device must stay live on the default Python backend",
            );
        }

        // Rust path. The dynamic gate only fires when the feature is
        // also compiled in — mirror `audio_pipeline_available()` here
        // so the test stays honest in both build modes. RAII guard
        // restores the original env value even if the asserts panic
        // (Codex P2 #415 pattern).
        let _env = EnvVarGuard::set(AUDIO_BACKEND_ENV, "rust");
        let keys = restart_required_keys(&before, &after);
        if cfg!(feature = "audio-in-rust") {
            assert_eq!(
                keys,
                vec!["audio_device"],
                "rust backend must surface a live device change as restart-required",
            );
        } else {
            assert!(
                keys.is_empty(),
                "without the audio-in-rust feature the env opt-in is ignored",
            );
        }
    }

    #[test]
    fn restart_required_keys_reports_restart_only_changes() {
        let before = AppSettings::default();
        let after = AppSettings {
            key: "shift_r+ctrl_r".to_owned(),
            lang: "da".to_owned(),
            inject_mode: "print".to_owned(),
            ..AppSettings::default()
        };

        assert_eq!(restart_required_keys(&before, &after), vec!["key"]);

        let after = AppSettings {
            quit_key: "f12".to_owned(),
            ..AppSettings::default()
        };

        assert_eq!(restart_required_keys(&before, &after), vec!["quit_key"]);

        let after = AppSettings {
            ui_theme: "light".to_owned(),
            ui_language: "da".to_owned(),
            ui_log_view: "diagnostic".to_owned(),
            ui_text_scale: "1.3".to_owned(),
            ..AppSettings::default()
        };

        assert!(restart_required_keys(&before, &after).is_empty());
    }
}
