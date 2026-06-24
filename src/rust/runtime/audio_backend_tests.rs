//! Unit coverage for the [`audio_pipeline_requested`] /
//! [`audio_pipeline_available`] gates that decide whether the supervisor
//! should use the new Rust capture path. These are pure env-var reads so
//! we keep the suite quick and CI-friendly.
//!
//! [`audio_pipeline_requested`]: super::audio_pipeline_requested
//! [`audio_pipeline_available`]: super::audio_pipeline_available

use crate::runtime::{audio_pipeline_available, audio_pipeline_requested, AUDIO_BACKEND_ENV};
use std::env;
use std::sync::Mutex;

// Env-var mutation is process-global; serialize the tests so one doesn't
// see another's value mid-flight. Using a module-local Mutex (rather than
// `serial_test`) keeps the existing test dep list unchanged.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        env::set_var(key, value);
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = env::var(key).ok();
        env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(prev) => env::set_var(self.key, prev),
            None => env::remove_var(self.key),
        }
    }
}

#[test]
fn pipeline_not_requested_when_env_unset() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_BACKEND_ENV);
    assert!(!audio_pipeline_requested());
}

#[test]
fn pipeline_requested_when_env_is_rust() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::set(AUDIO_BACKEND_ENV, "rust");
    assert!(audio_pipeline_requested());
}

#[test]
fn pipeline_requested_is_case_insensitive_and_trims_whitespace() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for value in ["Rust", "RUST", " rust ", "\trust\n"] {
        let _g = EnvGuard::set(AUDIO_BACKEND_ENV, value);
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
        let _g = EnvGuard::set(AUDIO_BACKEND_ENV, value);
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
