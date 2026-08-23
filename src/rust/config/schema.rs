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
use crate::config::settings::normalize_groq_post_model;

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
    /// Whether config.json may persist JSON null as an explicit instruction
    /// to clear this optional value and suppress ambient environment fallback.
    #[serde(default)]
    pub nullable: bool,
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

fn effective_runtime_env_with(
    ambient_env: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    effective_runtime_env_from_value(&raw_config, ambient_env)
}

fn effective_runtime_env_from_value(
    raw_config: &Value,
    ambient_env: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
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
    worker_env_overrides_with(None)
}

pub(crate) fn worker_env_overrides_from_env(
    ambient_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    worker_env_overrides_with(Some(ambient_env))
}

fn worker_env_overrides_with(
    ambient_env: Option<&BTreeMap<String, String>>,
) -> Vec<(String, String)> {
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    let mut resolved = effective_runtime_env_from_value(&raw_config, ambient_env);
    if let Some(object) = raw_config.as_object() {
        for setting in RUNTIME_SETTINGS.iter().filter(|setting| setting.nullable) {
            if object.contains_key(&setting.key)
                && object
                    .get(&setting.key)
                    .and_then(value_to_env_string)
                    .is_none()
            {
                // Empty is a process-boundary suppression marker: consumers
                // must not fall back to an inherited ambient value.
                resolved.insert(setting.env.clone(), String::new());
            }
        }
    }
    resolved.into_iter().collect()
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
            let configured_value = object
                .and_then(|object| object.get(setting.key.as_str()))
                .and_then(value_to_env_string);
            let configured = object.is_some_and(|object| object.contains_key(&setting.key))
                && (configured_value.is_some() || setting.nullable);
            let value = if configured {
                configured_value
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
    let Some(processor) = resolved.get(processor_key).cloned() else {
        return;
    };
    let Some(model) = resolved.get_mut(model_key) else {
        return;
    };
    normalize_groq_post_model(&processor, model);
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
    if object.is_some_and(|object| object.contains_key(setting.key.as_str())) {
        let configured = object
            .and_then(|object| object.get(setting.key.as_str()))
            .and_then(value_to_env_string);
        if configured.is_some() || setting.nullable {
            return configured;
        }
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
#[path = "schema_tests.rs"]
mod public_api_tests;
#[cfg(test)]
#[path = "schema_unit_tests.rs"]
mod tests;
