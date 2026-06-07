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
    use crate::config::test_support::{restore_env, ENV_LOCK};

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
                "debug": true
            })
            .to_string(),
        )
        .unwrap();

        let old_config = env::var_os(CONFIG_ENV);
        let old_model = env::var_os("VOICEPI_MODEL");
        let old_device = env::var_os("VOICEPI_DEVICE");
        let old_key = env::var_os("VOICEPI_KEY");
        let old_lang = env::var_os("VOICEPI_LANG");
        let old_debug = env::var_os("VOICEPI_DEBUG");

        env::set_var(CONFIG_ENV, &path);
        env::set_var("VOICEPI_MODEL", "env-model");
        env::set_var("VOICEPI_DEVICE", "cuda");
        env::remove_var("VOICEPI_KEY");
        env::set_var("VOICEPI_LANG", "en");
        env::remove_var("VOICEPI_DEBUG");

        let env_values = effective_runtime_env();

        assert_eq!(env_values["VOICEPI_MODEL"], "large-v3");
        assert_eq!(env_values["VOICEPI_LANG"], "da");
        assert_eq!(env_values["VOICEPI_DEVICE"], "cuda");
        assert_eq!(env_values["VOICEPI_KEY"], "ctrl_r");
        assert_eq!(env_values["VOICEPI_CONTEXT_MIN_SECONDS"], "5");
        assert_eq!(env_values["VOICEPI_DEBUG"], "True");

        restore_env(CONFIG_ENV, old_config);
        restore_env("VOICEPI_MODEL", old_model);
        restore_env("VOICEPI_DEVICE", old_device);
        restore_env("VOICEPI_KEY", old_key);
        restore_env("VOICEPI_LANG", old_lang);
        restore_env("VOICEPI_DEBUG", old_debug);
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
