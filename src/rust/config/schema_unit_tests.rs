use super::*;
use crate::config::io::CONFIG_ENV;
use crate::config::test_support::{restore_env, ENV_LOCK};
use crate::config::{restart_required_keys, AppSettings};

#[test]
fn copy_last_hotkey_is_opt_in_and_restart_bound() {
    let defaults = AppSettings::default();
    assert!(defaults.copy_last_hotkey.is_empty());
    let raw = serde_json::json!({"copy_last_hotkey":"ctrl+shift+f8"});
    let settings = AppSettings::from_value(raw.clone()).unwrap();
    assert_eq!(settings.copy_last_hotkey, "ctrl+shift+f8");
    let env = effective_runtime_env_from_value(&raw, None);
    assert_eq!(env["VOICEPI_COPY_LAST_HOTKEY"], "ctrl+shift+f8");
    assert!(restart_required_keys(&defaults, &settings).contains(&"copy_last_hotkey"));
}

#[test]
fn paste_last_hotkey_is_opt_in_and_restart_bound() {
    let defaults = AppSettings::default();
    assert!(defaults.paste_last_hotkey.is_empty());
    let raw = serde_json::json!({"paste_last_hotkey":"ctrl+shift+f9"});
    let settings = AppSettings::from_value(raw.clone()).unwrap();
    assert_eq!(settings.paste_last_hotkey, "ctrl+shift+f9");
    let env = effective_runtime_env_from_value(&raw, None);
    assert_eq!(env["VOICEPI_PASTE_LAST_HOTKEY"], "ctrl+shift+f9");
    assert!(restart_required_keys(&defaults, &settings).contains(&"paste_last_hotkey"));
}

#[test]
fn mode_shortcut_hotkeys_are_opt_in_and_restart_bound() {
    let defaults = AppSettings::default();
    assert!(defaults.cycle_mode_hotkey.is_empty());
    assert!(defaults.raw_mode_hotkey.is_empty());
    assert!(defaults.clean_mode_hotkey.is_empty());
    let raw = serde_json::json!({
        "cycle_mode_hotkey": "ctrl+shift+f10",
        "raw_mode_hotkey": "ctrl+shift+f11",
        "clean_mode_hotkey": "ctrl+shift+f6"
    });
    let settings = AppSettings::from_value(raw.clone()).unwrap();
    assert_eq!(settings.cycle_mode_hotkey, "ctrl+shift+f10");
    assert_eq!(settings.raw_mode_hotkey, "ctrl+shift+f11");
    assert_eq!(settings.clean_mode_hotkey, "ctrl+shift+f6");
    let env = effective_runtime_env_from_value(&raw, None);
    assert_eq!(env["VOICEPI_CYCLE_MODE_HOTKEY"], "ctrl+shift+f10");
    assert_eq!(env["VOICEPI_RAW_MODE_HOTKEY"], "ctrl+shift+f11");
    assert_eq!(env["VOICEPI_CLEAN_MODE_HOTKEY"], "ctrl+shift+f6");
    assert!(restart_required_keys(&defaults, &settings).contains(&"cycle_mode_hotkey"));
    assert!(restart_required_keys(&defaults, &settings).contains(&"raw_mode_hotkey"));
    assert!(restart_required_keys(&defaults, &settings).contains(&"clean_mode_hotkey"));
}

#[test]
fn paste_last_hotkey_description_documents_print_and_self_trigger_limits() {
    let setting = RUNTIME_SETTINGS
        .iter()
        .find(|setting| setting.key == "paste_last_hotkey")
        .expect("paste_last_hotkey is a runtime setting");
    // The advertised behaviour must match what the worker enforces: the
    // print-mode rejection (global or per-row profile) and the ctrl+v
    // self-trigger guard (Codex P2 docs/CONFIGURATION.md:120).
    assert!(setting.description.contains("print"));
    assert!(setting.description.contains("copy-last"));
    assert!(setting.description.contains("ctrl+v"));
}

#[test]
fn feedback_event_settings_flow_to_the_worker_independently() {
    let raw = serde_json::json!({
        "feedback_sounds": "1",
        "feedback_start": "0",
        "feedback_stop": "1",
        "feedback_done": "1"
    });
    let settings = crate::config::AppSettings::from_value(raw.clone()).unwrap();
    assert!(settings.feedback_sounds);
    assert!(!settings.feedback_start);
    assert!(settings.feedback_stop);
    assert!(settings.feedback_done);
    let env = effective_runtime_env_from_value(&raw, None);
    assert_eq!(env["VOICEPI_FEEDBACK_SOUNDS"], "1");
    assert_eq!(env["VOICEPI_FEEDBACK_START"], "0");
    assert_eq!(env["VOICEPI_FEEDBACK_STOP"], "1");
    assert_eq!(env["VOICEPI_FEEDBACK_DONE"], "1");
}

