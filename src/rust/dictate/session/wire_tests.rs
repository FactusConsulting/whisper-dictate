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

use super::types::{SessionConfig, TranscribeResult};
use super::wire::{
    build_utterance_payload, compact_text, UtteranceExtras, UtterancePost, TEXT_PREVIEW_LIMIT,
};

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

// -- provenance: engine / stt_impl / stt_accel --------------------------
//
// The record these tests exist for looked like this before the fields
// landed:
//
//   {"compute_type":"int8_float16","real_time_factor":0.23,
//    "compute_ms":351,"model":"large-v3-turbo",
//    "stt_backend":"whisper","device":"auto"}
//
// Every field there is emitted by BOTH the Rust session and the Python
// worker, and `stt_backend` names the CONFIGURED backend -- so the row
// could not say which runtime, which implementation, or which compute
// path produced it.

/// A payload built from a fully-populated config + result, so the
/// provenance assertions do not have to repeat the boilerplate.
fn provenance_payload(config: &SessionConfig, result: &TranscribeResult) -> serde_json::Value {
    build_utterance_payload(
        "hello world",
        result,
        serde_json::json!(1.0),
        UtterancePost {
            inject_error: None,
            post: None,
            replacements: &[],
        },
        UtteranceExtras {
            dictionary_text: "hello world",
            window: None,
            profile: None,
            config,
        },
    )
}

#[test]
fn utterance_payload_carries_engine_impl_and_accel() {
    let config = SessionConfig {
        engine: "rust-in-process".to_owned(),
        // The ambiguous fields from the original record, kept alongside
        // so the test pins that provenance is ADDITIVE rather than a
        // replacement.
        stt_backend: "whisper".to_owned(),
        device: "auto".to_owned(),
        compute_type: "int8_float16".to_owned(),
        model: "large-v3-turbo".to_owned(),
        ..SessionConfig::default()
    };
    let result = TranscribeResult {
        text: "hello world".to_owned(),
        stt_impl: "whisper.cpp".to_owned(),
        stt_accel: "vulkan".to_owned(),
        ..TranscribeResult::default()
    };
    let payload = provenance_payload(&config, &result);

    assert_eq!(payload["engine"], serde_json::json!("rust-in-process"));
    assert_eq!(payload["stt_impl"], serde_json::json!("whisper.cpp"));
    assert_eq!(payload["stt_accel"], serde_json::json!("vulkan"));
    // The pre-existing (ambiguous) fields must survive untouched.
    assert_eq!(payload["stt_backend"], serde_json::json!("whisper"));
    assert_eq!(payload["device"], serde_json::json!("auto"));
}

#[test]
fn stt_accel_comes_from_the_backend_result_not_the_device_setting() {
    // `device` is the SETTING (`auto` here, and it stays `auto` whatever
    // happens); `stt_accel` is the OUTCOME. A Vulkan-linked binary that
    // silently fell back to CPU must show `cpu` while `device` still says
    // `auto` -- that divergence is the entire point of the field.
    let config = SessionConfig {
        engine: "rust-in-process".to_owned(),
        device: "auto".to_owned(),
        ..SessionConfig::default()
    };
    let result = TranscribeResult {
        stt_impl: "whisper.cpp".to_owned(),
        stt_accel: "cpu".to_owned(),
        ..TranscribeResult::default()
    };
    let payload = provenance_payload(&config, &result);
    assert_eq!(payload["stt_accel"], serde_json::json!("cpu"));
    assert_eq!(payload["device"], serde_json::json!("auto"));
}

#[test]
fn provenance_fields_are_dropped_when_unset() {
    // A bare-Default session (unit-test backends, `simulate-session`)
    // must not emit blank `"engine": ""` rows -- same drop-on-empty rule
    // the other config-derived fields follow.
    let config = SessionConfig::default();
    let result = TranscribeResult::default();
    let payload = provenance_payload(&config, &result);
    for key in ["engine", "stt_impl", "stt_accel"] {
        assert!(
            payload.get(key).is_none(),
            "{key} must be omitted when empty, got {payload}"
        );
    }
}
