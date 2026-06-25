//! Suggest dictionary REPLACEMENTS from benchmark / history JSONL rows.
//!
//! Port of `vp_dictionary_suggest.py` (Wave 4-A of #348). Two match families:
//!
//! * **position match** — when the row has a `reference_text` AND
//!   `term_misses`, walk the hypothesis at the same word position as each
//!   missed term and propose `hypothesis_words → term`. Strongest signal,
//!   gets a high confidence floor (≥0.70).
//! * **fuzzy match** — slide an n-gram window of size around `len(words(target))`
//!   over the hypothesis and propose `ngram → target` when the Ratcliff–
//!   Obershelp ratio against the target is ≥ `min_confidence`. Used both for
//!   missed terms and (when the row has no reference context at all) for
//!   every dictionary term.
//!
//! Risky sources (sentence connectors like "the"/"og"/"med", lone 1–2 letter
//! tokens not on a tiny allow-list, etc.) are filtered out so the preview
//! doesn't drown in noise.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

const COMMON_SOURCE_WORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "be", "but", "by", "for", "from", "i", "in", "is", "it", "le",
    "of", "og", "on", "or", "skal", "the", "til", "to", "with", "de", "den", "det", "der", "du",
    "en", "et", "jeg", "kan", "med", "mig", "på", "så", "vi", "eller", "set", "fra", "type",
];

const COMMON_SOURCE_PHRASES: &[&str] = &[
    "begge",
    "begge forstå",
    "code",
    "code i",
    "consulting",
    "day",
    "kode",
    "large",
    "le code",
    "le terminal",
    "terminal",
    "two",
    "whisper",
    "claude",
    "consulting 2d",
    "contre celui",
    "dæv eller",
    "eller brød",
    "faktus consulting 2d",
    "faktus consulting og",
    "kom",
    "kobberites klosteret",
    "kodex versus",
    "køre",
    "køre klosteret",
    "large-v3 and",
    "mcp",
    "mcp rac",
    "pisit backend",
    "que",
    "serveren til remote lokal postprocessing",
    "serveren til remote lokal postprocessing.",
    "sit",
    "signal-to-noise-ratio tydelig i terminalen",
    "signal-to-noise-ratio tydelig i terminalen.",
    "typ",
    "voice pisit",
    "ændringen pudst",
    "ændringen pudste",
];

const SHORT_SOURCE_ALLOWLIST: &[&str] = &[
    "2d", "dbfs", "qn", "rac", "rag", "snr", "stt", "ui", "vad", "vlm", "xkb",
];

static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w.\-]+").unwrap());

fn words(text: &str) -> Vec<String> {
    WORD_RE
        .find_iter(text)
        .map(|m| m.as_str().to_owned())
        .collect()
}

fn normalize(text: &str) -> String {
    words(text).join(" ").to_lowercase()
}

fn is_common_source_word(token: &str) -> bool {
    COMMON_SOURCE_WORDS.contains(&token)
}

fn is_short_allowlisted(token: &str) -> bool {
    SHORT_SOURCE_ALLOWLIST.contains(&token)
}

fn is_common_source_phrase(phrase: &str) -> bool {
    COMMON_SOURCE_PHRASES.contains(&phrase)
}

