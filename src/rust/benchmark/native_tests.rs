//! Unit tests for the native `benchmark` runner. Exercise the full
//! JSONL + summary path against a `FixedTextBackend` stub so the tests
//! cover the scoring / annotate / emit / aggregate flow without needing a
//! cloud API key or a whisper.cpp model.
//!
//! Env-mutating tests take the crate-wide [`crate::test_env_lock::ENV_LOCK`]
//! for the whole save/set/run/restore window: several settings the runner
//! reads (`VOICEPI_STT_BACKEND`, `VOICEPI_STT_API_KEY`, `VOICEPI_LANG`, …)
//! are process-globals, and Cargo runs these tests in parallel.

use super::*;
use crate::corpus::CorpusItem;
use crate::dictionary::{Dictionary, Replacement, SessionDictionary};
use crate::postprocess::{settings_from_env_with, PostprocessSettings};
use crate::test_env_lock::ENV_LOCK;
use std::path::PathBuf;

/// Restore a `VOICEPI_*` variable to its snapshot value, then move on.
fn restore(name: &str, prev: Option<String>) {
    match prev {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

/// Snapshot-and-clear a batch of env vars, returning `(name, prev)` pairs
/// for the caller to feed back into [`restore`] in reverse.
fn snapshot_clear(names: &[&str]) -> Vec<(String, Option<String>)> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let prev = std::env::var(name).ok();
        std::env::remove_var(name);
        out.push(((*name).to_owned(), prev));
    }
    out
}

fn restore_all(pairs: Vec<(String, Option<String>)>) {
    for (name, prev) in pairs {
        restore(&name, prev);
    }
}

#[test]
fn explicit_clear_marker_blocks_benchmark_ambient_fallback() {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = std::env::var("VOICEPI_LANG").ok();
    std::env::set_var("VOICEPI_LANG", "da");
    let resolved = BTreeMap::from([("VOICEPI_LANG".to_owned(), String::new())]);

    let value = env_lookup(&resolved)("VOICEPI_LANG");

    restore("VOICEPI_LANG", previous);
    assert_eq!(value, None);
}

fn empty_dictionary() -> SessionDictionary {
    SessionDictionary {
        dictionary: Dictionary::default(),
        max_terms: 32,
        max_chars: 256,
        enabled: false,
    }
}

fn passthrough_post() -> PostprocessSettings {
    // `processor = "none"` short-circuits `postprocess_text` before any
    // provider call, so a bench with post-processing disabled is a
    // passthrough. Empty-lookup mirrors the "unset env" defaults the
    // `settings_from_env_with` parser applies (processor="none",
    // mode="raw") — matches the shipping default install.
    settings_from_env_with(|_| None)
}

fn tiny_wav(dir: &std::path::Path, name: &str) -> PathBuf {
    // Write a tiny 16 kHz mono int16 WAV (a few frames of silence). The
    // FixedTextBackend below ignores the samples, so any WAV that decodes
    // cleanly under `decode_wav_16k_mono` will do — this keeps the fixture
    // creation dependency-free (hound is already in the tree).
    let path = dir.join(name);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    for _ in 0..3_200 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();
    path
}

fn item(id: &str, text: &str, audio: PathBuf) -> CorpusItem {
    item_with_lang(id, text, audio, "en")
}

fn item_with_lang(id: &str, text: &str, audio: PathBuf, language: &str) -> CorpusItem {
    CorpusItem {
        id: id.to_owned(),
        text: text.to_owned(),
        audio,
        language: language.to_owned(),
        category: "short_english".to_owned(),
        terms: vec![],
    }
}

