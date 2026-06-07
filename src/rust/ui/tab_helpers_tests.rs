use super::tabs::{compact_label, empty_as_auto, empty_as_disabled};
use super::test_support::test_app;
use super::*;

#[test]
fn compact_label_truncates_with_ellipsis_only_past_the_budget() {
    // Shorter than the budget is returned unchanged.
    assert_eq!(compact_label("groq", 10), "groq");
    // Exactly the budget is returned unchanged (no trailing ellipsis).
    assert_eq!(compact_label("abcdef", 6), "abcdef");
    // One over the budget keeps the budgeted prefix and appends an ellipsis.
    assert_eq!(compact_label("abcdefg", 6), "abcdef...");
    // Empty input stays empty regardless of budget.
    assert_eq!(compact_label("", 8), "");
}

#[test]
fn compact_label_counts_unicode_scalar_values_not_bytes() {
    // "ø" is multi-byte; truncation must not split it or panic on a byte boundary.
    assert_eq!(compact_label("søren", 3), "sør...");
    assert_eq!(compact_label(" æøå", 4), " æøå");
    assert_eq!(compact_label(" æøåx", 4), " æøå...");
}

#[test]
fn empty_as_auto_labels_blank_local_runtime_values() {
    assert_eq!(empty_as_auto(""), "Auto");
    assert_eq!(empty_as_auto("   "), "Auto");
    assert_eq!(empty_as_auto("  cuda  "), "cuda");
    assert_eq!(empty_as_auto("int8_float16"), "int8_float16");
}

#[test]
fn empty_as_disabled_treats_blank_and_none_as_disabled() {
    assert_eq!(empty_as_disabled(""), "Disabled");
    assert_eq!(empty_as_disabled("   "), "Disabled");
    assert_eq!(empty_as_disabled("none"), "Disabled");
    assert_eq!(empty_as_disabled("  groq  "), "groq");
    // Only the exact token "none" is special; substrings are preserved.
    assert_eq!(empty_as_disabled("none-of-the-above"), "none-of-the-above");
}

#[test]
fn backend_summary_labels_each_speech_engine() {
    let whisper = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        ..Default::default()
    });
    assert_eq!(whisper.backend_summary(), "Whisper");

    let parakeet = test_app(AppSettings {
        stt_backend: "parakeet".to_owned(),
        ..Default::default()
    });
    assert_eq!(parakeet.backend_summary(), "Parakeet");

    // Unknown backends fall back to the local Whisper label.
    let unknown = test_app(AppSettings {
        stt_backend: "experimental".to_owned(),
        ..Default::default()
    });
    assert_eq!(unknown.backend_summary(), "Whisper");
}

#[test]
fn backend_summary_uses_cloud_provider_name_for_cloud_backend() {
    let groq = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        ..Default::default()
    });
    assert_eq!(groq.backend_summary(), "Groq");

    let openai = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        ..Default::default()
    });
    assert_eq!(openai.backend_summary(), "OpenAI");
}

#[test]
fn stt_detail_summary_reports_auto_for_blank_local_compute() {
    let app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        device: String::new(),
        compute_type: String::new(),
        ..Default::default()
    });

    let (label, _, value) = app.stt_detail_summary();

    assert_eq!(label, "Compute");
    assert_eq!(value, "Auto / Auto");
}

#[test]
fn stt_detail_summary_falls_back_to_provider_default_model_when_blank() {
    let groq = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_model: String::new(),
        ..Default::default()
    });
    let (label, _, value) = groq.stt_detail_summary();
    assert_eq!(label, "Model");
    assert_eq!(value, "whisper-large-v3-turbo");

    let openai = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        stt_model: "   ".to_owned(),
        ..Default::default()
    });
    let (_, _, value) = openai.stt_detail_summary();
    assert_eq!(value, "gpt-4o-mini-transcribe");
}

#[test]
fn stt_detail_summary_compacts_long_cloud_model_names() {
    let app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "openai".to_owned(),
        stt_model: "a-really-long-custom-transcription-model-name".to_owned(),
        ..Default::default()
    });

    let (label, _, value) = app.stt_detail_summary();

    assert_eq!(label, "Model");
    // compact_label caps the cloud model summary at 28 scalar values + ellipsis.
    assert_eq!(value, "a-really-long-custom-transcr...");
}
