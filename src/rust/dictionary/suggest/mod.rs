//! Suggest dictionary REPLACEMENTS from benchmark / history JSONL rows.
//!
//! Post audit item 4 (`docs/architecture-audit-2026-07-16.md`) this is the
//! sole implementation — the Python `vp_dictionary_suggest.py` parity was
//! retired. Split into smaller files to stay under the repo-wide ~500 LOC
//! per-file gate:
//!
//! * `filters` – curated risky-source word/phrase lists + the shared
//!   `words` / `normalize` helpers
//! * `similarity` – Ratcliff–Obershelp ratio used to score fuzzy matches
//! * `matching` – `SuggestionState`, position-match + fuzzy-match accumulators
//! * `cli` – the `dictionary suggest-replacements` CLI adapter
//!
//! The two match families are documented on the `matching` module. Risky
//! sources (sentence connectors like "the"/"og"/"med", lone 1–2 letter tokens
//! not on a tiny allow-list, etc.) are filtered out so the preview doesn't
//! drown in noise — see `filters`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod cli;
mod filters;
mod matching;
mod similarity;

pub use cli::{run_suggest_replacements, SuggestReplacementsOptions};

use filters::{normalize, words};
use matching::{add_fuzzy_matches, add_position_matches, known_targets, SuggestionState};

/// The live dictionary snapshot the suggester compares against. Provided by the
/// caller (Python passes the `all_terms`/`replacements` snapshot it got from
/// `dictionary-runtime`) so this module stays pure / unit-testable.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DictionarySnapshot {
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub replacements: BTreeMap<String, String>,
}

/// One row from the benchmark / history JSONL the suggester reads. Untyped
/// beyond the well-known fields so the Python emitter is free to add columns.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SuggestRow {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub dictionary_text: Option<String>,
    #[serde(default)]
    pub raw_text: Option<String>,
    #[serde(default)]
    pub reference_text: Option<String>,
    #[serde(default)]
    pub reference_terms: Option<TermsField>,
    #[serde(default)]
    pub term_misses: Option<TermsField>,
    #[serde(default)]
    pub term_hits: Option<TermsField>,
    #[serde(default)]
    pub wer: Option<f64>,
    #[serde(default)]
    pub cer: Option<f64>,
    #[serde(default)]
    pub corpus_id: Option<String>,
    #[serde(default)]
    pub target_title: Option<String>,
    #[serde(default)]
    pub source_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TermsField {
    One(String),
    Many(Vec<String>),
}

impl TermsField {
    fn values(&self) -> Vec<String> {
        match self {
            Self::One(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    Vec::new()
                } else {
                    vec![trimmed.to_owned()]
                }
            }
            Self::Many(values) => values
                .iter()
                .filter_map(|v| {
                    let trimmed = v.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_owned())
                })
                .collect(),
        }
    }

    fn is_truthy(&self) -> bool {
        match self {
            Self::One(value) => !value.trim().is_empty(),
            Self::Many(values) => !values.is_empty(),
        }
    }
}

impl SuggestRow {
    /// Selected transcript text. Mirrors Python's
    /// `row.get("text") or row.get("dictionary_text") or row.get("raw_text")`
    /// — an empty string is treated as falsy so a populated
    /// `dictionary_text` / `raw_text` still reaches the suggester even when
    /// `text` is `""`.
    fn text(&self) -> &str {
        fn non_empty(opt: Option<&str>) -> Option<&str> {
            opt.filter(|s| !s.is_empty())
        }
        non_empty(self.text.as_deref())
            .or_else(|| non_empty(self.dictionary_text.as_deref()))
            .or_else(|| non_empty(self.raw_text.as_deref()))
            .unwrap_or("")
    }

    fn sample(&self) -> String {
        self.corpus_id
            .clone()
            .or_else(|| self.target_title.clone())
            .or_else(|| self.source_file.clone())
            .unwrap_or_default()
    }

    fn missed_terms(&self) -> Vec<String> {
        self.term_misses
            .as_ref()
            .map(TermsField::values)
            .unwrap_or_default()
    }

    fn reference_text(&self) -> &str {
        self.reference_text.as_deref().unwrap_or("")
    }

