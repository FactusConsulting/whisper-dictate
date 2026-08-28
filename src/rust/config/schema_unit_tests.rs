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

#[test]
fn device_schema_advertises_nemotron_cuda_selector() {
    let device = RUNTIME_SETTINGS
        .iter()
        .find(|setting| setting.key == "device")
        .expect("device setting present in schema");
    assert_eq!(device.choices, ["auto", "vulkan", "cuda", "cpu"]);
}
