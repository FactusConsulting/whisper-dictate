//! Native Rust benchmark runner — the sole benchmark surface after step 2 of
//! the `vp_benchmark.py` retirement (#348).
//!
//! Drives the same corpus through the same [`crate::dictate::TranscribeBackend`]
//! the live dictation path uses. Preserves the [`super::format_summary_line`]
//! contract — user scripts grep for `[benchmark] …`, so byte parity with the
//! retired Python line is non-negotiable.
//!
//! # Scope
//!
//! Two backends are covered natively:
//!
//! * `openai` — cloud STT via [`crate::dictate::CloudTranscribeBackend`].
//!   Always compiled (cloud_api + hound are unconditional deps).
//! * `whisper` — local Whisper via
//!   [`crate::dictate::backends::WhisperLocalTranscribeBackend`]. Gated on
//!   `whisper-rs-local`; on a stock dev build the runner returns
//!   [`NativeBenchError::Unsupported`] and [`super::handle_bench`] surfaces
//!   the rebuild hint. The Python fallback that used to shell to
//!   `vp_benchmark.py` is gone.
//!
//! # Known limitations
//!
//! * Per-spec `spec.model` override on local Whisper — only
//!   `VOICEPI_WHISPER_MODEL_PATH` is honoured today (documented follow-up).
//!   Cloud specs DO honour `spec.model` (it overrides `config.model`).
//! * WAV shapes other than 16 kHz mono int/float — [`crate::whisper::wav`]
//!   rejects them and the item is recorded as a failure (not skipped).
//!
//! # Summary-line parity
//!
//! The runner routes through the same [`super::summarize_results`] +
//! [`super::format_summary_line`] the Python side uses (via the pure port in
//! `benchmark/reporting.rs`), so the `[benchmark] X/Y passed, …` line is
//! bit-identical to the Python worker's output — cross-checked by the pure
//! reporting-parity unit tests in `benchmark/reporting.rs`.

use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};

use super::{
    format_summary_line, parse_backend_specs, summarize_results, wer, BackendSpec, BenchmarkEvent,
    MISSING_AUDIO_REASON,
};
use crate::corpus::{corpus_search_paths, load_corpus, resolve_corpus_manifest, CorpusItem};
use crate::dictate::backends::cloud_transcribe::CloudTranscribeConfig;
use crate::dictate::backends::CloudTranscribeBackend;
use crate::dictate::{TranscribeBackend, TranscribeResult};

pub mod paths_use {
    // Re-export path helpers here so tests can reach them via the module
    // root; the runner uses the sibling `paths` module directly.
    pub use super::super::paths::{appdata_audio_dir, resolve_item_audio};
}

use super::paths::{appdata_audio_dir, resolve_item_audio};
use super::scoring::{cer, normalize_words, term_report};

/// Outcome of the native runner. `Unsupported` is the fallback signal: the
/// caller ([`super::handle_bench`]) shells out to the Python worker instead
/// of surfacing this as a hard error. `Other` is a real failure and bubbles
/// up as the process exit code.
pub enum NativeBenchError {
    /// The build cannot run the SELECTED backend natively. Carries the
    /// reason for the stderr fallback line.
    Unsupported(String),
    /// A hard error the caller should propagate.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for NativeBenchError {
    fn from(e: anyhow::Error) -> Self {
        NativeBenchError::Other(e)
    }
}

/// Entry point wired into [`super::handle_bench`].
///
/// Resolves the corpus manifest (`<app_root>/benchmark/corpus.json` →
/// `<appdata>/benchmark/corpus.json`), runs every item through each
/// configured backend spec, writes per-item JSONL to stdout, and prints the
/// final `[benchmark] …` summary line.
pub fn run() -> Result<(), NativeBenchError> {
    let mut stdout = io::stdout().lock();
    run_to_writer(&mut stdout)
}

