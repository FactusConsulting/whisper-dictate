//! Writing [`AppSettings`] back into a config JSON object.
//!
//! `apply_to_object` first clears every owned key (so removed/empty values do
//! not linger) and then writes the current typed values, preserving any keys
//! the app does not own.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::config::keys::{DEPRECATED_KEYS, SETTINGS_KEYS};
use crate::config::settings::AppSettings;

impl AppSettings {
    /// Serialize the typed settings into `object`, replacing the values for the
    /// keys this app owns while leaving unrelated keys untouched. Legacy keys
    /// listed in [`DEPRECATED_KEYS`] (e.g. `parakeet_*` after the Wave 8 of
    /// #348 backend removal) are stripped here as well, so they fade out of
    /// users' config.json after one save round-trip.
    pub(crate) fn apply_to_object(&self, object: &mut Map<String, Value>) {
        self.apply_to_object_with_explicit_nulls(object, &[]);
    }

    /// Same serializer with explicit mutation intent supplied by focused
    /// writers such as `wd config set KEY ""`. This is distinct from an
    /// unrelated save of a typed empty field whose key was absent on disk.
    pub(crate) fn apply_to_object_with_explicit_nulls(
        &self,
        object: &mut Map<String, Value>,
        explicit_nulls: &[&str],
    ) {
        let previously_present: HashSet<String> = object.keys().cloned().collect();
        let write_null =
            |key: &str| previously_present.contains(key) || explicit_nulls.contains(&key);
        for key in SETTINGS_KEYS {
            object.remove(*key);
        }
        for key in DEPRECATED_KEYS {
            object.remove(*key);
        }
        set_string(object, "key", &self.key);
        set_string(object, "model", &self.model);
        set_string(object, "stt_backend", &self.stt_backend);
        set_string(object, "stt_provider", &self.stt_provider);
        set_optional_string(
            object,
            "stt_model",
            &self.stt_model,
            write_null("stt_model"),
        );
        set_string(object, "stt_base_url", &self.stt_base_url);
        set_string(object, "stt_timeout_ms", &self.stt_timeout_ms);
        set_string(object, "device", &self.device);
        set_optional_string(
            object,
            "audio_device",
            &self.audio_device,
            write_null("audio_device"),
        );
        // Preserve an explicit Auto selection when `lang` already exists.
        // Clearing a configured language writes null, which suppresses a
        // stale ambient VOICEPI_LANG override. An originally absent field
        // stays absent so unrelated saves do not change env precedence.
        set_optional_string(object, "lang", &self.lang, write_null("lang"));
        set_optional_string(
            object,
            "xkb_layout",
            &self.xkb_layout,
            write_null("xkb_layout"),
        );
        set_optional_string(
            object,
            "initial_prompt",
            &self.initial_prompt,
            write_null("initial_prompt"),
        );
        set_string(object, "inject_mode", &self.inject_mode);
        set_string(object, "format_commands", &self.format_commands);
        set_string(object, "max_chars_per_second", &self.max_chars_per_second);
        set_string(object, "min_record_seconds", &self.min_record_seconds);
        set_string(object, "release_tail_ms", &self.release_tail_ms);
        set_string(object, "preview_seconds", &self.preview_seconds);
        set_string(object, "max_record_s", &self.max_record_s);
        set_string(object, "target_dbfs", &self.target_dbfs);
        set_string(object, "min_input_dbfs", &self.min_input_dbfs);
        set_string(object, "min_snr_db", &self.min_snr_db);
        set_bool(object, "audio_ducking", self.audio_ducking);
        set_string(object, "audio_ducking_level", &self.audio_ducking_level);
        set_optional_string(
            object,
            "dictionary",
            &self.dictionary,
            write_null("dictionary"),
        );
        set_bool(object, "dictionary_enabled", self.dictionary_enabled);
        set_string(object, "dictionary_max_terms", &self.dictionary_max_terms);
        set_string(
            object,
            "dictionary_prompt_chars",
            &self.dictionary_prompt_chars,
        );
        set_bool(object, "json_output", self.inject_json);
        set_optional_string(
            object,
            "metrics_jsonl",
            &self.metrics_jsonl,
            write_null("metrics_jsonl"),
        );
        set_optional_string(
            object,
            "command_hook",
            &self.command_hook,
            write_null("command_hook"),
        );
        set_string(
            object,
            "command_hook_timeout_ms",
            &self.command_hook_timeout_ms,
        );
        set_bool(object, "history_enabled", self.history_enabled);
        set_optional_string(
            object,
            "history_jsonl",
            &self.history_jsonl,
            write_null("history_jsonl"),
        );
        set_bool(object, "local_only", self.local_only);
        set_string(object, "post_processor", &self.post_processor);
        set_string(object, "post_mode", &self.post_mode);
        set_string(object, "post_model", &self.post_model);
        set_string(object, "post_base_url", &self.post_base_url);
        set_string(object, "post_timeout_ms", &self.post_timeout_ms);
        set_string(object, "post_max_input_chars", &self.post_max_input_chars);
        set_string(object, "post_max_output_chars", &self.post_max_output_chars);
        set_bool(object, "post_redact", self.post_redact);
        set_optional_string(
            object,
            "post_redact_terms",
            &self.post_redact_terms,
            write_null("post_redact_terms"),
        );
        set_bool(object, "feedback_sounds", self.feedback_sounds);
        set_string(object, "log_level", &self.log_level);
        set_bool(object, "toggle_mode", self.toggle_mode);
        set_bool(object, "update_check", self.update_check);
        set_string(
            object,
            "update_check_interval_minutes",
            &self.update_check_interval_minutes,
        );
        set_bool(
            object,
            "update_include_prereleases",
            self.update_include_prereleases,
        );
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

/// Insert a trimmed string value, removing the key entirely when empty.
fn set_string(object: &mut Map<String, Value>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        object.remove(key);
    } else {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn set_optional_string(
    object: &mut Map<String, Value>,
    key: &str,
    value: &str,
    previously_present: bool,
) {
    let value = value.trim();
    if value.is_empty() && previously_present {
        object.insert(key.to_owned(), Value::Null);
    } else if value.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_to_object_strips_deprecated_backend_and_listener_keys() {
        // Wave 8 of #348: a saved config carrying the obsolete `parakeet_*`
        // keys must lose them on the first save round-trip, so users don't
        // keep tripping the migration warning on every launch.
        let mut object: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "parakeet_model": "nvidia/parakeet-tdt-0.6b-v3",
            "parakeet_min_seconds": "2.0",
            "parakeet_force_pc": "1",
            "quit_key": "f12",
            "quit_count": "2",
            "quit_window_ms": "800",
            "unknown_preserved": "keep",
        }))
        .unwrap();

        AppSettings::default().apply_to_object(&mut object);

        assert!(!object.contains_key("parakeet_model"));
        assert!(!object.contains_key("parakeet_min_seconds"));
        assert!(!object.contains_key("parakeet_force_pc"));
        assert!(!object.contains_key("quit_key"));
        assert!(!object.contains_key("quit_count"));
        assert!(!object.contains_key("quit_window_ms"));
        assert_eq!(object["unknown_preserved"], "keep");
    }

    #[test]
    fn apply_to_object_keeps_fresh_empty_language_absent() {
        let mut object = Map::new();
        AppSettings::default().apply_to_object(&mut object);

        assert!(!object.contains_key("lang"));
    }
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod save_tests;