#[test]
fn annotate_event_populates_scoring_and_metadata() {
    let event = json!({
        "event": "benchmark_result",
        "text": "hello world",
        "raw_text": "hello world",
        "source_file": "x.wav",
        "benchmark_success": true,
        "benchmark_returncode": 0,
    });
    let spec = BackendSpec {
        raw: "openai:gpt-4o".to_owned(),
        backend: "openai".to_owned(),
        model: Some("gpt-4o".to_owned()),
    };
    let it = item("greet", "hello world", PathBuf::from("x.wav"));
    let annotated = annotate_event(event, &it, &spec);
    let map = annotated.as_object().unwrap();
    // Scoring fields must be present so summarize_results can aggregate.
    assert!((map["wer"].as_f64().unwrap() - 0.0).abs() < 1e-9);
    assert!((map["cer"].as_f64().unwrap() - 0.0).abs() < 1e-9);
    assert_eq!(map["exact_match"].as_bool(), Some(true));
    // Metadata fields the Python worker also emits.
    assert_eq!(map["benchmark_backend"].as_str(), Some("openai"));
    assert_eq!(map["benchmark_model"].as_str(), Some("gpt-4o"));
    assert_eq!(map["corpus_id"].as_str(), Some("greet"));
    assert_eq!(map["corpus_language"].as_str(), Some("en"));
    assert_eq!(map["reference_text"].as_str(), Some("hello world"));
}

#[test]
fn scoring_event_from_extracts_summary_fields() {
    let event = json!({
        "benchmark_success": true,
        "benchmark_skipped": false,
        "wer": 0.25,
        "cer": 0.1,
    });
    let ev = scoring_event_from(&event);
    assert!(ev.benchmark_success);
    assert!(!ev.benchmark_skipped);
    assert_eq!(ev.wer, Some(0.25));
    assert_eq!(ev.cer, Some(0.1));
}

#[test]
fn scoring_event_from_handles_skipped_row() {
    let event = json!({
        "benchmark_success": false,
        "benchmark_skipped": true,
        "benchmark_error": MISSING_AUDIO_REASON,
    });
    let ev = scoring_event_from(&event);
    assert!(ev.benchmark_skipped);
    assert_eq!(ev.benchmark_error.as_deref(), Some(MISSING_AUDIO_REASON));
    assert!(ev.wer.is_none());
}

#[test]
fn run_one_item_records_missing_audio_as_skip() {
    let tmp = tempfile::tempdir().unwrap();
    let it = item("nope", "hello world", tmp.path().join("missing.wav"));
    let backend = FixedTextBackend::new("hello world");
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let event = run_one_item(
        &it,
        &it.audio,
        &backend,
        &spec,
        &empty_dictionary(),
        &passthrough_post(),
    );
    let map = event.as_object().unwrap();
    assert_eq!(map["benchmark_skipped"].as_bool(), Some(true));
    assert_eq!(map["benchmark_error"].as_str(), Some(MISSING_AUDIO_REASON));
    assert_eq!(map["benchmark_success"].as_bool(), Some(false));
}

#[test]
fn run_one_item_success_populates_scoring_and_elapsed() {
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let it = item("greet", "hello world", audio.clone());
    let backend = FixedTextBackend::new("hello world");
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let event = run_one_item(
        &it,
        &audio,
        &backend,
        &spec,
        &empty_dictionary(),
        &passthrough_post(),
    );
    let map = event.as_object().unwrap();
    assert_eq!(map["benchmark_success"].as_bool(), Some(true));
    assert!((map["wer"].as_f64().unwrap() - 0.0).abs() < 1e-9);
    assert!(map.contains_key("benchmark_elapsed_s"));
    assert_eq!(map["text"].as_str(), Some("hello world"));
}

/// A cloud backend may leave `raw_text` empty; the runner falls back to the
/// transcript so benchmark rows retain the recognized text.
#[test]
fn run_one_item_raw_text_falls_back_to_transcript_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "cloud.wav");
    let it = item("cloud", "hello world", audio.clone());
    let backend = FixedTextBackend::new_cloud_shape("hello world");
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let event = run_one_item(
        &it,
        &audio,
        &backend,
        &spec,
        &empty_dictionary(),
        &passthrough_post(),
    );
    let map = event.as_object().unwrap();
    assert_eq!(map["raw_text"].as_str(), Some("hello world"));
}

