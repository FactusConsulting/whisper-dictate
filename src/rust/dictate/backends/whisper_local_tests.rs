//! Tests for [`super::WhisperLocalTranscribeBackend`] and the
//! [`super::is_hallucination`] blacklist filter.
//!
//! Live in a sibling file (declared via `#[path]` in the production
//! module) so the unit-test surface is co-located with the impl while
//! the production file stays well under the repo's ~500-line gate.
//!
//! Tests that need an actual whisper.cpp model would require a ~75 MB
//! GGML fixture in CI — instead we exercise the trait impl's error path
//! by giving the wrapped `IdleUnloadingModel` a loader that always
//! fails, which proves the error mapping
//! (`anyhow::Error → TranscribeError::Backend(_)`) without needing the
//! model. The happy-path (decoded text → `TranscribeResult`) is covered
//! by the existing `whisper::local::tests` (which already run against a
//! tiny CI-provided fixture, see `whisper::local::local_tests`) and by
//! the cross-module integration coverage that PR 5 will add when it
//! swaps the stub.

use std::time::Duration;

use anyhow::anyhow;

use super::{is_hallucination, WhisperBackendConfig, WhisperLocalTranscribeBackend};
use crate::dictate::session::types::{TranscribeBackend, TranscribeError};
use crate::whisper::{IdleUnloadingModel, LocalWhisper};

// ── is_hallucination — exact-blacklist matching ──────────────────────────────

#[test]
fn is_hallucination_matches_exact_blacklist_entry() {
    // Most frequent observed false positive on quiet Danish input.
    assert!(is_hallucination("tak"));
    assert!(is_hallucination("Tak"));
    assert!(is_hallucination("TAK"));
}

#[test]
fn is_hallucination_matches_with_trailing_whitespace() {
    // Python uses `text.lower().rstrip()` — trailing whitespace must
    // not defeat the match.
    assert!(is_hallucination("tak  \n"));
    assert!(is_hallucination("thank you for watching   "));
}

#[test]
fn is_hallucination_matches_danish_entries_case_insensitively() {
    // Non-ASCII (Danish "å") must still match under
    // `str::to_lowercase()` (Unicode-aware in Rust, matching Python).
    assert!(is_hallucination("Tak fordi du så med"));
    assert!(is_hallucination("Tak fordi du så med."));
}

#[test]
fn is_hallucination_does_not_match_normal_dictation() {
    assert!(!is_hallucination("hello world"));
    assert!(!is_hallucination("dette er en almindelig sætning"));
    // Leading whitespace is NOT stripped by Python (`rstrip` is
    // right-only); preserve that semantic so the blacklist exact-match
    // doesn't false-positive on substrings.
    assert!(!is_hallucination("  tak"));
}

#[test]
fn is_hallucination_does_not_match_partial_substring() {
    // Python's check is `text.lower().rstrip() in HALLUCINATIONS`
    // (whole-text exact match, not a substring scan). A real sentence
    // that contains "tak" inside it must NOT be flagged.
    assert!(!is_hallucination("tak for hjælpen"));
    assert!(!is_hallucination("thank you very much"));
}

#[test]
fn is_hallucination_is_empty_safe() {
    // `""` is not on the blacklist — the session's empty-text branch
    // handles it separately. We just make sure we don't panic on it.
    assert!(!is_hallucination(""));
}

// ── trait-impl error mapping ─────────────────────────────────────────────────

/// Build a wrapper whose loader always fails so the very first
/// `transcribe()` call exercises the error path without needing a model
/// file. `idle_timeout = None` keeps the wrapper from spawning a
/// watcher thread (we don't need the unload behaviour to verify the
/// error path).
fn failing_backend() -> WhisperLocalTranscribeBackend {
    let model = IdleUnloadingModel::<LocalWhisper>::new(
        || Err(anyhow!("test loader: refused to load model")),
        None,
    );
    WhisperLocalTranscribeBackend::new(model, WhisperBackendConfig::default())
}

#[test]
fn transcribe_maps_loader_failure_to_backend_error() {
    let backend = failing_backend();
    let pcm = vec![0.0_f32; 16_000];
    let err = backend
        .transcribe(&pcm, 16_000)
        .expect_err("loader failure should propagate as TranscribeError");
    match err {
        TranscribeError::Backend(msg) => {
            assert!(
                msg.contains("refused to load model"),
                "expected wrapped loader error, got: {msg}"
            );
        }
    }
}

#[test]
fn config_accessors_round_trip() {
    let backend = WhisperLocalTranscribeBackend::new(
        IdleUnloadingModel::<LocalWhisper>::new(
            || Err(anyhow!("never called by accessor tests")),
            None,
        ),
        WhisperBackendConfig {
            language: Some("da".to_owned()),
            initial_prompt: Some("foo bar".to_owned()),
        },
    );
    assert_eq!(backend.config().language.as_deref(), Some("da"));
    assert_eq!(backend.config().initial_prompt.as_deref(), Some("foo bar"));
    // model() returns a borrow we can interrogate for the configured
    // idle timeout — proves the wrapper's lifetime is wired through.
    assert_eq!(backend.model().idle_timeout(), None);
}

#[test]
fn default_config_has_no_hints() {
    let cfg = WhisperBackendConfig::default();
    assert!(cfg.language.is_none());
    assert!(cfg.initial_prompt.is_none());
}

/// Sanity check: constructing with a real idle timeout must not panic
/// (the watcher thread spawn lives inside `IdleUnloadingModel::new`).
/// Drop the wrapper at scope exit so the watcher is joined — proves
/// the lifetime story is sound even when no transcribe is ever called.
#[test]
fn construction_with_idle_timeout_spawns_and_joins_cleanly() {
    let model = IdleUnloadingModel::<LocalWhisper>::new(
        || Err(anyhow!("never invoked — no transcribe call in this test")),
        Some(Duration::from_secs(60)),
    );
    let backend = WhisperLocalTranscribeBackend::new(model, WhisperBackendConfig::default());
    // Watcher hasn't loaded anything yet — model slot is empty.
    assert!(!backend.model().is_loaded());
    // Drop on scope exit; if the watcher thread fails to join the test
    // process will hang and CI will time out.
}
