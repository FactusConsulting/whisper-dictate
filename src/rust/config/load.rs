//! Reading [`AppSettings`] from untyped config JSON.
//!
//! `from_value` is decomposed into per-category appliers so each unit stays
//! small and the field-by-field mapping is easy to scan.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use anyhow::Result;
use serde_json::{Map, Value};

use crate::config::settings::{
    groq_post_model_is_retired, normalize_groq_post_model, AppSettings, DEFAULT_GROQ_POST_MODEL,
};

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
            migrate_removed_groq_post_models(&mut settings);
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
        // Codex P2 #655 r3663634829: canonicalise the on-disk device value
        // (trim + lower-case ASCII) so a hand-edited `config.json` with
        // `"  CUDA  "` — a legacy spelling from faster-whisper — resolves to
        // the actual native backend name `"vulkan"` in memory. The
        // corresponding `apply_to_object` writer then persists the
        // canonical form on the next save, so the file self-heals without
        // a heavy migration pass (the migration pass was removed in #648
        // because it silently coerced CLI-set values).
        //
        // Empty is preserved so a bare `"device"` key falls back to the
        // schema default (`auto`) via `string_value`'s fallback path.
        if !self.device.is_empty() {
            self.device = crate::whisper::device_options::canonicalize_device_value(&self.device);
        }
        self.audio_device = string_value(object, "audio_device", "");
        self.lang = string_value(object, "lang", "");
        self.xkb_layout = string_value(object, "xkb_layout", "");
        self.initial_prompt = string_value(object, "initial_prompt", "");
        self.inject_mode = string_value(object, "inject_mode", &defaults.inject_mode);
        self.format_commands = string_value(object, "format_commands", &defaults.format_commands);
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
        self.target_dbfs = string_value(object, "target_dbfs", &defaults.target_dbfs);
        self.min_input_dbfs = string_value(object, "min_input_dbfs", &defaults.min_input_dbfs);
        self.min_snr_db = string_value(object, "min_snr_db", &defaults.min_snr_db);
        self.audio_ducking = bool_value(object, "audio_ducking", defaults.audio_ducking);
        self.audio_ducking_level =
            string_value(object, "audio_ducking_level", &defaults.audio_ducking_level);
    }

    /// Dictionary path and prompt-injection budget settings.
    fn apply_dictionary(&mut self, object: &Map<String, Value>, defaults: &Self) {
        self.dictionary = if object.contains_key("dictionary") {
            string_value(object, "dictionary", "")
        } else {
            defaults.dictionary.clone()
        };
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
        self.feedback_sounds = bool_value(object, "feedback_sounds", defaults.feedback_sounds);
        self.log_level = string_value(object, "log_level", &defaults.log_level);
        if !object.contains_key("log_level") {
            self.log_level = if bool_value(object, "trace", false) {
                "trace"
            } else if bool_value(object, "debug", false) || bool_value(object, "stt_debug", false) {
                "debug"
            } else {
                "info"
            }
            .to_owned();
        }
        self.toggle_mode = bool_value(object, "toggle_mode", defaults.toggle_mode);
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
    }
}

/// Replace a known retired Groq cleanup model while the config is loaded. This
/// makes upgraded installations safe before the user opens Settings or presses
/// Save without rejecting custom or newly released Groq model IDs.
fn migrate_removed_groq_post_models(settings: &mut AppSettings) {
    let top_model = settings.post_model.clone();
    let top_retired = groq_post_model_is_retired(&top_model);
    if normalize_groq_post_model(&settings.post_processor, &mut settings.post_model) && top_retired
    {
        warn_groq_migration_once(&top_model, None);
    }

    let Ok(mut profiles) = serde_json::from_str::<Value>(&settings.profiles_json) else {
        return;
    };
    let Some(profile_array) = profiles.as_array_mut() else {
        return;
    };
    let mut changed = false;
    for profile in profile_array {
        let Some(profile_object) = profile.as_object_mut() else {
            continue;
        };
        let profile_name = profile_object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed")
            .to_owned();
        let Some(overrides) = profile_object
            .get_mut("settings")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let processor = overrides
            .get("post_processor")
            .and_then(Value::as_str)
            .unwrap_or(&settings.post_processor);
        let Some(model) = overrides
            .get("post_model")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let mut normalized_model = model.clone();
        let retired = groq_post_model_is_retired(&model);
        if !normalize_groq_post_model(processor, &mut normalized_model) {
            continue;
        }
        if retired {
            warn_groq_migration_once(&model, Some(&profile_name));
        }
        overrides.insert("post_model".to_owned(), Value::String(normalized_model));
        changed = true;
    }
    if changed {
        settings.profiles_json = serde_json::to_string_pretty(&profiles)
            .unwrap_or_else(|_| settings.profiles_json.clone());
    }
}

