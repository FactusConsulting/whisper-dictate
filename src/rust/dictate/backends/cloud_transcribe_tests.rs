//! Tests for [`super`] -- the cloud STT backend. All hermetic: the WAV
//! encoder is exercised directly, config resolution runs through an
//! injected lookup, and the transcribe error paths trip
//! `cloud_transcribe`'s empty-key / empty-model guards BEFORE any network
//! call, so no live endpoint is contacted.

use std::collections::HashMap;
use std::io::Cursor;

use super::*;
use crate::dictate::provenance::{STT_IMPL_CLOUD_GROQ, STT_IMPL_CLOUD_OPENAI};

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

// The trim + gate + boost pre-model pipeline (`prepare_for_transcription`) is
// now shared with the local backend and unit-tested in `audio_dsp::gate_tests`.

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
fn nemotron_auto_language_is_explicit_multi() {
    let backend = CloudTranscribeBackend::new_nemotron(CloudTranscribeConfig {
        base_url: "http://localhost:9000/v1".to_owned(),
        api_key: String::new(),
        model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        timeout_ms: 30_000,
        language: None,
        prompt: None,
    });
    assert_eq!(backend.request_language().as_deref(), Some("multi"));
}

#[test]
fn nemotron_explicit_language_wins_over_auto_mode() {
    let backend = CloudTranscribeBackend::new_nemotron(CloudTranscribeConfig {
        base_url: "http://localhost:9000/v1".to_owned(),
        api_key: String::new(),
        model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        timeout_ms: 30_000,
        language: Some("en-US".to_owned()),
        prompt: None,
    });
    assert_eq!(backend.request_language().as_deref(), Some("en-US"));
}

#[test]
fn nemotron_startup_provenance_matches_utterance_provenance() {
    let backend = CloudTranscribeBackend::new_nemotron(CloudTranscribeConfig {
        base_url: "http://localhost:9000/v1".to_owned(),
        api_key: String::new(),
        model: NEMOTRON_MODEL.to_owned(),
        timeout_ms: 30_000,
        language: None,
        prompt: None,
    });
    assert_eq!(backend.stt_impl(), "cloud-nemotron");
}

