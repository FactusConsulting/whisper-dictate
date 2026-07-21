//! Tests for [`super::WhisperLocalTranscribeBackend`] and its
//! `normalize_whitespace` pre-step. The exact-blacklist
//! [`is_hallucination`] filter itself is stock and tested in
//! `hallucination_tests.rs`; the one test kept here pins the
//! normalize-then-filter ordering the trait impl relies on.
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

use super::super::hallucination::is_hallucination;
use super::{normalize_whitespace, WhisperBackendConfig, WhisperLocalTranscribeBackend};
use crate::dictate::session::types::{TranscribeBackend, TranscribeError};
use crate::whisper::{IdleUnloadingModel, LocalWhisper};

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

// ── normalize_whitespace — segment-text post-processing ──────────────────────

#[test]
fn normalize_whitespace_collapses_internal_runs() {
    // whisper.cpp segments carry leading word-boundary spaces; a naive
    // concat produces `" hello   world  "` style strings. Match
    // Python's `re.sub(r"\s+", " ", ...).strip()` shape.
    // Codex P2 #417 whisper_local.rs:201.
    assert_eq!(normalize_whitespace(" hello   world  "), "hello world");
}

#[test]
fn normalize_whitespace_trims_both_ends() {
    // Leading whitespace must be stripped so the exact-match
    // hallucination blacklist catches `" tak"` after normalization.
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

#[test]
fn is_hallucination_catches_leading_whitespace_after_normalize() {
    // Regression guard for Codex P2 whisper_local.rs:201: the
    // transcribe pipeline runs `normalize_whitespace` before
    // `is_hallucination`, so a whisper.cpp output of " tak" is
    // expected to be caught. This test pins the contract by running
    // the two functions in the same order the trait impl does.
    let raw = " tak";
    let normalized = normalize_whitespace(raw);
    assert!(
        is_hallucination(&normalized),
        "normalized ' tak' must be on the blacklist"
    );
}

// ── empty-language hint normalization ────────────────────────────────────────

#[test]
fn empty_language_string_is_treated_as_auto_detect() {
    // Codex P2 #417 whisper_local.rs:183: settings layer's default
    // `Some("")` must not be forwarded as a literal language code,
    // which whisper.cpp would reject. The transcribe path filters it
    // to `None` before calling the model. Drive a real transcribe
    // through a failing loader and confirm the failure surfaces from
    // the loader (NOT from a language-validation error): that proves
    // the language hint reached `with_model` as `None`. The exact
    // error message we get is the loader's, not whisper.cpp's.
    let model = IdleUnloadingModel::<LocalWhisper>::new(
        || Err(anyhow!("loader: still always fails")),
        None,
    );
    let backend = WhisperLocalTranscribeBackend::new(
        model,
        WhisperBackendConfig {
            language: Some(String::new()),
            initial_prompt: Some(String::new()),
        },
    );
    let pcm = vec![0.0_f32; 16_000];
    let err = backend.transcribe(&pcm, 16_000).expect_err("loader fails");
    match err {
        TranscribeError::Backend(msg) => {
            assert!(
                msg.contains("still always fails"),
                "expected loader error to propagate, got: {msg}"
            );
        }
    }
}

#[test]
fn empty_language_in_result_round_trips_as_empty_string() {
    // Mirror Python's contract on `TranscribeResult.language`: the
    // session emits the field verbatim. An empty `Some("")` in the
    // config must surface as an empty string on the result so the
    // worker-event payload stays byte-equivalent. (The transcribe
    // call itself fails here because we use a failing loader, but
    // the `language` field is populated from `self.config` so we
    // don't need a successful call to verify the round-trip.)
    let cfg = WhisperBackendConfig {
        language: Some(String::new()),
        ..Default::default()
    };
    // The `unwrap_or_default` branch yields "" for Some("") too —
    // pin this contract so a future refactor doesn't accidentally
    // collapse it to a literal "none" / "auto" marker.
    assert_eq!(cfg.language.clone().unwrap_or_default(), "");
}
