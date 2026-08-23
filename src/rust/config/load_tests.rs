use super::*;

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
