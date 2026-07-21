//! Tests for [`super::is_hallucination`] — the stock exact-blacklist filter
//! shared by the local + cloud transcribe backends. Pure (no cargo
//! feature, no model), so these run in the required `rust` matrix on every
//! build.

use super::is_hallucination;

#[test]
fn matches_exact_blacklist_entry() {
    // Most frequent observed false positive on quiet Danish input.
    assert!(is_hallucination("tak"));
    assert!(is_hallucination("Tak"));
    assert!(is_hallucination("TAK"));
}

#[test]
fn matches_with_trailing_whitespace() {
    // Python uses `text.lower().rstrip()` — trailing whitespace must
    // not defeat the match.
    assert!(is_hallucination("tak  \n"));
    assert!(is_hallucination("thank you for watching   "));
}

#[test]
fn matches_danish_entries_case_insensitively() {
    // Non-ASCII (Danish "å") must still match under
    // `str::to_lowercase()` (Unicode-aware in Rust, matching Python).
    assert!(is_hallucination("Tak fordi du så med"));
    assert!(is_hallucination("Tak fordi du så med."));
}

#[test]
fn does_not_match_normal_dictation() {
    assert!(!is_hallucination("hello world"));
    assert!(!is_hallucination("dette er en almindelig sætning"));
    // Leading whitespace is NOT stripped by Python (`rstrip` is
    // right-only); preserve that semantic so the blacklist exact-match
    // doesn't false-positive on substrings.
    assert!(!is_hallucination("  tak"));
}

#[test]
fn does_not_match_partial_substring() {
    // Python's check is `text.lower().rstrip() in HALLUCINATIONS`
    // (whole-text exact match, not a substring scan). A real sentence
    // that contains "tak" inside it must NOT be flagged.
    assert!(!is_hallucination("tak for hjælpen"));
    assert!(!is_hallucination("thank you very much"));
}

#[test]
fn is_empty_safe() {
    // `""` is not on the blacklist — the session's empty-text branch
    // handles it separately. We just make sure we don't panic on it.
    assert!(!is_hallucination(""));
}