/// Same as [`run`], but writes every JSONL line + the summary line to `out`
/// instead of the process stdout. Used by the System tab's "Run benchmark"
/// button to capture the runner's output on a background thread and hand it
/// to the existing `apply_benchmark_results` parser as a synthesised
/// `BackgroundTaskResult.stdout` — no subprocess, no Python.
pub fn run_to_writer(out: &mut dyn Write) -> Result<(), NativeBenchError> {
    let app_root = crate::runtime::resource_app_root();
    let appdata = crate::config::platform_config_dir();
    let manifest = resolve_corpus_manifest(Some(&app_root), None, Some(&appdata));
    let Some(manifest) = manifest else {
        let looked = corpus_search_paths(Some(&app_root), Some(&appdata))
            .into_iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "[benchmark] no corpus manifest found (looked: {looked}) - \
             see docs/CONFIGURATION.md (Benchmark corpus)"
        );
        return Ok(());
    };
    let items =
        load_corpus(&manifest).map_err(|e| NativeBenchError::Other(anyhow::anyhow!("{e:#}")))?;
    run_with_writer(&items, &appdata, out)
}

/// Testable core: given a pre-resolved corpus + appdata base, execute the
/// runner and write the JSONL + summary line to stdout. Retained as the thin
/// wrapper unit tests + module callers reach for; new callers should prefer
/// [`run_with_writer`] with an explicit sink.
pub fn run_with(items: &[CorpusItem], appdata: &Path) -> Result<(), NativeBenchError> {
    let mut stdout = io::stdout().lock();
    run_with_writer(items, appdata, &mut stdout)
}

/// The writer-parameterised core of [`run_with`]. Split out so the UI thread
/// can capture the output into a `Vec<u8>` for its `BackgroundTaskResult`
/// stdout envelope without any process/pipe machinery.
pub fn run_with_writer(
    items: &[CorpusItem],
    appdata: &Path,
    out: &mut dyn Write,
) -> Result<(), NativeBenchError> {
    let raw_spec = std::env::var("VOICEPI_STT_BACKEND").unwrap_or_default();
    let spec_str = if raw_spec.trim().is_empty() {
        "whisper"
    } else {
        raw_spec.as_str()
    };
    let specs = parse_backend_specs(spec_str).map_err(NativeBenchError::Other)?;

    // Reject specs that cannot run in this build BEFORE any I/O so the
    // rebuild-hint reaches the caller quickly.
    for spec in &specs {
        if spec.backend == "whisper" && !cfg!(feature = "whisper-rs-local") {
            return Err(NativeBenchError::Unsupported(
                "native requires --features whisper-rs-local".to_owned(),
            ));
        }
    }

    let mut scoring_events: Vec<BenchmarkEvent> = Vec::new();
    for spec in &specs {
        let backend = build_backend(spec)?;
        for item in items {
            let audio = resolve_item_audio(&item.audio, Some(appdata));
            let event = run_one_item(item, &audio, backend.as_ref(), spec);
            emit_jsonl(&event, out);
            scoring_events.push(scoring_event_from(&event));
        }
    }
    let summary = summarize_results(&scoring_events);
    let hint = appdata_audio_dir(appdata);
    let _ = writeln!(out, "{}", format_summary_line(&summary, Some(&hint)));
    Ok(())
}

/// Object-safe transcribe backend so per-spec build can hand back either the
/// cloud or the local variant behind one type without paying vtable cost on
/// the hot session path — this runner is CLI-only, not the live dictation
/// loop, so an extra indirect call per corpus item is irrelevant.
trait AnyTranscribeBackend {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String>;
}

struct CloudDyn(CloudTranscribeBackend);
impl AnyTranscribeBackend for CloudDyn {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String> {
        self.0
            .transcribe(pcm, sample_rate)
            .map_err(|e| format!("{e:?}"))
    }
}

#[cfg(feature = "whisper-rs-local")]
struct LocalDyn(crate::dictate::backends::WhisperLocalTranscribeBackend);
#[cfg(feature = "whisper-rs-local")]
impl AnyTranscribeBackend for LocalDyn {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String> {
        self.0
            .transcribe(pcm, sample_rate)
            .map_err(|e| format!("{e:?}"))
    }
}

