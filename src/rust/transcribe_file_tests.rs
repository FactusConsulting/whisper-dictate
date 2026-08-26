use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::Mutex;

use crate::dictate::CloudTranscribeConfig;
use crate::dictate::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::dictionary::{Dictionary, Replacement, SessionDictionary};
use crate::postprocess::settings_from_env_with;
use crate::transcribe_file::{
    build_cloud_backend, compact_text, dictionary_replacements_or_original,
    initialize_after_input_validation, load_configured_backend,
    materialize_runtime_environment_with, prompt_for, report_language, transcribe_path,
    validate_input_path, write_report, ConfiguredBackend,
};

struct RecordingBackend {
    seen: Mutex<Option<(usize, u32)>>,
}

struct FixedBackend(TranscribeResult);

impl TranscribeBackend for FixedBackend {
    fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        Ok(self.0.clone())
    }
}

impl TranscribeBackend for RecordingBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        *self.seen.lock().unwrap() = Some((pcm.len(), sample_rate));
        Ok(TranscribeResult {
            text: "hello Cloud Code".to_owned(),
            duration_s: pcm.len() as f64 / sample_rate as f64,
            language: "en".to_owned(),
            stt_impl: "test".to_owned(),
            ..TranscribeResult::default()
        })
    }
}

fn dictionary() -> SessionDictionary {
    SessionDictionary {
        dictionary: Dictionary {
            terms: vec!["Cloud Code".to_owned(), "WhisperDictate".to_owned()],
            replacements: vec![Replacement {
                from: "Cloud Code".to_owned(),
                to: "Claude Code".to_owned(),
            }],
        },
        max_terms: 1,
        max_chars: 50,
        enabled: true,
    }
}

fn cloud_config(model: &str, api_key: &str) -> CloudTranscribeConfig {
    CloudTranscribeConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: api_key.to_owned(),
        model: model.to_owned(),
        timeout_ms: 30_000,
        language: None,
        prompt: None,
    }
}

fn write_silent_wav(path: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    writer.write_sample(0_i16).unwrap();
    writer.finalize().unwrap();
}

fn replacement_dictionary(from: &str, to: &str) -> SessionDictionary {
    SessionDictionary {
        dictionary: Dictionary {
            terms: Vec::new(),
            replacements: vec![Replacement {
                from: from.to_owned(),
                to: to.to_owned(),
            }],
        },
        max_terms: 10,
        max_chars: 100,
        enabled: true,
    }
}

#[test]
fn backend_selection_accepts_live_values_and_rejects_unknown() {
    assert_eq!(
        ConfiguredBackend::from_value("whisper").unwrap(),
        ConfiguredBackend::Whisper
    );
    assert_eq!(
        ConfiguredBackend::from_value("OPENAI").unwrap(),
        ConfiguredBackend::Cloud
    );
    assert!(ConfiguredBackend::from_value("parakeet").is_err());
    assert!(ConfiguredBackend::from_value("python").is_err());
}

#[test]
fn saved_parakeet_migrates_but_an_ambient_override_does_not() {
    let raw = serde_json::json!({"stt_backend": "parakeet"});
    let settings = crate::config::AppSettings::from_value(raw.clone()).unwrap();
    assert_eq!(
        ConfiguredBackend::from_runtime_sources(&raw, &settings, Some("parakeet")).unwrap(),
        ConfiguredBackend::Whisper
    );

    let empty = serde_json::json!({});
    let defaults = crate::config::AppSettings::from_value(empty.clone()).unwrap();
    assert!(ConfiguredBackend::from_runtime_sources(&empty, &defaults, Some("parakeet")).is_err());
}

#[test]
fn malformed_active_config_is_rejected_before_runtime_materialization() {
    let _guard = crate::config::test_support::ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("broken-config.json");
    std::fs::write(&config, "{not valid json").unwrap();
    let previous = std::env::var_os("VOICEPI_CONFIG");
    std::env::set_var("VOICEPI_CONFIG", &config);

    let error = load_configured_backend().unwrap_err().to_string();

    crate::config::test_support::restore_env("VOICEPI_CONFIG", previous);
    assert!(error.contains("load active configuration"));
}

