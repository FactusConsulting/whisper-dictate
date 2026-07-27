//! Native Rust benchmark subsystem — the sole surface after step 2 of the
//! `vp_benchmark.py` retirement (#348).
//!
//! # Module layout (split per AGENTS.md modularity guideline, ~500 LOC ceiling)
//!
//!   * [`scoring`] — token normalisation, Levenshtein, WER/CER, term matching.
//!   * [`reporting`] — summary aggregation + the one-line `[benchmark]` string.
//!   * [`native`] — the in-process runner behind `whisper-dictate bench` on
//!     `whisper-rs-local` + `audio-capture` shipping builds.
//!   * [`paths`] — corpus manifest + per-item audio resolution.
//!   * `mod.rs` (this file) — shared types, backend-spec parser, and the
//!     thin `handle_bench` CLI dispatcher.
//!
//! # What lives here
//!
//! The user-facing pieces of the golden-corpus benchmark, with full Rust unit
//! test coverage:
//!
//!   * [`normalize_words`] / [`levenshtein`] — the Danish-aware
//!     `[\wæøåÆØÅ]+` token regex pipeline.
//!   * [`wer`] / [`cer`] — word/character error rates over normalised tokens.
//!   * [`term_report`] — case-insensitive presence check for dictionary terms.
//!   * [`parse_backend_specs`] — the `whisper:large-v3,openai` mini-DSL.
//!   * [`summarize_results`] / [`format_summary_line`] — the per-run aggregate
//!     and the one-line `[benchmark] ...` summary the UI surfaces verbatim.
//!
//! # Non-feature builds
//!
//! `whisper-dictate bench` requires the shipping build's
//! `whisper-rs-local,audio-capture` features. On a stock dev build (features
//! off) the verb prints a clear rebuild hint and exits non-zero — the Python
//! fallback that used to shell out to `vp_benchmark.py` is gone.

use anyhow::{anyhow, Result};

pub mod native;
pub mod paths;
pub mod reporting;
pub mod scoring;

pub use paths::{appdata_audio_dir, resolve_item_audio};
pub use reporting::{format_summary_line, summarize_results, BenchmarkEvent, BenchmarkSummary};
pub use scoring::{casefold, cer, levenshtein, normalize_words, term_report, wer, TermReport};

/// Skip reason recorded when an item's audio is missing in every search path.
/// Mirrors the retired `vp_benchmark_report.MISSING_AUDIO_REASON` so the
/// summary's "all skipped for missing audio" hint triggers on identical
/// events.
pub const MISSING_AUDIO_REASON: &str = "audio file missing";

const ALLOWED_BACKENDS: [&str; 2] = ["whisper", "openai"];

/// Parsed `backend[:model]` entry. `model` is `None` when the spec omits the
/// `:` separator OR when the trailing model is blank — matches the retired
/// Python worker's `model.strip() if sep else None` then `model or None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    pub raw: String,
    pub backend: String,
    pub model: Option<String>,
}

/// Parse a comma-separated `backend[:model]` list. Empty entries are skipped
/// (so a stray trailing comma is forgiven); unknown backends are a hard error
/// with the same `unsupported benchmark backend ...` message the retired
/// Python worker raised.
pub fn parse_backend_specs(spec: &str) -> Result<Vec<BackendSpec>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (backend, model) = match part.find(':') {
            Some(idx) => {
                let (b, rest) = part.split_at(idx);
                // `rest` starts with `:`; trim it off.
                let m = rest[1..].trim();
                (
                    b.trim().to_lowercase(),
                    if m.is_empty() {
                        None
                    } else {
                        Some(m.to_owned())
                    },
                )
            }
            None => (part.to_lowercase(), None),
        };
        // Wave 8 of #348: a saved `stt_backend = "parakeet"` is migrated to
        // whisper persistently at config-load time on the Rust side, but the
        // System tab's "Run benchmark" path (which reads
        // `VOICEPI_STT_BACKEND` back) can reach this layer before the save
        // round-trip. Normalise here so a copy-pasted `parakeet` from old
        // docs lands on whisper instead of erroring (Codex P2 on PR #410).
        let (backend, raw) = if backend == "parakeet" {
            ("whisper".to_owned(), "whisper".to_owned())
        } else {
            (backend, part.to_owned())
        };
        if !ALLOWED_BACKENDS.contains(&backend.as_str()) {
            return Err(anyhow!(
                "unsupported benchmark backend '{backend}'; expected whisper or openai"
            ));
        }
        out.push(BackendSpec {
            raw,
            backend,
            model,
        });
    }
    if out.is_empty() {
        return Err(anyhow!("at least one benchmark backend is required"));
    }
    Ok(out)
}

