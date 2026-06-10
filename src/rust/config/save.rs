//! Writing [`AppSettings`] back into a config JSON object.
//!
//! `apply_to_object` first clears every owned key (so removed/empty values do
//! not linger) and then writes the current typed values, preserving any keys
//! the app does not own.

use serde_json::{Map, Value};

use crate::config::keys::{DEFAULT_PARAKEET_MODEL, SETTINGS_KEYS};
use crate::config::settings::AppSettings;

impl AppSettings {
    /// Serialize the typed settings into `object`, replacing the values for the
    /// keys this app owns while leaving unrelated keys untouched.
    pub(crate) fn apply_to_object(&self, object: &mut Map<String, Value>) {
        for key in SETTINGS_KEYS {
            object.remove(*key);
        }
        set_string(object, "key", &self.key);
        set_string(object, "model", &self.model);
        set_string(object, "stt_backend", &self.stt_backend);
        set_string(object, "stt_provider", &self.stt_provider);
        set_string(object, "stt_model", &self.stt_model);
        set_string(object, "stt_base_url", &self.stt_base_url);
        set_string(object, "stt_timeout_ms", &self.stt_timeout_ms);
        if self.parakeet_model != DEFAULT_PARAKEET_MODEL {
            set_string(object, "parakeet_model", &self.parakeet_model);
        }
        set_string(object, "device", &self.device);
        set_string(object, "compute_type", &self.compute_type);
        set_string(object, "audio_device", &self.audio_device);
        set_string(object, "lang", &self.lang);
        set_string(object, "xkb_layout", &self.xkb_layout);
        set_string(object, "initial_prompt", &self.initial_prompt);
        set_string(object, "inject_mode", &self.inject_mode);
        set_string(object, "format_commands", &self.format_commands);
        set_string(object, "beam_size", &self.beam_size);
        set_string(object, "temperature", &self.temperature);
        set_string(object, "context_min_seconds", &self.context_min_seconds);
        set_bool(object, "hallucination_guard", self.hallucination_guard);
        set_string(object, "parakeet_min_seconds", &self.parakeet_min_seconds);
        set_string(object, "release_tail_ms", &self.release_tail_ms);
        set_string(object, "preview_seconds", &self.preview_seconds);
        set_string(object, "max_record_s", &self.max_record_s);
        set_string(object, "vad_threshold", &self.vad_threshold);
        set_string(object, "vad_min_silence_ms", &self.vad_min_silence_ms);
        set_string(object, "vad_speech_pad_ms", &self.vad_speech_pad_ms);
        set_string(object, "target_dbfs", &self.target_dbfs);
        set_string(object, "min_input_dbfs", &self.min_input_dbfs);
        set_string(object, "min_snr_db", &self.min_snr_db);
        set_bool(object, "audio_ducking", self.audio_ducking);
        set_string(object, "audio_ducking_level", &self.audio_ducking_level);
        set_string(object, "dictionary", &self.dictionary);
        set_bool(object, "dictionary_enabled", self.dictionary_enabled);
        set_string(object, "dictionary_max_terms", &self.dictionary_max_terms);
        set_string(
            object,
            "dictionary_prompt_chars",
            &self.dictionary_prompt_chars,
        );
        set_bool(object, "json_output", self.inject_json);
        set_string(object, "metrics_jsonl", &self.metrics_jsonl);
        set_string(object, "command_hook", &self.command_hook);
        set_string(
            object,
            "command_hook_timeout_ms",
            &self.command_hook_timeout_ms,
        );
        set_bool(object, "history_enabled", self.history_enabled);
        set_string(object, "history_jsonl", &self.history_jsonl);
        set_bool(object, "local_only", self.local_only);
        set_string(object, "post_processor", &self.post_processor);
        set_string(object, "post_mode", &self.post_mode);
        set_string(object, "post_model", &self.post_model);
        set_string(object, "post_base_url", &self.post_base_url);
        set_string(object, "post_timeout_ms", &self.post_timeout_ms);
        set_string(object, "post_max_input_chars", &self.post_max_input_chars);
        set_string(object, "post_max_output_chars", &self.post_max_output_chars);
        set_bool(object, "post_redact", self.post_redact);
        set_string(object, "post_redact_terms", &self.post_redact_terms);
        set_bool(object, "debug", self.debug);
        set_bool(object, "stt_debug", self.stt_debug);
        set_bool(object, "toggle_mode", self.toggle_mode);
        set_string(object, "quit_key", &self.quit_key);
        set_string(object, "quit_count", &self.quit_count);
        set_string(object, "quit_window_ms", &self.quit_window_ms);
        set_string(object, "ui_theme", &self.ui_theme);
        set_string(object, "ui_language", &self.ui_language);
        set_string(object, "ui_log_view", &self.ui_log_view);
        set_string(object, "ui_text_scale", &self.ui_text_scale);
        if let Ok(profiles) = serde_json::from_str::<Value>(&self.profiles_json) {
            if !profiles.as_array().is_some_and(Vec::is_empty) {
                object.insert("profiles".to_owned(), profiles);
            } else {
                object.remove("profiles");
            }
        }
    }
}

/// Insert a trimmed string value, removing the key entirely when empty so the
/// config file never carries blank fields.
fn set_string(object: &mut Map<String, Value>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        object.remove(key);
    } else {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

/// Persist a boolean as the `"1"`/`"0"` string the worker expects.
fn set_bool(object: &mut Map<String, Value>, key: &str, value: bool) {
    object.insert(
        key.to_owned(),
        Value::String(if value { "1" } else { "0" }.to_owned()),
    );
}
