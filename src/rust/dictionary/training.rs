//! Pure corpus → dictionary training helpers (port of
//! `vp_dictionary_training.py`, Wave 4-A of the Python-removal roadmap #348).
//!
//! Two flavours of "grow the prompt vocabulary":
//!
//! 1. [`extract_candidate_terms`] mines proper nouns / technical tokens out of
//!    corpus reference TEXT — capitalised tokens, multi-word capitalised runs
//!    ("Claude Code"), all-caps acronyms ("MCP", "RAG"), digit-mixed identifiers
//!    ("large-v3"), studly-caps product names ("vLLM"). Each term counted once
//!    per corpus item, frequency-filtered, curated terms always kept.
//! 2. [`suggest_terms_from_misses`] surfaces the domain terms the benchmark
//!    GOT WRONG (annotated `term_misses`) as SUGGESTED additions — preview-only.
//!
//! [`merge_terms`] is the common case-insensitive append helper used to turn
//! the candidate list into a [`MergePreview`] (what would be added vs already
//! present) without writing the file.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// A proposed dictionary term plus *why* it was proposed (for the preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCandidate {
    pub term: String,
    #[serde(default = "default_one")]
    pub count: usize,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub samples: Vec<String>,
}

/// A SUGGESTED dictionary term from benchmark misses (preview, confirm first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSuggestion {
    pub term: String,
    #[serde(default = "default_one")]
    pub count: usize,
    #[serde(default)]
    pub samples: Vec<String>,
    #[serde(default)]
    pub already_in_dictionary: bool,
}

/// Outcome of merging candidates into the existing dictionary (no IO done).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MergePreview {
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub skipped_existing: Vec<String>,
    #[serde(default)]
    pub result_terms: Vec<String>,
    #[serde(default)]
    pub existing_count: usize,
}

impl MergePreview {
    pub fn added_count(&self) -> usize {
        self.added.len()
    }
}

fn default_one() -> usize {
    1
}

static WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\wÀ-ɏ]+(?:[.\-][\wÀ-ɏ]+)*").unwrap());
static SEGMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[^,;:.!?()\[\]{}"']+"#).unwrap());

/// Sentence-initial / generic words that are capitalised only because they
/// start a sentence — not domain terms. Lower-cased. Matches the Python set
/// (English + Danish) verbatim.
const STOPWORDS: &[&str] = &[
    // English
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "create", "do", "for", "from",
    "i", "if", "in", "is", "it", "its", "me", "new", "of", "on", "or", "please", "run", "set",
    "should", "tell", "the", "their", "them", "then", "they", "this", "to", "via", "want",
    "whether", "with", "you", // Danish
    "af", "behold", "bliver", "brug", "commit", "der", "det", "du", "eller", "en", "er", "et",
    "fra", "gerne", "her", "hvis", "hvor", "ikke", "jeg", "kan", "lad", "lave", "lige", "med",
    "modellen", "nye", "og", "om", "op", "opret", "os", "på", "se", "skal", "skift", "som",
    "stadig", "så", "til", "tjek", "vi", "vil", "være",
];

fn is_stopword(token: &str) -> bool {
    let lower = token.to_lowercase();
    STOPWORDS.iter().any(|word| *word == lower)
}

fn normalize(term: &str) -> String {
    term.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn words(text: &str) -> Vec<String> {
    WORD_RE
        .find_iter(text)
        .map(|m| m.as_str().to_owned())
        .collect()
}

fn first_char_is_upper(token: &str) -> bool {
    token
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
}

fn is_capitalized_token(token: &str) -> bool {
    if token.is_empty() || !first_char_is_upper(token) {
        return false;
    }
    if is_stopword(token) {
        return false;
    }
    if token.chars().count() == 1 {
        return false;
    }
    token.chars().any(|c| c.is_alphabetic())
}

fn is_technical_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let letters: Vec<char> = token.chars().filter(|c| c.is_alphabetic()).collect();
    if !letters.is_empty() && letters.iter().all(|c| c.is_uppercase()) && token.chars().count() >= 2
    {
        return true;
    }
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_alpha = token.chars().any(|c| c.is_alphabetic());
    if has_digit && has_alpha {
        return true;
    }
    if (token.contains('-') || token.contains('.')) && token.chars().count() >= 4 {
        return true;
    }
    if token.chars().count() >= 3 && token.chars().skip(1).any(|c| c.is_uppercase()) {
        return true;
    }
    false
}

fn multi_word_candidates(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    for segment in SEGMENT_RE.find_iter(text) {
        let mut current: Vec<String> = Vec::new();
        for token in words(segment.as_str()) {
            if is_capitalized_token(&token) {
                current.push(token);
                continue;
            }
            if current.len() >= 2 {
                runs.push(current.join(" "));
            }
            current.clear();
        }
        if current.len() >= 2 {
            runs.push(current.join(" "));
        }
    }
    runs
}

fn single_token_candidates(text: &str) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for token in words(text) {
        if is_technical_token(&token) {
            out.push((token, "technical"));
        } else if is_capitalized_token(&token) {
            out.push((token, "capitalized"));
        }
    }
    out
}

