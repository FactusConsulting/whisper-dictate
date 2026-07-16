//! CLI adapter for `whisper-dictate config get / set / list`.
//!
//! Wraps the existing typed-settings load/save library so a scripted caller
//! can inspect and mutate a single key without hand-editing config.json. The
//! set path re-uses [`AppSettings::validate`] (invoked from
//! [`save_settings_to_path`]) as the single source of truth for what counts
//! as a legal value — invalid values fail *without* touching the file on
//! disk.
//!
//! Precedence for the config file location is decided in
//! [`super::handle_command`]: `--config PATH` > `VOICEPI_CONFIG` env var >
//! platform user config. These helpers only ever see the resolved absolute
//! path so their unit tests are fully hermetic.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use crate::config::io::save_settings_to_path;
use crate::config::keys::SETTINGS_KEYS;
use crate::config::load_settings_from_path;
use crate::config::settings::AppSettings;

/// Every settings key the CLI `get`/`set`/`list` verbs recognise, in the
/// stable declaration order from [`SETTINGS_KEYS`].
///
/// Exposed so callers (`--help` text, error messages) can render the full
/// allow-list without duplicating it.
pub fn valid_keys() -> &'static [&'static str] {
    SETTINGS_KEYS
}

/// Return the current value of `key` from the config file at `path`.
///
/// Behaviour:
/// - Unknown key → error listing every valid key (the caller exits 1).
/// - Missing file → treated as an empty config (defaults everywhere).
/// - Empty string values are returned as `Value::String("")` even though
///   [`AppSettings::apply_to_object`] strips them from the serialised map,
///   because the CLI contract is "print a value, not an error, for a valid
///   but unset key".
pub fn get_value(key: &str, path: &Path) -> Result<Value> {
    require_valid_key(key)?;
    let settings = load_settings_from_path(path)?;
    Ok(value_for_key(&settings, key))
}

/// Set `key = value` on the config file at `path`, validating and persisting
/// through the same code paths `AppSettings::save_settings` uses. Returns
/// the resolved on-disk path (mirrors [`save_settings_to_path`]).
///
/// The value is written into the raw JSON as a plain string; the typed
/// [`AppSettings::from_value`] loader then normalises booleans (accepts
/// `1`/`0`/`true`/`false`/…) and strings (empty means "clear the key,
/// fall back to schema default"). Validation runs BEFORE the file is
/// touched, so a rejected value leaves the previous config intact.
pub fn set_value(key: &str, value: &str, path: &Path) -> Result<PathBuf> {
    require_valid_key(key)?;
    // Merge into the existing file (preserving unknown keys) instead of
    // rebuilding from AppSettings — this matches the UI's save contract.
    let mut object = match load_raw_config_object(path)? {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert(key.to_owned(), Value::String(value.to_owned()));
    let settings = AppSettings::from_value(Value::Object(object))?;
    save_settings_to_path(&settings, path)
}

/// List every settings key with its current value, sorted by
/// [`SETTINGS_KEYS`] declaration order (stable + human-friendly).
/// Missing values appear as `Value::String("")` — same rule as
/// [`get_value`], so `list` and `get` never contradict each other.
pub fn list_values(path: &Path) -> Result<Vec<(String, Value)>> {
    let settings = load_settings_from_path(path)?;
    Ok(SETTINGS_KEYS
        .iter()
        .map(|key| ((*key).to_owned(), value_for_key(&settings, key)))
        .collect())
}

/// Render a single `get` result for stdout. `--json` produces a compact
/// one-line envelope `{"key": "...", "value": ...}` (arrays/objects survive
/// verbatim); the plain form prints just the value's string content so
/// shell scripts can `X=$(whisper-dictate config get X)` without parsing.
pub fn format_get_value(key: &str, value: &Value, json: bool) -> Result<String> {
    if json {
        let envelope = serde_json::json!({ "key": key, "value": value });
        Ok(serde_json::to_string(&envelope)?)
    } else {
        Ok(match value {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other)?,
        })
    }
}

/// Reject unknown keys with a message that lists every valid key, so the
/// CLI user does not have to re-read the schema to spell one correctly.
fn require_valid_key(key: &str) -> Result<()> {
    if SETTINGS_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(anyhow!(
            "unknown config key: {key:?}\nvalid keys: {}",
            SETTINGS_KEYS.join(", ")
        ))
    }
}

/// Read the file at `path` as raw JSON, treating "missing" and "empty" as
/// `{}`. Kept private because it's a `set`-only concern (get/list read via
/// the typed [`load_settings_from_path`]).
fn load_raw_config_object(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    Ok(serde_json::from_str(&raw)?)
}

