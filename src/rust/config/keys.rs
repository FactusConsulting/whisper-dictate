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
/// remove stale keys before writing the current typed values back, and also by
/// the `config get`/`set` CLI verbs as the allow-list of user-editable keys
/// ([`crate::config::valid_keys`] returns a stable-order borrow of it).
pub(crate) const SETTINGS_KEYS: &[&str] = &[
    "key",
    "model",
    "stt_backend",
    "stt_provider",
    "stt_model",
    "stt_base_url",
    "stt_timeout_ms",
    "device",
    "audio_device",
    "lang",
    "xkb_layout",
    "initial_prompt",
    "inject_mode",
    "format_commands",
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
    "audio_device",
    "local_only",
    "toggle_mode",
    "quit_key",
    "quit_count",
    "quit_window_ms",
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
    // faster-whisper/CTranslate2 controls retired with the Python engine.
    // Native whisper.cpp selects quantisation from the model file and uses
    // its own fixed decoding strategy, so retaining these would be misleading.
    "compute_type",
    "beam_size",
    "temperature",
];

/// Report which [`RESTART_KEYS`] differ between two settings snapshots, so the
/// UI can warn that a restart is required.
///
/// The native supervisor opens the CPAL stream at worker start and does not
/// listen for live device changes, so `audio_device` is always static now
/// that the Python audio path has retired.
pub fn restart_required_keys(before: &AppSettings, after: &AppSettings) -> Vec<&'static str> {
    RESTART_KEYS
        .iter()
        .copied()
        .filter(|key| before.setting_value(key) != after.setting_value(key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changing the microphone must require a restart because the native
    /// supervisor opens the CPAL stream at start and has no live device swap.
    #[test]
    fn restart_required_keys_marks_audio_device_for_native_runtime() {
        let before = AppSettings::default();
        let after = AppSettings {
            audio_device: "Yeti X".to_owned(),
            ..AppSettings::default()
        };

        assert_eq!(restart_required_keys(&before, &after), vec!["audio_device"]);
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

    #[test]
    fn retired_python_decoder_controls_cannot_reenter_runtime_config() {
        for key in ["compute_type", "beam_size", "temperature"] {
            assert!(!SETTINGS_KEYS.contains(&key), "{key} must not be editable");
            assert!(DEPRECATED_KEYS.contains(&key), "{key} must be stripped");
            assert!(
                !crate::config::runtime_settings()
                    .iter()
                    .any(|setting| setting.key == key),
                "{key} must not be exposed by the schema"
            );
        }

        let native_backend = include_str!("../runtime/rust_session_real_backends.rs");
        let native_file = include_str!("../transcribe_file.rs");
        assert!(!native_backend.contains("VOICEPI_COMPUTE_TYPE"));
        assert!(!native_file.contains("VOICEPI_COMPUTE_TYPE"));
        assert!(!native_backend.contains("VOICEPI_BEAM_SIZE"));
        assert!(!native_backend.contains("VOICEPI_TEMPERATURE"));
    }
}
