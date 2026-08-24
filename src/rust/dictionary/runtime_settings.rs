//! Effective dictionary settings and path resolution.
//!
//! This module owns the two resolution policies used by the dictionary
//! runtime: environment-first startup/RPC resolution and config-first live
//! reload resolution. Loading, caching, and request handling remain in
//! [`super::runtime`].

use std::path::PathBuf;

use serde_json::Value;

use crate::config;

use super::{env_bool, env_paths, env_usize};

/// Effective settings used by the `dictionary-runtime` handler. Env vars win
/// over `config.json`; missing values fall back to the defaults baked into the
/// Python side so the Python and Rust runtimes stay byte-identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDictionarySettings {
    pub enabled: bool,
    pub paths: Vec<PathBuf>,
    pub max_terms: usize,
    pub max_chars: usize,
}

impl RuntimeDictionarySettings {
    pub fn new(enabled: bool, paths: Vec<PathBuf>, max_terms: usize, max_chars: usize) -> Self {
        Self {
            enabled,
            paths,
            max_terms,
            max_chars,
        }
    }

    /// Build the dictionary view owned by an in-process runtime snapshot.
    /// No process environment or config file is consulted here.
    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    pub(crate) fn from_app_settings(settings: &config::AppSettings) -> Self {
        Self::new(
            settings.dictionary_enabled,
            config_dictionary_paths(settings),
            settings.dictionary_max_terms.parse().unwrap_or(80),
            settings.dictionary_prompt_chars.parse().unwrap_or(1200),
        )
    }

    pub(super) fn update_from_live_values(
        &mut self,
        values: &std::collections::BTreeMap<String, String>,
    ) {
        if let Some(value) = values.get("dictionary_enabled") {
            self.enabled = !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            );
        }
        if let Some(value) = values.get("dictionary") {
            self.paths = std::env::split_paths(value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect();
        }
        if let Some(value) = values.get("dictionary_max_terms") {
            if let Ok(parsed) = value.parse() {
                self.max_terms = parsed;
            }
        }
        if let Some(value) = values.get("dictionary_prompt_chars") {
            if let Ok(parsed) = value.parse() {
                self.max_chars = parsed;
            }
        }
    }

    /// Resolve the effective settings ENV-FIRST: the process env wins over
    /// `config.json`, then the baked defaults. Used by the `dictionary-runtime`
    /// RPC and the env-driven `simulate-session` verb, where the caller passes
    /// the resolved value in via env.
    pub(super) fn from_env_and_config() -> Self {
        let configured = config::load_settings().unwrap_or_default();
        Self::new(
            env_bool("VOICEPI_DICTIONARY_ENABLED").unwrap_or(configured.dictionary_enabled),
            env_paths("VOICEPI_DICTIONARY").unwrap_or_else(|| config_dictionary_paths(&configured)),
            env_usize("VOICEPI_DICTIONARY_MAX_TERMS")
                .or_else(|| configured.dictionary_max_terms.parse().ok())
                .unwrap_or(80),
            env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS")
                .or_else(|| configured.dictionary_prompt_chars.parse().ok())
                .unwrap_or(1200),
        )
    }

    /// Resolve the effective settings CONFIG-FIRST for the live-reload path:
    /// for each dictionary key actually PRESENT in the raw `config.json`, config
    /// wins; for a key the file omits, the process env is the fallback (then the
    /// default). This keeps a saved Settings value authoritative over the stale
    /// startup env (the worker exports these once) while still honouring an
    /// explicit env override for a key the config omits.
    ///
    /// Returns `None` when the config file EXISTS but cannot be read/parsed -- a
    /// transient failure (e.g. a non-atomic Settings save caught mid-rewrite),
    /// so the caller keeps its last-good state and retries -- versus a MISSING
    /// file, which `load_raw_config` reports as `{}` (all keys absent -> env
    /// fallback), not an error.
    pub(super) fn from_config_and_env() -> Option<Self> {
        let raw = config::load_raw_config().ok()?;
        let present = |key: &str| {
            raw.as_object()
                .map(|obj| obj.contains_key(key))
                .unwrap_or(false)
        };
        let (has_enabled, has_path, has_terms, has_chars) = (
            present("dictionary_enabled"),
            present("dictionary"),
            present("dictionary_max_terms"),
            present("dictionary_prompt_chars"),
        );
        // Read the enable flag straight off the RAW value (JSON bool OR string)
        // before the typed loader collapses it -- `AppSettings::from_value`'s
        // `bool_value` only parses string booleans, so a hand-written
        // `"dictionary_enabled": false` would otherwise fall back to the
        // default `true` and re-enable a disabled dictionary.
        let raw_enabled = raw_bool(&raw, "dictionary_enabled");
        let configured = config::AppSettings::from_value(raw).unwrap_or_default();

        let enabled = if has_enabled {
            raw_enabled.unwrap_or(configured.dictionary_enabled)
        } else {
            env_bool("VOICEPI_DICTIONARY_ENABLED").unwrap_or(configured.dictionary_enabled)
        };
        let paths = if has_path {
            config_dictionary_paths(&configured)
        } else {
            env_paths("VOICEPI_DICTIONARY").unwrap_or_else(|| config_dictionary_paths(&configured))
        };
        let max_terms = if has_terms {
            configured.dictionary_max_terms.parse().unwrap_or(80)
        } else {
            env_usize("VOICEPI_DICTIONARY_MAX_TERMS")
                .or_else(|| configured.dictionary_max_terms.parse().ok())
                .unwrap_or(80)
        };
        let max_chars = if has_chars {
            configured.dictionary_prompt_chars.parse().unwrap_or(1200)
        } else {
            env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS")
                .or_else(|| configured.dictionary_prompt_chars.parse().ok())
                .unwrap_or(1200)
        };
        Some(Self::new(enabled, paths, max_terms, max_chars))
    }
}