/// Look up the stored value for `key` on a typed [`AppSettings`] snapshot.
///
/// [`AppSettings::apply_to_object`] strips empty strings from the serialised
/// map (so config.json never carries blank fields). The CLI has the opposite
/// need: `get`/`list` should still SHOW a valid-but-empty key as `""` rather
/// than error out or fall through to a schema default. This helper glues
/// those two contracts.
fn value_for_key(settings: &AppSettings, key: &str) -> Value {
    let mut object = Map::new();
    settings.apply_to_object(&mut object);
    object.remove(key).unwrap_or(Value::String(String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::io::CONFIG_ENV;
    use crate::config::test_support::ENV_LOCK;

    fn scratch(tempdir: &tempfile::TempDir) -> PathBuf {
        tempdir.path().join("config.json")
    }

    #[test]
    fn valid_keys_include_common_settings() {
        let keys = valid_keys();
        for expected in ["audio_device", "model", "stt_backend", "ui_theme"] {
            assert!(
                keys.contains(&expected),
                "valid_keys() missing {expected:?} (got {keys:?})",
            );
        }
    }

    #[test]
    fn get_unknown_key_errors_and_lists_valid_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let err = get_value("does-not-exist", &path).unwrap_err().to_string();
        assert!(err.contains("does-not-exist"), "err = {err}");
        assert!(err.contains("valid keys"), "err = {err}");
        // At least one representative real key should appear so the user has
        // something to correct-spell against.
        assert!(err.contains("audio_device"), "err = {err}");
    }

    #[test]
    fn get_missing_key_returns_empty_string_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        // audio_device is empty by default; get must still print "" instead
        // of erroring — that's the "valid but unset" contract.
        let value = get_value("audio_device", &path).unwrap();
        assert_eq!(value, Value::String(String::new()));
    }

    #[test]
    fn set_then_get_roundtrips_a_string_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("audio_device", "Yeti X", &path).unwrap();
        assert_eq!(
            get_value("audio_device", &path).unwrap(),
            Value::String("Yeti X".to_owned())
        );
    }

    #[test]
    fn set_then_get_roundtrips_a_bool_value_as_the_stored_form() {
        // Booleans are stored as "1" / "0" in the config file (worker
        // contract). `bool_value` in load.rs accepts "true" case-insensitively
        // so the user-facing CLI does too — the value survives normalised to
        // the canonical "1".
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("debug", "true", &path).unwrap();
        assert_eq!(
            get_value("debug", &path).unwrap(),
            Value::String("1".to_owned())
        );
        set_value("debug", "0", &path).unwrap();
        assert_eq!(
            get_value("debug", &path).unwrap(),
            Value::String("0".to_owned())
        );
    }

    #[test]
    fn set_empty_string_clears_the_key() {
        // The `set_string` writer removes an empty value from the JSON map
        // rather than persisting a blank field, so `set audio_device ""`
        // reverts a previously-set device to "use the system default".
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("audio_device", "Yeti X", &path).unwrap();
        set_value("audio_device", "", &path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let object: Value = serde_json::from_str(&raw).unwrap();
        assert!(
            object.get("audio_device").is_none(),
            "audio_device should be removed from the file, got: {raw}",
        );
        // But the CLI-visible view still returns "" — get/list must never
        // contradict the "valid but unset" reading.
        assert_eq!(
            get_value("audio_device", &path).unwrap(),
            Value::String(String::new())
        );
    }

    #[test]
    fn set_invalid_enum_value_errors_without_touching_the_file() {
        // ui_theme accepts "dark" | "light"; anything else must fail
        // validation. And the file must not be mutated on the failed save.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("ui_theme", "dark", &path).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let err = set_value("ui_theme", "solarized", &path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ui_theme"), "err = {err}");

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "file must not change on a rejected value");
    }

    #[test]
    fn set_invalid_numeric_value_errors_cleanly() {
        // beam_size must parse as u32 >= 1. "fast" trips `validate_numbers`.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let err = set_value("beam_size", "fast", &path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("beam_size"), "err = {err}");
    }

    #[test]
    fn set_preserves_unknown_keys_in_the_file() {
        // The UI-side save contract is "keep keys we don't own"; the CLI
        // adapter must honour it too, so a user's hand-added key survives a
        // `config set` on an unrelated field.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        fs::write(
            &path,
            r#"{"unknown_field":"keep me","audio_device":"old mic"}"#,
        )
        .unwrap();
        set_value("audio_device", "new mic", &path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let object: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(object["unknown_field"], "keep me");
        assert_eq!(object["audio_device"], "new mic");
    }

    #[test]
    fn list_values_returns_every_settings_key_in_declaration_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let entries = list_values(&path).unwrap();
        assert_eq!(entries.len(), SETTINGS_KEYS.len());
        let listed: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        let expected: Vec<&str> = SETTINGS_KEYS.to_vec();
        assert_eq!(listed, expected);
    }

    #[test]
    fn format_get_value_plain_prints_only_the_string() {
        let value = Value::String("Yeti X".to_owned());
        assert_eq!(
            format_get_value("audio_device", &value, false).unwrap(),
            "Yeti X"
        );
    }

    #[test]
    fn format_get_value_json_wraps_in_envelope() {
        let value = Value::String("large-v3-turbo".to_owned());
        let rendered = format_get_value("model", &value, true).unwrap();
        // Parse it back so we're not asserting on a specific whitespace layout.
        let parsed: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["key"], "model");
        assert_eq!(parsed["value"], "large-v3-turbo");
    }

    #[test]
    fn dispatch_get_and_set_via_handle_command_roundtrips() {
        // End-to-end coverage of the public dispatch: the CLI verb path (with
        // an explicit --config override translated to `Some(path)`) is what
        // the smoke script exercises, so lock it in with a test too.
        //
        // Uses ENV_LOCK because CONFIG_ENV is process-global — even though
        // this test only passes a `--config` override, other tests in the
        // suite mutate CONFIG_ENV and we must not race them.
        use crate::cli::ConfigCommand;
        use crate::config::handle_command;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(CONFIG_ENV);
        std::env::remove_var(CONFIG_ENV);

        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let path_str = path.to_string_lossy().into_owned();

        handle_command(ConfigCommand::Set {
            key: "audio_device".to_owned(),
            value: "USB mic".to_owned(),
            config: Some(path_str.clone()),
        })
        .unwrap();

        handle_command(ConfigCommand::Get {
            key: "audio_device".to_owned(),
            json: true,
            config: Some(path_str),
        })
        .unwrap();

        // The stored value is what matters — stdout capture would need a
        // print interceptor; we already prove format_get_value above.
        assert_eq!(
            get_value("audio_device", &path).unwrap(),
            Value::String("USB mic".to_owned()),
        );

        crate::config::test_support::restore_env(CONFIG_ENV, prev);
    }
}