    /// Mirrors Python's `_has_reference_context` — uses truthiness, so
    /// `wer: 0.0` / `cer: 0.0` do NOT count as reference context (Python's
    /// `row.get("wer")` short-circuits on a falsy `0.0`). Without this, the
    /// fallback "scan the whole dictionary" path is wrongly suppressed for
    /// history rows that always carry zero metrics.
    fn has_reference_context(&self) -> bool {
        self.reference_text
            .as_deref()
            .is_some_and(|t| !t.is_empty())
            || self
                .reference_terms
                .as_ref()
                .is_some_and(TermsField::is_truthy)
            || self.term_misses.as_ref().is_some_and(TermsField::is_truthy)
            || self.term_hits.as_ref().is_some_and(TermsField::is_truthy)
            || self.wer.is_some_and(|v| v != 0.0)
            || self.cer.is_some_and(|v| v != 0.0)
    }

    pub(super) fn row_term_values(&self) -> Vec<String> {
        let mut out = Vec::new();
        for values in [&self.reference_terms, &self.term_misses, &self.term_hits]
            .into_iter()
            .flatten()
        {
            out.extend(values.values());
        }
        out
    }
}

/// Replacement suggestion ready for the preview / `--json` output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplacementSuggestion {
    pub source: String,
    pub target: String,
    pub count: usize,
    pub confidence: f64,
    pub reason: String,
    pub samples: Vec<String>,
}

