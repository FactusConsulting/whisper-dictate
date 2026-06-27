use super::test_support::test_app;
use super::*;

#[test]
fn stt_backend_mode_maps_only_active_backend() {
    assert_eq!(SttBackendMode::from_raw("whisper"), SttBackendMode::Whisper);
    assert_eq!(SttBackendMode::from_raw("openai"), SttBackendMode::Cloud);
    assert_eq!(SttBackendMode::from_raw(""), SttBackendMode::Whisper);
}

#[test]
fn stt_backend_dropdown_no_longer_offers_parakeet() {
    // Wave 8 of #348 removed the Parakeet entry from the picker. A user
    // can no longer reach the legacy backend by clicking it; saved configs
    // still carrying "parakeet" are migrated to whisper at load time.
    for (value, _display) in STT_BACKEND_OPTIONS {
        assert_ne!(*value, "parakeet");
    }
    let labels: Vec<&str> = STT_BACKEND_OPTIONS.iter().map(|(_, d)| *d).collect();
    assert!(!labels.iter().any(|d| d.contains("Parakeet")));
    // The legacy raw value must still degrade gracefully: from_raw maps
    // anything that isn't `"openai"` to Whisper so a stale `"parakeet"`
    // string slipping through (e.g. mid-migration) renders the Whisper
    // settings instead of crashing.
    assert_eq!(
        SttBackendMode::from_raw("parakeet"),
        SttBackendMode::Whisper
    );
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
        "Custom (OpenAI-compatible)"
    );
}

#[test]
fn top_status_detail_uses_cloud_model_instead_of_local_compute() {
    let app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_model: "whisper-large-v3".to_owned(),
        device: "cuda".to_owned(),
        compute_type: "int8_float16".to_owned(),
        ..Default::default()
    });

    let (label, _, value) = app.stt_detail_summary();

    assert_eq!(label, "Model");
    assert_eq!(value, "whisper-large-v3");
    assert!(!value.contains("cuda"));
    assert!(!value.contains("int8"));
}

#[test]
fn top_status_detail_keeps_compute_for_local_backend() {
    let app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        device: "cuda".to_owned(),
        compute_type: "int8_float16".to_owned(),
        ..Default::default()
    });

    let (label, _, value) = app.stt_detail_summary();

    assert_eq!(label, "Compute");
    assert_eq!(value, "cuda / int8_float16");
}
