//! CLI-verb tests for `wd config get / set / list`: key allow-listing,
//! typed normalization, schema-validated persistence, and the device /
//! ui_settings_mode pre-validation seams.

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
    set_value("history_enabled", "true", &path).unwrap();
    assert_eq!(
        get_value("history_enabled", &path).unwrap(),
        Value::String("1".to_owned())
    );
    set_value("history_enabled", "0", &path).unwrap();
    assert_eq!(
        get_value("history_enabled", &path).unwrap(),
        Value::String("0".to_owned())
    );
}

#[test]
fn set_empty_string_clears_the_key() {
    // Nullable settings persist an explicit null so runtime environment
    // values cannot resurrect after `set audio_device ""`.
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    set_value("audio_device", "Yeti X", &path).unwrap();
    set_value("audio_device", "", &path).unwrap();
    let raw = fs::read_to_string(&path).unwrap();
    let object: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        object.get("audio_device"),
        Some(&Value::Null),
        "audio_device should be explicitly cleared, got: {raw}",
    );
    // But the CLI-visible view still returns "" — get/list must never
    // contradict the "valid but unset" reading.
    assert_eq!(
        get_value("audio_device", &path).unwrap(),
        Value::String(String::new())
    );
}

#[test]
fn set_empty_string_clears_an_absent_nullable_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);

    set_value("lang", "", &path).unwrap();

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw.get("lang"), Some(&Value::Null));
}

#[test]
fn set_empty_string_clears_the_defaulted_dictionary_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);

    set_value("dictionary", "", &path).unwrap();

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw.get("dictionary"), Some(&Value::Null));
    assert_eq!(
        get_value("dictionary", &path).unwrap(),
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

// -- device pre-validation + canonicalisation ----------------------

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

#[cfg(feature = "whisper-rs-vulkan")]
#[test]
fn set_device_migrates_legacy_cuda_to_vulkan_on_gpu_builds() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    set_value("device", "cuda", &path).unwrap();
    assert_eq!(
        get_value("device", &path).unwrap(),
        Value::String("vulkan".to_owned()),
    );
}

#[cfg(not(feature = "whisper-rs-vulkan"))]
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
    // `cli_ops::set_value` validates before writing instead of relying on
    // a load-time migration to
    // silently rewrite unsupported values, so `set device <garbage>`
    // exit 0 and persist the wrong thing. Pre-validation must
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

/// A load-time normalization added for `ui_settings_mode`
/// (`AppSettings::apply_ui` now tolerantly rewrites any unrecognized raw
/// value to `"advanced"` so a hand-edited config.json still loads
/// cleanly) had silently defeated `set_value`'s own validation, since
/// `from_value` normalizes BEFORE `validate()` ever sees the caller's
/// literal input — `wd config set ui_settings_mode bogus` exited 0 and
/// wrote `"advanced"` instead of being refused. Mirrors
/// `set_device_rejects_unknown_value_before_touching_file`.
#[test]
fn set_ui_settings_mode_rejects_unknown_value_before_touching_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    set_value("ui_settings_mode", "simple", &path).unwrap();
    let before = fs::read_to_string(&path).unwrap();

    let err = set_value("ui_settings_mode", "bogus", &path)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ui_settings_mode"), "err = {err}");
    assert!(
        err.contains("bogus"),
        "err should echo the rejected value, got: {err}",
    );

    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(
        before, after,
        "file must not change on a rejected ui_settings_mode value",
    );
}

#[test]
fn set_ui_settings_mode_accepts_both_valid_choices() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    set_value("ui_settings_mode", "simple", &path).unwrap();
    assert_eq!(
        load_settings_from_path(&path).unwrap().ui_settings_mode,
        "simple"
    );
    set_value("ui_settings_mode", "advanced", &path).unwrap();
    assert_eq!(
        load_settings_from_path(&path).unwrap().ui_settings_mode,
        "advanced"
    );
}