/// Each corpus item's language reaches the backend as its per-item hint.
#[test]
fn run_one_item_applies_per_item_language_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let audio_en = tiny_wav(tmp.path(), "en.wav");
    let audio_da = tiny_wav(tmp.path(), "da.wav");
    let en = item_with_lang("en1", "hello", audio_en.clone(), "en");
    let da = item_with_lang("da1", "hej", audio_da.clone(), "da");
    let backend = FixedTextBackend::new("hi");
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let _ = run_one_item(
        &en,
        &audio_en,
        &backend,
        &spec,
        &empty_dictionary(),
        &passthrough_post(),
    );
    assert_eq!(
        backend.last_language.lock().unwrap().as_deref(),
        Some("en"),
        "English row must set language override to 'en'"
    );
    let _ = run_one_item(
        &da,
        &audio_da,
        &backend,
        &spec,
        &empty_dictionary(),
        &passthrough_post(),
    );
    assert_eq!(
        backend.last_language.lock().unwrap().as_deref(),
        Some("da"),
        "Danish row must set language override to 'da'"
    );
}

/// Dictionary replacements affect the benchmark transcript and its scores.
#[test]
fn run_one_item_applies_dictionary_replacements() {
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let it = item("greet", "Claude Code virker", audio.clone());
    // Backend returns the mis-spelled variant; dictionary rewrites it.
    let backend = FixedTextBackend::new("cloud code virker");
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let dictionary = SessionDictionary {
        dictionary: Dictionary {
            terms: vec!["Claude Code".to_owned()],
            replacements: vec![Replacement {
                from: "cloud code".to_owned(),
                to: "Claude Code".to_owned(),
            }],
        },
        max_terms: 32,
        max_chars: 256,
        enabled: true,
    };
    let event = run_one_item(
        &it,
        &audio,
        &backend,
        &spec,
        &dictionary,
        &passthrough_post(),
    );
    let map = event.as_object().unwrap();
    assert_eq!(map["text"].as_str(), Some("Claude Code virker"));
    // WER over normalised tokens: "claude code virker" == "claude code virker" → 0.
    assert!((map["wer"].as_f64().unwrap() - 0.0).abs() < 1e-9);
    assert_eq!(map["exact_match"].as_bool(), Some(true));
    // Per-replacement telemetry is preserved.
    let reps = map["dictionary_replacements"].as_array().unwrap();
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0]["from"].as_str(), Some("cloud code"));
    assert_eq!(reps[0]["to"].as_str(), Some("Claude Code"));
}

#[test]
fn scoring_and_reporting_agree_on_a_small_run() {
    // Two items, one perfect + one with a single-word deletion → WER 1/3.
    let tmp = tempfile::tempdir().unwrap();
    let audio_a = tiny_wav(tmp.path(), "a.wav");
    let audio_b = tiny_wav(tmp.path(), "b.wav");
    let items = [
        item("a", "hello world", audio_a.clone()),
        item("b", "Claude Code virker", audio_b.clone()),
    ];
    let backend_a = FixedTextBackend::new("hello world");
    let backend_b = FixedTextBackend::new("Claude virker");
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let dictionary = empty_dictionary();
    let post = passthrough_post();
    let ev_a = scoring_event_from(&run_one_item(
        &items[0],
        &audio_a,
        &backend_a,
        &spec,
        &dictionary,
        &post,
    ));
    let ev_b = scoring_event_from(&run_one_item(
        &items[1],
        &audio_b,
        &backend_b,
        &spec,
        &dictionary,
        &post,
    ));
    let summary = summarize_results(&[ev_a, ev_b]);
    assert_eq!(summary.total, 2);
    assert_eq!(summary.passed, 2);
    // Item A WER=0, Item B WER=1/3 → avg = 1/6.
    assert!((summary.avg_wer.unwrap() - (1.0 / 6.0)).abs() < 1e-9);
    // Confirm the summary line renders the same way both here and via the
    // reporting parity tests would.
    let line = format_summary_line(&summary, None);
    assert!(line.starts_with("[benchmark] 2/2 passed"));
}

