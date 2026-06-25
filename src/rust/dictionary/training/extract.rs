//! Mine domain-term candidates from corpus reference text. The heuristic mix:
//! capitalised tokens, multi-word capitalised runs ("Claude Code"), all-caps
//! acronyms ("MCP", "RAG"), digit-mixed identifiers ("large-v3"), studly-caps
//! product names ("vLLM"). Each term is counted once per corpus item;
//! frequency-filtered via `min_count`; curated terms (`item_terms`) are always
//! kept regardless of `min_count`.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::{normalize, words, TermCandidate};

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

fn first_char_is_upper(token: &str) -> bool {
    token
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
}

pub(super) fn is_capitalized_token(token: &str) -> bool {
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
/// module docs for the heuristic mix; `min_count` filters one-off noise
/// (curated terms always survive regardless of `min_count`).
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
}
