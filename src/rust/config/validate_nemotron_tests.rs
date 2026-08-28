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
