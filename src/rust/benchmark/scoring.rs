//! Tokenisation, edit-distance, WER/CER and term-presence helpers.
//!
//! Split out of `benchmark` per the AGENTS.md modularity guideline so each
//! file stays well under ~500 LOC. All public functions here are pure +
//! allocation-light so they are cheap to unit-test in isolation and so a
//! future fully-Rust benchmark runner can call them without dragging in the
//! `runtime` shell-out machinery.
//!
//! ## Why Unicode casefolding (not lowercasing)
//!
//! The Python reference (`vp_benchmark._normalize_words`,
//! `vp_benchmark_report.term_report`) calls `str.casefold()`, which applies
//! the Unicode *default case folding* mapping (Common + Full). That mapping
//! collapses pairs that `to_lowercase()` leaves distinct, e.g.:
//!   * German `ß` ⇒ `ss` (one-to-many)
//!   * Greek `ς` and `σ` ⇒ `σ`
//!   * `İ` (U+0130, Turkish dotted capital I) ⇒ `i` + combining dot above
//!   * `ﬁ`, `ﬂ` and other Latin ligatures ⇒ `fi`, `fl`, …
//!
//! `str::to_lowercase` follows `Lowercase_Mapping` from UnicodeData.txt,
//! which does **not** include these one-to-many or "compatibility" mappings —
//! so a Rust port using it would diverge from the Python WER for any corpus
//! item or dictionary term that hits one of those characters
//! (`wer("Straße", "STRASSE")` reporting 1.0 instead of 0.0). We therefore
//! route everything that the Python code casefolded through the `caseless`
//! crate's `default_case_fold`, which mirrors `str.casefold()` exactly.

use caseless::Caseless;
use serde::Serialize;

/// Result of `term_report`: per Python, two ordered lists preserving the
/// original term order so the UI's hits / misses display is stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TermReport {
    pub hits: Vec<String>,
    pub misses: Vec<String>,
}

/// Lower-case via Unicode default case folding (Common + Full mappings).
///
/// Equivalent to Python's `str.casefold()`. Prefer this over
/// `str::to_lowercase` anywhere we need case-insensitive matching for
/// benchmark scoring, so corpus items or dictionary terms containing
/// `ß` / `İ` / Latin ligatures collapse to the same form on both sides of
/// the comparison.
pub fn casefold(text: &str) -> String {
    text.chars().default_case_fold().collect()
}

/// Tokenise `text` exactly like Python's `re.findall(r"[\wæøåÆØÅ]+",
/// text.casefold(), flags=re.UNICODE)`.
///
/// "Word characters" here means Unicode alphanumerics + `_` + the Danish vowels
/// `æ ø å` (case-folded). We avoid pulling in a regex crate by walking codepoints
/// directly — the rule is small and deterministic.
pub fn normalize_words(text: &str) -> Vec<String> {
    let lowered = casefold(text);
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
    // explicit æ/ø/å (and their uppercase, but casefolding lowers them first).
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
///
/// Uses [`casefold`] (Unicode default case folding) on both the haystack and
/// each needle, so terms containing `ß` / `İ` / ligatures match the same way
/// Python's `term.casefold() in hypothesis.casefold()` does.
pub fn term_report<I, S>(terms: I, hypothesis: &str) -> TermReport
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let haystack = casefold(hypothesis);
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for term in terms {
        let term = term.as_ref();
        if haystack.contains(&casefold(term)) {
            hits.push(term.to_owned());
        } else {
            misses.push(term.to_owned());
        }
    }
    TermReport { hits, misses }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn casefold_expands_sharp_s() {
        // Python: "Straße".casefold() == "strasse"
        // str::to_lowercase would leave "straße" — the Codex P2 finding.
        assert_eq!(casefold("Straße"), "strasse");
        assert_eq!(casefold("STRASSE"), "strasse");
    }

    #[test]
    fn casefold_lowers_latin_ligatures() {
        // `ﬁ` (U+FB01) — Python casefolds to "fi"; to_lowercase leaves it alone.
        assert_eq!(casefold("ﬁle"), "file");
        assert_eq!(casefold("ﬂow"), "flow");
    }

    #[test]
    fn casefold_handles_turkish_dotted_capital_i() {
        // Python: "İ".casefold() == "i\u{0307}" (i + combining dot above).
        assert_eq!(casefold("İ"), "i\u{0307}");
    }

    #[test]
    fn normalize_words_collapses_sharp_s_to_double_s() {
        // Regression for the Codex P2 casefold finding: WER between cased
        // and uncased forms of `ß` must be 0 (not 1) because both sides
        // case-fold to "strasse".
        assert_eq!(normalize_words("Straße"), vec!["strasse"]);
        assert_eq!(normalize_words("STRASSE"), vec!["strasse"]);
    }

    #[test]
    fn wer_handles_german_sharp_s_via_casefold() {
        // Regression for the Codex P2 casefold finding.
        assert_eq!(wer("Straße", "STRASSE"), 0.0);
        assert_eq!(wer("Die Straße ist breit", "die strasse ist breit"), 0.0);
    }

    #[test]
    fn term_report_matches_via_unicode_casefold() {
        // Regression for the Codex P2 casefold finding on `term_report`:
        // a `ß`-bearing term must hit a `ss` hypothesis (and vice versa),
        // because both sides casefold to the same string.
        let report = term_report(["Straße"], "Die strasse ist breit");
        assert_eq!(report.hits, vec!["Straße".to_owned()]);
        assert!(report.misses.is_empty());

        let report = term_report(["strasse"], "Die Straße ist breit");
        assert_eq!(report.hits, vec!["strasse".to_owned()]);
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
}