fn item_hits(text: &str, curated: &[String]) -> Vec<(String, &'static str)> {
    let mut hits: Vec<(String, &'static str)> = curated
        .iter()
        .filter_map(|term| {
            let cleaned = term.trim();
            if cleaned.is_empty() {
                None
            } else {
                Some((cleaned.to_owned(), "curated_term"))
            }
        })
        .collect();
    for run in multi_word_candidates(text) {
        hits.push((run, "multi_word"));
    }
    hits.extend(single_token_candidates(text));
    hits
}

#[derive(Default)]
struct CandidateAccumulator {
    counts: BTreeMap<String, usize>,
    insertion: Vec<String>,
    reasons: BTreeMap<String, &'static str>,
    surface: BTreeMap<String, String>,
    samples: BTreeMap<String, Vec<String>>,
    curated: HashSet<String>,
}

impl CandidateAccumulator {
    fn add_item(&mut self, text: &str, curated: &[String], sample_id: &str) {
        let mut counted: HashSet<String> = HashSet::new();
        for (term, reason) in item_hits(text, curated) {
            if let Some(norm) = self.record_surface(&term, reason, sample_id) {
                if counted.insert(norm.clone()) {
                    *self.counts.entry(norm).or_insert(0) += 1;
                }
            }
        }
    }

    fn record_surface(
        &mut self,
        term: &str,
        reason: &'static str,
        sample_id: &str,
    ) -> Option<String> {
        let cleaned = term.trim();
        let norm = normalize(cleaned);
        if norm.is_empty() {
            return None;
        }
        let first_seen = !self.surface.contains_key(&norm);
        let curated_upgrade =
            reason == "curated_term" && self.reasons.get(&norm).copied() != Some("curated_term");
        if first_seen {
            self.insertion.push(norm.clone());
        }
        if first_seen || curated_upgrade {
            self.surface.insert(norm.clone(), cleaned.to_owned());
            self.reasons.insert(norm.clone(), reason);
        }
        if reason == "curated_term" {
            self.curated.insert(norm.clone());
        }
        if !sample_id.is_empty() {
            let entry = self.samples.entry(norm.clone()).or_default();
            if !entry.iter().any(|s| s == sample_id) {
                entry.push(sample_id.to_owned());
            }
        }
        Some(norm)
    }

    fn results(&self, min_count: usize) -> Vec<TermCandidate> {
        let threshold = std::cmp::max(1, min_count);
        let mut candidates: Vec<TermCandidate> = self
            .insertion
            .iter()
            .filter_map(|norm| {
                let count = *self.counts.get(norm).unwrap_or(&0);
                if !self.curated.contains(norm) && count < threshold {
                    return None;
                }
                Some(TermCandidate {
                    term: self
                        .surface
                        .get(norm)
                        .cloned()
                        .unwrap_or_else(|| norm.clone()),
                    count,
                    reason: self.reasons.get(norm).copied().unwrap_or("").to_owned(),
                    samples: self
                        .samples
                        .get(norm)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .take(5)
                        .collect(),
                })
            })
            .collect();
        candidates.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.term.to_lowercase().cmp(&b.term.to_lowercase()))
        });
        candidates
    }
}

