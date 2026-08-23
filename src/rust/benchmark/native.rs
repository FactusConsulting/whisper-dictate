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
//! # Configuration source of truth
//!
//! Every setting is resolved via [`crate::config::worker_env_overrides`] —
//! the same `config.json → env → schema-default` layering the Python worker
//! command applies when it spawns a child. Reading raw `env::var` would
//! silently ignore a persisted `stt_backend`/`stt_model`/`lang` when the
//! calling shell has NOT exported the matching `VOICEPI_*` variable (Codex
//! P1 on PRs #625/#626).
//!
//! # Known limitations
//!
//! * Per-spec `spec.model` override on local Whisper — the local builder does
//!   NOT re-resolve the requested GGML file, so `whisper:tiny,whisper:large-v3`
//!   would silently benchmark the same env-selected model twice with
//!   mislabeled rows. The runner rejects such specs up-front (Codex P1 on
//!   PR #625 — `benchmark/native.rs:201`).
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

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};

use super::{
    format_summary_line, parse_backend_specs, summarize_results, wer, BackendSpec, BenchmarkEvent,
    MISSING_AUDIO_REASON,
};
use crate::corpus::{corpus_search_paths, load_corpus, resolve_corpus_manifest, CorpusItem};
use crate::dictate::backends::cloud_transcribe::{
    cloud_backend_local_only_checked, CloudTranscribeConfig,
};
use crate::dictate::backends::CloudTranscribeBackend;
use crate::dictate::{TranscribeBackend, TranscribeResult};
use crate::dictionary::SessionDictionary;
use crate::postprocess::{postprocess_text, settings_from_env_with, PostprocessSettings};

pub mod paths_use {
    // Re-export path helpers here so tests can reach them via the module
    // root; the runner uses the sibling `paths` module directly.
    pub use super::super::paths::{appdata_audio_dir, resolve_item_audio};
}

use super::paths::{appdata_audio_dir, resolve_item_audio};
use super::scoring::{cer, normalize_words, term_report};

/// Outcome of the native runner. `Unsupported` is the fallback signal: the
/// caller ([`super::handle_bench`]) surfaces a "rebuild with these features"
/// hint. `Other` is a real failure and bubbles up as the process exit code.
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

/// Default cloud STT model when the caller selects `stt_backend=openai` but
/// has NOT set `VOICEPI_STT_MODEL` (nor a per-spec `openai:<model>`). Matches
/// the Python benchmark's `spec.model or get_value("VOICEPI_STT_MODEL",
/// "gpt-4o-mini-transcribe")` fallback so a bare `openai` bench never sends
/// requests with an empty `model` field (Codex P1 on PR #625).
const DEFAULT_CLOUD_MODEL: &str = "gpt-4o-mini-transcribe";

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
        writeln!(
            out,
            "[benchmark] no corpus manifest found (looked: {looked}) - \
             see docs/CONFIGURATION.md (Benchmark corpus)"
        )
        .map_err(|e| NativeBenchError::Other(anyhow::anyhow!("write no-manifest line: {e}")))?;
        let _ = out.flush();
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

/// Snapshot of the `VOICEPI_*` env the runner should honour, resolved via
/// [`crate::config::worker_env_overrides`] once at the top of [`run_with_writer`]
/// so a persisted `config.json` beats a stale calling-shell env (Codex P1 on
/// PRs #625/#626). Direct `env::var` still wins when the setting is not in
/// `settings_schema.json` (e.g. `VOICEPI_STT_API_KEY`, `OPENAI_API_KEY`,
/// `GROQ_API_KEY`, `VOICEPI_WHISPER_MODEL_PATH`).
fn resolved_env() -> BTreeMap<String, String> {
    crate::config::worker_env_overrides().into_iter().collect()
}

/// Lookup helper the backend builders share: schema-resolved value first,
/// then the raw process env, then `None`. Empty / whitespace-only strings
/// collapse to `None` so downstream `from_env_with` parsers keep their
/// blank-is-unset semantics.
fn env_lookup<'a>(resolved: &'a BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + 'a {
    move |name: &str| {
        resolved
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    }
}