#[test]
fn localhost_custom_model_is_not_misclassified_as_nemotron() {
    let backend = cloud_backend_local_only_checked(
        false,
        CloudTranscribeConfig {
            base_url: "http://localhost:9000/v1".to_owned(),
            api_key: String::new(),
            model: "custom-model".to_owned(),
            timeout_ms: 30_000,
            language: None,
            prompt: None,
        },
    )
    .expect("loopback custom endpoint should be accepted");
    assert_eq!(backend.stt_impl(), "cloud-custom");
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

/// RAII scaffold for the prompt-reload tests: takes the crate-wide
/// [`crate::test_env_lock::ENV_LOCK`], snapshots the dictionary env keys, then
/// points them at `dict` with pinned budgets (so an ambient `=0` / tiny value
/// can't drop the vocabulary) and a cleared `VOICEPI_CONFIG` (so only the env
/// dictionary is in play). Restores every key on drop -- even on a panicking
/// assertion -- so a failure can't leak into sibling tests. Rust has no
/// test-fixture support, so both tests share this instead of open-coding the
/// same lock + save/set/restore window.
struct DictEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

impl DictEnvGuard {
    const KEYS: [&'static str; 5] = [
        "VOICEPI_DICTIONARY",
        "VOICEPI_DICTIONARY_ENABLED",
        "VOICEPI_DICTIONARY_MAX_TERMS",
        "VOICEPI_DICTIONARY_PROMPT_CHARS",
        "VOICEPI_CONFIG",
    ];

    fn with_dict(dict: &std::path::Path) -> Self {
        let lock = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let saved = Self::KEYS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        std::env::set_var("VOICEPI_DICTIONARY", dict);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::set_var("VOICEPI_DICTIONARY_MAX_TERMS", "80");
        std::env::set_var("VOICEPI_DICTIONARY_PROMPT_CHARS", "1200");
        std::env::remove_var("VOICEPI_CONFIG");
        Self { _lock: lock, saved }
    }
}

impl Drop for DictEnvGuard {
    fn drop(&mut self) {
        for (key, prior) in &self.saved {
            match prior {
                Some(val) => std::env::set_var(key, val),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// A cloud backend whose BASE STT prompt is `"base"` and whose effective
/// prompt live-reloads the env dictionary terms (`EnvFirst`) -- the shared
/// subject of the prompt-reload tests.
fn reloading_prompt_backend() -> CloudTranscribeBackend {
    CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: "k".to_owned(),
        model: "whisper-1".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: Some("base".to_owned()),
    })
    .with_reloading_prompt(crate::dictionary::ReloadPrecedence::EnvFirst)
}

#[test]
fn effective_prompt_refolds_reloaded_dictionary_terms() {
    // The reloading prompt keeps `config.prompt` as the BASE and re-folds the
    // live dictionary terms into it on each call, reloading on a term edit --
    // the backend glue for the per-utterance prompt biasing.
    let dir = tempfile::tempdir().unwrap();
    let dict = dir.path().join("dict.json");
    std::fs::write(&dict, r#"{"terms":["Codex"]}"#).unwrap();
    let _env = DictEnvGuard::with_dict(&dict);

    let backend = reloading_prompt_backend();
    let first = backend.effective_prompt();
    std::fs::write(&dict, r#"{"terms":["Codex","Slack"]}"#).unwrap();
    let second = backend.effective_prompt();

    assert_eq!(first.0.as_deref(), Some("base\nVocabulary: Codex"));
    assert_eq!(first.1, ["Codex"]);
    assert_eq!(
        second.0.as_deref(),
        Some("base\nVocabulary: Codex, Slack"),
        "editing the dictionary terms must re-fold the STT prompt"
    );
    assert_eq!(second.1, ["Codex", "Slack"]);
}

#[test]
fn profile_prompt_omits_dictionary_terms() {
    let backend = reloading_prompt_backend();
    let profile = std::collections::BTreeMap::from([(
        "initial_prompt".to_owned(),
        "profile vocabulary".to_owned(),
    )]);
    backend.apply_profile_overrides(&profile);

    assert_eq!(
        backend.effective_prompt(),
        (Some("profile vocabulary".to_owned()), Vec::new())
    );
}

#[test]
fn prompt_and_replacements_reload_from_the_same_dictionary() {
    // The split wiring -- prompt biasing on the backend, replacement table on
    // the session provider -- must read the SAME live dictionary: one file with
    // both `terms` and `replacements` biases the STT prompt AND rewrites the
    // transcript.
    use crate::dictionary::DictionaryProvider;

    let dir = tempfile::tempdir().unwrap();
    let dict = dir.path().join("dict.json");
    std::fs::write(
        &dict,
        r#"{"terms":["Codex"],"replacements":{"cloud code":"Claude Code"}}"#,
    )
    .unwrap();
    let _env = DictEnvGuard::with_dict(&dict);

    // Prompt half (backend) and replacement half (session provider), both from
    // the same env dictionary.
    let prompt = reloading_prompt_backend().effective_prompt();

    let mut replacements =
        crate::dictionary::ReloadingDictionary::new(crate::dictionary::ReloadPrecedence::EnvFirst);
    let (rewritten, _) = replacements
        .current()
        .apply_replacements("open cloud code")
        .unwrap();

    assert_eq!(
        prompt.0.as_deref(),
        Some("base\nVocabulary: Codex"),
        "the term biases the STT prompt"
    );
    assert_eq!(
        rewritten, "open Claude Code",
        "the replacement rewrites the transcript from the same dictionary"
    );
}

// ── local-only privacy gate (#540) ──────────────────────────────────

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

// ── map_cloud_result — response mapping + hallucination gate (#543) ──

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
    let result = map_cloud_result(
        cloud_response("tak", None),
        12,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert!(result.is_hallucination, "blacklisted 'tak' must be flagged");
    assert_eq!(result.text, "tak");
}

#[test]
fn map_cloud_result_trims_before_the_blacklist_check() {
    // Endpoint whitespace must not defeat the match (leading space would,
    // since the blacklist rstrips only) — mirrors normalize_whitespace.
    let result = map_cloud_result(
        cloud_response("  tak  ", None),
        0,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert!(
        result.is_hallucination,
        "surrounding whitespace must be trimmed before the check"
    );
}

#[test]
fn map_cloud_result_keeps_normal_dictation() {
    let result = map_cloud_result(
        cloud_response("hello world", Some("en")),
        5,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert!(
        !result.is_hallucination,
        "normal dictation must not be flagged"
    );
    assert_eq!(result.text, "hello world");
    assert_eq!(result.language, "en");
}

#[test]
fn map_cloud_result_falls_back_to_the_requested_language() {
    // The standard `json` response format usually
    // OMITS `language`. The post-processor keeps its own configured `lang`
    // when the result reports nothing, so a profile that switched STT to `en`
    // (via the `language` alias) while the saved config says `da` would get a
    // Danish cleanup prompt for an English transcript. The requested
    // language is the effective one and must be reported.
    let result = map_cloud_result(
        cloud_response("hello there", None),
        5,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        Some("en"),
    );
    assert_eq!(result.language, "en");

    // The endpoint's own answer still wins when it sends one (auto-detect
    // with no request hint is the interesting case).
    let detected = map_cloud_result(
        cloud_response("guten tag", Some("de")),
        5,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert_eq!(detected.language, "de");
    // ...and it wins over the request hint too, since it describes the audio.
    let both = map_cloud_result(
        cloud_response("guten tag", Some("de")),
        5,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        Some("en"),
    );
    assert_eq!(both.language, "de");

    // Blank on both sides stays the honest "unknown" (auto-detect, nothing
    // reported) so the prompt names no language at all.
    let unknown = map_cloud_result(
        cloud_response("x", Some("  ")),
        5,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        Some(" "),
    );
    assert_eq!(unknown.language, "");
}

#[test]
fn map_cloud_result_blanks_impossibly_fast_transcript() {
    // 100 chars over pcm_len=160 @ 16 kHz = 0.01 s (floored to 0.1 s) =>
    // 1000 chars/s > 30 default: the transcript is blanked so the session
    // emits an `empty` no-text event instead of injecting a hallucination.
    let long = "x".repeat(100);
    let result = map_cloud_result(
        cloud_response(&long, None),
        0,
        160,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert!(
        result.text.is_empty(),
        "over-fast transcript must be blanked, got {:?}",
        result.text
    );
    assert!(!result.is_hallucination);
}

#[test]
fn map_cloud_result_keeps_normal_rate_transcript() {
    // "hello world" over 1 s (16 000 samples) = 11 chars/s < 30: kept.
    let result = map_cloud_result(
        cloud_response("hello world", None),
        0,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert_eq!(result.text, "hello world");
}

#[test]
fn map_cloud_result_maps_fields_and_duration() {
    // Absent language collapses to ""; duration_s = pcm_len / sample_rate.
    let result = map_cloud_result(
        cloud_response("noget tekst", None),
        42,
        8_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
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

// -- provenance: stt_impl / stt_accel ------------------------------------

#[test]
fn map_cloud_result_stamps_the_provider_impl_it_was_given() {
    // The `stt_impl` field must name the PROVIDER, not the `stt_backend`
    // setting -- which is `openai` for Groq too, so a record carrying only
    // the setting cannot tell the two services apart.
    let openai = map_cloud_result(
        cloud_response("hello", None),
        1,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert_eq!(openai.stt_impl, "cloud-openai");
    let groq = map_cloud_result(
        cloud_response("hello", None),
        1,
        16_000,
        16_000,
        STT_IMPL_CLOUD_GROQ,
        None,
    );
    assert_eq!(groq.stt_impl, "cloud-groq");
}

#[test]
fn map_cloud_result_reports_unknown_accel_never_a_guess() {
    // Nothing in a cloud response reveals the provider's compute path.
    // `unknown` is the honest answer; `cpu` or a GPU label would be the
    // same class of lie the provenance fields exist to remove.
    let result = map_cloud_result(
        cloud_response("hello", None),
        1,
        16_000,
        16_000,
        STT_IMPL_CLOUD_OPENAI,
        None,
    );
    assert_eq!(result.stt_accel, "unknown");
}

#[test]
fn cloud_backend_resolves_its_impl_label_from_the_configured_base_url() {
    // Pins the wiring between the backend's live config and the provider
    // label the utterance record ends up carrying.
    let openai = CloudTranscribeBackend::new(cloud_config("https://api.openai.com/v1"));
    assert_eq!(
        crate::dictate::provenance::cloud_stt_impl_for_base_url(&openai.config().base_url),
        STT_IMPL_CLOUD_OPENAI
    );
    let groq = CloudTranscribeBackend::new(cloud_config("https://api.groq.com/openai/v1"));
    assert_eq!(
        crate::dictate::provenance::cloud_stt_impl_for_base_url(&groq.config().base_url),
        STT_IMPL_CLOUD_GROQ
    );
}

#[test]
fn cloud_backend_uses_owned_guards_and_applies_live_thresholds() {
    let guards = TranscriptionGuards::from_lookup(lookup_from(&[
        (crate::audio_dsp::TARGET_DBFS_ENV, "-17"),
        (crate::audio_dsp::MIN_INPUT_DBFS_ENV, "-49"),
        (crate::audio_dsp::MIN_SNR_DB_ENV, "7"),
        (
            crate::dictate::backends::hallucination::MAX_CHARS_PER_SECOND_ENV,
            "35",
        ),
    ]));
    let backend = CloudTranscribeBackend::new(cloud_config("https://api.openai.com/v1"))
        .with_transcription_guards(guards);

    let initial = backend.effective_transcription_guards();
    assert_eq!(initial.thresholds.target_dbfs, -17.0);
    assert_eq!(initial.thresholds.min_input_dbfs, -49.0);
    assert_eq!(initial.thresholds.min_input_snr_db, 7.0);
    assert_eq!(initial.max_chars_per_second, 35.0);

    <CloudTranscribeBackend as TranscribeBackend>::apply_profile_overrides(
        &backend,
        &std::collections::BTreeMap::from([
            ("target_dbfs".to_owned(), "-15".to_owned()),
            ("min_input_dbfs".to_owned(), "-45".to_owned()),
            ("min_snr_db".to_owned(), "10".to_owned()),
            ("max_chars_per_second".to_owned(), "22".to_owned()),
        ]),
    );
    let live = backend.effective_transcription_guards();
    assert_eq!(live.thresholds.target_dbfs, -15.0);
    assert_eq!(live.thresholds.min_input_dbfs, -45.0);
    assert_eq!(live.thresholds.min_input_snr_db, 10.0);
    assert_eq!(live.max_chars_per_second, 22.0);
}

#[test]
fn cloud_prompt_terms_switch_to_the_stop_boundary_dictionary() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    std::fs::write(&first, r#"{"terms":["AlphaTerm"]}"#).unwrap();
    std::fs::write(&second, r#"{"terms":["BetaTerm"]}"#).unwrap();

    let backend =
        CloudTranscribeBackend::new(CloudTranscribeConfig {
            prompt: Some("Base".to_owned()),
            ..cloud_config("https://api.openai.com/v1")
        })
        .with_reloading_prompt_settings(
            crate::dictionary::RuntimeDictionarySettings::new(true, vec![first], 10, 1_200),
        );
    let (initial_prompt, initial_terms) = backend.effective_prompt();
    assert!(initial_prompt.unwrap().contains("AlphaTerm"));
    assert_eq!(initial_terms, vec!["AlphaTerm"]);

    <CloudTranscribeBackend as TranscribeBackend>::apply_profile_overrides(
        &backend,
        &std::collections::BTreeMap::from([(
            "dictionary".to_owned(),
            second.display().to_string(),
        )]),
    );
    let (live_prompt, live_terms) = backend.effective_prompt();
    let live_prompt = live_prompt.unwrap();
    assert!(live_prompt.contains("BetaTerm"));
    assert!(!live_prompt.contains("AlphaTerm"));
    assert_eq!(live_terms, vec!["BetaTerm"]);
}
