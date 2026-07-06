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
    ///
    /// Wave 8 (#348) drops the Parakeet/NeMo backend. Saved configs that still
    /// carry `stt_backend = "parakeet"` are migrated to the schema default
    /// (`"whisper"`) with a one-line warning on stderr; the obsolete
    /// `parakeet_*` keys are dropped on the next save via
    /// [`crate::config::keys::DEPRECATED_KEYS`].
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
                .unwrap_or_else(|| defaults.profiles_json.clone());
            migrate_parakeet_backend(&mut settings, object, &defaults);
            migrate_settings_mode(&mut settings, object);
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
        self.device = string_value(object, "device", &defaults.device);
        self.compute_type = string_value(object, "compute_type", "");
        self.audio_device = string_value(object, "audio_device", "");
        self.lang = string_value(object, "lang", "");
        self.xkb_layout = string_value(object, "xkb_layout", "");
        self.initial_prompt = string_value(object, "initial_prompt", "");
        self.inject_mode = string_value(object, "inject_mode", &defaults.inject_mode);
        self.format_commands = string_value(object, "format_commands", &defaults.format_commands);
        self.beam_size = string_value(object, "beam_size", &defaults.beam_size);
        self.temperature = string_value(object, "temperature", &defaults.temperature);
        self.context_min_seconds =
            string_value(object, "context_min_seconds", &defaults.context_min_seconds);
        self.hallucination_guard =
            bool_value(object, "hallucination_guard", defaults.hallucination_guard);
        self.max_chars_per_second = string_value(
            object,
            "max_chars_per_second",
            &defaults.max_chars_per_second,
        );
        self.min_record_seconds =
            string_value(object, "min_record_seconds", &defaults.min_record_seconds);
        self.release_tail_ms = string_value(object, "release_tail_ms", &defaults.release_tail_ms);
        self.preview_seconds = string_value(object, "preview_seconds", &defaults.preview_seconds);
        self.max_record_s = string_value(object, "max_record_s", &defaults.max_record_s);
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
        // Issue #322: auto-mute the system output while recording. Opt-in bool,
        // parsed with the same "0"/"1"/"true"/"false" contract as the rest.
        self.mute_output_while_recording = bool_value(
            object,
            "mute_output_while_recording",
            defaults.mute_output_while_recording,
        );
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
        self.postprocess_hotkey = string_value(object, "postprocess_hotkey", "");
        // Codex-2 finding on #439: users following the schema doc write
        // `"postprocess_profiles": [{...}]` (a JSON array) in config.json,
        // but the string-only path silently dropped the array and the
        // next save wiped it. Accept both shapes — a raw string
        // round-trips verbatim, an array is re-encoded to the string form
        // the rest of the pipeline consumes.
        self.postprocess_profiles = string_or_json_value(object, "postprocess_profiles", "");
        self.postprocess_profile_index = string_value(
            object,
            "postprocess_profile_index",
            &defaults.postprocess_profile_index,
        );
    }

    /// Debug flags and quit-shortcut settings.
    fn apply_misc(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.local_only = bool_value(object, "local_only", defaults.local_only);
        self.feedback_sounds = bool_value(object, "feedback_sounds", defaults.feedback_sounds);
        self.feedback_notify = bool_value(object, "feedback_notify", defaults.feedback_notify);
        self.debug = bool_value(object, "debug", defaults.debug);
        self.stt_debug = bool_value(object, "stt_debug", defaults.stt_debug);
        self.trace = bool_value(object, "trace", defaults.trace);
        self.toggle_mode = bool_value(object, "toggle_mode", defaults.toggle_mode);
        self.quit_key = string_value(object, "quit_key", &defaults.quit_key);
        self.quit_count = string_value(object, "quit_count", &defaults.quit_count);
        self.quit_window_ms = string_value(object, "quit_window_ms", &defaults.quit_window_ms);
        self.update_check = bool_value(object, "update_check", defaults.update_check);
        self.update_check_interval_minutes = string_value(
            object,
            "update_check_interval_minutes",
            &defaults.update_check_interval_minutes,
        );
        self.update_include_prereleases = bool_value(
            object,
            "update_include_prereleases",
            defaults.update_include_prereleases,
        );
    }

    /// UI-only presentation settings (theme, language, log view, text scale).
    fn apply_ui(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.ui_theme = string_value(object, "ui_theme", &defaults.ui_theme);
        self.ui_language = string_value(object, "ui_language", &defaults.ui_language);
        self.ui_log_view = string_value(object, "ui_log_view", &defaults.ui_log_view);
        self.ui_text_scale = string_value(object, "ui_text_scale", &defaults.ui_text_scale);
        self.overlay_enabled = bool_value(object, "overlay_enabled", defaults.overlay_enabled);
        self.overlay_position =
            string_value(object, "overlay_position", &defaults.overlay_position);
        self.overlay_show_on_idle = bool_value(
            object,
            "overlay_show_on_idle",
            defaults.overlay_show_on_idle,
        );
        // Issue #328: first-run onboarding gate + last-seen timestamp.
        self.onboarding_completed = bool_value(
            object,
            "onboarding_completed",
            defaults.onboarding_completed,
        );
        self.onboarding_seen_at = string_value(object, "onboarding_seen_at", "");
        // Load the persisted Simple/Advanced choice. Missing → keep the
        // default; the "existing user" migration below then upgrades the
        // implicit default when the rest of the config is non-empty.
        self.settings_mode = string_value(object, "settings_mode", &defaults.settings_mode);
    }
}

