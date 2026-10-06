//! CLI adapter for `wd config get / set / list`.
//!
//! Wraps the existing typed-settings load/save library so a scripted caller
//! can inspect and mutate a single key without hand-editing config.json. The
//! set path re-uses [`AppSettings::validate`] as the single source of truth for what counts
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

use crate::config::keys::SETTINGS_KEYS;
use crate::config::load_settings_from_path;
use crate::config::runtime_settings;
use crate::config::settings::AppSettings;
use crate::whisper::device_options::{
    available_device_values, canonicalize_device_value_for_provider,
    is_device_supported_for_provider, missing_device_hint,
};

/// Settings key whose set-path needs the device-aware pre-validation and
/// canonicalisation (trim + lower-case). Named to make the wiring in
/// [`set_value`] self-documenting.
const DEVICE_KEY: &str = "device";

/// Settings key whose set-path needs strict pre-validation, for the same
/// reason `device` does but via a different mechanism: `ui_settings_mode` is
/// not a `settings_schema.json` field (it is the desktop UI's Simple/Advanced
/// switch — see `AppSettings::apply_ui`), so it has no `choices`-based
/// rejection in [`AppSettings::validate`]'s schema-driven path. It DOES have
/// an explicit `validate_choice` call — but `apply_ui` (invoked earlier, by
/// [`AppSettings::from_value`]) now tolerantly NORMALIZES any unrecognized
/// raw value to `"advanced"` so a hand-edited/garbage config.json still loads
/// cleanly. That tolerance is right for an ordinary LOAD, but wrong
/// for an EXPLICIT `wd config set ui_settings_mode <value>` request: by the
/// time `from_value` reaches `validate()`, the bad input has already been
/// silently rewritten to a valid one, so `set device`-style rejection never
/// fires and the CLI exits 0 having written the wrong value. Pre-validate the
/// caller's literal `value` here, before it ever reaches the tolerant load
/// path.
const UI_SETTINGS_MODE_KEY: &str = "ui_settings_mode";
const UI_SETTINGS_MODE_CHOICES: &[&str] = &["simple", "advanced"];

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
/// through the same validation and normalization as the settings UI. Returns
/// the resolved on-disk path (mirrors [`crate::config::save_settings_to_path`]).
///
/// The value is written into the raw JSON as a plain string; the typed
/// [`AppSettings::from_value`] loader then normalises booleans (accepts
/// `1`/`0`/`true`/`false`/…) and strings (empty means "clear the key,
/// fall back to schema default"). Validation runs BEFORE the file is
/// touched, so a rejected value leaves the previous config intact.
///
/// The `device` key gets an extra pre-validation step for local runtimes: values are
/// canonicalised (trim + lower-case ASCII), with legacy `"cuda"` migrated to
/// `"vulkan"` so the saved value names the backend the native runtime uses
/// and unsupported device values are refused up front with the
/// [`missing_device_hint`] explanation instead of being silently coerced
/// by a load-time migration. Remote cloud runtimes retain the canonical value
/// but skip host capability checks because they do not consume this setting.
/// An empty device value falls through unchanged so `set device ""` still
/// clears the key back to the schema default (matches every other key).
pub fn set_value(key: &str, value: &str, path: &Path) -> Result<PathBuf> {
    require_valid_key(key)?;
    if !value.trim().is_empty() {
        super::numeric::validate_numeric(key, value)?;
    }
    // Resolve the existing typed snapshot before canonicalising `device` so
    // the Nemotron provider can retain its distinct CUDA runtime selector.
    // This also infers Nemotron for older configs that only persisted its
    // in-process endpoint or model id.
    let mut object = match load_raw_config_object(path)? {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    let existing = AppSettings::from_value(Value::Object(object.clone()))?;
    let provider_for_device = if existing.stt_backend.eq_ignore_ascii_case("openai") {
        existing.stt_provider.as_str()
    } else {
        ""
    };
    let write_value = if key == DEVICE_KEY {
        normalise_device_for_set(
            value,
            provider_for_device,
            device_uses_local_runtime(&existing),
        )?
    } else if key == UI_SETTINGS_MODE_KEY {
        validate_ui_settings_mode_for_set(value)?
    } else {
        value.to_owned()
    };
    // Merge into the existing file (preserving unknown keys) instead of
    // rebuilding from AppSettings — this matches the UI's save contract.
    let explicit_null = write_value.trim().is_empty()
        && runtime_settings()
            .iter()
            .any(|setting| setting.key == key && setting.nullable);
    object.insert(key.to_owned(), Value::String(write_value));
    let settings = AppSettings::from_value(Value::Object(object.clone()))?;
    settings.validate()?;
    let explicit_nulls = if explicit_null { &[key][..] } else { &[] };
    // Use the typed serializer only to normalize the requested key. Persisting
    // the whole snapshot would materialize unrelated defaults and suppress
    // environment fallbacks (including the local-only privacy lock).
    let mut normalized = Map::new();
    settings.apply_to_object_with_explicit_nulls(&mut normalized, explicit_nulls);
    if let Some(value) = normalized.remove(key) {
        object.insert(key.to_owned(), value);
    } else {
        object.remove(key);
    }
    save_raw_config_object(object, path)
}

/// Write `key = value` as a raw string into the JSON object at `path`,
/// preserving every other key exactly as it already is on disk — no schema
/// defaults materialized for absent known settings and no revalidation or
/// normalization. Used by the UI's validated Simple/Advanced toggle; CLI
/// callers should use [`set_value`] to validate and normalize their input.
pub fn set_raw_string_key(key: &str, value: &str, path: &Path) -> Result<PathBuf> {
    // Read-modify-write under CONFIG_WRITE_LOCK so a concurrent Settings
    // save cannot overwrite this key with its own older snapshot (and so
    // this focused update cannot clobber a save that just landed).
    let _guard = super::io::config_write_guard();
    set_raw_string_key_under_lock(key, value, path)
}

/// Core of [`set_raw_string_key`] for callers that already hold
/// [`super::io::CONFIG_WRITE_LOCK`] and must keep it across a wider
/// resolve-then-persist sequence (the mode-shortcut worker resolves the
/// current mode and then writes under one critical section).
pub(crate) fn set_raw_string_key_under_lock(
    key: &str,
    value: &str,
    path: &Path,
) -> Result<PathBuf> {
    let mut object = match load_raw_config_object(path)? {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert(key.to_owned(), Value::String(value.to_owned()));
    save_raw_config_object(object, path)
}

fn save_raw_config_object(object: Map<String, Value>, path: &Path) -> Result<PathBuf> {
    path.parent().map(fs::create_dir_all).transpose()?;
    crate::atomic_file::write(
        path,
        (serde_json::to_string_pretty(&Value::Object(object))? + "\n").as_bytes(),
    )?;
    Ok(path.to_path_buf())
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
fn device_uses_local_runtime(settings: &AppSettings) -> bool {
    !settings.stt_backend.eq_ignore_ascii_case("openai")
        || (settings.stt_provider.eq_ignore_ascii_case("nemotron")
            && crate::cloud_api::is_nemotron_in_process_endpoint(&settings.stt_base_url))
}

fn normalise_device_for_set(
    value: &str,
    provider: &str,
    enforce_host_capability: bool,
) -> Result<String> {
    let canonical = canonicalize_device_value_for_provider(value, provider);
    if enforce_host_capability && !is_device_supported_for_provider(&canonical, provider) {
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

/// Reject an explicit `wd config set ui_settings_mode <value>` request that
/// isn't `"simple"`, `"advanced"`, or empty (empty means "clear back to the
/// default", the same convention every other non-nullable key follows — it
/// normalizes to `"advanced"` on the very next load, same as a missing key).
/// See [`UI_SETTINGS_MODE_KEY`]'s doc comment for why this can't simply defer
/// to `AppSettings::validate`.
///
/// Returns the CANONICAL (trimmed) value to store, not the caller's literal
/// `value` — `wd config set ui_settings_mode " simple "` passed validation
/// (it validates `trimmed`) but a previous version of this function stored
/// the untrimmed original, so the padded string then failed `apply_ui`'s
/// exact `"simple"`/anything-else match on the NEXT load and silently
/// normalized to `"advanced"` — the command exited 0 while landing on the
/// opposite mode from the one requested.
fn validate_ui_settings_mode_for_set(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || UI_SETTINGS_MODE_CHOICES.contains(&trimmed) {
        Ok(trimmed.to_owned())
    } else {
        Err(anyhow!(
            "invalid ui_settings_mode value {value:?}: must be one of {}",
            UI_SETTINGS_MODE_CHOICES.join(", ")
        ))
    }
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
/// shell scripts can `X=$(wd config get X)` without parsing.
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
    object
        .remove(key)
        .filter(|value| !value.is_null())
        .unwrap_or(Value::String(String::new()))
}

#[cfg(test)]
#[path = "cli_ops_tests.rs"]
mod reset_tests;

#[cfg(test)]
#[path = "cli_ops_verbs_tests.rs"]
mod verb_tests;
