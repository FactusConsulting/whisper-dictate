//! Config-key catalogs and restart-impact comparison.
//!
//! [`SETTINGS_KEYS`] lists every config.json key the typed [`AppSettings`]
//! owns (used to wipe stale entries before re-serializing). [`RESTART_KEYS`] is
//! the subset whose change requires a worker restart; it must stay consistent
//! with the schema's `live` flag (guarded by a test in the parent module).

use crate::config::AppSettings;

/// Default Parakeet model id. Kept here because both the typed defaults and the
/// serializer (which omits the value when unchanged) reference it.
pub(crate) const DEFAULT_PARAKEET_MODEL: &str = "nvidia/parakeet-tdt-0.6b-v3";

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
    "parakeet_model",
    "device",
    "compute_type",
    "lang",
    "xkb_layout",
    "initial_prompt",
    "inject_mode",
    "format_commands",
    "beam_size",
    "temperature",
    "context_min_seconds",
    "hallucination_guard",
    "parakeet_min_seconds",
    "release_tail_ms",
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
    "debug",
    "stt_debug",
    "quit_key",
    "quit_count",
    "quit_window_ms",
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
    "parakeet_model",
    "device",
    "compute_type",
    "local_only",
    "quit_key",
    "quit_count",
    "quit_window_ms",
];

/// Report which [`RESTART_KEYS`] differ between two settings snapshots, so the
/// UI can warn that a restart is required.
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
