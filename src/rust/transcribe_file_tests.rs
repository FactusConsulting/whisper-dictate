use std::sync::Mutex;

use crate::dictate::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::dictionary::{Dictionary, Replacement, SessionDictionary};
use crate::postprocess::settings_from_env_with;
use crate::transcribe_file::{
    prompt_for, transcribe_path, validate_input_path, write_report, ConfiguredBackend,
};

struct RecordingBackend {
    seen: Mutex<Option<(usize, u32)>>,
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
    assert!(ConfiguredBackend::from_value("python").is_err());
}

#[test]
fn input_validation_is_actionable_for_missing_and_non_wav_files() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.wav");
    assert!(validate_input_path(&missing)
        .unwrap_err()
        .to_string()
        .contains("does not exist"));

    let mp3 = temp.path().join("recording.mp3");
    std::fs::write(&mp3, b"not audio").unwrap();
    let error = validate_input_path(&mp3).unwrap_err().to_string();
    assert!(error.contains("only 16 kHz mono WAV"));
    assert!(error.contains("ffmpeg -i INPUT -ac 1 -ar 16000 OUTPUT.wav"));
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
}

#[test]
fn prompt_terms_stay_within_configured_budget() {
    let prompt = prompt_for(&dictionary(), Some("Base prompt")).unwrap();
    assert_eq!(prompt, "Base prompt\nVocabulary: Cloud Code");
    assert!(!prompt.contains("WhisperDictate"));
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
    assert_eq!(value["backend"], "openai");
    assert_eq!(value["text"], "hello Claude Code");
}
