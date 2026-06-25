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

use std::path::Path;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::runtime;

/// Skip reason recorded when an item's audio is missing in every search path.
/// Mirrors `vp_benchmark_report.MISSING_AUDIO_REASON` so the summary's "all
/// skipped for missing audio" hint triggers on identical events.
pub const MISSING_AUDIO_REASON: &str = "audio file missing";

const ALLOWED_BACKENDS: [&str; 3] = ["whisper", "parakeet", "openai"];

/// Parsed `backend[:model]` entry. `model` is `None` when the spec omits the
/// `:` separator OR when the trailing model is blank — matches Python's
/// `model.strip() if sep else None` then `model or None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    pub raw: String,
    pub backend: String,
    pub model: Option<String>,
}

/// Aggregated counts/averages over a corpus run. `avg_wer` / `avg_cer` are
/// `None` when no scored row contributes one — same contract as the Python
/// dict the UI summary line reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub skipped_no_audio: usize,
    pub scored: usize,
    pub avg_wer: Option<f64>,
    pub avg_cer: Option<f64>,
}

/// One benchmark result event as the Python worker writes it (JSONL). Only the
/// fields the Rust scoring/reporting code reads are typed; the rest of the
/// envelope (text, IDs, terms…) is irrelevant to [`summarize_results`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BenchmarkEvent {
    #[serde(default)]
    pub benchmark_success: bool,
    #[serde(default)]
    pub benchmark_skipped: bool,
    #[serde(default)]
    pub benchmark_error: Option<String>,
    #[serde(default)]
    pub wer: Option<f64>,
    #[serde(default)]
    pub cer: Option<f64>,
}

/// Result of `term_report`: per Python, two ordered lists preserving the
/// original term order so the UI's hits / misses display is stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TermReport {
    pub hits: Vec<String>,
    pub misses: Vec<String>,
}

