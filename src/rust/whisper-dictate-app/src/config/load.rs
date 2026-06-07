//! Reading [`AppSettings`] from untyped config JSON.
//!
//! `from_value` is decomposed into per-category appliers so each unit stays
//! small and the field-by-field mapping is easy to scan.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::config::settings::AppSettings;

impl AppSettings {
    /// Build [`AppSettings`] from untyped config JSON, falling back to defaults
    /// for missing keys.
    pub fn from_value(value: Value) -> Result<Self> {
        let defaults = Self::default();
        let mut settings = defaults.clone();
        if let Some(object) = value.as_object() {
            settings.apply_stt(object, &defaults);
            settings.apply_audio(object, &defaults);
            settings.apply_dictionary(object, &defaults);
            settings.apply_output(object, &defaults);
            settings.apply_post(object, &defaults);
            settings.apply_misc(object, &defaults);
            settings.apply_ui(object, &defaults);
            settings.profiles_json = object
                .get("profiles")
                .map(serde_json::to_string_pretty)
                .transpose()?
                .unwrap_or(defaults.profiles_json);
        }
        Ok(settings)
    }

    /// Speech-to-text engine, provider, model and connection settings.
    fn apply_stt(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.key = string_value(object, "key", &defaults.key);
        self.model = string_value(object, "model", &defaults.model);
        self.stt_backend = string_value(object, "stt_backend", &defaults.stt_backend);
        self.stt_provider = string_value(object, "stt_provider", "");
        self.stt_model = string_value(object, "stt_model", "");
        self.stt_base_url = string_value(object, "stt_base_url", &defaults.stt_base_url);
        if self.stt_provider.trim().is_empty() {
            self.stt_provider = if self
                .stt_base_url
                .to_ascii_lowercase()
                .contains("api.groq.com")
            {
                "groq".to_owned()
            } else {
                defaults.stt_provider.clone()
            };
        }
        self.stt_timeout_ms = string_value(object, "stt_timeout_ms", &defaults.stt_timeout_ms);
        self.parakeet_model = string_value(object, "parakeet_model", &defaults.parakeet_model);
        self.device = string_value(object, "device", &defaults.device);
        self.compute_type = string_value(object, "compute_type", "");
        self.lang = string_value(object, "lang", "");
        self.xkb_layout = string_value(object, "xkb_layout", "");
        self.initial_prompt = string_value(object, "initial_prompt", "");
        self.inject_mode = string_value(object, "inject_mode", &defaults.inject_mode);
        self.format_commands = string_value(object, "format_commands", &defaults.format_commands);
        self.beam_size = string_value(object, "beam_size", &defaults.beam_size);
        self.temperature = string_value(object, "temperature", &defaults.temperature);
        self.context_min_seconds =
            string_value(object, "context_min_seconds", &defaults.context_min_seconds);
        self.parakeet_min_seconds = string_value(
            object,
            "parakeet_min_seconds",
            &defaults.parakeet_min_seconds,
        );
        self.release_tail_ms = string_value(object, "release_tail_ms", &defaults.release_tail_ms);
    }

    /// Voice-activity-detection and audio level/ducking settings.
    fn apply_audio(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.vad_threshold = string_value(object, "vad_threshold", &defaults.vad_threshold);
        self.vad_min_silence_ms =
            string_value(object, "vad_min_silence_ms", &defaults.vad_min_silence_ms);
        self.vad_speech_pad_ms =
            string_value(object, "vad_speech_pad_ms", &defaults.vad_speech_pad_ms);
        self.target_dbfs = string_value(object, "target_dbfs", &defaults.target_dbfs);
        self.min_input_dbfs = string_value(object, "min_input_dbfs", &defaults.min_input_dbfs);
        self.min_snr_db = string_value(object, "min_snr_db", &defaults.min_snr_db);
        self.audio_ducking = bool_value(object, "audio_ducking", defaults.audio_ducking);
        self.audio_ducking_level =
            string_value(object, "audio_ducking_level", &defaults.audio_ducking_level);
    }

    /// Dictionary path and prompt-injection budget settings.
    fn apply_dictionary(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.dictionary = string_value(object, "dictionary", &defaults.dictionary);
        self.dictionary_enabled =
            bool_value(object, "dictionary_enabled", defaults.dictionary_enabled);
        self.dictionary_max_terms = string_value(
            object,
            "dictionary_max_terms",
            &defaults.dictionary_max_terms,
        );
        self.dictionary_prompt_chars = string_value(
            object,
            "dictionary_prompt_chars",
            &defaults.dictionary_prompt_chars,
        );
    }

