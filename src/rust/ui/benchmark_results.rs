//! Pure parser + display model + localized strings for the "Run benchmark"
//! results view (System tab).
//!
//! The worker's `--run-benchmark` prints one `benchmark_result` JSON object per
//! corpus item on stdout, then a final `[benchmark] …` summary line. The
//! background task captures the whole stdout and hands it here once finished;
//! [`parse_benchmark_results`] scans it line-by-line (tolerating interleaved log
//! noise and non-`benchmark_result` JSON), turning each per-item object into a
//! [`BenchmarkRow`] and folding an aggregate [`BenchmarkSummary`] over them. The
//! System tab then renders a coloured headline + a worst-WER-first table instead
//! of the raw JSONL wall the user used to read in the runtime log.
//!
//! Kept pure and free of egui so it unit-tests without a UI, and self-contained
//! (its own localized strings, NOT added to the shared `UiTextKey` enum) so the
//! feature stays mergeable alongside parallel `text.rs` edits — mirroring the
//! `corpus_record.rs` pattern.
//!
//! ## Field-name contract (from `vp_benchmark.py` / `vp_benchmark_report.py`)
//! Each per-item object carries: `event="benchmark_result"`, `corpus_id`,
//! `corpus_language`, `corpus_category`, `benchmark_success` (bool),
//! `benchmark_skipped` (bool), `benchmark_error` (string|absent), `wer`/`cer`
//! (0..1 floats), `exact_match` (bool), `term_hits`/`term_misses` (arrays). The
//! aggregate mirrors `summarize_results`: averages are over the SCORED rows only
//! (not skipped, WER-bearing), and `avg_cer` is over the scored rows that
//! actually carry a `cer` field (matching the prior `vp_benchmark_report` fix).

use serde::Deserialize;

/// Localized strings for the benchmark-results view. Self-contained (not added
/// to the shared `UiTextKey` enum) so this feature stays mergeable alongside
/// parallel `text.rs` edits. EN + DA are both provided so the headline, table
/// columns and status words read natively in either UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum BenchmarkText {
    /// Section label for the results view ("Benchmark results").
    SectionLabel,
    /// The "Show raw output" toggle that reveals the captured JSONL.
    ShowRaw,
    /// Table column: the corpus item id.
    ColItem,
    /// Table column: the language tag.
    ColLang,
    /// Table column: word error rate, shown as a percentage.
    ColWer,
    /// Table column: character error rate, shown as a percentage.
    ColCer,
    /// Table column: per-item status (scored / skipped / error).
    ColStatus,
    /// Headline/word: count of scored items.
    Scored,
    /// Headline/word + status: skipped items.
    Skipped,
    /// Headline/word + status: errored items.
    Error,
    /// Status word for a scored row.
    StatusScored,
    /// Headline fragment: "avg WER".
    AvgWer,
    /// Headline fragment: "avg CER".
    AvgCer,
    /// Note shown when no per-item rows were parsed but raw output is present.
    NoRowsParsed,
}

impl BenchmarkText {
    fn label(self, danish: bool) -> &'static str {
        if danish {
            self.danish()
        } else {
            self.english()
        }
    }

    fn english(self) -> &'static str {
        match self {
            Self::SectionLabel => "Benchmark results",
            Self::ShowRaw => "Show raw output",
            Self::ColItem => "Item",
            Self::ColLang => "Lang",
            Self::ColWer => "WER%",
            Self::ColCer => "CER%",
            Self::ColStatus => "Status",
            Self::Scored => "scored",
            Self::Skipped => "skipped",
            Self::Error => "error",
            Self::StatusScored => "Scored",
            Self::AvgWer => "avg WER",
            Self::AvgCer => "avg CER",
            Self::NoRowsParsed => "No per-item results were parsed from this run.",
        }
    }

    fn danish(self) -> &'static str {
        match self {
            Self::SectionLabel => "Benchmark-resultater",
            Self::ShowRaw => "Vis rå output",
            Self::ColItem => "Element",
            Self::ColLang => "Sprog",
            Self::ColWer => "WER%",
            Self::ColCer => "CER%",
            Self::ColStatus => "Status",
            Self::Scored => "scoret",
            Self::Skipped => "sprunget over",
            Self::Error => "fejl",
            Self::StatusScored => "Scoret",
            Self::AvgWer => "gns. WER",
            Self::AvgCer => "gns. CER",
            Self::NoRowsParsed => "Ingen resultater pr. element blev fortolket fra dette kørsel.",
        }
    }
}

