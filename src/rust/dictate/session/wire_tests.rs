//! Narrow unit tests for the [`super::wire`] helpers.
//!
//! Companion to `tests_transitions.rs` / `tests_history_sink.rs` etc,
//! but scoped specifically to the symbol-level contracts introduced
//! for the Codex P1 #606 metrics-schema follow-up: the
//! `TEXT_PREVIEW_LIMIT` constant, the `UtteranceExtras` param struct,
//! and the `compact_text` whitespace-collapse + truncate helper.
//!
//! The state-machine paths that _use_ these are covered end-to-end by
//! the existing session tests; these micro-tests pin the individual
//! symbol behaviours so the regression-test-discipline scanner can see
//! that the new public API surface is directly exercised by name.

use super::types::SessionConfig;
use super::wire::{compact_text, UtteranceExtras, TEXT_PREVIEW_LIMIT};

#[test]
fn text_preview_limit_matches_python_compact_text_ceiling() {
    // Python's `_compact_text(text, limit=240)` in `vp_events.py` is the
    // spec this constant mirrors. If it ever drifts, the utterance-event
    // `text_preview` field silently disagrees with the Python worker
    // and downstream consumers (metrics tail, log render) render a
    // differently-truncated string. Pin it explicitly.
    assert_eq!(TEXT_PREVIEW_LIMIT, 240);
}

#[test]
fn compact_text_collapses_whitespace_runs_to_single_spaces() {
    // Whitespace-heavy input: multiple spaces, tabs, newlines all fold
    // to a single space; leading/trailing whitespace is stripped
    // (matching Python's `" ".join(text.split())`).
    let raw = "  hello \t world\n\n  again  ";
    assert_eq!(compact_text(raw), "hello world again");
}

#[test]
fn compact_text_returns_input_unchanged_when_under_limit() {
    // A short single-spaced string round-trips verbatim.
    let short = "quick brown fox";
    assert_eq!(compact_text(short), short);
}

#[test]
fn compact_text_truncates_and_appends_ellipsis_at_limit() {
    // 500 'a's is safely over the 240-char limit; the result should be
    // exactly TEXT_PREVIEW_LIMIT visible chars and end in "...".
    let long = "a".repeat(500);
    let out = compact_text(&long);
    assert_eq!(out.chars().count(), TEXT_PREVIEW_LIMIT);
    assert!(out.ends_with("..."));
}

#[test]
fn utterance_extras_holds_borrowed_context_fields() {
    // `UtteranceExtras` is a plain param struct -- no logic, just field
    // wiring. This test pins the field names + lifetime shape so a
    // rename shows up as a compile break AND the scanner sees the
    // symbol referenced by name.
    let cfg = SessionConfig::default();
    let extras = UtteranceExtras {
        dictionary_text: "hello",
        window: None,
        profile: Some("email"),
        config: &cfg,
    };
    assert_eq!(extras.dictionary_text, "hello");
    assert!(extras.window.is_none());
    assert_eq!(extras.profile, Some("email"));
    // Round-trip through Clone (derive) so a field-list drift there
    // surfaces here rather than in a session-level test.
    let cloned = extras.clone();
    assert_eq!(cloned.dictionary_text, extras.dictionary_text);
    assert_eq!(cloned.profile, extras.profile);
}