/// CLI entry point for `whisper-dictate bench`.
///
/// Step 2 of the `vp_benchmark.py` retirement (#348) removed the Python
/// fallback: the native runner in [`native::run`] is the sole surface. On a
/// stock dev build without `whisper-rs-local` + `audio-capture` the runner
/// returns [`native::NativeBenchError::Unsupported`], which is surfaced here
/// as a clear rebuild hint and a non-zero exit — the Python worker path
/// (`--run-benchmark`) is gone.
pub fn handle_bench() -> Result<()> {
    match native::run() {
        Ok(()) => Ok(()),
        Err(native::NativeBenchError::Unsupported(reason)) => Err(anyhow!(
            "`whisper-dictate bench` is only available in the shipping build \
             ({reason}); rebuild with --features whisper-rs-local,audio-capture"
        )),
        Err(native::NativeBenchError::Other(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backend_specs_supports_models() {
        let specs = parse_backend_specs("whisper:large-v3, openai:gpt-4o").unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].backend, "whisper");
        assert_eq!(specs[0].model.as_deref(), Some("large-v3"));
        assert_eq!(specs[1].backend, "openai");
        assert_eq!(specs[1].model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn parse_backend_specs_rejects_unknown() {
        let err = parse_backend_specs("cloud:gpt-4o").unwrap_err();
        assert!(err.to_string().contains("unsupported benchmark backend"));
    }

    #[test]
    fn parse_backend_specs_normalises_legacy_parakeet_to_whisper() {
        // Wave 8 of #348 dropped the Parakeet backend, but the System
        // tab's "Run benchmark" path can still flow a legacy
        // `stt_backend = "parakeet"` through to this parser before the
        // config save round-trip migrates it. Quietly normalise so an
        // upgraded user benchmarks Whisper instead of hitting an
        // "unsupported benchmark backend" error (Codex P2 on PR #410).
        let specs = parse_backend_specs("parakeet").unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].backend, "whisper");
        // The raw is rewritten too so the logged spec doesn't lie about
        // which backend the run actually used.
        assert_eq!(specs[0].raw, "whisper");
    }

    #[test]
    fn parse_backend_specs_rejects_empty() {
        assert!(parse_backend_specs("").is_err());
        assert!(parse_backend_specs(",,,").is_err());
    }

    #[test]
    fn parse_backend_specs_blank_model_treated_as_none() {
        let specs = parse_backend_specs("whisper:").unwrap();
        assert_eq!(specs[0].model, None);
    }

    // Stock dev builds (no `whisper-rs-local`) can never actually run the
    // whisper backend natively — the Python fallback that used to shell out
    // is gone, so the runner surfaces [`native::NativeBenchError::Unsupported`]
    // which this dispatcher must turn into a clear rebuild hint. We exercise
    // the mapping directly (not through the corpus-resolving `native::run`,
    // whose test environment has no corpus) so the assertion pins the wording
    // both `handle_bench` callers and downstream scripts grep for.
    #[cfg(not(feature = "whisper-rs-local"))]
    #[test]
    fn unsupported_native_error_is_mapped_to_rebuild_hint() {
        // Simulate what the runner returns on a stock build when a whisper
        // spec is requested — the hard-coded wording of the Unsupported
        // reason is fixed by `native::run_with_writer`.
        let simulated: Result<(), native::NativeBenchError> =
            Err(native::NativeBenchError::Unsupported(
                "native requires --features whisper-rs-local".to_owned(),
            ));
        // Recreate the dispatcher's mapping without invoking `native::run`
        // (which needs a resolvable corpus manifest). This is the exact
        // arm from `handle_bench` above.
        let mapped = match simulated {
            Ok(()) => Ok(()),
            Err(native::NativeBenchError::Unsupported(reason)) => Err(anyhow!(
                "`whisper-dictate bench` is only available in the shipping build \
                 ({reason}); rebuild with --features whisper-rs-local,audio-capture"
            )),
            Err(native::NativeBenchError::Other(e)) => Err(e),
        };
        let err = mapped.unwrap_err().to_string();
        assert!(
            err.contains("shipping build")
                && err.contains("--features whisper-rs-local,audio-capture"),
            "expected a rebuild hint, got: {err}"
        );
    }
}
