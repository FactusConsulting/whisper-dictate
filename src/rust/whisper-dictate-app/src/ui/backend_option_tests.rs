use super::*;

#[test]
fn stt_backend_mode_maps_only_active_backend() {
    assert_eq!(SttBackendMode::from_raw("whisper"), SttBackendMode::Whisper);
    assert_eq!(
        SttBackendMode::from_raw("parakeet"),
        SttBackendMode::Parakeet
    );
    assert_eq!(SttBackendMode::from_raw("openai"), SttBackendMode::Cloud);
    assert_eq!(SttBackendMode::from_raw(""), SttBackendMode::Whisper);
}

#[test]
fn combo_labels_keep_cloud_backend_distinct_from_openai_provider() {
    assert_eq!(
        selected_option_label("openai", STT_BACKEND_OPTIONS),
        "Cloud STT (Groq/OpenAI)"
    );
    assert_eq!(
        selected_option_label("openai", CLOUD_PROVIDER_OPTIONS),
        "OpenAI"
    );
    assert_eq!(
        selected_option_label("groq", CLOUD_PROVIDER_OPTIONS),
        "Groq"
    );
    assert_eq!(
        selected_option_label("custom", CLOUD_PROVIDER_OPTIONS),
        "custom"
    );
}