#[test]
fn legacy_feedback_config_keeps_start_and_stop_enabled_on_live_reload() {
    let raw = serde_json::json!({"feedback_sounds": true});
    let runtime = effective_runtime_env_from_value(&raw, None);
    assert_eq!(runtime["VOICEPI_FEEDBACK_START"], "true");
    assert_eq!(runtime["VOICEPI_FEEDBACK_STOP"], "true");
    assert_eq!(runtime["VOICEPI_FEEDBACK_DONE"], "false");

    let live = effective_live_runtime_settings_from_raw(&raw);
    assert_eq!(live["feedback_start"].1.as_deref(), Some("true"));
    assert_eq!(live["feedback_stop"].1.as_deref(), Some("true"));
    assert_eq!(live["feedback_done"].1.as_deref(), Some("false"));
    assert!(!live["feedback_start"].2);
}

#[test]
fn single_key_write_retains_privacy_env_precedence_and_explicit_nulls() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    std::fs::write(&path, r#"{"lang":"da","audio_device":null}"#).unwrap();
    let ambient = BTreeMap::from([
        ("VOICEPI_LOCAL_ONLY".to_owned(), "1".to_owned()),
        (
            "VOICEPI_AUDIO_DEVICE".to_owned(),
            "environment mic".to_owned(),
        ),
    ]);
    crate::config::set_value("lang", "en", &path).unwrap();
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let resolved = effective_runtime_env_from_value(&stored, Some(&ambient));
    assert_eq!(resolved["VOICEPI_LOCAL_ONLY"], "1");
    assert!(!resolved.contains_key("VOICEPI_AUDIO_DEVICE"));
}

#[test]
fn invalid_explicit_history_limit_disables_pruning_instead_of_inheriting_an_env_cap() {
    let ambient = BTreeMap::from([("VOICEPI_HISTORY_MAX_ENTRIES".to_owned(), "3".to_owned())]);
    assert_eq!(
        effective_runtime_env_from_value(&serde_json::json!({}), Some(&ambient))
            ["VOICEPI_HISTORY_MAX_ENTRIES"],
        "3"
    );
    for invalid in [
        serde_json::json!(true),
        serde_json::json!(false),
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!(1.5),
        serde_json::json!(1.0),
        serde_json::json!(-1),
        serde_json::json!(100001),
        serde_json::json!(""),
        serde_json::json!("broken"),
    ] {
        let raw = serde_json::json!({"history_max_entries": invalid});
        assert_eq!(
            effective_runtime_env_from_value(&raw, Some(&ambient))["VOICEPI_HISTORY_MAX_ENTRIES"],
            "0"
        );
        assert_eq!(
            crate::config::AppSettings::from_value(raw)
                .unwrap()
                .history_max_entries,
            "0"
        );
    }
    for valid in [serde_json::json!(1), serde_json::json!("1")] {
        let raw = serde_json::json!({"history_max_entries": valid});
        assert_eq!(
            effective_runtime_env_from_value(&raw, Some(&ambient))["VOICEPI_HISTORY_MAX_ENTRIES"],
            "1"
        );
        assert_eq!(
            crate::config::AppSettings::from_value(raw)
                .unwrap()
                .history_max_entries,
            "1"
        );
    }
}

#[test]
fn invalid_config_and_environment_numbers_use_defaults_without_changing_precedence() {
    let ambient = BTreeMap::from([
        ("VOICEPI_RELEASE_TAIL_MS".to_owned(), "60000".to_owned()),
        ("VOICEPI_MIN_INPUT_DBFS".to_owned(), "NaN".to_owned()),
    ]);
    let empty = serde_json::json!({});
    let resolved = effective_runtime_env_from_value(&empty, Some(&ambient));
    assert_eq!(resolved["VOICEPI_RELEASE_TAIL_MS"], "200");
    assert_eq!(resolved["VOICEPI_MIN_INPUT_DBFS"], "-55");
    let configured = serde_json::json!({"release_tail_ms":"2000"});
    let resolved = effective_runtime_env_from_value(&configured, Some(&ambient));
    assert_eq!(resolved["VOICEPI_RELEASE_TAIL_MS"], "2000");
    let bad_config = serde_json::json!({"release_tail_ms":"60000"});
    let live = effective_live_runtime_settings_from_raw(&bad_config);
    assert_eq!(live["release_tail_ms"].1.as_deref(), Some("200"));
    assert!(live["release_tail_ms"].2);
}

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
