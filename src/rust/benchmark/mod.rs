//! Pure-logic Rust port of the user-facing benchmark helpers (Wave 6 of the
//! Python-removal roadmap, #348).
//!
//! The actual STT inference + corpus orchestration still lives in
//! `whisper_dictate.vp_benchmark` because the model load, audio decode and the
//! "loop over backend specs" code path is all wrapped around heavyweight Python
//! deps (faster-whisper / parakeet / OpenAI client). Porting that wholesale
//! would be a large rewrite for very little user-visible win, so [`handle_bench`]
//! shells out to the Python worker via the existing
//! [`runtime::benchmark_command`] worker command — the same one the UI's "Run
//! benchmark" button already drives.
//!
//! What DOES live here, ported with full Rust unit-test coverage, are the
//! deterministic scoring + reporting pieces that the Python module exposes for
//! callers and tests:
//!
//!   * [`normalize_words`] / [`levenshtein`] — the same edit-distance pipeline
//!     `vp_benchmark._normalize_words` / `_levenshtein` use, including the
//!     Danish-aware `[\wæøåÆØÅ]+` token regex.
//!   * [`wer`] / [`cer`] — word/character error rates over normalised tokens.
//!   * [`term_report`] — case-insensitive presence check for dictionary terms.
//!   * [`parse_backend_specs`] — the `whisper:large-v3,parakeet` mini-DSL.
//!   * [`summarize_results`] / [`format_summary_line`] — the per-run aggregate
//!     and the one-line `[benchmark] ...` summary the UI surfaces verbatim.
//!
//! These are split off so a future fully-Rust benchmark runner can drop them in
//! without re-deriving the scoring contract, and so the Python wiring can be
//! cross-checked against an independent implementation in CI.
//!
//! Module layout (split per AGENTS.md modularity guideline, ~500 LOC ceiling):
//!   * [`scoring`] — token normalisation, Levenshtein, WER/CER, term matching.
//!   * [`reporting`] — summary aggregation + the one-line `[benchmark]` string.
//!   * `mod.rs` (this file) — shared types, backend-spec parser, and the
//!     thin `handle_bench` CLI dispatcher.

use anyhow::{anyhow, Result};

use crate::runtime;

pub mod reporting;
pub mod scoring;

pub use reporting::{format_summary_line, summarize_results, BenchmarkEvent, BenchmarkSummary};
pub use scoring::{casefold, cer, levenshtein, normalize_words, term_report, wer, TermReport};

/// Skip reason recorded when an item's audio is missing in every search path.
/// Mirrors `vp_benchmark_report.MISSING_AUDIO_REASON` so the summary's "all
/// skipped for missing audio" hint triggers on identical events.
pub const MISSING_AUDIO_REASON: &str = "audio file missing";

const ALLOWED_BACKENDS: [&str; 2] = ["whisper", "openai"];

/// Parsed `backend[:model]` entry. `model` is `None` when the spec omits the
/// `:` separator OR when the trailing model is blank — matches Python's
/// `model.strip() if sep else None` then `model or None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    pub raw: String,
    pub backend: String,
    pub model: Option<String>,
}

/// Parse a comma-separated `backend[:model]` list. Empty entries are skipped
/// (so a stray trailing comma is forgiven); unknown backends are a hard error
/// with the same `unsupported benchmark backend ...` message Python raises.
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
        // System tab's "Run benchmark" path (which calls the Python
        // `parse_backend_specs(None)` after reading
        // `VOICEPI_STT_BACKEND` back through Python) can reach this layer
        // before the save round-trip. Normalise here too so the two
        // implementations stay in lock-step and a copy-pasted
        // `--backends parakeet` from old docs lands on whisper instead of
        // erroring (Codex P2 on PR #410).
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

/// CLI entry point for `whisper-dictate bench`. Shells out to the Python
/// worker via the same [`runtime::benchmark_command`] the UI button drives, so
/// the corpus resolution + JSONL output + final `[benchmark] ...` summary line
/// is bit-identical no matter who started the run. The foreground worker is
/// launched with `PYTHONUTF8=1` / `PYTHONIOENCODING=utf-8` (see
/// `runtime::run_foreground`) so a redirected stdout never mojibakes the
/// Danish corpus text or `ensure_ascii=False` JSONL on Windows.
pub fn handle_bench() -> Result<()> {
    runtime::run_foreground(&runtime::benchmark_command())
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
        // config save round-trip migrates it. Match the Python side
        // (vp_benchmark.parse_backend_specs) and quietly normalise to
        // whisper so an upgraded user benchmarks Whisper instead of hitting
        // an "unsupported benchmark backend" error (Codex P2 on PR #410).
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
}