/// Tokenise `text` exactly like Python's `re.findall(r"[\wæøåÆØÅ]+",
/// text.casefold(), flags=re.UNICODE)`.
///
/// "Word characters" here means Unicode alphanumerics + `_` + the Danish vowels
/// `æ ø å` (case-folded). We avoid pulling in a regex crate by walking codepoints
/// directly — the rule is small and deterministic.
pub fn normalize_words(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in lowered.chars() {
        if is_word_char(ch) {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_word_char(ch: char) -> bool {
    // Python's `\w` under re.UNICODE is "letter, digit, or underscore" plus the
    // explicit æ/ø/å (and their uppercase, but `casefold` lowers them first).
    ch.is_alphanumeric() || ch == '_'
}

/// Edit distance between two token slices. Allocation-light O(min(m, n)) by
/// keeping a single previous-row vector — direct port of the Python double-loop
/// in `_levenshtein`.
pub fn levenshtein<T: Eq>(a: &[T], b: &[T]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, x) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let cost = if x == y { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Word error rate. Returns 0.0 when both reference and hypothesis are empty,
/// 1.0 when the reference is empty but the hypothesis is not (matches the
/// Python "0.0 if not hyp else 1.0" branch).
pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r = normalize_words(reference);
    let h = normalize_words(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    levenshtein(&r, &h) as f64 / r.len() as f64
}

/// Character error rate over the *concatenated normalised tokens* (i.e.
/// punctuation/whitespace stripped first). Same shape as `wer`.
pub fn cer(reference: &str, hypothesis: &str) -> f64 {
    let r: String = normalize_words(reference).concat();
    let h: String = normalize_words(hypothesis).concat();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let r_chars: Vec<char> = r.chars().collect();
    let h_chars: Vec<char> = h.chars().collect();
    levenshtein(&r_chars, &h_chars) as f64 / r_chars.len() as f64
}

/// Case-insensitive substring presence check for each dictionary `term` in
/// `hypothesis`. The result preserves term order so the per-item event has a
/// stable hits/misses listing.
pub fn term_report<I, S>(terms: I, hypothesis: &str) -> TermReport
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let haystack = hypothesis.to_lowercase();
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for term in terms {
        let term = term.as_ref();
        if haystack.contains(&term.to_lowercase()) {
            hits.push(term.to_owned());
        } else {
            misses.push(term.to_owned());
        }
    }
    TermReport { hits, misses }
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
        if !ALLOWED_BACKENDS.contains(&backend.as_str()) {
            return Err(anyhow!(
                "unsupported benchmark backend '{backend}'; expected whisper, parakeet or openai"
            ));
        }
        out.push(BackendSpec {
            raw: part.to_owned(),
            backend,
            model,
        });
    }
    if out.is_empty() {
        return Err(anyhow!("at least one benchmark backend is required"));
    }
    Ok(out)
}

/// Collapse per-item benchmark events into an aggregate. Pure & I/O-free so it
/// is unit-testable, matching `vp_benchmark_report.summarize_results`.
pub fn summarize_results(results: &[BenchmarkEvent]) -> BenchmarkSummary {
    let total = results.len();
    let passed = results.iter().filter(|r| r.benchmark_success).count();
    let skipped = results.iter().filter(|r| r.benchmark_skipped).count();
    let skipped_no_audio = results
        .iter()
        .filter(|r| {
            r.benchmark_skipped && r.benchmark_error.as_deref() == Some(MISSING_AUDIO_REASON)
        })
        .count();
    let failed = total.saturating_sub(passed).saturating_sub(skipped);

    let scored: Vec<&BenchmarkEvent> = results
        .iter()
        .filter(|r| !r.benchmark_skipped && r.wer.is_some())
        .collect();
    let avg_wer = if scored.is_empty() {
        None
    } else {
        let sum: f64 = scored.iter().map(|r| r.wer.unwrap_or(0.0)).sum();
        Some(sum / scored.len() as f64)
    };
    let cer_rows: Vec<&&BenchmarkEvent> = scored.iter().filter(|r| r.cer.is_some()).collect();
    let avg_cer = if cer_rows.is_empty() {
        None
    } else {
        let sum: f64 = cer_rows.iter().map(|r| r.cer.unwrap_or(0.0)).sum();
        Some(sum / cer_rows.len() as f64)
    };

    BenchmarkSummary {
        total,
        passed,
        failed,
        skipped,
        skipped_no_audio,
        scored: scored.len(),
        avg_wer,
        avg_cer,
    }
}

/// Render the single-line `[benchmark] ...` summary the UI surfaces verbatim
/// in the runtime log. When EVERY item was skipped purely for missing audio AND
/// an `audio_hint_path` is supplied, a "record corpus audio to <path>" hint is
/// appended — same exact phrasing as the Python implementation.
pub fn format_summary_line(summary: &BenchmarkSummary, audio_hint_path: Option<&Path>) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{}/{} passed", summary.passed, summary.total));
    if summary.skipped > 0 {
        let suffix = if summary.skipped_no_audio == summary.skipped {
            " (no audio)".to_owned()
        } else if summary.skipped_no_audio > 0 {
            format!(" ({} no audio)", summary.skipped_no_audio)
        } else {
            String::new()
        };
        parts.push(format!("{} skipped{}", summary.skipped, suffix));
    }
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }
    if let Some(wer) = summary.avg_wer {
        parts.push(format!("avg WER {:.1}%", wer * 100.0));
    }
    if let Some(cer) = summary.avg_cer {
        parts.push(format!("avg CER {:.1}%", cer * 100.0));
    }
    let mut line = format!("[benchmark] {}", parts.join(", "));
    if let Some(path) = audio_hint_path {
        if summary.total > 0 && summary.skipped_no_audio == summary.total {
            line.push_str(&format!(" — record corpus audio to {}", path.display()));
        }
    }
    line
}