/// Issue #334 migration: an existing config that predates the
/// Simple/Advanced toggle has no `settings_mode` key. Those users should
/// default to **Advanced** so nothing they previously configured
/// disappears. New/empty configs keep the schema default (`"simple"`)
/// picked in [`AppSettings::default`], so first-time users land on the
/// slim view.
///
/// "Existing user" is detected by looking for ANY other saved key on
/// disk (`stt_backend`, `key`, etc.); an empty `{}` config counts as a
/// fresh install even if the file itself was created accidentally.
fn migrate_settings_mode(settings: &mut AppSettings, object: &Map<String, Value>) {
    if object.contains_key("settings_mode") {
        return;
    }
    // Any other key implies "the user has saved settings before" — flip
    // them to Advanced. A stray empty object stays on the fresh-install
    // default.
    let has_other_settings = object.keys().any(|key| key != "settings_mode");
    if has_other_settings {
        settings.settings_mode = "advanced".to_owned();
    }
}

/// Wave 8 (#348) migration: the Parakeet/NeMo backend was dropped, so any
/// saved `stt_backend = "parakeet"` is rewritten to the schema default
/// (`"whisper"`) with a one-line warning. Also surfaces a warning when any
/// legacy `parakeet_*` key is present (those are stripped on the next save
/// via [`crate::config::keys::DEPRECATED_KEYS`]).
///
/// The migration is deliberately quiet on a fresh config: a user who never
/// set the Parakeet backend never sees these warnings.
fn migrate_parakeet_backend(
    settings: &mut AppSettings,
    object: &Map<String, Value>,
    defaults: &AppSettings,
) {
    let parakeet_backend = settings.stt_backend.eq_ignore_ascii_case("parakeet");
    let legacy_keys: Vec<&'static str> = [
        "parakeet_model",
        "parakeet_min_seconds",
        "parakeet_force_pc",
    ]
    .into_iter()
    .filter(|key| object.contains_key(*key))
    .collect();

    if parakeet_backend {
        eprintln!(
            "[config] stt_backend=\"parakeet\" is no longer supported \
             (NeMo/Parakeet backend removed in Wave 8 of #348); migrating \
             to stt_backend={:?}. Use whisper-large-v3-turbo for the same \
             Danish/mixed-language use case.",
            defaults.stt_backend,
        );
        settings.stt_backend = defaults.stt_backend.clone();
    }
    if !legacy_keys.is_empty() {
        eprintln!(
            "[config] dropping obsolete parakeet_* keys on next save: {}",
            legacy_keys.join(", "),
        );
    }
}