/// Resolve a benchmark-results string for the active UI language. Mirrors the
/// raw `"da"` → Danish mapping used everywhere else (any other value is
/// English), kept local so this module stays self-contained for clean parallel
/// merges.
pub(in crate::ui) fn benchmark_text(raw_language: &str, key: BenchmarkText) -> &'static str {
    key.label(raw_language == "da")
}

/// The per-item status the view groups + colours rows by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum BenchmarkStatus {
    /// Item ran and was scored (real WER/CER).
    Scored,
    /// Item was skipped (e.g. audio missing) — not scored.
    Skipped,
    /// Item ran but errored (no usable transcription / non-zero exit).
    Error,
}

/// The raw `benchmark_result` object fields the view reads. Every field is
/// `#[serde(default)]` so a missing key never fails the whole parse — a
/// truncated/partial object degrades gracefully (e.g. no `wer` → unscored row).
#[derive(Debug, Clone, Deserialize)]
struct RawResult {
    #[serde(default)]
    event: String,
    #[serde(default)]
    corpus_id: Option<String>,
    #[serde(default)]
    corpus_language: Option<String>,
    #[serde(default)]
    corpus_category: Option<String>,
    #[serde(default)]
    benchmark_success: bool,
    #[serde(default)]
    benchmark_skipped: bool,
    #[serde(default)]
    benchmark_error: Option<String>,
    #[serde(default)]
    wer: Option<f32>,
    #[serde(default)]
    cer: Option<f32>,
    #[serde(default)]
    exact_match: bool,
    #[serde(default)]
    term_hits: Vec<String>,
    #[serde(default)]
    term_misses: Vec<String>,
}

/// One parsed per-item benchmark row, ready for the table.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) struct BenchmarkRow {
    /// The corpus item id (falls back to "?" when the object lacks one).
    pub(in crate::ui) id: String,
    /// The language tag (e.g. "da"/"en"), empty when absent.
    pub(in crate::ui) language: String,
    /// The corpus category, empty when absent.
    pub(in crate::ui) category: String,
    pub(in crate::ui) status: BenchmarkStatus,
    /// Word error rate as a 0..1 fraction (None for skipped/unscored rows).
    pub(in crate::ui) wer: Option<f32>,
    /// Character error rate as a 0..1 fraction (None when the row lacks `cer`).
    pub(in crate::ui) cer: Option<f32>,
    pub(in crate::ui) exact_match: bool,
    pub(in crate::ui) term_hits: usize,
    pub(in crate::ui) term_misses: usize,
    /// The skip/error reason, when one was reported.
    pub(in crate::ui) error: Option<String>,
}

/// The aggregate summary folded over the rows. Mirrors `summarize_results` in
/// `vp_benchmark_report.py`: averages are over the SCORED rows only, and
/// `avg_cer` is over the scored rows that actually carry a `cer` value.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) struct BenchmarkSummary {
    pub(in crate::ui) total: usize,
    pub(in crate::ui) scored: usize,
    pub(in crate::ui) skipped: usize,
    pub(in crate::ui) error: usize,
    /// Mean WER over scored rows (0..1), `None` when nothing was scored.
    pub(in crate::ui) avg_wer: Option<f32>,
    /// Mean CER over scored rows that carry a `cer` (0..1), `None` when none do.
    pub(in crate::ui) avg_cer: Option<f32>,
}

/// The full parsed model the System tab renders + the raw stdout it was built
/// from (kept so a "show raw" affordance can reveal the original JSONL without
/// re-reading the runtime log).
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) struct BenchmarkResults {
    pub(in crate::ui) summary: BenchmarkSummary,
    /// Rows sorted worst-WER-first among scored, then error rows, then skipped
    /// rows last (grey, de-emphasized) — so problems surface at the top.
    pub(in crate::ui) rows: Vec<BenchmarkRow>,
    /// The raw captured stdout (the per-item JSONL + the summary line), kept for
    /// the optional "show raw" panel.
    pub(in crate::ui) raw: String,
}

