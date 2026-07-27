//! Unit tests for the native `benchmark` runner. Exercise the full
//! JSONL + summary path against a `FixedTextBackend` stub so the tests
//! cover the scoring / annotate / emit / aggregate flow without needing a
//! cloud API key or a whisper.cpp model.

use super::*;
use crate::corpus::CorpusItem;
use std::fs;
use std::path::PathBuf;

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
    CorpusItem {
        id: id.to_owned(),
        text: text.to_owned(),
        audio,
        language: "en".to_owned(),
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
    let backend = FixedTextBackend {
        text: "hello world".into(),
    };
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let event = run_one_item(&it, &it.audio, &backend, &spec);
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
    let backend = FixedTextBackend {
        text: "hello world".into(),
    };
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let event = run_one_item(&it, &audio, &backend, &spec);
    let map = event.as_object().unwrap();
    assert_eq!(map["benchmark_success"].as_bool(), Some(true));
    assert!((map["wer"].as_f64().unwrap() - 0.0).abs() < 1e-9);
    assert!(map.contains_key("benchmark_elapsed_s"));
    assert_eq!(map["text"].as_str(), Some("hello world"));
}

#[test]
fn scoring_and_reporting_agree_on_a_small_run() {
    // Two items, one perfect + one with a single-word deletion → WER 1/3.
    let tmp = tempfile::tempdir().unwrap();
    let audio_a = tiny_wav(tmp.path(), "a.wav");
    let audio_b = tiny_wav(tmp.path(), "b.wav");
    let items = vec![
        item("a", "hello world", audio_a.clone()),
        item("b", "Claude Code virker", audio_b.clone()),
    ];
    let backend_a = FixedTextBackend {
        text: "hello world".into(),
    };
    let backend_b = FixedTextBackend {
        text: "Claude virker".into(),
    };
    let spec = BackendSpec {
        raw: "openai".into(),
        backend: "openai".into(),
        model: None,
    };
    let ev_a = scoring_event_from(&run_one_item(&items[0], &audio_a, &backend_a, &spec));
    let ev_b = scoring_event_from(&run_one_item(&items[1], &audio_b, &backend_b, &spec));
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

#[test]
fn run_with_falls_back_when_local_whisper_feature_absent() {
    // Without the `whisper-rs-local` feature we cannot build the local
    // Whisper backend natively; the runner must surface the Unsupported
    // signal so `handle_bench` shells to Python. On feature-on builds this
    // path is not reachable (the backend builds successfully), so the
    // assertion is cfg-gated.
    if cfg!(feature = "whisper-rs-local") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let audio = tiny_wav(tmp.path(), "greet.wav");
    let items = vec![item("greet", "hello", audio)];
    let prev = std::env::var("VOICEPI_STT_BACKEND").ok();
    std::env::set_var("VOICEPI_STT_BACKEND", "whisper");
    let result = super::run_with(&items, tmp.path());
    // Restore before asserting so a failure doesn't leak env for later tests.
    match prev {
        Some(v) => std::env::set_var("VOICEPI_STT_BACKEND", v),
        None => std::env::remove_var("VOICEPI_STT_BACKEND"),
    }
    match result {
        Err(NativeBenchError::Unsupported(msg)) => {
            assert!(msg.contains("whisper-rs-local"), "got: {msg}");
        }
        _ => panic!("expected Unsupported error on stock build"),
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

#[test]
fn fallback_message_prefix_is_stable() {
    // The message users grep for stays intact across refactors.
    assert!(FALLBACK_MESSAGE_PREFIX.starts_with("[benchmark]"));
    assert!(FALLBACK_MESSAGE_PREFIX.contains("using Python fallback"));
}
