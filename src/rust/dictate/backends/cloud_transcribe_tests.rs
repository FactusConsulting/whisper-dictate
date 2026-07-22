//! Tests for [`super`] -- the cloud STT backend. All hermetic: the WAV
//! encoder is exercised directly, config resolution runs through an
//! injected lookup, and the transcribe error paths trip
//! `cloud_transcribe`'s empty-key / empty-model guards BEFORE any network
//! call, so no live endpoint is contacted.

use std::collections::HashMap;
use std::io::Cursor;

use super::*;

fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

/// PCM that PASSES the pre-transcription speech gate: 6 frames of 480 samples
/// alternating quiet/loud and ending loud (so trailing-silence trim keeps the
/// contrast) -> healthy level + high SNR. Used by the network-guard error
/// tests so they reach the empty-key/model checks rather than being
/// short-circuited by the gate.
fn gate_passing_pcm() -> Vec<f32> {
    let mut pcm = Vec::with_capacity(6 * 480);
    for amp in [0.001_f32, 0.5, 0.001, 0.5, 0.001, 0.5] {
        pcm.extend(std::iter::repeat_n(amp, 480));
    }
    pcm
}

#[test]
fn encode_wav_produces_readable_mono_16bit() {
    let pcm = [0.0_f32, 0.5, -0.5, 1.0, -1.0];
    let bytes = encode_wav_mono_16bit(&pcm, 16_000).expect("encode");
    let reader = hound::WavReader::new(Cursor::new(bytes)).expect("valid WAV");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(reader.len(), pcm.len() as u32);
}

#[test]
fn encode_wav_clamps_out_of_range_samples_without_wrap() {
    // 2.0 must clamp to +full-scale, not wrap to a negative i16.
    let bytes = encode_wav_mono_16bit(&[2.0, -2.0], 16_000).expect("encode");
    let samples: Vec<i16> = hound::WavReader::new(Cursor::new(bytes))
        .unwrap()
        .into_samples::<i16>()
        .map(Result::unwrap)
        .collect();
    assert_eq!(samples, vec![i16::MAX, i16::MIN + 1]);
}

#[test]
fn config_from_env_uses_defaults_when_unset() {
    let cfg = CloudTranscribeConfig::from_env_with(lookup_from(&[]));
    assert_eq!(cfg.base_url, "https://api.openai.com/v1");
    assert_eq!(cfg.timeout_ms, 30_000);
    assert!(cfg.model.is_empty());
    assert!(cfg.api_key.is_empty());
    assert_eq!(cfg.language, None);
    assert_eq!(cfg.prompt, None);
}

#[test]
fn config_api_key_is_provider_aware_by_base_url() {
    // Groq base_url + only GROQ_API_KEY -> groq key.
    let groq = CloudTranscribeConfig::from_env_with(lookup_from(&[
        (STT_BASE_URL_ENV, "https://api.groq.com/openai/v1"),
        ("OPENAI_API_KEY", "openai-key"),
        ("GROQ_API_KEY", "groq-key"),
    ]));
    assert_eq!(groq.api_key, "groq-key");

    // OpenAI base_url -> OPENAI_API_KEY even though both are present.
    let openai = CloudTranscribeConfig::from_env_with(lookup_from(&[
        (STT_BASE_URL_ENV, "https://api.openai.com/v1"),
        ("OPENAI_API_KEY", "openai-key"),
        ("GROQ_API_KEY", "groq-key"),
    ]));
    assert_eq!(openai.api_key, "openai-key");

    // The STT-specific key wins over any provider generic.
    let stt = CloudTranscribeConfig::from_env_with(lookup_from(&[
        (STT_BASE_URL_ENV, "https://api.groq.com/openai/v1"),
        ("VOICEPI_STT_API_KEY", "stt-key"),
        ("GROQ_API_KEY", "groq-key"),
    ]));
    assert_eq!(stt.api_key, "stt-key");
}

#[test]
fn config_timeout_clamps_and_parses_like_python() {
    let below = CloudTranscribeConfig::from_env_with(lookup_from(&[(STT_TIMEOUT_MS_ENV, "50")]));
    assert_eq!(below.timeout_ms, 100, "below-min clamps to 100");
    let decimal =
        CloudTranscribeConfig::from_env_with(lookup_from(&[(STT_TIMEOUT_MS_ENV, "1500.0")]));
    assert_eq!(decimal.timeout_ms, 1500, "decimal parses as int(float())");
    let bad = CloudTranscribeConfig::from_env_with(lookup_from(&[(STT_TIMEOUT_MS_ENV, "nope")]));
    assert_eq!(bad.timeout_ms, 30_000, "unparseable falls back to default");
}

#[test]
fn config_reads_language_and_prompt_hints() {
    let cfg = CloudTranscribeConfig::from_env_with(lookup_from(&[
        (LANG_ENV, "da"),
        (INITIAL_PROMPT_ENV, "Whisper Dictate, Factus"),
    ]));
    assert_eq!(cfg.language.as_deref(), Some("da"));
    assert_eq!(cfg.prompt.as_deref(), Some("Whisper Dictate, Factus"));
}