impl BenchmarkResults {
    /// Whether the run produced any parseable per-item rows. An empty model is
    /// rendered as a localized "no results" placeholder rather than an empty
    /// table.
    pub(in crate::ui) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Classify a raw result object into the view's status. Skipped wins over error
/// (the worker sets `benchmark_skipped=true` for missing audio with a
/// `benchmark_error` reason — that is a skip, not a hard failure), then a
/// non-skipped non-success row is an error, otherwise it scored.
fn status_of(raw: &RawResult) -> BenchmarkStatus {
    if raw.benchmark_skipped {
        BenchmarkStatus::Skipped
    } else if !raw.benchmark_success {
        BenchmarkStatus::Error
    } else {
        BenchmarkStatus::Scored
    }
}

fn row_from_raw(raw: RawResult) -> BenchmarkRow {
    let status = status_of(&raw);
    // Only a scored row carries meaningful WER/CER. A skipped/error object can
    // still carry wer=1.0/cer=1.0 (the worker's `skipped_event` annotates with
    // the empty-hypothesis 1.0), which would skew the table + averages, so the
    // numbers are dropped for non-scored rows.
    let (wer, cer) = match status {
        BenchmarkStatus::Scored => (raw.wer, raw.cer),
        _ => (None, None),
    };
    BenchmarkRow {
        id: raw.corpus_id.unwrap_or_else(|| "?".to_owned()),
        language: raw.corpus_language.unwrap_or_default(),
        category: raw.corpus_category.unwrap_or_default(),
        status,
        wer,
        cer,
        exact_match: raw.exact_match,
        term_hits: raw.term_hits.len(),
        term_misses: raw.term_misses.len(),
        error: raw.benchmark_error,
    }
}

/// Parse one stdout line into a `benchmark_result` row, or `None` for log noise,
/// non-JSON, or JSON whose `event` is not `benchmark_result`. Reuses the shared
/// object extractor so a `benchmark_result` object embedded in a line with a
/// leading log tag still parses.
fn row_from_line(line: &str) -> Option<BenchmarkRow> {
    let span = super::worker_json::extract_json_object(line)?;
    let raw: RawResult = serde_json::from_str(span).ok()?;
    if raw.event != "benchmark_result" {
        return None;
    }
    Some(row_from_raw(raw))
}

/// Fold the parsed rows into the aggregate summary. Averages are over the SCORED
/// rows only; `avg_cer` is over the scored rows that actually carry a `cer`
/// value — both mirroring `summarize_results` in `vp_benchmark_report.py`.
fn summarize(rows: &[BenchmarkRow]) -> BenchmarkSummary {
    let scored: Vec<&BenchmarkRow> = rows
        .iter()
        .filter(|r| r.status == BenchmarkStatus::Scored)
        .collect();
    let skipped = rows
        .iter()
        .filter(|r| r.status == BenchmarkStatus::Skipped)
        .count();
    let error = rows
        .iter()
        .filter(|r| r.status == BenchmarkStatus::Error)
        .count();
    let avg_wer = mean(scored.iter().filter_map(|r| r.wer));
    let avg_cer = mean(scored.iter().filter_map(|r| r.cer));
    BenchmarkSummary {
        total: rows.len(),
        scored: scored.len(),
        skipped,
        error,
        avg_wer,
        avg_cer,
    }
}

/// Mean of an iterator of f32 values, or `None` when it is empty.
fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, count) = values.fold((0.0_f32, 0_usize), |(s, c), v| (s + v, c + 1));
    if count == 0 {
        None
    } else {
        Some(sum / count as f32)
    }
}

/// A sort key that puts scored rows first (worst WER first), then error rows,
/// then skipped rows — so the problems the user can act on surface at the top
/// and the de-emphasized skips sink to the bottom.
fn status_rank(status: BenchmarkStatus) -> u8 {
    match status {
        BenchmarkStatus::Scored => 0,
        BenchmarkStatus::Error => 1,
        BenchmarkStatus::Skipped => 2,
    }
}