fn build_backend(spec: &BackendSpec) -> Result<Box<dyn AnyTranscribeBackend>, NativeBenchError> {
    match spec.backend.as_str() {
        "openai" => {
            let mut config = CloudTranscribeConfig::from_env();
            // Per-spec model override — Python's `spec.model` → env
            // `VOICEPI_MODEL` (Python worker path). We apply it directly
            // to `config.model` since cloud reads from `VOICEPI_STT_MODEL`
            // in production; the spec model is the caller's explicit
            // choice so it wins over env.
            if let Some(model) = &spec.model {
                config.model = model.clone();
            }
            Ok(Box::new(CloudDyn(CloudTranscribeBackend::new(config))))
        }
        "whisper" => build_local_whisper(spec),
        other => Err(NativeBenchError::Other(anyhow::anyhow!(
            "unsupported benchmark backend '{other}'; expected whisper or openai"
        ))),
    }
}

#[cfg(feature = "whisper-rs-local")]
fn build_local_whisper(
    _spec: &BackendSpec,
) -> Result<Box<dyn AnyTranscribeBackend>, NativeBenchError> {
    use crate::dictate::backends::whisper_local::WhisperBackendConfig;
    use crate::dictate::backends::WhisperLocalTranscribeBackend;
    use crate::whisper::{parse_idle_timeout_from_env, IdleUnloadingModel};

    let model_path = crate::whisper::resolve_model_path_from_env().map_err(|e| {
        NativeBenchError::Other(anyhow::anyhow!("resolve whisper model path: {e:#}"))
    })?;
    let idle = parse_idle_timeout_from_env()
        .map_err(|e| NativeBenchError::Other(anyhow::anyhow!("whisper idle timeout: {e:#}")))?;
    let model = IdleUnloadingModel::for_local_whisper(model_path, idle);
    let language = std::env::var("VOICEPI_LANG")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let initial_prompt = std::env::var("VOICEPI_INITIAL_PROMPT")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let config = WhisperBackendConfig {
        language,
        initial_prompt,
    };
    Ok(Box::new(LocalDyn(WhisperLocalTranscribeBackend::new(
        model, config,
    ))))
}

#[cfg(not(feature = "whisper-rs-local"))]
fn build_local_whisper(
    _spec: &BackendSpec,
) -> Result<Box<dyn AnyTranscribeBackend>, NativeBenchError> {
    // Unreachable — `run_with` rejects a `whisper` spec before this is
    // called on a build without the feature. Kept as a defence-in-depth
    // return so a future refactor that skips the pre-check still errors
    // cleanly instead of compiling nothing on the miss.
    Err(NativeBenchError::Unsupported(
        "native requires --features whisper-rs-local".to_owned(),
    ))
}

/// Run one corpus item through `backend`, decoding its WAV first. Missing
/// audio is recorded as a skip (`benchmark_skipped=true`, reason =
/// `MISSING_AUDIO_REASON`) so the summary's "no audio" count is populated.
fn run_one_item(
    item: &CorpusItem,
    audio: &Path,
    backend: &dyn AnyTranscribeBackend,
    spec: &BackendSpec,
) -> Value {
    if !audio.exists() {
        return annotate_event(skipped_event(item, audio, MISSING_AUDIO_REASON), item, spec);
    }
    let started = Instant::now();
    let event_body = match crate::whisper::decode_wav_16k_mono(audio) {
        Ok(pcm) => match backend.transcribe(&pcm, 16_000) {
            Ok(result) => success_event(item, audio, &result),
            Err(e) => failure_event(item, audio, &format!("transcribe failed: {e}")),
        },
        Err(e) => failure_event(item, audio, &format!("decode {}: {e:#}", audio.display())),
    };
    let elapsed = started.elapsed().as_secs_f64();
    let mut event = annotate_event(event_body, item, spec);
    if let Value::Object(map) = &mut event {
        map.insert("benchmark_elapsed_s".to_owned(), json!(elapsed));
    }
    event
}

fn skipped_event(item: &CorpusItem, audio: &Path, reason: &str) -> Value {
    json!({
        "event": "benchmark_result",
        "text": "",
        "raw_text": "",
        "source_file": audio.display().to_string(),
        "benchmark_success": false,
        "benchmark_skipped": true,
        "benchmark_error": reason,
        "corpus_id": item.id,
    })
}

fn success_event(item: &CorpusItem, audio: &Path, result: &TranscribeResult) -> Value {
    let success = !result.text.is_empty();
    json!({
        "event": "benchmark_result",
        "text": result.text,
        "raw_text": result.raw_text,
        "source_file": audio.display().to_string(),
        "recording_s": result.duration_s,
        "audio_duration_s": result.duration_s,
        "compute_s": result.latency_ms as f64 / 1000.0,
        "language": result.language,
        "benchmark_success": success,
        "benchmark_returncode": 0,
        "corpus_id": item.id,
    })
}

