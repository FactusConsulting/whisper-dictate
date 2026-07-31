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
use crate::whisper::device_options::{
    available_device_values, canonicalize_device_value, is_device_supported, missing_device_hint,
};

/// Settings key whose set-path needs the device-aware pre-validation and
/// canonicalisation (trim + lower-case). Named to make the wiring in
/// [`set_value`] self-documenting.
const DEVICE_KEY: &str = "device";

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
///
/// The `device` key gets an extra pre-validation step: values are
/// canonicalised (trim + lower-case ASCII) so `"  CUDA  "` persists as
/// `"cuda"` — the Python fallback's `vp_cli._resolve_device` lower-cases
/// but does not trim, so an untrimmed value would fail on next startup —
/// and unsupported device values are refused up front with the
/// [`missing_device_hint`] explanation instead of being silently coerced
/// by a load-time migration (see #648 Codex thread P1 on `load.rs:37`).
/// An empty device value falls through unchanged so `set device ""` still
/// clears the key back to the schema default (matches every other key).
pub fn set_value(key: &str, value: &str, path: &Path) -> Result<PathBuf> {
    require_valid_key(key)?;
    let write_value = if key == DEVICE_KEY {
        normalise_device_for_set(value)?
    } else {
        value.to_owned()
    };
    // Merge into the existing file (preserving unknown keys) instead of
    // rebuilding from AppSettings — this matches the UI's save contract.
    let mut object = match load_raw_config_object(path)? {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert(key.to_owned(), Value::String(write_value));
    let settings = AppSettings::from_value(Value::Object(object))?;
    save_settings_to_path(&settings, path)
}

/// Canonicalise a `device` value about to be written by [`set_value`] and
/// refuse anything the current build cannot honour, before the file is
/// touched. Returns the string to persist (canonical form) or an error
/// with the [`missing_device_hint`] explanation appended when helpful.
///
/// An empty / whitespace-only input is rejected up front — `device` is a
/// validated enum with no legal empty form, so falling through to
/// [`AppSettings::validate`] would produce a less friendly error and,
/// more importantly, leaves the JSON insert done before validation. By
/// refusing here we keep the on-disk file byte-identical on failure.
fn normalise_device_for_set(value: &str) -> Result<String> {
    let canonical = canonicalize_device_value(value);
    if !is_device_supported(&canonical) {
        let allowed = available_device_values().join(", ");
        let extra = missing_device_hint(&canonical)
            .map(|hint| format!("\n{hint}"))
            .unwrap_or_default();
        return Err(anyhow!(
            "invalid device value {value:?}: not supported on this build\n\
             valid values: {allowed}{extra}",
        ));
    }
    Ok(canonical)
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
        // Numeric runtime controls still reject non-numeric input.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let err = set_value("max_chars_per_second", "fast", &path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("max_chars_per_second"), "err = {err}");
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

    // -- device pre-validation + canonicalisation (Codex #648 P1/P2) ---

    #[test]
    fn set_device_canonicalises_whitespace_and_case_before_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("device", "  CPU  ", &path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let object: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            object["device"], "cpu",
            "device must be persisted in canonical form, got: {raw}",
        );
    }

    #[cfg(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda"))]
    #[test]
    fn set_device_accepts_cuda_on_gpu_builds() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("device", "cuda", &path).unwrap();
        assert_eq!(
            get_value("device", &path).unwrap(),
            Value::String("cuda".to_owned()),
        );
    }

    #[cfg(not(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda")))]
    #[test]
    fn set_device_rejects_cuda_on_cpu_only_builds() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let error = set_value("device", "cuda", &path).unwrap_err().to_string();
        assert!(error.contains("unavailable"));
        assert!(!error.contains("Python"));
        assert!(!path.exists());
    }

    #[test]
    fn set_device_rejects_unknown_value_before_touching_file() {
        // Codex P1 (#648 load.rs:37 thread): `cli_ops::set_value` used to
        // insert into JSON first and rely on the load-time migration to
        // silently rewrite unsupported values, so `set device <garbage>`
        // exited 0 and persisted the wrong thing. Pre-validation must
        // reject up front and leave the file byte-identical.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("device", "auto", &path).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let err = set_value("device", "invalid_gpu", &path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("device"), "err = {err}");
        assert!(
            err.contains("invalid_gpu"),
            "err should echo the rejected value, got: {err}",
        );

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(
            before, after,
            "file must not change on a rejected device value",
        );
    }

    #[test]
    fn set_device_error_lists_valid_values_for_this_build() {
        // Scripting affordance: the rejection message must name at least
        // one canonical value so a shell user can correct-spell without
        // grepping source. `auto` is always available.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        let err = set_value("device", "gpu", &path).unwrap_err().to_string();
        assert!(err.contains("auto"), "err should list `auto`, got: {err}");
    }

    #[test]
    fn set_device_empty_string_is_rejected_and_leaves_file_intact() {
        // Unlike free-form string keys (audio_device, metrics_jsonl, …),
        // `device` is a validated enum with no legal empty form — the
        // validator would refuse `""` on save regardless. Refusing it up
        // front in the setter keeps the on-disk file byte-identical, so
        // `config set device ""` cannot leave the config in a state that
        // then fails to load. Matches the pre-existing behaviour on other
        // validated enum keys like `ui_theme`.
        let dir = tempfile::tempdir().unwrap();
        let path = scratch(&dir);
        set_value("device", "auto", &path).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        assert!(set_value("device", "", &path).is_err());
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "empty device must not touch the file");
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