fn warn_groq_migration_once(model: &str, profile_name: Option<&str>) {
    static WARNED_MODELS: LazyLock<Mutex<HashSet<String>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));
    let mut warned = WARNED_MODELS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if first_warning_for_model(&mut warned, model) {
        let message = match profile_name {
            Some(name) => format!(
                "[config] profile {:?} saved Groq post model {:?} is no longer supported; migrating to {:?}",
                name, model, DEFAULT_GROQ_POST_MODEL,
            ),
            None => format!(
                "[config] saved Groq post model {:?} is no longer supported; migrating to {:?}",
                model, DEFAULT_GROQ_POST_MODEL,
            ),
        };
        crate::diag::write_line(&message);
    }
}

fn first_warning_for_model(warned: &mut HashSet<String>, model: &str) -> bool {
    warned.insert(model.trim().to_owned())
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
#[path = "load_tests.rs"]
mod load_tests;

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
        assert_eq!(settings.key, "pause");
        assert_eq!(settings.ui_text_scale, "1.15");
        assert_eq!(settings.log_level, "info");
    }

    #[test]
    fn legacy_python_diagnostics_migrate_to_native_log_level() {
        let verbose = AppSettings::from_value(serde_json::json!({
            "debug": "1",
            "stt_debug": "1"
        }))
        .unwrap();
        assert_eq!(verbose.log_level, "debug");

        let trace = AppSettings::from_value(serde_json::json!({"trace": "1"})).unwrap();
        assert_eq!(trace.log_level, "trace");

        let explicit = AppSettings::from_value(serde_json::json!({
            "log_level": "off",
            "trace": "1"
        }))
        .unwrap();
        assert_eq!(explicit.log_level, "off");
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

    #[test]
    fn saved_cuda_device_migrates_to_the_native_vulkan_name() {
        // The retired faster-whisper runtime called its GPU preference
        // `cuda`. Standard native GPU builds use Vulkan, so preserve the
        // user's intent while migrating the saved value to the backend name
        // the current UI and CLI expose.
        let value = serde_json::json!({ "device": "cuda" });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.device, "vulkan");
    }

    #[test]
    fn supported_device_is_preserved_on_load() {
        // An `auto` / `cpu` config on a fresh install must round-trip
        // untouched — no phantom coercion, no stderr warnings on a healthy
        // launch.
        for value in ["auto", "cpu"] {
            let json = serde_json::json!({ "device": value });
            let settings = AppSettings::from_value(json).unwrap();
            assert_eq!(settings.device, value, "device={value:?} was rewritten");
        }
    }

    #[test]
    fn unrecognised_device_value_is_left_for_the_validator() {
        // A hand-edited / typo value (`"gpu"`, `"foo"`) must NOT be
        // silently rewritten to a valid device — that would hide typos on
        // save. The validator's "must be one of …" error is the intended
        // UX. Uses a probe-shaped value to lock in the invariant the
        // schema round-trip test relies on (see
        // config::tests::every_schema_setting_is_wired…).
        //
        // NOTE: after the load-time canonicalisation added for Codex P2
        // #655 r3663634829, the value IS trimmed + lower-cased (so
        // `"  GPU  "` becomes `"gpu"`), but only that shape-preserving
        // normalisation happens — an unrecognised token still stays
        // unrecognised. The probe fixture (`"auto_wdprobe"`) is already
        // trimmed lower-case ASCII, so this invariant is unchanged.
        let value = serde_json::json!({ "device": "auto_wdprobe" });
        let settings = AppSettings::from_value(value).unwrap();
        assert_eq!(settings.device, "auto_wdprobe");
    }

    #[test]
    fn hand_edited_device_value_is_canonicalised_on_load() {
        // Hand-edited values are normalised to the same stable form the CLI
        // writes. The legacy CUDA spelling also migrates to Vulkan so no
        // current code path has to infer which native backend it meant.
        for (raw, expected) in [
            ("  CUDA  ", "vulkan"),
            ("Auto", "auto"),
            ("\tCPU\n", "cpu"),
            ("cuda", "vulkan"),
            ("cpu", "cpu"),
        ] {
            let json = serde_json::json!({ "device": raw });
            let settings = AppSettings::from_value(json).unwrap();
            assert_eq!(
                settings.device, expected,
                "hand-edited device={raw:?} must canonicalise to {expected:?}"
            );
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
