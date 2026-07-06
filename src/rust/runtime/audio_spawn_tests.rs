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
    requested_but_unavailable_warning, resolve_audio_device_from_env, resolved_audio_device,
    should_use_rust_audio_backend, AUDIO_DEVICE_ENV,
};
use crate::runtime::AUDIO_BACKEND_ENV;
use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

#[test]
fn rust_backend_disabled_by_default_for_unset_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_BACKEND_ENV);
    assert!(
        !should_use_rust_audio_backend(),
        "unset VOICEPI_AUDIO_BACKEND must keep the Python sounddevice path",
    );
}

#[test]
fn rust_backend_engages_only_when_feature_compiled_and_env_opted_in() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::set(AUDIO_BACKEND_ENV, "rust");
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
    let _g = EnvVarGuard::set(AUDIO_DEVICE_ENV, "Yeti X");
    // Mirrors what the Python worker reads via `_audio_device_setting`,
    // so a user's saved mic choice applies to BOTH backends without a
    // separate Rust-only knob. Empty string (the unset case) means
    // "system default" — `audio::capture::start_capture` respects that.
    assert_eq!(resolved_audio_device(), "Yeti X");
}

#[test]
fn resolved_audio_device_defaults_to_empty_for_unset_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_DEVICE_ENV);
    assert!(
        resolved_audio_device().is_empty(),
        "unset VOICEPI_AUDIO_DEVICE must resolve to '' (= system default)",
    );
}

/// Iteration-2 review finding #1: the supervisor must read the device
/// from the EFFECTIVE worker env (i.e. the same `WorkerCommand.env`
/// it'll pass to the Python child), not just `std::env`. On a typical
/// Windows install the user picks a microphone in Settings and the
/// choice is persisted to the on-disk config, which
/// `config::worker_env_overrides()` materialises into the command env
/// — the parent shell typically never sets the variable, so a `std::env`
/// lookup alone returns "" and the Rust backend silently opens the
/// system default mic.
#[test]
fn resolve_audio_device_from_env_prefers_worker_command_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Even if the process env says something else (typical: shell
    // doesn't export it at all), the worker-command override wins.
    let _g = EnvVarGuard::set(AUDIO_DEVICE_ENV, "system-shell-mic");
    let overrides = vec![(AUDIO_DEVICE_ENV.to_owned(), "Saved Settings Mic".to_owned())];
    assert_eq!(
        resolve_audio_device_from_env(&overrides),
        "Saved Settings Mic",
    );
}

#[test]
fn resolve_audio_device_from_env_falls_back_to_process_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // No override in the command env → process env is the next-best
    // source so legacy shell-export workflows keep working.
    let _g = EnvVarGuard::set(AUDIO_DEVICE_ENV, "Process Env Mic");
    assert_eq!(
        resolve_audio_device_from_env(&[]),
        "Process Env Mic",
        "process env must serve as the legacy fallback",
    );
}

#[test]
fn resolve_audio_device_from_env_returns_empty_when_neither_set() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_DEVICE_ENV);
    assert!(
        resolve_audio_device_from_env(&[]).is_empty(),
        "neither source set → empty string = system default",
    );
}

/// Iteration-3 review finding #4: the Python capture path normalises
/// `VOICEPI_AUDIO_DEVICE` with `.strip()`, so values like `"  Yeti  "`
/// resolve to `"Yeti"` and a whitespace-only value collapses to `""`
/// (= system default). The Rust path must apply the same trimming so a
/// single saved setting selects the same mic on both backends; without
/// it the raw spaces get forwarded to CPAL's device matching and
/// either fail to match or are treated as a literal selector.
#[test]
fn resolve_audio_device_from_env_trims_whitespace_from_overrides() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_DEVICE_ENV);
    let overrides = vec![(AUDIO_DEVICE_ENV.to_owned(), "  Yeti X  ".to_owned())];
    assert_eq!(
        resolve_audio_device_from_env(&overrides),
        "Yeti X",
        "leading/trailing whitespace must be trimmed before CPAL lookup",
    );
}

#[test]
fn resolve_audio_device_from_env_collapses_blank_override_to_empty() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_DEVICE_ENV);
    let overrides = vec![(AUDIO_DEVICE_ENV.to_owned(), "   ".to_owned())];
    assert_eq!(
        resolve_audio_device_from_env(&overrides),
        "",
        "a whitespace-only override must collapse to '' (= system default)",
    );
}

#[test]
fn resolve_audio_device_from_env_trims_process_env_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // No worker-command override → process env is consulted, and it
    // must be trimmed the same way the override path is.
    let _g = EnvVarGuard::set(AUDIO_DEVICE_ENV, "\tHeadset Mic\n");
    assert_eq!(resolve_audio_device_from_env(&[]), "Headset Mic");
}

#[test]
fn resolve_audio_device_from_env_ignores_unrelated_overrides() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove(AUDIO_DEVICE_ENV);
    let overrides = vec![
        ("VOICEPI_MODEL".to_owned(), "small".to_owned()),
        ("PYTHONPATH".to_owned(), "/somewhere".to_owned()),
    ];
    assert!(
        resolve_audio_device_from_env(&overrides).is_empty(),
        "unrelated env keys must not be mistaken for the device override",
    );
}