#[test]
fn cloud_config_accepts_documented_model_env_fallback() {
    let _guard = crate::config::test_support::ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("cloud-config.json");
    std::fs::write(&config, r#"{"stt_backend":"openai"}"#).unwrap();
    let previous_config = std::env::var_os("VOICEPI_CONFIG");
    let previous_model = std::env::var_os("VOICEPI_STT_MODEL");
    std::env::set_var("VOICEPI_CONFIG", &config);
    std::env::set_var("VOICEPI_STT_MODEL", "env-whisper-model");

    let configured = load_configured_backend().unwrap();

    crate::config::test_support::restore_env("VOICEPI_STT_MODEL", previous_model);
    crate::config::test_support::restore_env("VOICEPI_CONFIG", previous_config);
    assert_eq!(configured, ConfiguredBackend::Cloud);
}

#[test]
fn cloud_backend_is_canonical_before_saved_key_resolution() {
    let writes = RefCell::new(Vec::<(String, String)>::new());
    let attached = Cell::new(false);
    let configured = ConfiguredBackend::from_value(" OPENAI ").unwrap();

    materialize_runtime_environment_with(
        configured,
        || {
            assert_eq!(
                writes.borrow().as_slice(),
                [("VOICEPI_STT_BACKEND".to_owned(), "openai".to_owned())]
            );
            vec![("VOICEPI_STT_BACKEND".to_owned(), " OPENAI ".to_owned())]
        },
        |name, value| writes.borrow_mut().push((name, value)),
        || {
            assert_eq!(
                writes.borrow().last(),
                Some(&("VOICEPI_STT_BACKEND".to_owned(), "openai".to_owned()))
            );
            attached.set(true);
        },
    );

    assert!(attached.get());
}

#[test]
fn materialization_writes_explicit_clear_markers() {
    let writes = RefCell::new(Vec::<(String, String)>::new());

    materialize_runtime_environment_with(
        ConfiguredBackend::Whisper,
        || vec![("VOICEPI_LANG".to_owned(), String::new())],
        |name, value| writes.borrow_mut().push((name, value)),
        || {},
    );

    assert!(writes
        .borrow()
        .contains(&("VOICEPI_LANG".to_owned(), String::new())));
}

#[test]
fn cloud_backend_rejects_missing_model_before_network() {
    let missing_model =
        build_cloud_backend(cloud_config("", "test-key"), &dictionary(), false, "openai")
            .err()
            .expect("empty model must be rejected");
    assert!(missing_model.to_string().contains("configured stt_model"));
}

#[test]
fn cloud_backend_rejects_missing_key_before_network() {
    let missing_key = build_cloud_backend(
        cloud_config("whisper-1", ""),
        &dictionary(),
        false,
        "openai",
    )
    .err()
    .expect("empty API key must be rejected");
    assert!(missing_key.to_string().contains("requires a saved API key"));
}

#[test]
fn cloud_backend_honors_local_only_privacy_gate() {
    let error = build_cloud_backend(
        cloud_config("whisper-1", "test-key"),
        &dictionary(),
        true,
        "openai",
    )
    .err()
    .expect("remote cloud endpoint must be blocked in local-only mode");
    assert!(error.to_string().contains("cloud backend rejected"));
}

#[test]
fn input_validation_is_actionable_for_missing_and_non_wav_files() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.wav");
    assert!(validate_input_path(&missing)
        .unwrap_err()
        .to_string()
        .contains("does not exist"));

    let directory_error = validate_input_path(temp.path()).unwrap_err().to_string();
    assert!(directory_error.contains("input path is not a file"));

    let mp3 = temp.path().join("recording.mp3");
    std::fs::write(&mp3, b"not audio").unwrap();
    let error = validate_input_path(&mp3).unwrap_err().to_string();
    assert!(error.contains("only 16 kHz mono WAV"));
    assert!(error.contains("ffmpeg -i INPUT -ac 1 -ar 16000 OUTPUT.wav"));
}

#[test]
fn invalid_wav_reports_decode_and_conversion_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("invalid.wav");
    std::fs::write(&wav, b"not a wav").unwrap();
    let backend = FixedBackend(TranscribeResult::default());
    let mut post = settings_from_env_with(|_| None);

    let error = transcribe_path(
        &wav,
        ConfiguredBackend::Whisper,
        &backend,
        "unused-model",
        &dictionary(),
        &mut post,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("cannot decode"));
    assert!(error.contains("ffmpeg -i INPUT -ac 1 -ar 16000 OUTPUT.wav"));
}

#[test]
fn invalid_input_is_rejected_before_backend_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let mp3 = temp.path().join("recording.mp3");
    std::fs::write(&mp3, b"not audio").unwrap();
    let initialized = Cell::new(false);

    let error = initialize_after_input_validation(&mp3, || {
        initialized.set(true);
        Ok(())
    })
    .unwrap_err();

    assert!(!initialized.get());
    assert!(error.to_string().contains("ffmpeg -i INPUT"));
}

