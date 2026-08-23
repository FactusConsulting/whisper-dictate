//! Runtime-settings schema: the single source of truth shared with the Python
//! worker for the `VOICEPI_* env var <-> config key <-> default` mapping.
//!
//! The schema JSON is embedded at compile time so the controller has no runtime
//! file dependency; add or change settings in `settings_schema.json`, not in a
//! table here. This module derives the effective worker environment from the
//! schema plus the on-disk config and the process environment.

use std::collections::BTreeMap;
use std::env;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config::io::load_raw_config;
use crate::config::settings::{groq_post_model_is_supported, DEFAULT_GROQ_POST_MODEL};

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSetting {
    pub env: String,
    pub key: String,
    #[serde(default)]
    pub default: Option<String>,
    /// Whether a running dictation session may apply this setting at the next
    /// utterance boundary without rebuilding the runtime.
    #[serde(default)]
    pub live: bool,
    /// Optional inclusive lower bound for numeric fields. The UI clamps user
    /// input to `[min, max]`; absent for free-text settings.
    #[serde(default)]
    pub min: Option<f64>,
    /// Optional inclusive upper bound for numeric fields (see [`Self::min`]).
    #[serde(default)]
    pub max: Option<f64>,
    /// Optional UI step granularity. Also used to infer integer-vs-float: a
    /// whole-number step (and whole default) means the field is an integer.
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_advanced")]
    pub advanced: bool,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub choices: Vec<String>,
}

fn default_advanced() -> bool {
    true
}

/// Inclusive numeric bounds for a settings field, surfaced from the schema so
/// the UI is the single enforcement point while the schema stays the single
/// source of truth.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericBounds {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// Whether the field is integer-valued (formatted without a decimal point).
    pub is_int: bool,
    /// The schema-declared default value (raw string), threaded through so the
    /// UI can clamp unparseable input to the field's *default* (not its min).
    /// Empty when the schema has no default for the field.
    pub default: String,
}

/// Look up the schema-defined numeric bounds for a settings key, if any.
/// Returns `None` for free-text fields (paths, URLs, keys, lists, …) that have
/// no `min`/`max` in `settings_schema.json`.
pub fn numeric_bounds(key: &str) -> Option<NumericBounds> {
    RUNTIME_SETTINGS
        .iter()
        .find(|s| s.key == key)
        .and_then(|s| match (s.min, s.max) {
            (Some(min), Some(max)) => {
                let step = s.step.unwrap_or(1.0);
                // Integer field when its step and default are both whole
                // numbers; a fractional step/default (0.1 s, 0.5 s) marks a
                // float so seconds/thresholds keep their decimals.
                let default_frac = s
                    .default
                    .as_deref()
                    .and_then(|d| d.trim().parse::<f64>().ok())
                    .map(|d| d.fract() != 0.0)
                    .unwrap_or(false);
                let is_int = step.fract() == 0.0 && !default_frac;
                Some(NumericBounds {
                    min,
                    max,
                    step,
                    is_int,
                    default: s.default.clone().unwrap_or_default(),
                })
            }
            _ => None,
        })
}

#[derive(Deserialize)]
struct SettingsSchema {
    settings: Vec<RuntimeSetting>,
}

// SINGLE SOURCE OF TRUTH for the VOICEPI_* env var <-> config key <-> default
// mapping, shared with the Python worker (vp_config.py reads the same file).
// Embedded at compile time so the controller has no runtime file dependency;
// add or change settings in settings_schema.json, not in a table here.
//
// NOTE: this `include_str!` path is relative to THIS file. From
// src/rust/config/schema.rs the repo root is three directories up.
pub(crate) static SETTINGS_SCHEMA_JSON: &str =
    include_str!("../../../shared/config/settings_schema.json");

pub(crate) static RUNTIME_SETTINGS: LazyLock<Vec<RuntimeSetting>> = LazyLock::new(|| {
    serde_json::from_str::<SettingsSchema>(SETTINGS_SCHEMA_JSON)
        .expect("settings_schema.json must be valid JSON")
        .settings
});