/// Suggest replacements for a list of rows. Mirrors the Python entry-point.
pub fn suggest_replacements_from_rows(
    rows: &[SuggestRow],
    snapshot: &DictionarySnapshot,
    min_confidence: f64,
) -> Vec<ReplacementSuggestion> {
    let targets = known_targets(rows, snapshot);
    let mut state = SuggestionState::new(snapshot);
    for row in rows {
        let text = row.text();
        if text.is_empty() {
            continue;
        }
        let sample = row.sample();
        let missed_terms = row.missed_terms();
        let row_targets: Vec<String> = if !missed_terms.is_empty() {
            missed_terms.clone()
        } else if row.has_reference_context() {
            Vec::new()
        } else {
            targets.clone()
        };
        let text_words = words(text);
        let reference_words = words(row.reference_text());
        let text_norm = normalize(text);
        add_position_matches(
            &text_words,
            &reference_words,
            &missed_terms,
            &sample,
            &mut state,
        );
        add_fuzzy_matches(
            &text_words,
            &text_norm,
            &row_targets,
            &missed_terms,
            min_confidence,
            &sample,
            &mut state,
        );
    }
    state.into_suggestions()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn dictionary(terms: &[&str], replacements: &[(&str, &str)]) -> DictionarySnapshot {
        DictionarySnapshot {
            terms: terms.iter().map(|s| (*s).to_owned()).collect(),
            replacements: replacements
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn suggests_replacements_from_benchmark_term_misses() {
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
            "corpus_id":"da-tech-004",
            "text":"Murch branchedes og de plåede den nye version bagefter.",
            "reference_text":"Merge branchen og deploy den nye version bagefter.",
            "term_misses":["merge","deploy"],
            "reference_terms":["merge","deploy"]
        }]"#,
        )
        .unwrap();
        let snapshot = dictionary(&[], &[]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.55);
        let pairs: HashSet<(String, String)> = suggestions
            .iter()
            .map(|s| (s.source.to_lowercase(), s.target.to_lowercase()))
            .collect();
        assert!(pairs.contains(&("murch".to_owned(), "merge".to_owned())));
        assert!(!pairs.contains(&("de".to_owned(), "deploy".to_owned())));
    }

    #[test]
    fn suggest_filters_common_word_sources() {
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
            "corpus_id":"sample",
            "text":"og de til le as skal Code Large MCP Claude Set",
            "reference_text":"vLLM deploy type Codex bullets Hetzner Codex large v3",
            "term_misses":["vLLM","deploy","type","Codex","bullets","Hetzner","large v3","RAG","Claude Code","STT"]
        }]"#,
        )
        .unwrap();
        let snapshot = dictionary(&["MCP", "Claude"], &[]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.55);
        let sources: HashSet<String> = suggestions
            .iter()
            .map(|s| s.source.to_lowercase())
            .collect();
        for forbidden in [
            "og", "de", "til", "le", "as", "skal", "code", "large", "mcp", "claude", "set", "mig",
            "køre", "typ",
        ] {
            assert!(
                !sources.contains(forbidden),
                "forbidden source: {forbidden}"
            );
        }
    }

    #[test]
    fn suggests_dictionary_term_near_misses() {
        let rows: Vec<SuggestRow> =
            serde_json::from_str(r#"[{"text":"Clort kode should work","corpus_id":"sample"}]"#)
                .unwrap();
        let snapshot = dictionary(&["Claude Code"], &[]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.45);
        assert!(suggestions.iter().any(|s| s.target == "Claude Code"));
    }

    #[test]
    fn rows_without_misses_do_not_scan_whole_dictionary() {
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
                "text":"and then continue",
                "reference_text":"and then continue",
                "reference_terms":[],
                "term_misses":[]
            }]"#,
        )
        .unwrap();
        let snapshot = dictionary(&["AMD"], &[]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn does_not_duplicate_existing_replacements() {
        let rows: Vec<SuggestRow> =
            serde_json::from_str(r#"[{"text":"lead death","term_misses":["lead dev"]}]"#).unwrap();
        let snapshot = dictionary(&["lead dev"], &[("lead death", "lead dev")]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.5);
        assert!(!suggestions
            .iter()
            .any(|s| s.source == "lead death" && s.target == "lead dev"));
    }

    #[test]
    fn empty_text_falls_back_to_dictionary_text_then_raw_text() {
        // Mirrors Python `row.get("text") or row.get("dictionary_text") or row.get("raw_text")`:
        // an empty `text` does NOT mask a populated alternate field.
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
                "text":"",
                "dictionary_text":"Clort kode should work",
                "corpus_id":"history-001"
            }]"#,
        )
        .unwrap();
        let snapshot = dictionary(&["Claude Code"], &[]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.45);
        assert!(
            suggestions.iter().any(|s| s.target == "Claude Code"),
            "expected suggestion via dictionary_text fallback, got {suggestions:?}"
        );

        // raw_text also reachable when both text and dictionary_text are empty.
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
                "text":"",
                "dictionary_text":"",
                "raw_text":"Clort kode should work",
                "corpus_id":"history-002"
            }]"#,
        )
        .unwrap();
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.45);
        assert!(
            suggestions.iter().any(|s| s.target == "Claude Code"),
            "expected suggestion via raw_text fallback, got {suggestions:?}"
        );
    }

    #[test]
    fn zero_wer_cer_do_not_suppress_dictionary_wide_scan() {
        // Mirrors Python `_has_reference_context` truthiness: wer/cer of 0.0
        // is falsy in Python's `row.get(...)` check, so a history row with
        // ONLY text + zero metrics MUST still fall through to the
        // dictionary-wide fuzzy scan rather than be treated as a fully
        // annotated benchmark row.
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
                "text":"Clort kode should work",
                "corpus_id":"history-zero-metrics",
                "wer":0.0,
                "cer":0.0
            }]"#,
        )
        .unwrap();
        let snapshot = dictionary(&["Claude Code"], &[]);
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.45);
        assert!(
            suggestions.iter().any(|s| s.target == "Claude Code"),
            "expected dictionary-wide scan to fire for row with zero metrics, got {suggestions:?}"
        );

        // Non-zero wer/cer still suppresses the dictionary-wide scan (the row
        // is treated as fully-annotated benchmark context).
        let rows: Vec<SuggestRow> = serde_json::from_str(
            r#"[{
                "text":"Clort kode should work",
                "corpus_id":"bench-real-metrics",
                "wer":0.42,
                "cer":0.18
            }]"#,
        )
        .unwrap();
        let suggestions = suggest_replacements_from_rows(&rows, &snapshot, 0.45);
        assert!(
            suggestions.is_empty(),
            "non-zero wer/cer should suppress the dictionary-wide scan, got {suggestions:?}"
        );
    }
}
