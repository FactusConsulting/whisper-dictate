//! Position-match + fuzzy n-gram match accumulation for the replacement
//! suggester. The two match families:
//!
//! * **position match** — when the row has a `reference_text` AND
//!   `term_misses`, walk the hypothesis at the same word position as each
//!   missed term and propose `hypothesis_words → term`. Gets a confidence
//!   floor of 0.70 because it's the strongest signal.
//! * **fuzzy match** — slide an n-gram window of size around `len(words(target))`
//!   over the hypothesis and propose `ngram → target` when the
//!   Ratcliff–Obershelp ratio against the target is ≥ `min_confidence`.

use std::collections::{BTreeMap, HashSet};

use super::filters::{is_risky_source, normalize, words};
use super::similarity::ratcliff_obershelp_ratio;
use super::{DictionarySnapshot, ReplacementSuggestion, SuggestRow};

#[derive(Default)]
pub(super) struct SuggestionState {
    pub(super) existing: HashSet<(String, String)>,
    known_sources: HashSet<String>,
    counts: BTreeMap<(String, String), usize>,
    insertion: Vec<(String, String)>,
    best: BTreeMap<(String, String), f64>,
    samples: BTreeMap<(String, String), Vec<String>>,
    reasons: BTreeMap<(String, String), String>,
}

impl SuggestionState {
    pub(super) fn new(snapshot: &DictionarySnapshot) -> Self {
        let existing: HashSet<(String, String)> = snapshot
            .replacements
            .iter()
            .map(|(src, dst)| (normalize(src), normalize(dst)))
            .collect();
        let known_sources: HashSet<String> = snapshot
            .terms
            .iter()
            .map(|t| normalize(t))
            .filter(|t| !t.is_empty())
            .collect();
        Self {
            existing,
            known_sources,
            ..Self::default()
        }
    }

    fn add(&mut self, source: &str, target: &str, confidence: f64, reason: &str, sample: &str) {
        let source = source.trim();
        let target = target.trim();
        let source_norm = normalize(source);
        let target_norm = normalize(target);
        if source_norm.is_empty() || target_norm.is_empty() || source_norm == target_norm {
            return;
        }
        if is_risky_source(source) {
            return;
        }
        if self.known_sources.contains(&source_norm) {
            return;
        }
        if self
            .existing
            .contains(&(source_norm.clone(), target_norm.clone()))
        {
            return;
        }
        let key = (source.to_owned(), target.to_owned());
        let new_entry = !self.counts.contains_key(&key);
        *self.counts.entry(key.clone()).or_insert(0) += 1;
        let best = self.best.entry(key.clone()).or_insert(0.0);
        if confidence > *best {
            *best = confidence;
        }
        self.reasons.insert(key.clone(), reason.to_owned());
        if !sample.is_empty() {
            let entry = self.samples.entry(key.clone()).or_default();
            if !entry.iter().any(|s| s == sample) {
                entry.push(sample.to_owned());
            }
        }
        if new_entry {
            self.insertion.push(key);
        }
    }

    pub(super) fn into_suggestions(self) -> Vec<ReplacementSuggestion> {
        let mut grouped: Vec<ReplacementSuggestion> = self
            .insertion
            .into_iter()
            .map(|key| {
                let count = self.counts.get(&key).copied().unwrap_or(0);
                let confidence = self.best.get(&key).copied().unwrap_or(0.0);
                let reason = self.reasons.get(&key).cloned().unwrap_or_default();
                let samples = self
                    .samples
                    .get(&key)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .take(5)
                    .collect();
                ReplacementSuggestion {
                    source: key.0,
                    target: key.1,
                    count,
                    confidence: (confidence * 1000.0).round() / 1000.0,
                    reason,
                    samples,
                }
            })
            .collect();
        grouped.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then_with(|| a.target.to_lowercase().cmp(&b.target.to_lowercase()))
        });
        grouped
    }
}

pub(super) fn known_targets(rows: &[SuggestRow], snapshot: &DictionarySnapshot) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut targets: Vec<String> = Vec::new();
    let from_dict = snapshot.terms.iter().cloned();
    let from_rows = rows
        .iter()
        .flat_map(|row| row.row_term_values().into_iter());
    for term in from_dict.chain(from_rows) {
        let norm = normalize(&term);
        if !norm.is_empty() && seen.insert(norm) {
            targets.push(term);
        }
    }
    targets
}

fn ngrams(words: &[String], size: usize) -> Vec<String> {
    if size == 0 || words.len() < size {
        return Vec::new();
    }
    (0..=words.len() - size)
        .map(|i| words[i..i + size].join(" "))
        .collect()
}

fn candidate_ngram_sizes(target: &str) -> Vec<usize> {
    let target_len = std::cmp::max(1, words(target).len());
    let mut sizes: HashSet<usize> = HashSet::new();
    if target_len > 1 {
        sizes.insert(target_len - 1);
    }
    sizes.insert(target_len);
    sizes.insert(target_len + 1);
    let mut out: Vec<usize> = sizes.into_iter().filter(|&s| s > 0).collect();
    out.sort_unstable();
    out
}

pub(super) fn add_position_matches(
    words_in: &[String],
    reference_words: &[String],
    missed_terms: &[String],
    sample: &str,
    state: &mut SuggestionState,
) {
    for term in missed_terms {
        let term_words = words(term);
        if term_words.is_empty() {
            continue;
        }
        let size = term_words.len();
        if reference_words.len() < size {
            continue;
        }
        for i in 0..=reference_words.len() - size {
            if normalize(&reference_words[i..i + size].join(" ")) != normalize(term) {
                continue;
            }
            let mut sizes: Vec<usize> = vec![size, size + 1];
            sizes.sort_unstable();
            sizes.dedup();
            for candidate_size in sizes {
                if i + candidate_size > words_in.len() {
                    continue;
                }
                let source = words_in[i..i + candidate_size].join(" ");
                let similarity = ratcliff_obershelp_ratio(&normalize(&source), &normalize(term));
                let confidence = if similarity > 0.70 { similarity } else { 0.70 };
                state.add(
                    &source,
                    term,
                    confidence,
                    "term_miss_position_match",
                    sample,
                );
            }
        }
    }
}

pub(super) fn add_fuzzy_matches(
    words_in: &[String],
    text_norm: &str,
    row_targets: &[String],
    missed_terms: &[String],
    min_confidence: f64,
    sample: &str,
    state: &mut SuggestionState,
) {
    let missed_set: HashSet<&str> = missed_terms.iter().map(String::as_str).collect();
    for target in row_targets {
        let target_norm = normalize(target);
        if target_norm.is_empty() || text_norm.contains(&target_norm) {
            continue;
        }
        for size in candidate_ngram_sizes(target) {
            for source in ngrams(words_in, size) {
                let source_norm = normalize(&source);
                if source_norm == target_norm
                    || state
                        .existing
                        .contains(&(source_norm.clone(), target_norm.clone()))
                {
                    continue;
                }
                let confidence = ratcliff_obershelp_ratio(&source_norm, &target_norm);
                if confidence < min_confidence {
                    continue;
                }
                let reason = if missed_set.contains(target.as_str()) {
                    "term_miss_fuzzy_match"
                } else {
                    "dictionary_fuzzy_match"
                };
                state.add(&source, target, confidence, reason, sample);
            }
        }
    }
}
