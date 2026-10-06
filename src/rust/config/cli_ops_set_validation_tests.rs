//! `wd config set` validation-seam tests: the device pre-validation
//! (canonicalisation, CUDA migration/refusal, host-capability gates) and
//! the ui_settings_mode allow-list that the tolerant load path would
//! otherwise mask.

use super::*;

fn scratch(tempdir: &tempfile::TempDir) -> PathBuf {
    tempdir.path().join("config.json")
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

/// `set_value` must store the CANONICAL trimmed value for
/// `ui_settings_mode`: the file has to select the requested mode on the
/// next load, and an untrimmed stored literal would fail `apply_ui`'s
/// exact match and silently normalize to `"advanced"` while the `set`
/// command exits 0.
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