fn failure_event(item: &CorpusItem, audio: &Path, error: &str) -> Value {
    json!({
        "event": "benchmark_result",
        "text": "",
        "raw_text": "",
        "source_file": audio.display().to_string(),
        "benchmark_success": false,
        "benchmark_returncode": 1,
        "benchmark_error": error,
        "corpus_id": item.id,
    })
}

/// Attach corpus metadata + WER/CER/term-report to a per-item event so the
/// JSONL row and the scoring aggregate agree — the shape Python's
/// `annotate_event` produces. Kept side-effect free so unit tests can pin
/// the schema without running the backend.
fn annotate_event(mut event: Value, item: &CorpusItem, spec: &BackendSpec) -> Value {
    let Value::Object(map) = &mut event else {
        return event;
    };
    let text = map
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let terms = term_report(item.terms.iter().map(String::as_str), &text);
    map.insert("benchmark_backend_spec".to_owned(), json!(spec.raw));
    map.insert("benchmark_backend".to_owned(), json!(spec.backend));
    map.insert("benchmark_model".to_owned(), json!(spec.model));
    map.insert("corpus_id".to_owned(), json!(item.id));
    map.insert("corpus_category".to_owned(), json!(item.category));
    map.insert("corpus_language".to_owned(), json!(item.language));
    map.insert("reference_text".to_owned(), json!(item.text));
    map.insert("reference_terms".to_owned(), json!(item.terms));
    map.insert("wer".to_owned(), json!(wer(&item.text, &text)));
    map.insert("cer".to_owned(), json!(cer(&item.text, &text)));
    map.insert(
        "exact_match".to_owned(),
        json!(normalize_words(&item.text) == normalize_words(&text)),
    );
    map.insert("term_hits".to_owned(), json!(terms.hits));
    map.insert("term_misses".to_owned(), json!(terms.misses));
    event
}

/// Emit one JSONL line to `out`, ensure_ascii=False style (serde_json is
/// UTF-8 by default). Mirrors the retired Python worker's per-item output so
/// the same downstream tooling (log tail, jq filters) works unchanged.
fn emit_jsonl(event: &Value, out: &mut dyn Write) {
    if let Ok(line) = serde_json::to_string(event) {
        let _ = writeln!(out, "{line}");
    }
}

/// Pull the scoring-relevant fields off a rich per-item event into the
/// summary-shape [`BenchmarkEvent`] the reporting layer aggregates over.
/// Missing fields fall back to safe defaults (success=false, no WER/CER)
/// so a malformed event contributes to totals but not to averages.
fn scoring_event_from(event: &Value) -> BenchmarkEvent {
    let map = match event {
        Value::Object(m) => m,
        _ => return BenchmarkEvent::default(),
    };
    BenchmarkEvent {
        benchmark_success: map
            .get("benchmark_success")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        benchmark_skipped: map
            .get("benchmark_skipped")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        benchmark_error: map
            .get("benchmark_error")
            .and_then(Value::as_str)
            .map(str::to_owned),
        wer: map.get("wer").and_then(Value::as_f64),
        cer: map.get("cer").and_then(Value::as_f64),
    }
}

/// Test-only stub backend that returns a caller-supplied fixed string. Used
/// by the runner unit tests to exercise the full JSONL + summary path
/// without a live cloud call or a whisper.cpp model load.
#[cfg(test)]
pub(crate) struct FixedTextBackend {
    pub text: String,
}

#[cfg(test)]
impl AnyTranscribeBackend for FixedTextBackend {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String> {
        let duration_s = pcm.len() as f64 / f64::from(sample_rate.max(1));
        Ok(TranscribeResult {
            text: self.text.clone(),
            raw_text: self.text.clone(),
            duration_s,
            latency_ms: 1,
            ..Default::default()
        })
    }
}

// Path resolution helpers used above; kept here rather than re-exported at
// the module root so callers outside the runner do not depend on them.
#[allow(dead_code, unused_imports)]
use paths_use::*;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
