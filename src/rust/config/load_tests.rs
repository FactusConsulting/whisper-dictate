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