#[test]
fn native_bench_error_wraps_anyhow() {
    let e: NativeBenchError = anyhow::anyhow!("boom").into();
    match e {
        NativeBenchError::Other(err) => assert!(err.to_string().contains("boom")),
        _ => panic!("expected Other variant"),
    }
}

/// Stock build (no `whisper-rs-local`): a `whisper` spec must surface as
/// `Unsupported` so `handle_bench` can print the rebuild hint. Skipped on a
/// feature-on build (the backend builds successfully there).
#[test]
fn run_with_reports_unsupported_when_local_whisper_feature_absent() {
    if cfg!(feature = "whisper-rs-local") {
        return;
    }
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let items = [item("greet", "hello", audio)];
    let prev = snapshot_clear(&["VOICEPI_STT_BACKEND"]);
    std::env::set_var("VOICEPI_STT_BACKEND", "whisper");
    let result = super::run_with(&items, tmp.path());
    restore_all(prev);
    match result {
        Err(NativeBenchError::Unsupported(msg)) => {
            assert!(msg.contains("whisper-rs-local"), "got: {msg}");
        }
        _ => panic!("expected Unsupported error on stock build"),
    }
}

#[test]
fn run_with_writer_captures_summary_line_to_buffer() {
    // The System tab's "Run benchmark" button captures the runner output
    // into a String on a background thread instead of shelling out to
    // Python. Prove the writer path emits the same `[benchmark] …` summary
    // line the stdout path does, since the UI's `apply_benchmark_results`
    // parser keys on it.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let items = [item("greet", "hello world", audio)];
    // Force the openai backend so we don't need the `whisper-rs-local`
    // feature to exercise this.
    let prev = snapshot_clear(&[
        "VOICEPI_STT_BACKEND",
        "VOICEPI_STT_API_KEY",
        "OPENAI_API_KEY",
        "GROQ_API_KEY",
        "VOICEPI_LOCAL_ONLY",
        "VOICEPI_STT_MODEL",
    ]);
    std::env::set_var("VOICEPI_STT_BACKEND", "openai");
    // Supply a fake API key so the pre-flight validation passes; the fixed
    // backend below is stubbed via the cloud-configured build path returning
    // an error on transcribe (no network), which the runner records as a
    // failed row — the summary line still emits.
    std::env::set_var("VOICEPI_STT_API_KEY", "test-key");
    let mut buf: Vec<u8> = Vec::new();
    let _ = super::run_with_writer(&items, tmp.path(), &mut buf);
    restore_all(prev);
    let out = String::from_utf8(buf).unwrap();
    assert!(
        out.contains("[benchmark]"),
        "writer must receive the summary line: {out}"
    );
}

/// An empty corpus is rejected instead of being reported as a successful run.
#[test]
fn run_with_writer_rejects_empty_corpus() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let prev = snapshot_clear(&[
        "VOICEPI_STT_BACKEND",
        "VOICEPI_STT_API_KEY",
        "OPENAI_API_KEY",
    ]);
    std::env::set_var("VOICEPI_STT_BACKEND", "openai");
    std::env::set_var("VOICEPI_STT_API_KEY", "test-key");
    let mut buf: Vec<u8> = Vec::new();
    let result = super::run_with_writer(&[], tmp.path(), &mut buf);
    restore_all(prev);
    match result {
        Err(NativeBenchError::Other(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("at least one benchmark corpus item is required"),
                "got: {msg}"
            );
        }
        _ => panic!("expected Other error for empty corpus"),
    }
}

/// A cloud backend without an API key is rejected before any audio is read.
#[test]
fn run_with_writer_rejects_cloud_backend_without_api_key() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let items = [item("greet", "hello world", audio)];
    let prev = snapshot_clear(&[
        "VOICEPI_STT_BACKEND",
        "VOICEPI_STT_API_KEY",
        "OPENAI_API_KEY",
        "GROQ_API_KEY",
        "VOICEPI_STT_BASE_URL",
    ]);
    std::env::set_var("VOICEPI_STT_BACKEND", "openai");
    let mut buf: Vec<u8> = Vec::new();
    let result = super::run_with_writer(&items, tmp.path(), &mut buf);
    restore_all(prev);
    match result {
        Err(NativeBenchError::Other(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("openai benchmark backend requires a cloud API key"),
                "got: {msg}"
            );
        }
        _ => panic!("expected Other error for missing cloud key"),
    }
}