/// Resolve every schema setting against (in priority order) the on-disk config,
/// the process environment, then the schema default, yielding the `VOICEPI_*`
/// environment the worker should run with.
pub fn effective_runtime_env() -> BTreeMap<String, String> {
    effective_runtime_env_with(None)
}

/// Resolve the runtime environment using an explicit ambient snapshot.
///
/// The UI uses this while a native session is active so preflight can inspect
/// the caller's values without temporarily changing the process environment.
pub(crate) fn effective_runtime_env_from(
    ambient_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    effective_runtime_env_with(Some(ambient_env))
}

fn effective_runtime_env_with(
    ambient_env: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    let object = raw_config.as_object();
    let mut resolved: BTreeMap<String, String> = RUNTIME_SETTINGS
        .iter()
        .filter_map(|setting| {
            runtime_setting_value(setting, object, ambient_env)
                .map(|value| (setting.env.to_owned(), value))
        })
        .collect();
    normalize_groq_model(
        &mut resolved,
        "VOICEPI_POST_PROCESSOR",
        "VOICEPI_POST_MODEL",
    );
    resolved
}

/// Resolve a caller-selected config document without changing
/// `VOICEPI_CONFIG`; used by the native terminal runtime.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(crate) fn effective_runtime_env_from_raw(raw_config: &Value) -> BTreeMap<String, String> {
    let object = raw_config.as_object();
    let mut resolved: BTreeMap<String, String> = RUNTIME_SETTINGS
        .iter()
        .filter_map(|setting| {
            runtime_setting_value(setting, object, None)
                .map(|value| (setting.env.to_owned(), value))
        })
        .collect();
    normalize_groq_model(
        &mut resolved,
        "VOICEPI_POST_PROCESSOR",
        "VOICEPI_POST_MODEL",
    );
    resolved
}

/// Effective values keyed by config key rather than environment variable.
pub fn effective_runtime_config() -> BTreeMap<String, String> {
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    let object = raw_config.as_object();
    let mut resolved: BTreeMap<String, String> = RUNTIME_SETTINGS
        .iter()
        .filter_map(|setting| {
            runtime_setting_value(setting, object, None)
                .map(|value| (setting.key.to_owned(), value))
        })
        .collect();
    normalize_groq_model(&mut resolved, "post_processor", "post_model");
    resolved
}

/// Metadata rows for the native headless setup wizard and exporter.
pub fn runtime_settings() -> &'static [RuntimeSetting] {
    RUNTIME_SETTINGS.as_slice()
}

/// Same resolution as [`effective_runtime_env`], shaped as the `(key, value)`
/// overrides the process spawner expects.
pub fn worker_env_overrides() -> Vec<(String, String)> {
    effective_runtime_env().into_iter().collect()
}

pub(crate) fn worker_env_overrides_from_env(
    ambient_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    effective_runtime_env_from(ambient_env)
        .into_iter()
        .collect()
}

/// Resolve only schema settings marked `live`, keyed by their config key and
/// carrying both the process environment name and config/default value.
///
/// Unlike startup resolution, this deliberately does not consult the process
/// environment. The native runtime writes resolved settings into that
/// environment at startup and after every reload, so reading it here would
/// resurrect a value the user subsequently cleared from config. `None` is an
/// explicit instruction for the reload path to remove the environment value
/// and clear the session overlay.
#[allow(dead_code)]
pub(crate) fn effective_live_runtime_settings() -> BTreeMap<String, (String, Option<String>, bool)>
{
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    effective_live_runtime_settings_from_raw(&raw_config)
}

/// Resolve live settings from a caller-selected config document without
/// consulting or changing `VOICEPI_CONFIG`.
pub(crate) fn effective_live_runtime_settings_from_raw(
    raw_config: &Value,
) -> BTreeMap<String, (String, Option<String>, bool)> {
    let object = raw_config.as_object();
    RUNTIME_SETTINGS
        .iter()
        .filter(|setting| setting.live)
        .map(|setting| {
            let configured = object.is_some_and(|object| object.contains_key(&setting.key));
            let value = if configured {
                object
                    .and_then(|object| object.get(setting.key.as_str()))
                    .and_then(value_to_env_string)
            } else {
                setting.default.clone()
            };
            (
                setting.key.clone(),
                (setting.env.clone(), value, configured),
            )
        })
        .collect()
}