fn is_risky_source(source: &str) -> bool {
    let normalised = normalize(source);
    if normalised.is_empty() {
        return true;
    }
    let normalised_words: Vec<&str> = normalised.split_whitespace().collect();
    if is_common_source_phrase(&normalised) {
        return true;
    }
    if normalised_words.len() == 1 {
        let word = normalised_words[0];
        return is_common_source_word(word)
            || (word.chars().count() <= 2 && !is_short_allowlisted(word));
    }
    if is_common_source_word(normalised_words[0])
        || is_common_source_word(normalised_words[normalised_words.len() - 1])
    {
        return true;
    }
    if normalised_words.len() <= 3 && normalised_words.iter().any(|w| is_common_source_word(w)) {
        return true;
    }
    false
}

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
    fn text(&self) -> &str {
        self.text
            .as_deref()
            .or(self.dictionary_text.as_deref())
            .or(self.raw_text.as_deref())
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
            || self.wer.is_some()
            || self.cer.is_some()
    }

    fn row_term_values(&self) -> Vec<String> {
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

#[derive(Default)]
struct SuggestionState {
    existing: HashSet<(String, String)>,
    known_sources: HashSet<String>,
    counts: BTreeMap<(String, String), usize>,
    insertion: Vec<(String, String)>,
    best: BTreeMap<(String, String), f64>,
    samples: BTreeMap<(String, String), Vec<String>>,
    reasons: BTreeMap<(String, String), String>,
}

impl SuggestionState {
    fn new(snapshot: &DictionarySnapshot) -> Self {
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

    fn into_suggestions(self) -> Vec<ReplacementSuggestion> {
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

fn known_targets(rows: &[SuggestRow], snapshot: &DictionarySnapshot) -> Vec<String> {
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

fn add_position_matches(
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

fn add_fuzzy_matches(
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

/// Ratcliff–Obershelp similarity ratio (matches Python's
/// `difflib.SequenceMatcher(None, a, b).ratio()` closely enough to share
/// confidence thresholds with the Python implementation). Operates on Unicode
/// `char`s, not raw bytes, so multi-byte letters compare correctly.
pub fn ratcliff_obershelp_ratio(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let total = a_chars.len() + b_chars.len();
    if total == 0 {
        return 1.0;
    }
    let matches = matching_chars(&a_chars, &b_chars);
    (2.0 * matches as f64) / (total as f64)
}

fn matching_chars(a: &[char], b: &[char]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (a_start, b_start, length) = longest_common_substring(a, b);
    if length == 0 {
        return 0;
    }
    let mut total = length;
    if a_start > 0 && b_start > 0 {
        total += matching_chars(&a[..a_start], &b[..b_start]);
    }
    let a_end = a_start + length;
    let b_end = b_start + length;
    if a_end < a.len() && b_end < b.len() {
        total += matching_chars(&a[a_end..], &b[b_end..]);
    }
    total
}

fn longest_common_substring(a: &[char], b: &[char]) -> (usize, usize, usize) {
    if a.is_empty() || b.is_empty() {
        return (0, 0, 0);
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut curr = vec![0usize; b.len() + 1];
    let mut best_a = 0usize;
    let mut best_b = 0usize;
    let mut best_len = 0usize;
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            if a[i - 1] == b[j - 1] {
                let length = prev[j - 1] + 1;
                curr[j] = length;
                if length > best_len {
                    best_len = length;
                    best_a = i - length;
                    best_b = j - length;
                }
            } else {
                curr[j] = 0;
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        for cell in curr.iter_mut() {
            *cell = 0;
        }
    }
    (best_a, best_b, best_len)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ratcliff_obershelp_basic() {
        // difflib.SequenceMatcher(None, "abcd", "abcd").ratio() == 1.0
        assert!((ratcliff_obershelp_ratio("abcd", "abcd") - 1.0).abs() < 1e-9);
        // SequenceMatcher(None, "", "").ratio() == 1.0 (Python convention)
        assert!((ratcliff_obershelp_ratio("", "") - 1.0).abs() < 1e-9);
        // SequenceMatcher(None, "abc", "xyz").ratio() == 0.0
        assert!(ratcliff_obershelp_ratio("abc", "xyz") < 1e-9);
        // "murch" vs "merge": LCS="r", recurse on "m" vs "me" (m matches=1)
        // and on "ch" vs "ge" (0). Total matches = 2; ratio = 2*2/(5+5) = 0.4
        let r = ratcliff_obershelp_ratio("murch", "merge");
        assert!((r - 0.4).abs() < 1e-9, "got {r}");
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
}