/// Codex: `validate_ui_settings_mode_for_set` validated the TRIMMED
/// value but a previous version stored the caller's literal (untrimmed)
/// string, so `" simple "` passed validation, was written to config.json
/// verbatim, and then failed `apply_ui`'s exact match on the very next
/// load — silently normalizing to `"advanced"`, the OPPOSITE of what was
/// requested, while the `set` command itself exited 0. The file must
/// contain the canonical trimmed value, and loading it must select the
/// requested mode.
#[test]
fn set_ui_settings_mode_trims_whitespace_before_storing() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);

    set_value("ui_settings_mode", " simple ", &path).unwrap();

    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("\"simple\"") && !raw.contains(" simple "),
        "config.json must store the canonical trimmed value, got: {raw}",
    );
    assert_eq!(
        load_settings_from_path(&path).unwrap().ui_settings_mode,
        "simple",
        "a whitespace-padded value must still select the requested mode",
    );
}

/// A hand-edited (or otherwise pre-existing) bogus value already sitting
/// in config.json must still load tolerantly as `"advanced"` — only an
/// EXPLICIT `set` request is strict. This is the load-time behaviour
/// `config::load`'s own tests cover directly; asserted here too so the
/// contrast with the rejection above is explicit in one place.
#[test]
fn hand_edited_bogus_ui_settings_mode_still_loads_as_advanced() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    fs::write(&path, r#"{"ui_settings_mode":"bogus"}"#).unwrap();

    let loaded = load_settings_from_path(&path).unwrap();

    assert_eq!(loaded.ui_settings_mode, "advanced");
}

/// [`set_raw_string_key`] must touch nothing but the requested
/// key -- a sparse config.json gains only `key`, every other key it
/// already had (known or unknown to this app) is untouched.
#[test]
fn set_raw_string_key_writes_only_the_requested_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    fs::write(&path, r#"{"lang":"da","totally_unknown_key":"kept"}"#).unwrap();

    set_raw_string_key("ui_settings_mode", "simple", &path).unwrap();

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let object = raw.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["lang", "totally_unknown_key", "ui_settings_mode"],
        "got keys: {keys:?}",
    );
    assert_eq!(object["lang"], Value::String("da".to_owned()));
    assert_eq!(
        object["totally_unknown_key"],
        Value::String("kept".to_owned())
    );
    assert_eq!(
        object["ui_settings_mode"],
        Value::String("simple".to_owned())
    );
}

#[test]
fn set_raw_string_key_creates_the_file_with_only_that_key_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    assert!(!path.exists());

    set_raw_string_key("ui_settings_mode", "advanced", &path).unwrap();

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let object = raw.as_object().unwrap();
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["ui_settings_mode"]);
}

#[test]
fn set_device_preserves_cuda_for_in_process_nemotron() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    set_value("stt_provider", "nemotron", &path).unwrap();
    set_value("stt_base_url", "inproc://nemotron", &path).unwrap();
    set_value("stt_model", "nvidia/nemotron-3.5-asr-streaming-0.6b", &path).unwrap();
    set_value("stt_backend", "openai", &path).unwrap();
    set_value("device", "cuda", &path).unwrap();

    assert_eq!(
        get_value("device", &path).unwrap(),
        Value::String("cuda".to_owned())
    );
}

#[test]
fn set_device_skips_host_capability_checks_for_remote_nemotron() {
    let dir = tempfile::tempdir().unwrap();
    let path = scratch(&dir);
    set_value("stt_provider", "nemotron", &path).unwrap();
    set_value("stt_base_url", "grpc://localhost:50051", &path).unwrap();
    set_value("stt_model", "nvidia/nemotron-3.5-asr-streaming-0.6b", &path).unwrap();
    set_value("stt_backend", "openai", &path).unwrap();

    let settings = load_settings_from_path(&path).unwrap();
    assert!(!device_uses_local_runtime(&settings));
    set_value("device", "cuda", &path).unwrap();

    assert_eq!(
        get_value("device", &path).unwrap(),
        Value::String("cuda".to_owned())
    );
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