fn normalize_groq_model(
    resolved: &mut BTreeMap<String, String>,
    processor_key: &str,
    model_key: &str,
) {
    if !resolved
        .get(processor_key)
        .is_some_and(|processor| processor.eq_ignore_ascii_case("groq"))
    {
        return;
    }
    let Some(model) = resolved.get_mut(model_key) else {
        return;
    };
    if !groq_post_model_is_supported(model) {
        *model = DEFAULT_GROQ_POST_MODEL.to_owned();
    }
}

/// Capture caller-owned live environment overrides before the in-process
/// runtime materialises its resolved WorkerCommand into the process.
pub(crate) fn ambient_live_runtime_env() -> BTreeMap<String, String> {
    RUNTIME_SETTINGS
        .iter()
        .filter(|setting| setting.live)
        .filter_map(|setting| {
            env::var(&setting.env)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| (setting.env.clone(), value))
        })
        .collect()
}

fn runtime_setting_value(
    setting: &RuntimeSetting,
    object: Option<&Map<String, Value>>,
    ambient_env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    if setting.live && object.is_some_and(|object| object.contains_key(setting.key.as_str())) {
        return object
            .and_then(|object| object.get(setting.key.as_str()))
            .and_then(value_to_env_string);
    }
    object
        .and_then(|object| object.get(setting.key.as_str()))
        .and_then(value_to_env_string)
        .or_else(|| {
            ambient_env
                .map(|values| values.get(&setting.env).cloned())
                .unwrap_or_else(|| env::var(&setting.env).ok())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| setting.default.clone())
}

fn value_to_env_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) if value.is_empty() => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(true) => Some("True".to_owned()),
        Value::Bool(false) => Some("False".to_owned()),
        value => Some(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::io::CONFIG_ENV;
    use crate::config::test_support::{restore_env, ENV_LOCK};

    #[test]
    fn effective_live_runtime_settings_filters_out_restart_only_keys() {
        let live = effective_live_runtime_settings();
        assert!(live.contains_key("release_tail_ms"));
        assert!(live.contains_key("inject_mode"));
        assert!(!live.contains_key("model"));
        assert!(!live.contains_key("stt_backend"));
    }

    #[test]
    fn live_settings_do_not_resurrect_process_environment_after_config_clear() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"lang":"","initial_prompt":null}"#).unwrap();

        let old_config = env::var_os(CONFIG_ENV);
        let old_lang = env::var_os("VOICEPI_LANG");
        let old_prompt = env::var_os("VOICEPI_INITIAL_PROMPT");
        env::set_var(CONFIG_ENV, &path);
        env::set_var("VOICEPI_LANG", "stale-session-lang");
        env::set_var("VOICEPI_INITIAL_PROMPT", "stale session prompt");

        let startup = effective_runtime_env();
        let live = effective_live_runtime_settings();

        assert!(!startup.contains_key("VOICEPI_LANG"));
        assert!(!startup.contains_key("VOICEPI_INITIAL_PROMPT"));
        assert_eq!(live["lang"].1, None);
        assert!(live["lang"].2);
        assert_eq!(live["initial_prompt"].1, None);
        assert!(live["initial_prompt"].2);

        restore_env(CONFIG_ENV, old_config);
        restore_env("VOICEPI_LANG", old_lang);
        restore_env("VOICEPI_INITIAL_PROMPT", old_prompt);
    }

    #[test]
    fn explicit_null_language_also_clears_ambient_language() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"lang":null}"#).unwrap();

        let old_config = env::var_os(CONFIG_ENV);
        let old_lang = env::var_os("VOICEPI_LANG");
        env::set_var(CONFIG_ENV, &path);
        env::set_var("VOICEPI_LANG", "da");

        let live = effective_live_runtime_settings();
        assert_eq!(live["lang"].1, None);
        assert!(live["lang"].2);

        restore_env(CONFIG_ENV, old_config);
        restore_env("VOICEPI_LANG", old_lang);
    }

    #[test]
    fn effective_runtime_env_uses_config_then_env_then_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "lang": "da",
                "model": "large-v3",
                "log_level": "debug"
            })
            .to_string(),
        )
        .unwrap();

        let old_config = env::var_os(CONFIG_ENV);
        let old_model = env::var_os("VOICEPI_MODEL");
        let old_device = env::var_os("VOICEPI_DEVICE");
        let old_key = env::var_os("VOICEPI_KEY");
        let old_lang = env::var_os("VOICEPI_LANG");
        let old_log_level = env::var_os("VOICEPI_LOG");

        env::set_var(CONFIG_ENV, &path);
        env::set_var("VOICEPI_MODEL", "env-model");
        env::set_var("VOICEPI_DEVICE", "cuda");
        env::remove_var("VOICEPI_KEY");
        env::set_var("VOICEPI_LANG", "en");
        env::remove_var("VOICEPI_LOG");

        let env_values = effective_runtime_env();

        assert_eq!(env_values["VOICEPI_MODEL"], "large-v3");
        assert_eq!(env_values["VOICEPI_LANG"], "da");
        assert_eq!(env_values["VOICEPI_DEVICE"], "cuda");
        assert_eq!(env_values["VOICEPI_KEY"], "pause");
        assert_eq!(env_values["VOICEPI_LOG"], "debug");

        restore_env(CONFIG_ENV, old_config);
        restore_env("VOICEPI_MODEL", old_model);
        restore_env("VOICEPI_DEVICE", old_device);
        restore_env("VOICEPI_KEY", old_key);
        restore_env("VOICEPI_LANG", old_lang);
        restore_env("VOICEPI_LOG", old_log_level);
    }

    #[test]
    fn native_setup_metadata_carries_choices_and_descriptions() {
        let backend = runtime_settings()
            .iter()
            .find(|setting| setting.key == "stt_backend")
            .unwrap();
        assert_eq!(backend.choices, ["whisper", "openai"]);
        assert!(!backend.description.is_empty());
        assert!(!backend.advanced);
        assert_eq!(backend.category, "core");
    }

    #[test]
    fn numeric_bounds_are_self_consistent_and_contain_defaults() {
        // Every schema setting that declares min/max must: have min <= max, and
        // have its own default parse and fall within [min, max]. This keeps the
        // schema (the single source of truth) from shipping a default the UI
        // would immediately clamp away.
        for setting in RUNTIME_SETTINGS.iter() {
            let (Some(min), Some(max)) = (setting.min, setting.max) else {
                continue;
            };
            assert!(
                min <= max,
                "setting '{}' has min {min} > max {max}",
                setting.key
            );
            let default = setting
                .default
                .as_deref()
                .expect("numeric setting must have a default")
                .trim()
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("setting '{}' default not numeric", setting.key));
            assert!(
                default >= min && default <= max,
                "setting '{}' default {default} outside [{min}, {max}]",
                setting.key
            );
        }
    }

    #[test]
    fn numeric_bounds_lookup_and_int_detection() {
        let mcps = numeric_bounds("max_chars_per_second").expect("max_chars_per_second has bounds");
        assert_eq!(mcps.default, "30", "default differs from min (0)");

        // min_record_seconds: whole bounds but fractional default/step -> float.
        let mrs = numeric_bounds("min_record_seconds").expect("min_record_seconds has bounds");
        assert!(!mrs.is_int, "min_record_seconds should be float");

        // A free-text field has no bounds.
        assert!(numeric_bounds("initial_prompt").is_none());
        assert!(numeric_bounds("model").is_none());
    }

    #[test]
    fn runtime_settings_load_from_embedded_schema() {
        // settings_schema.json is the single source of truth; confirm it parsed
        // and a representative entry survived the env/key/default round-trip.
        assert!(!RUNTIME_SETTINGS.is_empty());
        let model = RUNTIME_SETTINGS
            .iter()
            .find(|s| s.key == "model")
            .expect("model setting present in schema");
        assert_eq!(model.env, "VOICEPI_MODEL");
        assert_eq!(model.default.as_deref(), Some("large-v3-turbo"));
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod public_api_tests;
