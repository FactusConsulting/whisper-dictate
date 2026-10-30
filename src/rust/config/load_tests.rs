use super::*;
use crate::config::GROQ_POST_MODEL_OPTIONS;

#[test]
fn removed_groq_post_model_migrates_during_config_load() {
    let settings = AppSettings::from_value(serde_json::json!({
        "post_processor": "groq",
        "post_model": "qwen/qwen3-32b",
    }))
    .unwrap();

    assert_eq!(settings.post_model, DEFAULT_GROQ_POST_MODEL);
}

#[test]
fn supported_groq_post_models_survive_config_load() {
    for (model, _) in GROQ_POST_MODEL_OPTIONS {
        let settings = AppSettings::from_value(serde_json::json!({
            "post_processor": "groq",
            "post_model": model,
        }))
        .unwrap();
        assert_eq!(settings.post_model, *model);
    }
}

#[test]
fn non_groq_custom_post_model_survives_config_load() {
    let settings = AppSettings::from_value(serde_json::json!({
        "post_processor": "openai",
        "post_model": "account-specific-model",
    }))
    .unwrap();

    assert_eq!(settings.post_model, "account-specific-model");
}

#[test]
fn custom_groq_post_model_survives_config_load() {
    let settings = AppSettings::from_value(serde_json::json!({
        "post_processor": "groq",
        "post_model": " account-specific-model ",
    }))
    .unwrap();

    assert_eq!(settings.post_model, "account-specific-model");
}

#[test]
fn retired_groq_model_inside_matching_profile_migrates_on_load() {
    let settings = AppSettings::from_value(serde_json::json!({
        "profiles": [{
            "name": "editor",
            "match": {"process": "editor"},
            "settings": {
                "post_processor": "groq",
                "post_model": "qwen/qwen3-32b"
            }
        }]
    }))
    .unwrap();
    let profiles: Value = serde_json::from_str(&settings.profiles_json).unwrap();

    assert_eq!(
        profiles[0]["settings"]["post_model"],
        DEFAULT_GROQ_POST_MODEL
    );
}

#[test]
fn profile_inheriting_top_level_groq_also_migrates_its_model() {
    let settings = AppSettings::from_value(serde_json::json!({
        "post_processor": "groq",
        "post_model": DEFAULT_GROQ_POST_MODEL,
        "profiles": [{
            "name": "editor",
            "match": {"process": "editor"},
            "settings": {"post_model": "llama-3.1-8b-instant"}
        }]
    }))
    .unwrap();
    let profiles: Value = serde_json::from_str(&settings.profiles_json).unwrap();

    assert_eq!(
        profiles[0]["settings"]["post_model"],
        DEFAULT_GROQ_POST_MODEL
    );
}

#[test]
fn groq_catalog_values_and_processor_accept_surrounding_whitespace() {
    let settings = AppSettings::from_value(serde_json::json!({
        "post_processor": " groq ",
        "post_model": " openai/gpt-oss-120b ",
        "profiles": [{
            "name": "editor",
            "match": {},
            "settings": {
                "post_processor": " GROQ ",
                "post_model": " openai/gpt-oss-20b "
            }
        }]
    }))
    .unwrap();
    let profiles: Value = serde_json::from_str(&settings.profiles_json).unwrap();

    assert_eq!(settings.post_model, "openai/gpt-oss-120b");
    assert_eq!(profiles[0]["settings"]["post_model"], "openai/gpt-oss-20b");
}

#[test]
fn groq_migration_warning_keys_are_rate_limited_per_trimmed_model() {
    let mut warned = std::collections::HashSet::new();

    assert!(first_warning_for_model(&mut warned, "retired-model"));
    assert!(!first_warning_for_model(&mut warned, " retired-model "));
    assert!(first_warning_for_model(
        &mut warned,
        "another-retired-model"
    ));
}

#[test]
fn native_json_bool_loads_into_bool_settings() {
    let settings = AppSettings::from_value(serde_json::json!({"local_only": true})).unwrap();
    assert!(settings.local_only);
    let settings = AppSettings::from_value(serde_json::json!({"local_only": false})).unwrap();
    assert!(!settings.local_only);
}

#[test]
fn native_json_number_loads_into_string_settings() {
    let settings = AppSettings::from_value(serde_json::json!({"stt_timeout_ms": 5000})).unwrap();
    assert_eq!(settings.stt_timeout_ms, "5000");
}

#[test]
fn string_bool_forms_keep_truthy_table() {
    for (raw, expected) in [
        ("", false),
        ("0", false),
        ("false", false),
        ("No", false),
        ("OFF", false),
        ("1", true),
        ("true", true),
        ("yes", true),
    ] {
        let settings = AppSettings::from_value(serde_json::json!({"local_only": raw})).unwrap();
        assert_eq!(settings.local_only, expected, "local_only={raw:?}");
    }
}

#[test]
fn null_array_and_object_values_fall_back_to_defaults() {
    let defaults = AppSettings::from_value(serde_json::json!({})).unwrap();
    for invalid in [
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({}),
    ] {
        let settings = AppSettings::from_value(serde_json::json!({
            "local_only": invalid,
            "stt_timeout_ms": invalid,
        }))
        .unwrap();
        assert_eq!(settings.local_only, defaults.local_only);
        assert_eq!(settings.stt_timeout_ms, defaults.stt_timeout_ms);
    }
}
