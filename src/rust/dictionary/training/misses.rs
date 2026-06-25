//! Surface domain terms the benchmark GOT WRONG (annotated `term_misses`) as
//! SUGGESTED additions — preview-only, the CLI confirms before applying.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use super::{normalize, TermSuggestion};

/// One annotated benchmark result row (what `suggest_terms_from_misses` cares
/// about). Untyped beyond `term_misses` so the Python emitter is free to add
/// columns without breaking the wire contract.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BenchmarkRow {
    #[serde(default)]
    pub corpus_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub source_file: Option<String>,
    /// Either a single missed term or a list (the Python emitter does both).
    #[serde(default)]
    pub term_misses: TermMissesField,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub enum TermMissesField {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl BenchmarkRow {
    fn corpus_id(&self) -> String {
        self.corpus_id
            .clone()
            .or_else(|| self.id.clone())
            .or_else(|| self.source_file.clone())
            .unwrap_or_default()
    }

    fn term_misses(&self) -> Vec<String> {
        match &self.term_misses {
            TermMissesField::None => Vec::new(),
            TermMissesField::One(term) => {
                let trimmed = term.trim();
                if trimmed.is_empty() {
                    Vec::new()
                } else {
                    vec![trimmed.to_owned()]
                }
            }
            TermMissesField::Many(terms) => terms
                .iter()
                .filter_map(|t| {
                    let trimmed = t.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_owned())
                })
                .collect(),
        }
    }
}

/// Maximum sample ids surfaced per suggestion. Mirrors the Python preview
/// (`samples.get(norm, [])[:5]`) so the JSON / CLI payload stays bounded even
/// when a term is missed across a large benchmark.
const SAMPLE_PREVIEW_LIMIT: usize = 5;

/// Surface domain terms the benchmark got wrong as SUGGESTED additions (pure).
/// Counts per row, frequency-filters by `min_count`, flags terms already in the
/// caller-supplied dictionary, sorts by descending count then case-insensitive
/// term. Suggestions are NEVER auto-applied — the CLI confirms first.
pub fn suggest_terms_from_misses(
    rows: &[BenchmarkRow],
    existing_terms: &[String],
    min_count: usize,
) -> Vec<TermSuggestion> {
    let known: HashSet<String> = existing_terms
        .iter()
        .map(|t| normalize(t))
        .filter(|t| !t.is_empty())
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut insertion: Vec<String> = Vec::new();
    let mut surface: BTreeMap<String, String> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for row in rows {
        let sample = row.corpus_id();
        for term in row.term_misses() {
            let norm = normalize(&term);
            if norm.is_empty() {
                continue;
            }
            if counts
                .insert(norm.clone(), counts.get(&norm).copied().unwrap_or(0) + 1)
                .is_none()
            {
                insertion.push(norm.clone());
                surface.insert(norm.clone(), term.clone());
            }
            if !sample.is_empty() {
                let entry = samples.entry(norm.clone()).or_default();
                if !entry.iter().any(|s| s == &sample) {
                    entry.push(sample.clone());
                }
            }
        }
    }
    let threshold = std::cmp::max(1, min_count);
    let mut suggestions: Vec<TermSuggestion> = insertion
        .iter()
        .filter_map(|norm| {
            let count = *counts.get(norm).unwrap_or(&0);
            if count < threshold {
                return None;
            }
            Some(TermSuggestion {
                term: surface.get(norm).cloned().unwrap_or_else(|| norm.clone()),
                count,
                // Cap to SAMPLE_PREVIEW_LIMIT for parity with Python
                // `samples.get(norm, [])[:5]` — keeps the JSON/CLI payload
                // bounded when a term is missed across a large benchmark.
                samples: samples
                    .get(norm)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .take(SAMPLE_PREVIEW_LIMIT)
                    .collect(),
                already_in_dictionary: known.contains(norm),
            })
        })
        .collect();
    suggestions.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.term.to_lowercase().cmp(&b.term.to_lowercase()))
    });
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn miss_rows() -> Vec<BenchmarkRow> {
        let raw = r#"[
            {"corpus_id":"da-tech-004","term_misses":["merge","deploy"]},
            {"corpus_id":"da-tech-001","term_misses":["merge"]},
            {"corpus_id":"en-tech-002","term_misses":["NVIDIA Parakeet"]},
            {"corpus_id":"x","term_misses":[]},
            {"corpus_id":"y"}
        ]"#;
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn suggest_counts_and_sorts_misses() {
        let suggestions = suggest_terms_from_misses(&miss_rows(), &[], 1);
        let merge = suggestions.iter().find(|s| s.term == "merge").unwrap();
        let deploy = suggestions.iter().find(|s| s.term == "deploy").unwrap();
        assert_eq!(merge.count, 2);
        assert_eq!(deploy.count, 1);
        assert_eq!(suggestions[0].term, "merge");
    }

    #[test]
    fn suggest_flags_terms_already_in_dictionary() {
        let suggestions = suggest_terms_from_misses(&miss_rows(), &["Deploy".to_owned()], 1);
        let deploy = suggestions.iter().find(|s| s.term == "deploy").unwrap();
        assert!(deploy.already_in_dictionary);
        let merge = suggestions.iter().find(|s| s.term == "merge").unwrap();
        assert!(!merge.already_in_dictionary);
    }

    #[test]
    fn suggest_min_count_filters() {
        let suggestions = suggest_terms_from_misses(&miss_rows(), &[], 2);
        let terms: HashSet<String> = suggestions.iter().map(|s| s.term.clone()).collect();
        assert_eq!(terms, HashSet::from(["merge".to_owned()]));
    }

    #[test]
    fn suggest_string_term_misses_field_is_tolerated() {
        let raw = r#"[{"corpus_id":"z","term_misses":"merge"}]"#;
        let rows: Vec<BenchmarkRow> = serde_json::from_str(raw).unwrap();
        let suggestions = suggest_terms_from_misses(&rows, &[], 1);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].term, "merge");
    }

    #[test]
    fn suggest_samples_record_corpus_ids() {
        let suggestions = suggest_terms_from_misses(&miss_rows(), &[], 1);
        let merge = suggestions.iter().find(|s| s.term == "merge").unwrap();
        assert!(merge.samples.iter().any(|s| s == "da-tech-004"));
    }

    #[test]
    fn suggest_caps_samples_at_five_for_parity_with_python() {
        // Mirrors Python `samples.get(norm, [])[:5]` — a term missed across
        // many corpus items must NOT spew an unbounded sample list.
        let mut rows: Vec<BenchmarkRow> = Vec::new();
        for i in 0..12 {
            rows.push(
                serde_json::from_str::<BenchmarkRow>(&format!(
                    r#"{{"corpus_id":"sample-{i:02}","term_misses":["merge"]}}"#
                ))
                .unwrap(),
            );
        }
        let suggestions = suggest_terms_from_misses(&rows, &[], 1);
        let merge = suggestions.iter().find(|s| s.term == "merge").unwrap();
        assert_eq!(merge.count, 12, "all 12 hits should be counted");
        assert_eq!(
            merge.samples.len(),
            SAMPLE_PREVIEW_LIMIT,
            "samples must be capped to {SAMPLE_PREVIEW_LIMIT}, got {samples:?}",
            samples = merge.samples
        );
        // Cap keeps the FIRST samples (insertion order).
        assert_eq!(merge.samples[0], "sample-00");
        assert_eq!(merge.samples[4], "sample-04");
    }
}