#[test]
fn wav_is_decoded_then_dictionary_replacements_are_applied() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("sample.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
    for sample in [0_i16, 100, -100, 0] {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();

    let backend = RecordingBackend {
        seen: Mutex::new(None),
    };
    let mut post = settings_from_env_with(|_| None);
    let report = transcribe_path(
        &wav,
        ConfiguredBackend::Whisper,
        &backend,
        "resolved-custom-model.ggml",
        &dictionary(),
        &mut post,
    )
    .unwrap();

    assert_eq!(*backend.seen.lock().unwrap(), Some((4, 16_000)));
    assert_eq!(report.raw_text, "hello Cloud Code");
    assert_eq!(report.dictionary_text, "hello Claude Code");
    assert_eq!(report.text, "hello Claude Code");
    assert_eq!(report.dictionary_replacements.len(), 1);
    assert_eq!(report.dictionary_replacements[0].count, 1);
    assert_eq!(report.language, "en");
    assert_eq!(report.model, "resolved-custom-model.ggml");
}

#[test]
fn prompt_terms_stay_within_configured_budget() {
    let prompt = prompt_for(&dictionary(), Some("Base prompt")).unwrap();
    assert_eq!(prompt, "Base prompt\nVocabulary: Cloud Code");
    assert!(!prompt.contains("WhisperDictate"));
}

#[test]
fn replacement_failure_keeps_the_usable_transcript() {
    let (text, changes) = dictionary_replacements_or_original(
        "usable transcript",
        Err(anyhow::anyhow!("simulated regex compilation failure")),
    );
    assert_eq!(text, "usable transcript");
    assert!(changes.is_empty());
}

#[test]
fn report_language_prefers_detection_then_hint_then_auto() {
    assert_eq!(report_language("en", Some("da")), "en");
    assert_eq!(report_language("", Some(" da ")), "da");
    assert_eq!(report_language("  ", None), "auto");
}

#[test]
fn compact_preview_collapses_whitespace_and_truncates_at_character_limit() {
    assert_eq!(compact_text(" one\n two ", 20), "one two");
    assert_eq!(compact_text("abcdef", 5), "ab...");
}

#[test]
fn report_supports_plain_text_and_single_object_json() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("sample.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav_writer = hound::WavWriter::create(&wav, spec).unwrap();
    wav_writer.write_sample(0_i16).unwrap();
    wav_writer.finalize().unwrap();
    let backend = RecordingBackend {
        seen: Mutex::new(None),
    };
    let mut post = settings_from_env_with(|_| None);
    let report = transcribe_path(
        &wav,
        ConfiguredBackend::Cloud,
        &backend,
        "whisper-cloud-test",
        &dictionary(),
        &mut post,
    )
    .unwrap();

    let mut plain = Vec::new();
    write_report(&mut plain, &report, false).unwrap();
    assert_eq!(String::from_utf8(plain).unwrap(), "hello Claude Code\n");

    let mut json = Vec::new();
    write_report(&mut json, &report, true).unwrap();
    let json_text = String::from_utf8(json).unwrap();
    assert_eq!(json_text.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(json_text.trim()).unwrap();
    assert_eq!(value["event"], "file_transcription");
    assert_eq!(value["stt_backend"], "openai");
    assert_eq!(value["text"], "hello Claude Code");
    assert_eq!(value["model"], "whisper-cloud-test");
    assert!(
        value["language_probability"].is_null(),
        "unavailable confidence must remain unknown"
    );
    for legacy_key in [
        "ts",
        "text_preview",
        "text_chars",
        "recording_s",
        "audio_duration_s",
        "compute_s",
        "real_time_factor",
        "model",
        "device",
        "compute_type",
        "segments",
        "dictionary_terms",
        "post_model",
        "post_redacted",
        "post_redactions",
    ] {
        assert!(
            value.get(legacy_key).is_some(),
            "missing legacy file_transcription field {legacy_key}"
        );
    }
}

#[test]
fn dictionary_replacement_reclassifies_hallucination_before_postprocess() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("hallucination.wav");
    write_silent_wav(&wav);
    let dictionary = replacement_dictionary("ordinary words", "thank you");
    let backend = FixedBackend(TranscribeResult {
        text: "ordinary words".to_owned(),
        is_hallucination: false,
        ..TranscribeResult::default()
    });
    let mut post = settings_from_env_with(|_| None);

    let report = transcribe_path(
        &wav,
        ConfiguredBackend::Whisper,
        &backend,
        "test-model.ggml",
        &dictionary,
        &mut post,
    )
    .unwrap();

    assert_eq!(report.dictionary_text, "thank you");
    assert_eq!(report.text, "");
}

#[test]
fn dictionary_replacement_can_clear_backend_hallucination_flag() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("corrected.wav");
    write_silent_wav(&wav);
    let dictionary = replacement_dictionary("thank you", "corrected dictation");
    let backend = FixedBackend(TranscribeResult {
        text: "thank you".to_owned(),
        is_hallucination: true,
        ..TranscribeResult::default()
    });
    let mut post = settings_from_env_with(|_| None);

    let report = transcribe_path(
        &wav,
        ConfiguredBackend::Whisper,
        &backend,
        "test-model.ggml",
        &dictionary,
        &mut post,
    )
    .unwrap();

    assert_eq!(report.dictionary_text, "corrected dictation");
    assert_eq!(report.text, "corrected dictation");
}
