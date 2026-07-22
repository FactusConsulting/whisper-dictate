//! Tests for [`super::is_hallucination`] — the stock exact-blacklist filter
//! shared by the local + cloud transcribe backends. Pure (no cargo
//! feature, no model), so these run in the required `rust` matrix on every
//! build.

use super::{
    finalize_transcript, is_hallucination, normalize_whitespace, speech_rate_exceeded,
    DEFAULT_MAX_CHARS_PER_SECOND,
};

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

// ── anchored credit regex (parity with Python's _looks_like_credit) ──────────

#[test]
fn credit_regex_flags_whole_text_subtitle_credits_with_year() {
    // A phrase prefix + trailing year is a credit hallucination.
    assert!(is_hallucination("Undertekster af Nicolai Winther 2021"));
    assert!(is_hallucination("Danske tekster af TV2 2019."));
    assert!(is_hallucination("Tekstet af Someone 1998"));
    assert!(is_hallucination("Subtitles by Acme 2005"));
    // Case-insensitive + trailing punctuation/space tolerated.
    assert!(is_hallucination("  TRANSLATED BY BOB 2014 !!  "));
}

#[test]
fn credit_regex_flags_bare_company_names() {
    // Company-name branches match with an optional year.
    assert!(is_hallucination("Scandinavian Text Service"));
    assert!(is_hallucination("Broadcast Text International 2005"));
    assert!(is_hallucination("Dansk Videotekst"));
    assert!(is_hallucination("Dansk Video Tekst 2011"));
}

// ── speech-rate guard (parity with Python's _speech_rate_exceeded) ───────────

#[test]
fn speech_rate_exceeded_flags_impossibly_fast_transcripts() {
    // 200 chars in 0.5 s = 400 chars/s >> 30.
    let fast: String = "a".repeat(200);
    assert!(speech_rate_exceeded(
        &fast,
        0.5,
        DEFAULT_MAX_CHARS_PER_SECOND
    ));
}

#[test]
fn speech_rate_within_limit_is_not_flagged() {
    // "hello world" (11 chars) over 1 s = 11 chars/s < 30.
    assert!(!speech_rate_exceeded(
        "hello world",
        1.0,
        DEFAULT_MAX_CHARS_PER_SECOND
    ));
}

#[test]
fn speech_rate_guard_disabled_when_max_is_zero_or_negative() {
    let fast: String = "a".repeat(1000);
    assert!(!speech_rate_exceeded(&fast, 0.1, 0.0));
    assert!(!speech_rate_exceeded(&fast, 0.1, -1.0));
}

#[test]
fn speech_rate_clamps_tiny_durations_like_python() {
    // duration_s is floored at 0.1 s (matches Python's max(duration_s, 0.1)),
    // so a 4-char transcript over 0.001 s is 40 chars/s, not 4000.
    assert!(speech_rate_exceeded("abcd", 0.001, 30.0)); // 4 / 0.1 = 40 > 30
    assert!(!speech_rate_exceeded("abc", 0.001, 30.0)); // 3 / 0.1 = 30, not > 30
}

// ── normalize_whitespace — segment-text post-processing ──────────────────────

#[test]
fn normalize_whitespace_collapses_internal_runs() {
    // whisper.cpp segments carry leading word-boundary spaces; a naive concat
    // produces `" hello   world  "` strings. Match Python's
    // `re.sub(r"\s+", " ", ...).strip()` shape.
    assert_eq!(normalize_whitespace(" hello   world  "), "hello world");
}

#[test]
fn normalize_whitespace_trims_both_ends() {
    // Leading whitespace must be stripped so the exact-match blacklist catches
    // `" tak"` after normalization.
    assert_eq!(normalize_whitespace(" tak "), "tak");
    assert_eq!(normalize_whitespace("\n\ttak\r\n"), "tak");
}

#[test]
fn normalize_whitespace_preserves_internal_single_spaces() {
    assert_eq!(normalize_whitespace("foo bar baz"), "foo bar baz");
}

#[test]
fn normalize_whitespace_is_empty_safe() {
    assert_eq!(normalize_whitespace(""), "");
    assert_eq!(normalize_whitespace("   "), "");
}

// ── finalize_transcript — normalize + rate-guard + blacklist, in order ────────

#[test]
fn finalize_transcript_blanks_impossibly_fast_text() {
    // 200 chars over 0.5 s = 400 chars/s >> 30: the speech-rate guard blanks
    // the text so the session emits `empty`, not a hallucinated wall. A
    // regression (removing/misordering the guard in a backend) is caught here
    // without needing a whisper.cpp model.
    let fast = "a".repeat(200);
    let (text, hallucinated) = finalize_transcript(&fast, 0.5, 30.0);
    assert!(text.is_empty(), "over-rate transcript must be blanked");
    assert!(
        !hallucinated,
        "blanked text is empty, not a blacklist match"
    );
}

#[test]
fn finalize_transcript_keeps_normal_rate_text() {
    // "hello world" (11 chars) over 1 s = 11 chars/s < 30: preserved verbatim
    // (after whitespace normalization) and not flagged.
    let (text, hallucinated) = finalize_transcript("  hello   world  ", 1.0, 30.0);
    assert_eq!(text, "hello world");
    assert!(!hallucinated);
}

#[test]
fn finalize_transcript_flags_blacklisted_credit_after_normalize() {
    // normalize_whitespace trims first, so " tak" -> "tak" is flagged even
    // though the raw text had a leading space (the blacklist only rstrips).
    // This pins the normalize-before-blacklist ordering both backends rely on.
    let (text, hallucinated) = finalize_transcript(" tak", 1.0, 30.0);
    assert_eq!(text, "tak");
    assert!(hallucinated, "normalized ' tak' must hit the blacklist");
}

#[test]
fn finalize_transcript_disables_rate_guard_when_max_is_zero() {
    // max_cps <= 0 disables the guard (parity with Python): even absurd rates
    // are preserved, letting the blacklist be the only filter.
    let fast = "b".repeat(500);
    let (text, _) = finalize_transcript(&fast, 0.1, 0.0);
    assert_eq!(text.len(), 500, "rate guard off must preserve the text");
}

#[test]
fn credit_regex_does_not_flag_yearless_prefix_or_real_dictation() {
    // The whole-text gate requires the trailing year on a phrase prefix, so
    // real dictation that merely BEGINS like a credit must survive (the
    // year-less prefix path is Python's segment-level gate, not this one).
    assert!(!is_hallucination("danske tekster af høj kvalitet"));
    assert!(!is_hallucination("tekstet af hånd i dag"));
    // A credit phrase embedded mid-sentence is not an anchored whole-text
    // match.
    assert!(!is_hallucination(
        "jeg skrev undertekster af vane i 2021 og nød det"
    ));
    assert!(!is_hallucination("send oversat af to me"));
}