/// The writer-parameterised core of [`run_with`]. Split out so the UI thread
/// can capture the output into a `Vec<u8>` for its `BackgroundTaskResult`
/// stdout envelope without any process/pipe machinery.
pub fn run_with_writer(
    items: &[CorpusItem],
    appdata: &Path,
    out: &mut dyn Write,
) -> Result<(), NativeBenchError> {
    // Reject an empty corpus BEFORE constructing any backend — the retired
    // Python runner raised "at least one benchmark file or corpus item is
    // required" for the same input, so a manifest with `"items": []` must not
    // exit `0/0 passed` (Codex P2 on PRs #625/#626).
    if items.is_empty() {
        return Err(NativeBenchError::Other(anyhow::anyhow!(
            "at least one benchmark corpus item is required"
        )));
    }

    let resolved = resolved_env();
    let lookup = env_lookup(&resolved);

    let raw_spec = lookup("VOICEPI_STT_BACKEND").unwrap_or_default();
    let spec_str = if raw_spec.trim().is_empty() {
        "whisper"
    } else {
        raw_spec.as_str()
    };
    let specs = parse_backend_specs(spec_str).map_err(NativeBenchError::Other)?;

    // Feature + spec-shape gates BEFORE any I/O so the caller gets a fast,
    // clear reason. `Unsupported` is reserved for build-feature mismatches
    // (the dispatcher turns it into a `--features …` hint); everything else
    // uses `Other` so the message reaches the user verbatim.
    for spec in &specs {
        if spec.backend == "whisper" && !cfg!(feature = "whisper-rs-local") {
            return Err(NativeBenchError::Unsupported(
                "native requires --features whisper-rs-local".to_owned(),
            ));
        }
        // Codex P1 on PR #625: `whisper:<model>` specs would silently reuse
        // the env-cached GGML file for every entry (e.g. `whisper:tiny,
        // whisper:large-v3` benchmarks the same model twice) with each row
        // still LABELED with the requested model. The retired Python path
        // resolved the requested model natively; until the Rust local
        // builder re-resolves it too, reject the spec up-front so the user
        // is not handed mislabeled comparison data.
        if spec.backend == "whisper" && spec.model.is_some() {
            return Err(NativeBenchError::Other(anyhow::anyhow!(
                "local whisper backend does not support per-spec model qualifier \
                 ('{}'); set VOICEPI_WHISPER_MODEL_PATH or the persisted model \
                 setting and drop the ':<model>' suffix. See docs/CONFIGURATION.md \
                 (Benchmark corpus).",
                spec.raw
            )));
        }
        // Codex P2 on PR #625: an `openai` spec without an API key silently
        // passes construction and then produces N failed rows before exit —
        // the Python worker rejected this at argparse time. Validate here
        // so the user sees the failure before touching any audio.
        if spec.backend == "openai" {
            let cloud_cfg = CloudTranscribeConfig::from_env_with(&lookup);
            if cloud_cfg.api_key.is_empty()
                && !crate::privacy::is_loopback_url(cloud_cfg.base_url.trim())
            {
                return Err(NativeBenchError::Other(anyhow::anyhow!(
                    "openai benchmark backend requires a cloud API key \
                     (set VOICEPI_STT_API_KEY, OPENAI_API_KEY, or GROQ_API_KEY \
                     matching the configured base_url)"
                )));
            }
        }
    }

    // Pre-resolve every audio path so both the "skip all when everything is
    // missing" gate (Codex P2) and the per-item skip check see the same
    // per-user fallback dir. When NO recording is present, skip backend
    // construction entirely: on a fresh install the local whisper builder
    // would otherwise fail with "no model" before the runner can emit the
    // "record corpus audio to …" hint.
    let resolved_items: Vec<(&CorpusItem, PathBuf)> = items
        .iter()
        .map(|item| (item, resolve_item_audio(&item.audio, Some(appdata))))
        .collect();
    let any_audio_present = resolved_items.iter().any(|(_, p)| p.exists());

    // Dictionary + postprocess pipeline: loaded ONCE per bench run (matches
    // the Python worker's per-process load; the reloading providers used by
    // the live session are overkill for a short bench pass). Codex P1 on PRs
    // #625/#626 — without these, WER/exact-match/term-hit scores would
    // measure the raw backend text instead of the pipeline the user actually
    // dictates through, so a configured dictionary or post-processor was
    // silently absent from the numbers.
    let dictionary = crate::dictionary::load_session_dictionary_with(&lookup);
    let post_settings = settings_from_env_with(&lookup);

    let mut scoring_events: Vec<BenchmarkEvent> = Vec::new();
    for spec in &specs {
        let backend = if any_audio_present {
            Some(build_backend(spec, &lookup, &dictionary)?)
        } else {
            None
        };
        for (item, audio) in &resolved_items {
            let event = if !audio.exists() {
                annotate_event(skipped_event(item, audio, MISSING_AUDIO_REASON), item, spec)
            } else if let Some(backend) = backend.as_deref() {
                run_one_item(item, audio, backend, spec, &dictionary, &post_settings)
            } else {
                // Unreachable: `any_audio_present` is true iff at least one
                // resolved audio exists, and the outer `!audio.exists()`
                // branch handles the missing ones. Defence-in-depth path so
                // a refactor that skips the branch cannot silently drop rows.
                annotate_event(skipped_event(item, audio, MISSING_AUDIO_REASON), item, spec)
            };
            emit_jsonl(&event, out)?;
            scoring_events.push(scoring_event_from(&event));
        }
    }
    let summary = summarize_results(&scoring_events);
    let hint = appdata_audio_dir(appdata);
    writeln!(out, "{}", format_summary_line(&summary, Some(&hint)))
        .map_err(|e| NativeBenchError::Other(anyhow::anyhow!("write summary line: {e}")))?;
    // Flush after the summary so a piped reader (`tail`, `jq`, the UI's
    // background-task collector) sees the full run even when stdout is
    // block-buffered. Individual JSONL rows are flushed in `emit_jsonl`.
    let _ = out.flush();
    Ok(())
}