    /// Output sinks: JSON stdout, metrics, command hook and history.
    fn apply_output(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.inject_json = bool_value(object, "json_output", defaults.inject_json);
        self.metrics_jsonl = string_value(object, "metrics_jsonl", "");
        self.command_hook = string_value(object, "command_hook", "");
        self.command_hook_timeout_ms = string_value(
            object,
            "command_hook_timeout_ms",
            &defaults.command_hook_timeout_ms,
        );
        self.history_enabled = bool_value(object, "history_enabled", defaults.history_enabled);
        self.history_jsonl = string_value(object, "history_jsonl", "");
    }

    /// Post-processor model, limits and redaction settings.
    fn apply_post(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.post_processor = string_value(object, "post_processor", &defaults.post_processor);
        self.post_mode = string_value(object, "post_mode", &defaults.post_mode);
        self.post_model = string_value(object, "post_model", &defaults.post_model);
        self.post_base_url = string_value(object, "post_base_url", &defaults.post_base_url);
        self.post_timeout_ms = string_value(object, "post_timeout_ms", &defaults.post_timeout_ms);
        self.post_max_input_chars = string_value(
            object,
            "post_max_input_chars",
            &defaults.post_max_input_chars,
        );
        self.post_max_output_chars = string_value(
            object,
            "post_max_output_chars",
            &defaults.post_max_output_chars,
        );
        self.post_redact = bool_value(object, "post_redact", defaults.post_redact);
        self.post_redact_terms = string_value(object, "post_redact_terms", "");
    }

    /// Debug flags and quit-shortcut settings.
    fn apply_misc(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.local_only = bool_value(object, "local_only", defaults.local_only);
        self.debug = bool_value(object, "debug", defaults.debug);
        self.stt_debug = bool_value(object, "stt_debug", defaults.stt_debug);
        self.quit_key = string_value(object, "quit_key", &defaults.quit_key);
        self.quit_count = string_value(object, "quit_count", &defaults.quit_count);
        self.quit_window_ms = string_value(object, "quit_window_ms", &defaults.quit_window_ms);
    }

    /// UI-only presentation settings (theme, language, log view, text scale).
    fn apply_ui(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.ui_theme = string_value(object, "ui_theme", &defaults.ui_theme);
        self.ui_language = string_value(object, "ui_language", &defaults.ui_language);
        self.ui_log_view = string_value(object, "ui_log_view", &defaults.ui_log_view);
        self.ui_text_scale = string_value(object, "ui_text_scale", &defaults.ui_text_scale);
    }
}

fn string_value(object: &Map<String, Value>, key: &str, default: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_value(object: &Map<String, Value>, key: &str, default: bool) -> bool {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_load_defaults_and_existing_values() {
        let value = serde_json::json!({
            "stt_backend": "openai",
            "stt_provider": "groq",
            "lang": "da",
            "xkb_layout": "dk",
            "quit_key": "f12",
            "dictionary_enabled": "0",
            "json_output": "1",
            "audio_ducking": "1",
            "post_redact": "1",
            "post_redact_terms": "Lars Andersen",
            "ui_theme": "light",
            "ui_language": "da",
            "ui_log_view": "diagnostic",
            "profiles": [{"name": "terminal"}]
        });

        let settings = AppSettings::from_value(value).unwrap();

        assert_eq!(settings.stt_backend, "openai");
        assert_eq!(settings.stt_provider, "groq");
        assert_eq!(settings.lang, "da");
        assert_eq!(settings.xkb_layout, "dk");
        assert_eq!(settings.quit_key, "f12");
        assert!(!settings.dictionary_enabled);
        assert!(settings.inject_json);
        assert!(settings.audio_ducking);
        assert!(settings.post_redact);
        assert_eq!(settings.post_redact_terms, "Lars Andersen");
        assert_eq!(settings.ui_theme, "light");
        assert_eq!(settings.ui_language, "da");
        assert_eq!(settings.ui_log_view, "diagnostic");
        assert!(settings.profiles_json.contains("terminal"));
        assert_eq!(settings.model, "large-v3-turbo");
        assert_eq!(settings.context_min_seconds, "5");
        assert_eq!(settings.ui_text_scale, "1.15");
    }

    #[test]
    fn settings_infers_groq_provider_from_existing_base_url() {
        let value = serde_json::json!({
            "stt_backend": "openai",
            "stt_base_url": "https://api.groq.com/openai/v1",
            "stt_model": "whisper-large-v3-turbo"
        });

        let settings = AppSettings::from_value(value).unwrap();

        assert_eq!(settings.stt_provider, "groq");
        assert_eq!(settings.stt_base_url, "https://api.groq.com/openai/v1");
    }
}