/// CLI entry point for `whisper-dictate bench`. Shells out to the Python
/// worker via the same [`runtime::benchmark_command`] the UI button drives, so
/// the corpus resolution + JSONL output + final `[benchmark] ...` summary line
/// is bit-identical no matter who started the run.
pub fn handle_bench() -> Result<()> {
    runtime::run_foreground(&runtime::benchmark_command())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ev(
        success: bool,
        skipped: bool,
        err: Option<&str>,
        w: Option<f64>,
        c: Option<f64>,
    ) -> BenchmarkEvent {
        BenchmarkEvent {
            benchmark_success: success,
            benchmark_skipped: skipped,
            benchmark_error: err.map(str::to_owned),
            wer: w,
            cer: c,
        }
    }

    #[test]
    fn normalize_words_lowers_and_keeps_danish_vowels() {
        assert_eq!(
            normalize_words("Hej, Læs DENNE sætning!"),
            vec!["hej", "læs", "denne", "sætning"]
        );
    }

    #[test]
    fn normalize_words_handles_empty_and_punct_only() {
        assert!(normalize_words("").is_empty());
        assert!(normalize_words(",.!?").is_empty());
    }

    #[test]
    fn normalize_words_treats_underscore_as_word() {
        assert_eq!(normalize_words("foo_bar baz"), vec!["foo_bar", "baz"]);
    }

    #[test]
    fn levenshtein_basic_distances() {
        assert_eq!(levenshtein::<u8>(b"", b""), 0);
        assert_eq!(levenshtein(b"abc", b""), 3);
        assert_eq!(levenshtein(b"", b"abc"), 3);
        assert_eq!(levenshtein(b"abc", b"abc"), 0);
        assert_eq!(levenshtein(b"kitten", b"sitting"), 3);
    }

    #[test]
    fn wer_matches_python_one_third_case() {
        // The same case the test_benchmark_history Python test pins down.
        let value = wer("Claude Code virker", "Claude virker");
        assert!((value - 1.0 / 3.0).abs() < 1e-9, "got {value}");
    }

    #[test]
    fn wer_empty_reference() {
        assert_eq!(wer("", ""), 0.0);
        assert_eq!(wer("", "hello"), 1.0);
    }

    #[test]
    fn cer_counts_characters() {
        // "abc" vs "abd" → 1 substitution over 3 chars = 1/3.
        let value = cer("abc", "abd");
        assert!((value - 1.0 / 3.0).abs() < 1e-9, "got {value}");
        assert_eq!(cer("", ""), 0.0);
        assert_eq!(cer("", "x"), 1.0);
    }

    #[test]
    fn term_report_preserves_order_and_is_case_insensitive() {
        let report = term_report(["Claude Code", "Codex"], "Claude code works");
        assert_eq!(report.hits, vec!["Claude Code".to_owned()]);
        assert_eq!(report.misses, vec!["Codex".to_owned()]);
    }

    #[test]
    fn parse_backend_specs_supports_models() {
        let specs = parse_backend_specs("whisper:large-v3, parakeet , openai:gpt-4o").unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].backend, "whisper");
        assert_eq!(specs[0].model.as_deref(), Some("large-v3"));
        assert_eq!(specs[1].backend, "parakeet");
        assert_eq!(specs[1].model, None);
        assert_eq!(specs[2].backend, "openai");
        assert_eq!(specs[2].model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn parse_backend_specs_rejects_unknown() {
        let err = parse_backend_specs("cloud:gpt-4o").unwrap_err();
        assert!(err.to_string().contains("unsupported benchmark backend"));
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

    #[test]
    fn summarize_results_counts_and_averages() {
        let results = vec![
            ev(true, false, None, Some(0.0), Some(0.0)),
            ev(true, false, None, Some(0.2), Some(0.1)),
            // Skipped row: must NOT contribute to scored averages.
            ev(false, true, Some(MISSING_AUDIO_REASON), Some(1.0), None),
            // Hard failure: no wer field, contributes only to total + failed.
            ev(false, false, Some("boom"), None, None),
        ];
        let s = summarize_results(&results);
        assert_eq!(s.total, 4);
        assert_eq!(s.passed, 2);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.skipped_no_audio, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.scored, 2);
        assert!((s.avg_wer.unwrap() - 0.1).abs() < 1e-9);
        assert!((s.avg_cer.unwrap() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn summarize_results_all_skipped() {
        let s = summarize_results(&[ev(false, true, Some(MISSING_AUDIO_REASON), Some(1.0), None)]);
        assert_eq!(s.total, 1);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.skipped_no_audio, 1);
        assert!(s.avg_wer.is_none());
        assert!(s.avg_cer.is_none());
    }

    #[test]
    fn summarize_results_cer_divides_by_cer_bearing_rows() {
        // Two scored rows, only one has cer → avg_cer divides by 1, not 2.
        let results = vec![
            ev(true, false, None, Some(0.2), Some(0.4)),
            ev(true, false, None, Some(0.4), None),
        ];
        let s = summarize_results(&results);
        assert!((s.avg_wer.unwrap() - 0.3).abs() < 1e-9);
        assert!((s.avg_cer.unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn summarize_results_cer_none_when_no_row_has_cer() {
        let results = vec![
            ev(true, false, None, Some(0.2), None),
            ev(true, false, None, Some(0.4), None),
        ];
        let s = summarize_results(&results);
        assert!((s.avg_wer.unwrap() - 0.3).abs() < 1e-9);
        assert!(s.avg_cer.is_none());
    }

    #[test]
    fn format_summary_line_renders_concise_line() {
        let s = BenchmarkSummary {
            total: 3,
            passed: 2,
            failed: 1,
            skipped: 0,
            skipped_no_audio: 0,
            scored: 2,
            avg_wer: Some(0.1),
            avg_cer: Some(0.05),
        };
        let line = format_summary_line(&s, None);
        assert_eq!(
            line,
            "[benchmark] 2/3 passed, 1 failed, avg WER 10.0%, avg CER 5.0%"
        );
    }

    #[test]
    fn format_summary_line_appends_audio_hint_when_all_skipped_for_missing_audio() {
        let s = BenchmarkSummary {
            total: 2,
            passed: 0,
            failed: 0,
            skipped: 2,
            skipped_no_audio: 2,
            scored: 0,
            avg_wer: None,
            avg_cer: None,
        };
        let path = PathBuf::from("/tmp/audio");
        let line = format_summary_line(&s, Some(&path));
        assert!(line.contains("0/2 passed"));
        assert!(line.contains("2 skipped (no audio)"));
        assert!(line.contains("record corpus audio to "));
    }

    #[test]
    fn format_summary_line_no_audio_hint_when_some_scored() {
        let s = BenchmarkSummary {
            total: 2,
            passed: 1,
            failed: 0,
            skipped: 1,
            skipped_no_audio: 1,
            scored: 1,
            avg_wer: Some(0.0),
            avg_cer: None,
        };
        let path = PathBuf::from("/tmp/audio");
        let line = format_summary_line(&s, Some(&path));
        assert!(!line.contains("record corpus audio"));
    }

    #[test]
    fn format_summary_line_mixed_skips_show_no_audio_count() {
        let s = BenchmarkSummary {
            total: 5,
            passed: 1,
            failed: 0,
            skipped: 4,
            skipped_no_audio: 2,
            scored: 1,
            avg_wer: None,
            avg_cer: None,
        };
        let line = format_summary_line(&s, None);
        assert!(line.contains("4 skipped (2 no audio)"));
    }

    #[test]
    fn benchmark_event_deserialises_from_minimal_jsonl() {
        // The Python worker emits much fatter envelopes; we only care about the
        // scoring-relevant fields. serde(default) must cope with the rest.
        let json = r#"{"event":"benchmark_result","text":"hi","benchmark_success":true,"wer":0.1}"#;
        let ev: BenchmarkEvent = serde_json::from_str(json).unwrap();
        assert!(ev.benchmark_success);
        assert_eq!(ev.wer, Some(0.1));
        assert!(ev.cer.is_none());
        assert!(!ev.benchmark_skipped);
    }
}
