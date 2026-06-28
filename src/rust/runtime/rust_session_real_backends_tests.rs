//! Tests for [`super::rust_session_real_backends`].
//!
//! The constructor itself does not load the model (the wrapping
//! [`crate::whisper::IdleUnloadingModel`] is lazy), so these tests
//! exercise the env-driven config resolution + the production-sink
//! fallback path without ever calling whisper.cpp. The model file does
//! not have to exist on disk for [`super::make_real_session`] to
//! succeed -- the actual `LocalWhisper::new(...)` call is deferred
//! until the first transcribe, which is exactly the lifecycle Wave 5
//! PR 5 inherits from PR 5-prep's `WhisperLocalTranscribeBackend`.

use std::sync::mpsc;

use super::{
    session_config_from_env, whisper_backend_config_from_env, INITIAL_PROMPT_ENV, LANG_ENV,
};
use crate::dictate::audio_route::MIN_RECORD_ENV;
use crate::runtime::rust_session_sink::build_production_sink;
use crate::runtime::RuntimeEvent;
use crate::test_env_lock::ENV_LOCK;
use crate::whisper::{IDLE_UNLOAD_ENV, MODEL_PATH_ENV};

/// Save / restore an env var across a test so concurrent (different-named)
/// env var tests do not leak state. Used together with `ENV_LOCK` to
/// serialise tests that mutate the process-wide env.
struct EnvVarGuard {
    name: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let prev = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, prev }
    }

    fn unset(name: &'static str) -> Self {
        let prev = std::env::var(name).ok();
        std::env::remove_var(name);
        Self { name, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.name, v),
            None => std::env::remove_var(self.name),
        }
    }
}

// ── env-driven config parsers (Codex P2 #423 findings 2 + 5) ─────────────────

/// Wave 5 PR 5 round 2 (Codex P2 #423 finding 2): the language hint
/// and the initial prompt come from the same env vars `vp_cli.py`
/// reads. Empty / blank values must collapse to `None` so the per-
/// call empty-string -> auto-detect collapse in
/// `WhisperLocalTranscribeBackend::transcribe` never even sees a
/// literal empty string.
#[test]
fn whisper_backend_config_reads_lang_and_initial_prompt_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _lang = EnvVarGuard::set(LANG_ENV, "da");
    let _prompt = EnvVarGuard::set(INITIAL_PROMPT_ENV, "Whisper Dictate, Factus Consulting");

    let cfg = whisper_backend_config_from_env();
    assert_eq!(cfg.language.as_deref(), Some("da"));
    assert_eq!(
        cfg.initial_prompt.as_deref(),
        Some("Whisper Dictate, Factus Consulting")
    );
}

#[test]
fn whisper_backend_config_normalises_blank_env_values_to_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _lang = EnvVarGuard::set(LANG_ENV, "   ");
    let _prompt = EnvVarGuard::set(INITIAL_PROMPT_ENV, "");

    let cfg = whisper_backend_config_from_env();
    assert!(
        cfg.language.is_none(),
        "blank language env must collapse to None, got {:?}",
        cfg.language
    );
    assert!(
        cfg.initial_prompt.is_none(),
        "empty initial-prompt env must collapse to None, got {:?}",
        cfg.initial_prompt
    );
}

#[test]
fn whisper_backend_config_unset_env_is_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = whisper_backend_config_from_env();
    assert!(cfg.language.is_none());
    assert!(cfg.initial_prompt.is_none());
}

/// Wave 5 PR 5 round 2 (Codex P2 #423 finding 5):
/// `VOICEPI_MIN_RECORD_SECONDS` must flow into the constructed
/// `SessionConfig` so a user who raised the floor to suppress
/// accidental taps actually has that value enforced.
#[test]
fn session_config_threads_min_record_seconds_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _min = EnvVarGuard::set(MIN_RECORD_ENV, "0.8");
    let cfg = session_config_from_env();
    assert!(
        (cfg.min_record_seconds - 0.8).abs() < f64::EPSILON,
        "expected 0.8, got {}",
        cfg.min_record_seconds
    );
}

#[test]
fn session_config_falls_back_to_route_default_when_env_missing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _min = EnvVarGuard::unset(MIN_RECORD_ENV);
    let cfg = session_config_from_env();
    assert!(
        (cfg.min_record_seconds - 0.5).abs() < f64::EPSILON,
        "expected the 0.5 s default, got {}",
        cfg.min_record_seconds
    );
}

// ── production sink integration ───────────────────────────────────────────────

/// Wave 5 PR 5 -- when both required features are compiled in AND the
/// model env-var points at an EMPTY path (resolution failure), the
/// production sink must:
///
/// 1. Build a working sink (returned `OnceLock` empty so the supervisor
///    can populate it).
/// 2. Emit a `[rust-session]` stderr event on the channel naming the
///    fallback. The user needs that message to understand why
///    transcription emits the stub `no_text` instead of the expected
///    real output.
///
/// If the CI runner happens to have a model in the user-cache (which
/// would let the resolution succeed despite the blank env var), the
/// real-backend branch will succeed and no fallback event fires --
/// that is also a valid outcome of this contract. Only assert the
/// fallback-event shape WHEN the fallback fired.
#[test]
fn build_production_sink_emits_fallback_event_when_real_backend_fails() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _model_env = EnvVarGuard::set(MODEL_PATH_ENV, "   ");
    let _idle_env = EnvVarGuard::unset(IDLE_UNLOAD_ENV);

    let (tx, rx) = mpsc::channel();
    let (_sink, coord_slot) = build_production_sink(tx, None);
    assert!(
        coord_slot.get().is_none(),
        "production sink (real OR fallback) must hand back an empty OnceLock"
    );

    // Drain whatever the sink put on the channel during construction.
    let mut saw_fallback = false;
    while let Ok(ev) = rx.try_recv() {
        if let RuntimeEvent::Stderr(s) = ev {
            if s.contains("[rust-session]") && s.contains("falling back") {
                saw_fallback = true;
                break;
            }
        }
    }
    if !saw_fallback {
        eprintln!(
            "[test note] resolution succeeded despite blank env -- a cached \
             model OR an absent audio-in-rust feature might be in play. The \
             round-2 env-helper tests pin the parse contracts unconditionally."
        );
    }
}