#[test]
fn transcribe_empty_api_key_errors_before_network() {
    let backend = CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: String::new(),
        model: "whisper-1".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    });
    let err = backend
        .transcribe(&gate_passing_pcm(), 16_000)
        .expect_err("empty key must error");
    assert!(matches!(err, TranscribeError::Backend(_)));
}

// ── local-only privacy gate (Codex P1 #540) ──────────────────────────────────

fn cloud_config(base_url: &str) -> CloudTranscribeConfig {
    CloudTranscribeConfig {
        base_url: base_url.to_owned(),
        api_key: "test-key".to_owned(),
        model: "whisper-large-v3-turbo".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    }
}

#[test]
fn cloud_checked_allows_remote_when_local_only_off() {
    // local_only disabled: a remote endpoint is fine.
    let backend =
        cloud_backend_local_only_checked(false, cloud_config("https://api.groq.com/openai/v1"))
            .expect("remote allowed when local-only is off");
    assert_eq!(backend.config().base_url, "https://api.groq.com/openai/v1");
}

#[test]
fn cloud_checked_blocks_remote_under_local_only() {
    // local_only on + non-loopback remote: must be refused so mic audio
    // never leaves the machine.
    match cloud_backend_local_only_checked(true, cloud_config("https://api.groq.com/openai/v1")) {
        Ok(_) => panic!("remote must be blocked under local-only"),
        Err(e) => assert!(e.contains("LOCAL_ONLY"), "{e}"),
    }
}

#[test]
fn cloud_checked_allows_loopback_under_local_only() {
    // A self-hosted endpoint on loopback never leaves the box, so it stays
    // allowed even under local-only (the documented exception).
    for url in [
        "http://127.0.0.1:8080/v1",
        "http://localhost:1234/v1",
        "http://[::1]:9000/v1",
    ] {
        let backend = cloud_backend_local_only_checked(true, cloud_config(url))
            .unwrap_or_else(|e| panic!("loopback {url} must be allowed under local-only: {e}"));
        assert_eq!(backend.config().base_url, url);
    }
}

// ── map_cloud_result — response mapping + hallucination gate (Codex P2 #543) ──

fn cloud_response(text: &str, language: Option<&str>) -> CloudTranscriptionResult {
    CloudTranscriptionResult {
        text: text.to_owned(),
        language: language.map(str::to_owned),
    }
}

#[test]
fn map_cloud_result_flags_blacklisted_transcript_as_hallucination() {
    // A blacklisted credit ("tak") from the cloud endpoint must set
    // is_hallucination so the session drops it as no_speech — the parity
    // fix this guards against a revert to `false`.
    let result = map_cloud_result(cloud_response("tak", None), 12, 16_000, 16_000);
    assert!(result.is_hallucination, "blacklisted 'tak' must be flagged");
    assert_eq!(result.text, "tak");
}

#[test]
fn map_cloud_result_trims_before_the_blacklist_check() {
    // Endpoint whitespace must not defeat the match (leading space would,
    // since the blacklist rstrips only) — mirrors normalize_whitespace.
    let result = map_cloud_result(cloud_response("  tak  ", None), 0, 16_000, 16_000);
    assert!(
        result.is_hallucination,
        "surrounding whitespace must be trimmed before the check"
    );
}

#[test]
fn map_cloud_result_keeps_normal_dictation() {
    let result = map_cloud_result(cloud_response("hello world", Some("en")), 5, 16_000, 16_000);
    assert!(
        !result.is_hallucination,
        "normal dictation must not be flagged"
    );
    assert_eq!(result.text, "hello world");
    assert_eq!(result.language, "en");
}

#[test]
fn map_cloud_result_maps_fields_and_duration() {
    // Absent language collapses to ""; duration_s = pcm_len / sample_rate.
    let result = map_cloud_result(cloud_response("noget tekst", None), 42, 8_000, 16_000);
    assert_eq!(result.language, "");
    assert_eq!(result.latency_ms, 42);
    assert!(
        (result.duration_s - 0.5).abs() < 1e-9,
        "{}",
        result.duration_s
    );
    assert_eq!(result.gate, None);
}

#[test]
fn transcribe_empty_model_errors_before_network() {
    let backend = CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: "test-key".to_owned(),
        model: String::new(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    });
    let err = backend
        .transcribe(&gate_passing_pcm(), 16_000)
        .expect_err("empty model must error");
    assert!(matches!(err, TranscribeError::Backend(_)));
}

#[test]
fn transcribe_gates_silence_before_network() {
    // Silent input is rejected by the speech gate BEFORE any network call,
    // so even an empty api-key does not error — it returns an empty text
    // carrying the gate reason, which the session maps to a too_quiet
    // no-text event.
    let backend = CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: "https://api.groq.com/openai/v1".to_owned(),
        api_key: String::new(),
        model: "whisper-large-v3-turbo".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    });
    let silence = vec![0.0_f32; 6 * 480];
    let result = backend
        .transcribe(&silence, 16_000)
        .expect("gated silence returns Ok, not a backend error");
    assert!(result.text.is_empty());
    let gate = result.gate.expect("gate reason present");
    assert!(gate.contains("too quiet"), "{gate}");
}