fn string_value(object: &Map<String, Value>, key: &str, default: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

/// Read a config value that the schema calls a string but power users
/// may hand-edit as a raw JSON array/object. Strings pass through
/// verbatim; anything else is re-encoded to JSON so downstream string
/// parsers still get the same shape. Used by `postprocess_profiles`
/// (Codex-2 finding on #439).
fn string_or_json_value(object: &Map<String, Value>, key: &str, default: &str) -> String {
    match object.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| default.to_owned()),
        None => default.to_owned(),
    }
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
    fn parakeet_backend_migrates_to_default() {
        // Wave 8 of #348: a saved `stt_backend = "parakeet"` is rewritten to
        // the schema default ("whisper"), preserving everything else.
        let value = serde_json::json!({
            "stt_backend": "parakeet",
            "lang": "da",
        });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.stt_backend, "whisper");
        assert_eq!(settings.lang, "da");
    }

    #[test]
    fn parakeet_backend_migration_is_case_insensitive() {
        // The legacy env-var path accepts uppercase + mixed case ("Parakeet",
        // "PARAKEET"); the migration must catch those the same way. We do not
        // try to trim whitespace — the wizard / Rust UI only ever writes
        // canonical lowercase enum tokens, and a hand-edited
        // " parakeet " would already fail validation downstream regardless of
        // the migration.
        for raw in ["PARAKEET", "Parakeet", "parakeet"] {
            let value = serde_json::json!({ "stt_backend": raw });
            let settings = AppSettings::from_value(value).unwrap();
            assert_eq!(
                settings.stt_backend, "whisper",
                "stt_backend={raw:?} must migrate to whisper",
            );
        }
    }

    #[test]
    fn obsolete_parakeet_keys_do_not_block_load() {
        // A config carrying the deprecated parakeet_* keys still loads
        // cleanly; the keys are stripped on the next save (see
        // `apply_to_object` + DEPRECATED_KEYS).
        let value = serde_json::json!({
            "stt_backend": "whisper",
            "parakeet_model": "nvidia/parakeet-tdt-0.6b-v3",
            "parakeet_min_seconds": "2.0",
            "parakeet_force_pc": "1",
        });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.stt_backend, "whisper");
    }

    #[test]
    fn fresh_whisper_config_skips_parakeet_migration_path() {
        // Sanity check: a clean config never triggers the migration; the
        // stderr warning would otherwise spam every healthy launch.
        let value = serde_json::json!({ "stt_backend": "whisper" });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.stt_backend, "whisper");
    }

    /// Codex-2 finding on #439: schema/docs describe
    /// `postprocess_profiles` as a JSON array, but the string-only load
    /// path silently dropped array values and the next save (which owns
    /// the key via `SETTINGS_KEYS`) wiped them. This test asserts both
    /// shapes are accepted and end up as the string form the rest of the
    /// pipeline consumes.
    #[test]
    fn postprocess_profiles_accepts_json_array_and_reencodes_to_string() {
        let value = serde_json::json!({
            "postprocess_profiles": [
                {"name": "Grammar", "processor": "ollama", "mode": "clean"},
                {"name": "Bullets", "processor": "ollama", "mode": "bullets"},
            ],
        });
        let settings = AppSettings::from_value(value).unwrap();
        assert!(!settings.postprocess_profiles.is_empty());
        assert!(settings.postprocess_profiles.contains("Grammar"));
        assert!(settings.postprocess_profiles.contains("Bullets"));
        // The re-encoded value must round-trip back through the profile
        // registry loader so the runtime keeps consuming it as-is.
        let parsed: serde_json::Value =
            serde_json::from_str(&settings.postprocess_profiles).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[test]
    fn postprocess_profiles_accepts_string_form_verbatim() {
        // Back-compat: the previous string-only representation must
        // still land in the field untouched (the load path used to be
        // string-only, and hand-written configs / test fixtures may
        // still use it).
        let raw = r#"[{"name":"Only","processor":"none","mode":"raw"}]"#;
        let value = serde_json::json!({
            "postprocess_profiles": raw,
        });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.postprocess_profiles, raw);
    }

    #[test]
    fn settings_mode_defaults_to_simple_on_empty_config() {
        // Issue #334: a fresh install (empty JSON object) is a first-time
        // user; keep the Simple default so onboarding stays slim.
        let settings = AppSettings::from_value(serde_json::json!({})).unwrap();
        assert_eq!(settings.settings_mode, "simple");
    }

    #[test]
    fn settings_mode_migrates_to_advanced_for_existing_users() {
        // Issue #334: an existing config that predates the toggle has no
        // `settings_mode` key. Anything ELSE saved in the file means "the
        // user has settings history" — flip them to Advanced so nothing
        // they previously configured disappears from the UI.
        let value = serde_json::json!({
            "lang": "da",
            "stt_backend": "whisper",
        });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.settings_mode, "advanced");
    }

    #[test]
    fn settings_mode_is_respected_when_explicitly_saved() {
        // When the user has already picked a mode, the saved value wins
        // and the migration is a no-op regardless of what else is saved.
        for mode in ["simple", "advanced"] {
            let value = serde_json::json!({
                "settings_mode": mode,
                "lang": "en",
            });
            let settings = AppSettings::from_value(value).unwrap();
            assert_eq!(settings.settings_mode, mode);
        }
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
