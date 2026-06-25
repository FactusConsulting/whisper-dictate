//! Tests for the runtime ↔ audio-pipeline wiring (PR #341 Phase 1).
//!
//! Covers the supervisor's decision to splice the Rust capture path
//! into the worker command:
//!
//! 1. When `VOICEPI_AUDIO_BACKEND=rust` is set AND the feature is
//!    compiled in, the worker command gains `--audio-source=rust-stdin`
//!    AND `Stdio::piped()` is set on stdin (we test this indirectly via
//!    the bridge spawn helper, which is gated on the same conditions).
//! 2. When the env var is unset, the supervisor behaves byte-identically
//!    to today's Python path — no extra arg, no stdin pipe, no bridge.
//! 3. When the env var is set but the feature is OFF, the supervisor
//!    falls back to the Python path AND emits a warning so the user
//!    knows their opt-in was ignored.
//!
//! These are pure logic tests against the gate helpers; the cpal /
//! Silero half is exercised by `tests/audio_pipeline.rs`.

use crate::runtime::audio_spawn::{
    requested_but_unavailable_warning, resolved_audio_device, should_use_rust_audio_backend,
};
use crate::runtime::AUDIO_BACKEND_ENV;
use crate::test_env_lock::ENV_LOCK;
use std::env;

const AUDIO_DEVICE_ENV: &str = "VOICEPI_AUDIO_DEVICE";

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = env::var(key).ok();
        env::set_var(key, value);
        Self { key, prev }
    }
    fn unset(key: &'static str) -> Self {
        let prev = env::var(key).ok();
        env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => env::set_var(self.key, v),
            None => env::remove_var(self.key),
        }
    }
}

#[test]
fn rust_backend_disabled_by_default_for_unset_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_BACKEND_ENV);
    assert!(
        !should_use_rust_audio_backend(),
        "unset VOICEPI_AUDIO_BACKEND must keep the Python sounddevice path",
    );
}

#[test]
fn rust_backend_engages_only_when_feature_compiled_and_env_opted_in() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::set(AUDIO_BACKEND_ENV, "rust");
    // The Phase-1 gate is the AND of feature + env var. Mirror the cfg
    // check here so the test stays honest in both build modes — without
    // the feature, the env var is acknowledged with a warning but the
    // gate stays false; with the feature, the gate is true.
    assert_eq!(
        should_use_rust_audio_backend(),
        cfg!(feature = "audio-in-rust")
    );
}

#[test]
fn warning_is_actionable_when_user_opts_in_without_feature() {
    // Pure-string assertion; runs in every build mode. The supervisor
    // emits this on its event channel as a `Stderr` event so it shows
    // up in the runtime log alongside other start-up diagnostics.
    let msg = requested_but_unavailable_warning();
    assert!(
        msg.contains("VOICEPI_AUDIO_BACKEND=rust"),
        "warning must quote the env var so users can grep it: {msg}",
    );
    assert!(
        msg.contains("audio-in-rust"),
        "warning must name the cargo feature so the fix is obvious: {msg}",
    );
    assert!(
        msg.contains("sounddevice") || msg.contains("Python"),
        "warning must tell the user what we did INSTEAD: {msg}",
    );
}

#[test]
fn resolved_audio_device_honours_voicepi_audio_device_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::set(AUDIO_DEVICE_ENV, "Yeti X");
    // Mirrors what the Python worker reads via `_audio_device_setting`,
    // so a user's saved mic choice applies to BOTH backends without a
    // separate Rust-only knob. Empty string (the unset case) means
    // "system default" — `audio::capture::start_capture` respects that.
    assert_eq!(resolved_audio_device(), "Yeti X");
}

#[test]
fn resolved_audio_device_defaults_to_empty_for_unset_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_DEVICE_ENV);
    assert!(
        resolved_audio_device().is_empty(),
        "unset VOICEPI_AUDIO_DEVICE must resolve to '' (= system default)",
    );
}
