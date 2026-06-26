//! Per-run summary aggregation + the one-line `[benchmark] ...` string.
//!
//! Split out of `benchmark` per the AGENTS.md modularity guideline so each
//! file stays well under ~500 LOC. Pure + I/O-free — these helpers operate on
//! already-decoded JSONL events and a finished summary struct, so they are
//! cheap to unit-test in isolation and identical whether the run was driven
//! by the UI button or the `whisper-dictate bench` CLI.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::MISSING_AUDIO_REASON;

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