/// Object-safe transcribe backend so per-spec build can hand back either the
/// cloud or the local variant behind one type without paying vtable cost on
/// the hot session path — this runner is CLI-only, not the live dictation
/// loop, so an extra indirect call per corpus item is irrelevant.
trait AnyTranscribeBackend {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String>;
    /// Apply this item's language hint before the next `transcribe` call. A
    /// bench corpus can mix languages (Danish + English fixtures in the same
    /// run); without this override the backend would be built once with the
    /// global `VOICEPI_LANG` and every English row would decode with a Danish
    /// hint (Codex P1 on PRs #625/#626). The empty string ("auto") is passed
    /// through as `None` on the override so `effective_language` falls back
    /// to the config hint / auto-detect.
    fn apply_item_language(&self, language: Option<&str>);
}

struct CloudDyn(CloudTranscribeBackend);
impl AnyTranscribeBackend for CloudDyn {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String> {
        self.0
            .transcribe(pcm, sample_rate)
            .map_err(|e| format!("{e:?}"))
    }
    fn apply_item_language(&self, language: Option<&str>) {
        apply_language_via_profile(&self.0, language);
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
    fn apply_item_language(&self, language: Option<&str>) {
        apply_language_via_profile(&self.0, language);
    }
}

/// Shared per-item language override for both backends. Reuses the profile
/// override seam ([`crate::dictate::TranscribeBackend::apply_profile_overrides`])
/// each backend already exposes for the live target-profile matcher, since
/// both wire the resulting override into `effective_language`. The map is
/// scoped to `"language"` only — we do not touch `initial_prompt` or `model`
/// so the reloading-dictionary prompt built at construction stays intact.
fn apply_language_via_profile<B: TranscribeBackend + ?Sized>(backend: &B, language: Option<&str>) {
    let mut overrides = BTreeMap::new();
    if let Some(lang) = language.map(str::trim).filter(|s| !s.is_empty()) {
        overrides.insert("language".to_owned(), lang.to_owned());
    }
    backend.apply_profile_overrides(&overrides);
}

fn build_backend<F>(
    spec: &BackendSpec,
    lookup: &F,
    dictionary: &SessionDictionary,
) -> Result<Box<dyn AnyTranscribeBackend>, NativeBenchError>
where
    F: Fn(&str) -> Option<String>,
{
    match spec.backend.as_str() {
        "openai" => {
            let mut config = CloudTranscribeConfig::from_env_with(lookup);
            // Per-spec model override — Python's `spec.model` → env
            // `VOICEPI_MODEL` (Python worker path). We apply it directly
            // to `config.model` since cloud reads from `VOICEPI_STT_MODEL`
            // in production; the spec model is the caller's explicit
            // choice so it wins over env.
            if let Some(model) = &spec.model {
                config.model = model.clone();
            }
            // Codex P1 on PR #625: when neither the spec NOR the env sets
            // a model, the retired Python path defaulted to
            // "gpt-4o-mini-transcribe". Preserve that default so a bare
            // `stt_backend=openai` bench never posts an empty `model`.
            if config.model.is_empty() {
                config.model = DEFAULT_CLOUD_MODEL.to_owned();
            }
            // Codex P1 on PR #625: fold dictionary terms into the cloud
            // prompt so dictionary-biased runs measure the same prompt
            // the live session would send. Static fold (not
            // `with_reloading_prompt`) — the dictionary is loaded once
            // per bench run and does not need mid-run reloads.
            dictionary.fold_into_prompt(&mut config.prompt);
            // Codex P1 on PR #625: enforce the local-only privacy lock
            // BEFORE constructing the backend. The direct constructor
            // would silently POST corpus audio to a remote endpoint when
            // `VOICEPI_LOCAL_ONLY=1`; the live session rejects the same
            // combination through this guarded constructor and the bench
            // must not have a lower privacy bar.
            let local_only = crate::whisper::model_manager::is_local_only();
            let backend = cloud_backend_local_only_checked(local_only, config).map_err(|e| {
                NativeBenchError::Other(anyhow::anyhow!("cloud backend rejected: {e}"))
            })?;
            Ok(Box::new(CloudDyn(backend)))
        }
        "whisper" => build_local_whisper(spec, dictionary, lookup),
        other => Err(NativeBenchError::Other(anyhow::anyhow!(
            "unsupported benchmark backend '{other}'; expected whisper or openai"
        ))),
    }
}

#[cfg(feature = "whisper-rs-local")]
fn build_local_whisper(
    _spec: &BackendSpec,
    dictionary: &SessionDictionary,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Box<dyn AnyTranscribeBackend>, NativeBenchError> {
    Ok(Box::new(LocalDyn(build_local_whisper_backend(
        dictionary, lookup,
    )?)))
}

#[cfg(feature = "whisper-rs-local")]
fn build_local_whisper_backend(
    dictionary: &SessionDictionary,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<crate::dictate::backends::WhisperLocalTranscribeBackend, NativeBenchError> {
    use crate::dictate::backends::whisper_local::WhisperBackendConfig;
    use crate::dictate::backends::WhisperLocalTranscribeBackend;
    use crate::whisper::{parse_idle_timeout_from_env, IdleUnloadingModel};

    // Reach into `whisper::dispatch` directly instead of via the
    // `crate::whisper` re-export: the crate-level re-export is gated on
    // BOTH `whisper-rs-local` AND `rust-injection`, so a build with just
    // `whisper-rs-local` (release smoke leg, some tests) would otherwise
    // fail to resolve the symbol.
    let model_path = crate::whisper::dispatch::resolve_model_path_from_env().map_err(|e| {
        NativeBenchError::Other(anyhow::anyhow!("resolve whisper model path: {e:#}"))
    })?;
    let idle = parse_idle_timeout_from_env()
        .map_err(|e| NativeBenchError::Other(anyhow::anyhow!("whisper idle timeout: {e:#}")))?;
    let model = IdleUnloadingModel::for_local_whisper(model_path, idle);
    let language = lookup("VOICEPI_LANG");
    let mut initial_prompt = lookup("VOICEPI_INITIAL_PROMPT");
    // Codex P1 on PR #626: fold dictionary terms into the local Whisper
    // `initial_prompt` so a bench with dictionary terms configured measures
    // the same biased prompt the live session sends.
    dictionary.fold_into_prompt(&mut initial_prompt);
    let config = WhisperBackendConfig {
        language,
        initial_prompt,
    };
    Ok(WhisperLocalTranscribeBackend::new(model, config))
}

#[cfg(not(feature = "whisper-rs-local"))]
fn build_local_whisper(
    _spec: &BackendSpec,
    _dictionary: &SessionDictionary,
    _lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Box<dyn AnyTranscribeBackend>, NativeBenchError> {
    // Unreachable — `run_with_writer` rejects a `whisper` spec before this
    // is called on a build without the feature. Kept as a defence-in-depth
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
    dictionary: &SessionDictionary,
    post_settings: &PostprocessSettings,
) -> Value {
    if !audio.exists() {
        return annotate_event(skipped_event(item, audio, MISSING_AUDIO_REASON), item, spec);
    }
    // Apply this item's language hint (empty → auto/None) BEFORE the
    // transcribe call so a mixed-language corpus decodes each row with the
    // right hint (Codex P1 on PRs #625/#626).
    let per_item_lang = if item.language.is_empty() {
        None
    } else {
        Some(item.language.as_str())
    };
    backend.apply_item_language(per_item_lang);
    let started = Instant::now();
    let event_body = match crate::whisper::decode_wav_16k_mono(audio) {
        Ok(pcm) => match backend.transcribe(&pcm, 16_000) {
            Ok(result) => success_event(item, audio, &result, dictionary, post_settings),
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

fn success_event(
    item: &CorpusItem,
    audio: &Path,
    result: &TranscribeResult,
    dictionary: &SessionDictionary,
    post_settings: &PostprocessSettings,
) -> Value {
    // Apply the dictionary replacement table so the benchmarked text matches
    // what the live session would inject. An empty / disabled dictionary is
    // a passthrough. Codex P1 on PRs #625/#626.
    let (dictionary_text, replacements) = dictionary
        .dictionary
        .apply_replacements(&result.text)
        .unwrap_or_else(|_| (result.text.clone(), Vec::new()));
    // Then the post-processor (parity with `vp_audio_file.transcribe_file_event`
    // and the live session's `SessionPostProcess`). `postprocess_text` is a
    // no-op when the processor is `none` / the mode is `raw` / the text is
    // empty, and it falls back to the input on any provider error, so an
    // unconfigured install pays zero cost and a broken post-processor cannot
    // drop the transcript. Codex P1 on PRs #625/#626.
    let post = postprocess_text(&dictionary_text, post_settings);
    let final_text = post.text.clone();
    let success = !final_text.is_empty();
    // `raw_text` fallback: cloud backend intentionally leaves the field
    // empty and expects the session/event layer to fall back to the
    // (pre-dictionary) transcript. Codex P2 on PR #625.
    let raw_text = if result.raw_text.is_empty() {
        result.text.clone()
    } else {
        result.raw_text.clone()
    };
    let dictionary_replacements: Vec<Value> = replacements
        .iter()
        .map(|change| {
            json!({
                "from": change.from,
                "to": change.to,
                "count": change.count,
            })
        })
        .collect();
    json!({
        "event": "benchmark_result",
        "text": final_text,
        "raw_text": raw_text,
        "dictionary_text": dictionary_text,
        "dictionary_replacements": dictionary_replacements,
        "post_processor": post.provider,
        "post_mode": post.mode,
        "post_changed": post.changed,
        "post_fallback": post.fallback,
        "post_error": post.error,
        "post_latency_ms": post.latency_ms,
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

/// Emit one JSONL line to `out`. Flushes after each row so a piped reader
/// (`tail`, `jq`, the UI's background-task collector) sees incremental
/// progress on a long run instead of a batch dump when the buffer fills —
/// matching the retired Python `_write_benchmark_event(sink)` which called
/// `sink.flush()` per row (Codex P2 on PR #625). Write errors propagate so
/// a broken pipe fails the run rather than producing a silently-truncated
/// JSONL (Codex P2 on PR #626).
fn emit_jsonl(event: &Value, out: &mut dyn Write) -> Result<(), NativeBenchError> {
    let line = serde_json::to_string(event)
        .map_err(|e| NativeBenchError::Other(anyhow::anyhow!("serialise benchmark event: {e}")))?;
    writeln!(out, "{line}")
        .map_err(|e| NativeBenchError::Other(anyhow::anyhow!("write benchmark event: {e}")))?;
    let _ = out.flush();
    Ok(())
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
    pub raw_text: String,
    /// Captures the last language passed via `apply_item_language`, so the
    /// per-item-language regression test can assert the runner threaded the
    /// corpus-item hint through instead of reusing the global config hint.
    pub last_language: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl FixedTextBackend {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            raw_text: text.clone(),
            text,
            last_language: std::sync::Mutex::new(None),
        }
    }

    /// Empty `raw_text` variant that mirrors what `CloudTranscribeBackend`
    /// returns on a successful transcription (the session/event layer is
    /// expected to fall back to the transcript). Used by the raw-text
    /// fallback regression test.
    pub fn new_cloud_shape(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            raw_text: String::new(),
            last_language: std::sync::Mutex::new(None),
        }
    }
}

#[cfg(test)]
impl AnyTranscribeBackend for FixedTextBackend {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<TranscribeResult, String> {
        let duration_s = pcm.len() as f64 / f64::from(sample_rate.max(1));
        Ok(TranscribeResult {
            text: self.text.clone(),
            raw_text: self.raw_text.clone(),
            duration_s,
            latency_ms: 1,
            ..Default::default()
        })
    }
    fn apply_item_language(&self, language: Option<&str>) {
        *self.last_language.lock().unwrap() = language.map(str::to_owned);
    }
}

// Path resolution helpers used above; kept here rather than re-exported at
// the module root so callers outside the runner do not depend on them.
#[allow(dead_code, unused_imports)]
use paths_use::*;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
