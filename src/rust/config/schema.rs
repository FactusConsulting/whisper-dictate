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

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuntimeSetting {
    pub(crate) env: String,
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) default: Option<String>,
    /// Optional inclusive lower bound for numeric fields. The UI clamps user
    /// input to `[min, max]`; absent for free-text settings.
    #[serde(default)]
    pub(crate) min: Option<f64>,
    /// Optional inclusive upper bound for numeric fields (see [`Self::min`]).
    #[serde(default)]
    pub(crate) max: Option<f64>,
    /// Optional UI step granularity. Also used to infer integer-vs-float: a
    /// whole-number step (and whole default) means the field is an integer.
    #[serde(default)]
    pub(crate) step: Option<f64>,
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
// src/rust/config/schema.rs the repo's `src/` is two directories up, so the
// path is `../../python/whisper_dictate/settings_schema.json`.
pub(crate) static SETTINGS_SCHEMA_JSON: &str =
    include_str!("../../python/whisper_dictate/settings_schema.json");

pub(crate) static RUNTIME_SETTINGS: LazyLock<Vec<RuntimeSetting>> = LazyLock::new(|| {
    serde_json::from_str::<SettingsSchema>(SETTINGS_SCHEMA_JSON)
        .expect("settings_schema.json must be valid JSON")
        .settings
});

/// Resolve every schema setting against (in priority order) the on-disk config,
/// the process environment, then the schema default, yielding the `VOICEPI_*`
/// environment the worker should run with.
pub fn effective_runtime_env() -> BTreeMap<String, String> {
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    let object = raw_config.as_object();
    RUNTIME_SETTINGS
        .iter()
        .filter_map(|setting| {
            runtime_setting_value(setting, object).map(|value| (setting.env.to_owned(), value))
        })
        .collect()
}

/// Same resolution as [`effective_runtime_env`], shaped as the `(key, value)`
/// overrides the process spawner expects.
pub fn worker_env_overrides() -> Vec<(String, String)> {
    effective_runtime_env().into_iter().collect()
}

fn runtime_setting_value(
    setting: &RuntimeSetting,
    object: Option<&Map<String, Value>>,
) -> Option<String> {
    object
        .and_then(|object| object.get(setting.key.as_str()))
        .and_then(value_to_env_string)
        .or_else(|| {
            env::var(&setting.env)
                .ok()
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
    use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

    #[test]
    fn effective_runtime_env_uses_config_then_env_then_defaults() {
        // Each mutation is wrapped in an RAII `EnvVarGuard` so the original
        // value is restored on Drop even when an assertion below panics —
        // the old tail-of-test `restore_env` calls would never run on
        // panic, leaking six env vars into the next test (Codex P2 #415).
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "lang": "da",
                "model": "large-v3",
                "debug": true
            })
            .to_string(),
        )
        .unwrap();

        let _g_config = EnvVarGuard::set(CONFIG_ENV, &path);
        let _g_model = EnvVarGuard::set("VOICEPI_MODEL", "env-model");
        let _g_device = EnvVarGuard::set("VOICEPI_DEVICE", "cuda");
        let _g_key = EnvVarGuard::remove("VOICEPI_KEY");
        let _g_lang = EnvVarGuard::set("VOICEPI_LANG", "en");
        let _g_debug = EnvVarGuard::remove("VOICEPI_DEBUG");

        let env_values = effective_runtime_env();

        assert_eq!(env_values["VOICEPI_MODEL"], "large-v3");
        assert_eq!(env_values["VOICEPI_LANG"], "da");
        assert_eq!(env_values["VOICEPI_DEVICE"], "cuda");
        assert_eq!(env_values["VOICEPI_KEY"], "ctrl_r");
        assert_eq!(env_values["VOICEPI_CONTEXT_MIN_SECONDS"], "5");
        assert_eq!(env_values["VOICEPI_DEBUG"], "True");
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
        // beam_size: integer field, 1..=10.
        let beam = numeric_bounds("beam_size").expect("beam_size has bounds");
        assert_eq!(beam.min, 1.0);
        assert_eq!(beam.max, 10.0);
        assert!(beam.is_int, "beam_size should be integer");
        // The schema default is threaded through so the UI clamps garbage to the
        // field's default (not its min); see clamp_on_commit / FINDING 1.
        assert_eq!(beam.default, "1");
        let mcps = numeric_bounds("max_chars_per_second").expect("max_chars_per_second has bounds");
        assert_eq!(mcps.default, "30", "default differs from min (0)");

        // min_record_seconds: whole bounds but fractional default/step -> float.
        let mrs = numeric_bounds("min_record_seconds").expect("min_record_seconds has bounds");
        assert!(!mrs.is_int, "min_record_seconds should be float");

        // vad_threshold: fractional bounds -> float.
        let vad = numeric_bounds("vad_threshold").expect("vad_threshold has bounds");
        assert!(!vad.is_int, "vad_threshold should be float");

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