/// Mine domain-term candidates from corpus reference texts (pure). See the
/// module docs for the heuristic mix; `min_count` filters one-off noise (curated
/// terms always survive regardless of `min_count`).
pub fn extract_candidate_terms(
    texts: &[String],
    item_terms: Option<&[Vec<String>]>,
    item_ids: Option<&[String]>,
    min_count: usize,
) -> Vec<TermCandidate> {
    let curated_per_item = item_terms.map(<[Vec<String>]>::to_vec).unwrap_or_default();
    let ids = aligned_ids(item_ids, texts.len());
    let mut acc = CandidateAccumulator::default();
    for (index, text) in texts.iter().enumerate() {
        let curated_slice: &[String] = curated_per_item
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let id = ids.get(index).map(String::as_str).unwrap_or("");
        acc.add_item(text, curated_slice, id);
    }
    acc.results(min_count)
}

fn aligned_ids(item_ids: Option<&[String]>, n: usize) -> Vec<String> {
    let mut ids: Vec<String> = item_ids.map(<[String]>::to_vec).unwrap_or_default();
    if ids.len() < n {
        ids.resize(n, String::new());
    }
    ids
}

/// Append candidate terms to `existing`, deduping case-insensitively. Mirrors
/// the Python `merge_terms` — `candidates` may be plain strings; existing
/// terms are preserved in order; the `skipped_existing` list is deduped by
/// normalised form so "Kubectl" + "kubectl" against an existing "kubectl"
/// register only once as a skip.
pub fn merge_terms(existing: &[String], candidates: &[String]) -> MergePreview {
    let existing_terms: Vec<String> = existing
        .iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    let mut seen: HashSet<String> = existing_terms.iter().map(|t| normalize(t)).collect();
    let mut skipped_seen: HashSet<String> = HashSet::new();
    let mut preview = MergePreview {
        result_terms: existing_terms.clone(),
        existing_count: existing_terms.len(),
        ..MergePreview::default()
    };
    for candidate in candidates {
        let term = candidate.trim();
        let norm = normalize(term);
        if norm.is_empty() {
            continue;
        }
        if seen.contains(&norm) {
            if skipped_seen.insert(norm) {
                preview.skipped_existing.push(term.to_owned());
            }
            continue;
        }
        seen.insert(norm);
        preview.added.push(term.to_owned());
        preview.result_terms.push(term.to_owned());
    }
    preview
}

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
                samples: samples.get(norm).cloned().unwrap_or_default(),
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

    fn texts(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| (*s).to_owned()).collect()
    }

    fn ids(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| (*s).to_owned()).collect()
    }

    fn item_terms(strs: &[&[&str]]) -> Vec<Vec<String>> {
        strs.iter()
            .map(|inner| inner.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    }

    #[test]
    fn curated_terms_are_always_kept() {
        let cands = extract_candidate_terms(
            &texts(&["plain lowercase sentence with nothing special"]),
            Some(&item_terms(&[&["Parakeet", "git tag"]])),
            Some(&ids(&["a"])),
            5,
        );
        let terms: HashSet<String> = cands.iter().map(|c| c.term.clone()).collect();
        assert!(terms.contains("Parakeet"));
        assert!(terms.contains("git tag"));
    }

    #[test]
    fn capitalized_multi_word_run_detected() {
        let cands = extract_candidate_terms(
            &texts(&["Jeg tester Claude Code i Windows Terminal."]),
            None,
            Some(&ids(&["a"])),
            1,
        );
        let terms: HashSet<String> = cands.iter().map(|c| c.term.clone()).collect();
        assert!(terms.contains("Claude Code"));
        assert!(terms.contains("Windows Terminal"));
    }

    #[test]
    fn comma_list_does_not_collapse_into_one_phrase() {
        let cands = extract_candidate_terms(
            &texts(&["OpenClaw, MCP, RAG og vLLM."]),
            None,
            Some(&ids(&["a"])),
            1,
        );
        let terms: HashSet<String> = cands.iter().map(|c| c.term.clone()).collect();
        assert!(!terms.contains("OpenClaw MCP RAG"));
        assert!(terms.contains("MCP"));
        assert!(terms.contains("RAG"));
        assert!(terms.contains("vLLM"));
    }

    #[test]
    fn technical_tokens_detected() {
        let cands = extract_candidate_terms(
            &texts(&["Run with large-v3 and the RTX server."]),
            None,
            Some(&ids(&["a"])),
            1,
        );
        let terms: HashSet<String> = cands.iter().map(|c| c.term.clone()).collect();
        assert!(terms.contains("large-v3"));
        assert!(terms.contains("RTX"));
    }

    #[test]
    fn sentence_initial_stopword_not_a_candidate() {
        let cands = extract_candidate_terms(
            &texts(&["Skift backend til parakeet.", "Run the build now."]),
            None,
            Some(&ids(&["a", "b"])),
            1,
        );
        let lower: HashSet<String> = cands.iter().map(|c| c.term.to_lowercase()).collect();
        assert!(!lower.contains("skift"));
        assert!(!lower.contains("run"));
    }

    #[test]
    fn min_count_filters_one_off_noise() {
        let cands = extract_candidate_terms(
            &texts(&[
                "Hetzner is mentioned once.",
                "Kubernetes here.",
                "Kubernetes again.",
            ]),
            None,
            Some(&ids(&["a", "b", "c"])),
            2,
        );
        let terms: HashSet<String> = cands.iter().map(|c| c.term.clone()).collect();
        assert!(terms.contains("Kubernetes"));
        assert!(!terms.contains("Hetzner"));
    }

    #[test]
    fn samples_capture_item_ids() {
        let cands = extract_candidate_terms(
            &texts(&["Kubernetes cluster."]),
            Some(&item_terms(&[&["Kubernetes"]])),
            Some(&ids(&["da-tech-003"])),
            1,
        );
        let kube = cands.iter().find(|c| c.term == "Kubernetes").unwrap();
        assert!(kube.samples.iter().any(|s| s == "da-tech-003"));
    }

    #[test]
    fn count_is_per_item_not_per_occurrence() {
        let cands = extract_candidate_terms(
            &texts(&["Kubernetes runs Kubernetes again."]),
            Some(&item_terms(&[&["Kubernetes"]])),
            Some(&ids(&["only"])),
            1,
        );
        let kube = cands.iter().find(|c| c.term == "Kubernetes").unwrap();
        assert_eq!(kube.count, 1);
    }

    #[test]
    fn count_increments_once_per_distinct_item() {
        let cands = extract_candidate_terms(
            &texts(&[
                "Kubernetes here.",
                "Kubernetes Kubernetes there.",
                "nothing",
            ]),
            None,
            Some(&ids(&["a", "b", "c"])),
            1,
        );
        let kube = cands.iter().find(|c| c.term == "Kubernetes").unwrap();
        assert_eq!(kube.count, 2);
    }

    #[test]
    fn single_capital_letter_is_not_a_candidate() {
        assert!(!is_capitalized_token("X"));
        assert!(!is_capitalized_token("A"));
        let cands = extract_candidate_terms(
            &texts(&["X marks Kubernetes."]),
            None,
            Some(&ids(&["a"])),
            1,
        );
        let terms: HashSet<String> = cands.iter().map(|c| c.term.clone()).collect();
        assert!(!terms.contains("X"));
        assert!(terms.contains("Kubernetes"));
    }

    #[test]
    fn merge_appends_new_terms() {
        let preview = merge_terms(
            &["Existing".to_owned()],
            &["New".to_owned(), "Another".to_owned()],
        );
        assert_eq!(preview.added, vec!["New", "Another"]);
        assert_eq!(preview.result_terms, vec!["Existing", "New", "Another"]);
        assert_eq!(preview.existing_count, 1);
    }

    #[test]
    fn merge_dedup_case_insensitive_against_existing() {
        let preview = merge_terms(
            &["Claude Code".to_owned()],
            &["claude code".to_owned(), "Codex".to_owned()],
        );
        assert_eq!(preview.added, vec!["Codex"]);
        assert!(preview.skipped_existing.contains(&"claude code".to_owned()));
    }

    #[test]
    fn merge_skipped_existing_deduped_case_insensitively() {
        let preview = merge_terms(
            &["kubectl".to_owned()],
            &[
                "Kubectl".to_owned(),
                "kubectl".to_owned(),
                "KUBECTL".to_owned(),
            ],
        );
        assert!(preview.added.is_empty());
        assert_eq!(preview.skipped_existing.len(), 1);
    }

    #[test]
    fn merge_dedup_within_candidates() {
        let preview = merge_terms(&[], &["MCP".to_owned(), "mcp".to_owned(), "RAG".to_owned()]);
        assert_eq!(preview.added, vec!["MCP", "RAG"]);
    }

    #[test]
    fn merge_blank_candidates_ignored() {
        let preview = merge_terms(&[], &["  ".to_owned(), "".to_owned(), "Real".to_owned()]);
        assert_eq!(preview.added, vec!["Real"]);
    }

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
}