/// Sort rows in place: scored (worst WER first) → error → skipped. Within a
/// group, ties break on id for a stable, scannable order.
fn sort_worst_first(rows: &mut [BenchmarkRow]) {
    rows.sort_by(|a, b| {
        status_rank(a.status)
            .cmp(&status_rank(b.status))
            .then_with(|| {
                // Higher WER first among scored. Missing WER sorts last within
                // the (otherwise scored) group, which won't happen in practice
                // since scored rows carry a WER — but it is total + panic-free.
                let aw = a.wer.unwrap_or(f32::NEG_INFINITY);
                let bw = b.wer.unwrap_or(f32::NEG_INFINITY);
                bw.partial_cmp(&aw).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Parse the worker's captured `--run-benchmark` stdout into the full results
/// model: per-item rows (sorted worst-WER-first among scored), the aggregate
/// summary, and the raw stdout retained for the "show raw" panel.
///
/// Tolerant by design — interleaved log lines, non-JSON lines, the trailing
/// `[benchmark] …` summary line, and JSON objects whose `event` is not
/// `benchmark_result` are all skipped. Never panics; missing fields fall back to
/// serde defaults. When no `benchmark_result` rows are present the model is
/// empty (rendered as a localized placeholder).
pub(in crate::ui) fn parse_benchmark_results(stdout: &str) -> BenchmarkResults {
    let mut rows: Vec<BenchmarkRow> = stdout.lines().filter_map(row_from_line).collect();
    let summary = summarize(&rows);
    sort_worst_first(&mut rows);
    BenchmarkResults {
        summary,
        rows,
        raw: stdout.to_owned(),
    }
}

/// Format a 0..1 error-rate fraction as a 1-decimal percentage string, e.g.
/// `0.084` → `"8.4%"`. `None` renders as an em dash so the table cell is never
/// blank. Pure so the headline + table share one formatter and it is testable.
pub(in crate::ui) fn format_rate_percent(rate: Option<f32>) -> String {
    match rate {
        Some(rate) => format!("{:.1}%", rate * 100.0),
        None => "—".to_owned(),
    }
}

/// A plain, language-agnostic one-line summary of a parsed results model, used
/// for the runtime log (the localized headline/table rendering lives in the
/// System tab). Mirrors the headline content so the log + view never disagree.
pub(in crate::ui) fn benchmark_results_log_detail(results: &BenchmarkResults) -> String {
    let s = &results.summary;
    let mut parts = vec![format!("{}/{} scored", s.scored, s.total)];
    if let Some(avg) = s.avg_wer {
        parts.push(format!("avg WER {:.1}%", avg * 100.0));
    }
    if let Some(avg) = s.avg_cer {
        parts.push(format!("avg CER {:.1}%", avg * 100.0));
    }
    if s.skipped > 0 {
        parts.push(format!("{} skipped", s.skipped));
    }
    if s.error > 0 {
        parts.push(format!("{} error", s.error));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic multi-line stdout: a leading version banner + log noise, then
    // a mix of scored / skipped-no-audio / errored `benchmark_result` objects,
    // an unrelated JSON event, and the trailing `[benchmark]` summary line. The
    // numbers are chosen so the averages are easy to verify by hand.
    const SAMPLE: &str = "whisper-dictate 1.12.0
[cap] probing device
{\"event\":\"benchmark_result\",\"text\":\"hej med dig\",\"source_file\":\"a.wav\",\"benchmark_success\":true,\"benchmark_skipped\":false,\"corpus_id\":\"da-short-001\",\"corpus_category\":\"short_danish\",\"corpus_language\":\"da\",\"reference_text\":\"hej med dig\",\"reference_terms\":[\"hej\"],\"wer\":0.1,\"cer\":0.05,\"exact_match\":false,\"term_hits\":[\"hej\"],\"term_misses\":[],\"benchmark_backend\":\"whisper\",\"benchmark_model\":null}
{\"event\":\"benchmark_result\",\"text\":\"goddag\",\"source_file\":\"b.wav\",\"benchmark_success\":true,\"benchmark_skipped\":false,\"corpus_id\":\"da-long-002\",\"corpus_category\":\"long_danish\",\"corpus_language\":\"da\",\"wer\":0.5,\"cer\":0.25,\"exact_match\":false,\"term_hits\":[],\"term_misses\":[\"foo\",\"bar\"]}
{\"event\":\"benchmark_result\",\"text\":\"\",\"source_file\":\"c.wav\",\"benchmark_success\":false,\"benchmark_skipped\":true,\"benchmark_error\":\"audio file missing\",\"corpus_id\":\"da-short-003\",\"corpus_category\":\"short_danish\",\"corpus_language\":\"da\",\"wer\":1.0,\"cer\":1.0,\"exact_match\":false,\"term_hits\":[],\"term_misses\":[\"baz\"]}
{\"event\":\"benchmark_result\",\"text\":\"\",\"source_file\":\"d.wav\",\"benchmark_success\":false,\"benchmark_skipped\":false,\"benchmark_error\":\"model crashed\",\"corpus_id\":\"en-001\",\"corpus_language\":\"en\",\"exact_match\":false}
{\"event\":\"status\",\"state\":\"ready\"}
[benchmark] 2/4 passed, 1 skipped (no audio), 1 failed, avg WER 30.0%, avg CER 15.0%
";

    #[test]
    fn parses_mixed_rows_and_aggregate_over_scored_only() {
        let results = parse_benchmark_results(SAMPLE);
        let s = &results.summary;
        // 4 benchmark_result objects (the status event + log lines are ignored).
        assert_eq!(s.total, 4);
        assert_eq!(s.scored, 2);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.error, 1);
        // Averages over the two SCORED rows only: (0.1 + 0.5)/2 = 0.3,
        // (0.05 + 0.25)/2 = 0.15 — the skipped row's wer=1.0/cer=1.0 must NOT
        // pull the averages up.
        assert!(
            (s.avg_wer.unwrap() - 0.3).abs() < 1e-6,
            "avg_wer={:?}",
            s.avg_wer
        );
        assert!(
            (s.avg_cer.unwrap() - 0.15).abs() < 1e-6,
            "avg_cer={:?}",
            s.avg_cer
        );
    }

    #[test]
    fn skipped_and_error_rows_drop_their_wer_cer() {
        let results = parse_benchmark_results(SAMPLE);
        let skipped = results
            .rows
            .iter()
            .find(|r| r.id == "da-short-003")
            .unwrap();
        assert_eq!(skipped.status, BenchmarkStatus::Skipped);
        // The object carried wer=1.0/cer=1.0 but it is a skip → dropped.
        assert!(skipped.wer.is_none());
        assert!(skipped.cer.is_none());
        assert_eq!(skipped.error.as_deref(), Some("audio file missing"));

        let errored = results.rows.iter().find(|r| r.id == "en-001").unwrap();
        assert_eq!(errored.status, BenchmarkStatus::Error);
        assert_eq!(errored.error.as_deref(), Some("model crashed"));
    }

    #[test]
    fn rows_are_sorted_worst_wer_first_then_error_then_skipped() {
        let results = parse_benchmark_results(SAMPLE);
        let ids: Vec<&str> = results.rows.iter().map(|r| r.id.as_str()).collect();
        // Scored worst-first: da-long-002 (0.5) before da-short-001 (0.1), then
        // the error row (en-001), then the skipped row (da-short-003) last.
        assert_eq!(
            ids,
            ["da-long-002", "da-short-001", "en-001", "da-short-003"]
        );
    }

    #[test]
    fn captures_term_hits_and_misses_counts() {
        let results = parse_benchmark_results(SAMPLE);
        let first = results
            .rows
            .iter()
            .find(|r| r.id == "da-short-001")
            .unwrap();
        assert_eq!(first.term_hits, 1);
        assert_eq!(first.term_misses, 0);
        let second = results.rows.iter().find(|r| r.id == "da-long-002").unwrap();
        assert_eq!(second.term_hits, 0);
        assert_eq!(second.term_misses, 2);
    }

    #[test]
    fn empty_or_noise_only_stdout_yields_empty_results_no_panic() {
        let results = parse_benchmark_results("");
        assert!(results.is_empty());
        assert_eq!(results.summary.total, 0);
        assert!(results.summary.avg_wer.is_none());
        assert!(results.summary.avg_cer.is_none());

        let noise = parse_benchmark_results("[cap] probing\nplain log line\nnot json\n");
        assert!(noise.is_empty());
    }

    #[test]
    fn missing_fields_are_tolerated() {
        // A minimal scored object missing language/category/cer/terms must still
        // parse into a usable row (serde defaults), never panic.
        let stdout = "{\"event\":\"benchmark_result\",\"benchmark_success\":true,\"benchmark_skipped\":false,\"corpus_id\":\"x\",\"wer\":0.2}\n";
        let results = parse_benchmark_results(stdout);
        assert_eq!(results.rows.len(), 1);
        let row = &results.rows[0];
        assert_eq!(row.id, "x");
        assert_eq!(row.language, "");
        assert_eq!(row.status, BenchmarkStatus::Scored);
        assert_eq!(row.wer, Some(0.2));
        assert!(row.cer.is_none());
        assert_eq!(row.term_hits, 0);
    }

    #[test]
    fn object_without_corpus_id_falls_back_to_question_mark() {
        let stdout =
            "{\"event\":\"benchmark_result\",\"benchmark_success\":true,\"benchmark_skipped\":false,\"wer\":0.0}\n";
        let results = parse_benchmark_results(stdout);
        assert_eq!(results.rows[0].id, "?");
    }

    #[test]
    fn non_benchmark_result_objects_are_skipped() {
        // A `corpus_record_done` / `status` object on a line must not become a row.
        let stdout = "{\"event\":\"status\",\"state\":\"ready\"}\n{\"event\":\"corpus_record_done\",\"id\":\"z\"}\n";
        assert!(parse_benchmark_results(stdout).is_empty());
    }

    #[test]
    fn raw_stdout_is_retained_for_the_show_raw_panel() {
        let results = parse_benchmark_results(SAMPLE);
        assert_eq!(results.raw, SAMPLE);
        // The trailing summary line the user already saw stays available verbatim.
        assert!(results.raw.contains("[benchmark] 2/4 passed"));
    }

    #[test]
    fn format_rate_percent_renders_one_decimal_and_dash_for_none() {
        assert_eq!(format_rate_percent(Some(0.084)), "8.4%");
        assert_eq!(format_rate_percent(Some(0.0)), "0.0%");
        assert_eq!(format_rate_percent(Some(1.0)), "100.0%");
        assert_eq!(format_rate_percent(Some(0.031)), "3.1%");
        assert_eq!(format_rate_percent(None), "—");
    }

    #[test]
    fn log_detail_mirrors_the_headline_content() {
        let results = parse_benchmark_results(SAMPLE);
        let detail = benchmark_results_log_detail(&results);
        assert!(detail.contains("2/4 scored"), "{detail}");
        assert!(detail.contains("avg WER 30.0%"), "{detail}");
        assert!(detail.contains("avg CER 15.0%"), "{detail}");
        assert!(detail.contains("1 skipped"), "{detail}");
        assert!(detail.contains("1 error"), "{detail}");
    }

    #[test]
    fn strings_present_in_en_and_da() {
        for key in [
            BenchmarkText::SectionLabel,
            BenchmarkText::ShowRaw,
            BenchmarkText::ColItem,
            BenchmarkText::ColLang,
            BenchmarkText::Scored,
            BenchmarkText::Skipped,
            BenchmarkText::Error,
            BenchmarkText::StatusScored,
            BenchmarkText::AvgWer,
            BenchmarkText::AvgCer,
            BenchmarkText::NoRowsParsed,
        ] {
            let en = benchmark_text("en", key);
            let da = benchmark_text("da", key);
            assert!(!en.is_empty(), "EN empty for {key:?}");
            assert!(!da.is_empty(), "DA empty for {key:?}");
            assert_ne!(en, da, "EN and DA must differ for {key:?}");
        }
        // The WER%/CER% column headers (universal abbreviations) and "Status" (a
        // Danish cognate) are intentionally identical in both languages, so they
        // are checked for presence only — not for divergence.
        for key in [
            BenchmarkText::ColWer,
            BenchmarkText::ColCer,
            BenchmarkText::ColStatus,
        ] {
            assert_eq!(benchmark_text("en", key), benchmark_text("da", key));
            assert!(!benchmark_text("en", key).is_empty());
        }
    }

    #[test]
    fn section_label_is_localized() {
        assert_eq!(
            benchmark_text("en", BenchmarkText::SectionLabel),
            "Benchmark results"
        );
        assert_eq!(
            benchmark_text("da", BenchmarkText::SectionLabel),
            "Benchmark-resultater"
        );
    }
}
