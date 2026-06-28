//! Unit coverage for the [`audio_pipeline_requested`] /
//! [`audio_pipeline_available`] gates that decide whether the supervisor
//! should use the new Rust capture path. These are pure env-var reads so
//! we keep the suite quick and CI-friendly.
//!
//! [`audio_pipeline_requested`]: super::audio_pipeline_requested
//! [`audio_pipeline_available`]: super::audio_pipeline_available

use crate::runtime::{audio_pipeline_available, audio_pipeline_requested, AUDIO_BACKEND_ENV};
use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

// Env-var mutation is process-global; serialise on the crate-wide lock so
// tests in this module don't race AGAINST tests in other modules touching the
// same process env. The shared `EnvVarGuard` restores the original value on
// Drop (even on panic) — see `crate::test_env_lock`.

#[test]
fn pipeline_not_requested_when_env_unset() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_BACKEND_ENV);
    assert!(!audio_pipeline_requested());
}

#[test]
fn pipeline_requested_when_env_is_rust() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::set(AUDIO_BACKEND_ENV, "rust");
    assert!(audio_pipeline_requested());
}

#[test]
fn pipeline_requested_is_case_insensitive_and_trims_whitespace() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for value in ["Rust", "RUST", " rust ", "\trust\n"] {
        let _g = EnvVarGuard::set(AUDIO_BACKEND_ENV, value);
        assert!(
            audio_pipeline_requested(),
            "value {value:?} should request the pipeline",
        );
    }
}

#[test]
fn pipeline_not_requested_for_other_values() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for value in [
        "",
        "python",
        "sounddevice",
        "rust-experimental",
        "0",
        "false",
    ] {
        let _g = EnvVarGuard::set(AUDIO_BACKEND_ENV, value);
        assert!(
            !audio_pipeline_requested(),
            "value {value:?} must not request the pipeline",
        );
    }
}

#[test]
fn pipeline_available_matches_compiled_feature() {
    // The feature gate is a `cfg!` macro; mirror it here so the test
    // pins the function down rather than tautologically delegating.
    let expected_via_cfg = cfg!(feature = "audio-in-rust");
    assert_eq!(audio_pipeline_available(), expected_via_cfg);
}
