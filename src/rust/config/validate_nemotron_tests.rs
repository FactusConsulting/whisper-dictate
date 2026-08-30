use super::AppSettings;

#[test]
fn validator_rejects_multilingual_profile_on_public_hosted_function() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "https://grpc.nvcf.nvidia.com:443".to_owned(),
        stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        ..AppSettings::default()
    };

    let error = settings
        .validate_nemotron_profile_language()
        .unwrap_err()
        .to_string();
    assert!(error.contains("English-only"));
    assert!(error.contains("function-id"));
}

#[test]
fn validator_applies_public_hosted_english_contract_without_model_label() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "https://grpc.nvcf.nvidia.com:443".to_owned(),
        stt_model: "legacy-or-hand-edited-model".to_owned(),
        lang: "da".to_owned(),
        ..AppSettings::default()
    };

    let error = settings
        .validate_nemotron_profile_language()
        .unwrap_err()
        .to_string();
    assert!(error.contains("English-only"));
    assert!(error.contains("Language=English"));
}

#[test]
fn validator_allows_public_hosted_english_for_any_model_label() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "https://grpc.nvcf.nvidia.com:443".to_owned(),
        stt_model: "legacy-or-hand-edited-model".to_owned(),
        lang: "en".to_owned(),
        ..AppSettings::default()
    };

    settings.validate_nemotron_profile_language().unwrap();
}

#[test]
fn validator_rejects_public_function_id_query_for_multilingual_language() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: format!(
            "https://grpc.nvcf.nvidia.com:443?function-id={}",
            crate::cloud_api::NEMOTRON_NVCF_FUNCTION_ID
        ),
        stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        ..AppSettings::default()
    };

    let error = settings
        .validate_nemotron_profile_language()
        .unwrap_err()
        .to_string();
    assert!(error.contains("English-only"));
}

#[test]
fn validator_accepts_keyless_in_process_endpoint() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "inproc://nemotron".to_owned(),
        stt_model: "C:/models/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf".to_owned(),
        ..AppSettings::default()
    };

    settings.validate().unwrap();
}

#[test]
fn validator_rejects_unsupported_multilingual_locale_in_process() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: "inproc://nemotron".to_owned(),
        stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        lang: "en-AU".to_owned(),
        ..AppSettings::default()
    };

    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("supported locale"));
    assert!(error.contains("en-AU"));
}