/// Per-spec model qualifiers are rejected so rows cannot be mislabeled by a
/// shared environment-selected local model.
#[test]
fn run_with_writer_rejects_local_whisper_model_qualifier() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let items = [item("greet", "hello world", audio)];
    let prev = snapshot_clear(&["VOICEPI_STT_BACKEND"]);
    std::env::set_var("VOICEPI_STT_BACKEND", "whisper:tiny,whisper:large-v3");
    let mut buf: Vec<u8> = Vec::new();
    let result = super::run_with_writer(&items, tmp.path(), &mut buf);
    restore_all(prev);
    match result {
        Err(NativeBenchError::Other(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("does not support per-spec model qualifier"),
                "got: {msg}"
            );
        }
        // On a stock (no `whisper-rs-local`) build the feature-gate
        // rejection fires first — either shape confirms the invalid
        // comparison never emits mislabeled rows.
        Err(NativeBenchError::Unsupported(_)) => {}
        other => panic!(
            "expected model-qualifier rejection, got {:?}",
            other.is_ok()
        ),
    }
}

/// When every audio file is missing, the runner skips backend construction and
/// reports skipped rows instead of loading a local model.
#[test]
fn run_with_writer_skips_backend_construction_when_all_audio_missing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    // Note: no `tiny_wav(...)` call — audio files are intentionally absent.
    let items = [item("missing", "hello", tmp.path().join("nope.wav"))];
    let prev = snapshot_clear(&[
        "VOICEPI_STT_BACKEND",
        "VOICEPI_STT_API_KEY",
        "OPENAI_API_KEY",
        "GROQ_API_KEY",
        "VOICEPI_WHISPER_MODEL_PATH",
    ]);
    // Selecting whisper WITHOUT any model path would normally blow up in
    // `resolve_model_path_from_env`; the runner must skip the whole build
    // step when nothing to transcribe exists. Skipped on stock (no
    // `whisper-rs-local`) builds where the feature gate would fire first.
    std::env::set_var("VOICEPI_STT_BACKEND", "whisper");
    let mut buf: Vec<u8> = Vec::new();
    let result = super::run_with_writer(&items, tmp.path(), &mut buf);
    restore_all(prev);
    if cfg!(feature = "whisper-rs-local") {
        assert!(
            result.is_ok(),
            "all-missing corpus must succeed (as skipped rows)"
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains(MISSING_AUDIO_REASON),
            "each row should be marked missing audio: {out}"
        );
        assert!(out.contains("[benchmark]"), "summary must still emit");
    } else {
        // On a stock build the feature-gate rejection fires first — which
        // still proves the runner never got to backend construction.
        match result {
            Err(NativeBenchError::Unsupported(_)) => {}
            Err(NativeBenchError::Other(e)) => {
                // Accept the model-path failure ONLY if it came from the
                // gate check itself (i.e. not from `resolve_model_path_from_env`).
                panic!("unexpected Other error: {e:#}");
            }
            Ok(()) => panic!("stock build must reject whisper spec"),
        }
    }
}

#[test]
fn tiny_wav_decodes_via_public_helper() {
    // Sanity check on the fixture writer so any decoder tweak up-stream
    // fails here, not deep in run_one_item.
    let tmp = tempfile::tempdir().unwrap();
    let path = tiny_wav(tmp.path(), "sanity.wav");
    let pcm = crate::whisper::decode_wav_16k_mono(&path).unwrap();
    assert_eq!(pcm.len(), 3_200);
}

// Step 2 of the vp_benchmark retirement removed FALLBACK_MESSAGE_PREFIX +
// the corresponding stability test — the Python fallback is gone, so there
// is no fallback line for users to grep for.
