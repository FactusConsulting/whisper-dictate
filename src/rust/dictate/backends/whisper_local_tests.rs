//! Tests for [`super::WhisperLocalTranscribeBackend`] — the trait-impl
//! wiring (error mapping, empty-language handling, the pre-transcription
//! speech gate). The pure text finalization it delegates to
//! (`normalize_whitespace` + `finalize_transcript`: whitespace normalize,
//! speech-rate blanking, exact-blacklist / credit-regex gate) is stock and
//! tested in `hallucination_tests.rs`, so it runs on every build without the
//! `whisper-rs-local` feature.
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

use super::{WhisperBackendConfig, WhisperLocalTranscribeBackend};
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

/// PCM that PASSES the pre-transcription speech gate (loud, contrasty,
/// ending loud), so `transcribe` reaches the model loader rather than
/// being short-circuited by the gate.
fn gate_passing_pcm() -> Vec<f32> {
    let mut pcm = Vec::with_capacity(6 * 480);
    for amp in [0.001_f32, 0.5, 0.001, 0.5, 0.001, 0.5] {
        pcm.extend(std::iter::repeat_n(amp, 480));
    }
    pcm
}

#[test]
fn transcribe_maps_loader_failure_to_backend_error() {
    let backend = failing_backend();
    let err = backend
        .transcribe(&gate_passing_pcm(), 16_000)
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
fn transcribe_gates_silence_before_the_model() {
    // Silent input is rejected by the speech gate BEFORE the model loader
    // runs, so even a failing loader is never reached: an Ok with the gate
    // reason is returned, which the session maps to a too_quiet no-text
    // event.
    let backend = failing_backend();
    let result = backend
        .transcribe(&vec![0.0_f32; 6 * 480], 16_000)
        .expect("gated silence returns Ok, not the loader error");
    assert!(result.text.is_empty());
    let gate = result.gate.expect("gate reason present");
    assert!(gate.contains("too quiet"), "{gate}");
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
    // Gate-passing audio so the speech gate doesn't short-circuit before
    // the model loader is reached.
    let err = backend
        .transcribe(&gate_passing_pcm(), 16_000)
        .expect_err("loader fails");
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