/// Resolve configured dictionary paths using the platform path separator.
/// Empty config values intentionally return no paths so callers can fall back
/// to an environment value or the normal default path.
pub(super) fn config_dictionary_paths(configured: &config::AppSettings) -> Vec<PathBuf> {
    let value = configured.dictionary.trim();
    if value.is_empty() {
        return Vec::new();
    }
    std::env::split_paths(value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

/// Read a `dictionary_enabled`-style flag from a raw config value, accepting a
/// JSON boolean, a JSON number (`0` = false), or a string (`"0"`/`"false"`/...
/// = false). `None` means the key is absent or has an unrecognised shape.
fn raw_bool(raw: &Value, key: &str) -> Option<bool> {
    match raw.get(key) {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(number)) => number.as_i64().map(|n| n != 0),
        Some(Value::String(text)) => {
            let value = text.trim().to_ascii_lowercase();
            if value.is_empty() {
                None
            } else {
                Some(!matches!(value.as_str(), "0" | "false" | "no" | "off"))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::config;

    use super::*;

    struct DictEnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl DictEnvGuard {
        fn new() -> Self {
            let keys = [
                "VOICEPI_DICTIONARY",
                "VOICEPI_DICTIONARY_ENABLED",
                "VOICEPI_DICTIONARY_MAX_TERMS",
                "VOICEPI_DICTIONARY_PROMPT_CHARS",
                "VOICEPI_CONFIG",
            ];
            let saved = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            Self { saved }
        }
    }

    impl Drop for DictEnvGuard {
        fn drop(&mut self) {
            for (key, prior) in &self.saved {
                match prior {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn write_dictionary_config(config_path: &Path, dict_path: &Path, enabled: bool) {
        let settings = config::AppSettings {
            dictionary: dict_path.display().to_string(),
            dictionary_enabled: enabled,
            ..config::AppSettings::default()
        };
        config::save_settings_to_path(&settings, config_path).expect("write temp config.json");
        std::env::set_var("VOICEPI_CONFIG", config_path);
    }

    #[test]
    fn reload_resolves_config_first_over_stale_env() {
        // P1 regression: the worker exports `VOICEPI_DICTIONARY_ENABLED` once at
        // startup and a Settings save only rewrites config.json (no restart), so
        // the live-reload must honour config over the stale startup env or a
        // disable/enable in Settings never takes effect. `from_config_and_env`
        // (used by the reload) returns the config value even when env disagrees;
        // `from_env_and_config` (the `dictionary-runtime` RPC path) keeps env
        // precedence.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        // config.json says DISABLED; a stale startup env says ENABLED.
        write_dictionary_config(&dir.path().join("config.json"), &dict, false);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");

        assert!(
            !RuntimeDictionarySettings::from_config_and_env()
                .expect("config readable")
                .enabled,
            "config-first reload must honour the saved (disabled) config over stale env"
        );
        assert!(
            RuntimeDictionarySettings::from_env_and_config().enabled,
            "the RPC path keeps env precedence"
        );
    }

    #[test]
    fn config_first_falls_back_to_env_for_absent_keys() {
        // A partial/legacy config.json that OMITS the dictionary keys must not
        // let the non-empty DEFAULT dictionary path shadow an env-supplied one:
        // absent keys fall back to the process env (then default).
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(&config, "{}").unwrap(); // no dictionary keys at all
        std::env::set_var("VOICEPI_CONFIG", &config);
        std::env::set_var("VOICEPI_DICTIONARY", &dict);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");

        let settings = RuntimeDictionarySettings::from_config_and_env().expect("config readable");
        assert_eq!(
            settings.paths,
            vec![dict],
            "an absent config path must fall back to the env path"
        );
        assert!(settings.enabled, "absent enabled must fall back to env");
    }

    #[test]
    fn config_first_returns_none_when_config_is_unreadable() {
        // A present-but-unparseable config.json (e.g. caught mid Settings save)
        // must resolve to None so the reload keeps its last-good table, rather
        // than `unwrap_or_default()` masking the failure as valid defaults.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(&config, "{ this is not json").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &config);

        assert!(
            RuntimeDictionarySettings::from_config_and_env().is_none(),
            "an unreadable config must yield None (keep last-good), not defaults"
        );
    }
    #[test]
    fn config_dictionary_paths_splits_multi_file_lists() {
        // A `dictionary` config value that is a platform path-separator list
        // (e.g. `a.json;b.json` on Windows) must split into one path per file,
        // matching `env_paths`, not wrap the whole list in one bogus PathBuf.
        let joined =
            std::env::join_paths([PathBuf::from("a.json"), PathBuf::from("b.json")]).unwrap();
        let configured = config::AppSettings {
            dictionary: joined.to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert_eq!(
            config_dictionary_paths(&configured),
            vec![PathBuf::from("a.json"), PathBuf::from("b.json")]
        );
    }

    #[test]
    fn config_first_honours_json_boolean_enabled() {
        // A hand-written config.json with a JSON boolean `false` (not the string
        // "0") must disable the dictionary -- the typed loader's `bool_value`
        // only parses strings, so config-first reads the raw value directly.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let config = dir.path().join("config.json");
        let raw = serde_json::json!({
            "dictionary": dict.display().to_string(),
            "dictionary_enabled": false,
        });
        std::fs::write(&config, serde_json::to_string(&raw).unwrap()).unwrap();
        std::env::set_var("VOICEPI_CONFIG", &config);

        assert!(
            !RuntimeDictionarySettings::from_config_and_env()
                .expect("config readable")
                .enabled,
            "a JSON boolean false must disable the dictionary"
        );
    }
}
