//! Helpers that wire the Rust audio pipeline into the worker child
//! process. Kept in its own file so `runtime.rs` doesn't balloon past
//! the project's 500-LOC-per-file guideline.
//!
//! Rollout plan (Phase 1 — see PR description):
//!
//! 1. The supervisor calls [`should_use_rust_audio_backend`] to decide
//!    whether to take the Rust capture path for THIS worker spawn. The
//!    decision is logged once on the supervisor's stderr-derived event
//!    channel so the user can tell which path actually ran when an
//!    issue is filed.
//! 2. If yes, the supervisor configures the child's stdin as a pipe
//!    AND appends `--audio-source=rust-stdin` to the args; if no, the
//!    child runs exactly as it did before the feature.
//! 3. After spawn, [`spawn_audio_bridge_for_child`] grabs the child's
//!    stdin handle and starts the [`stdin_bridge::BridgeHandle`]. The
//!    handle is owned by the supervisor for the lifetime of the worker;
//!    drop semantics in `stdin_bridge` close cpal AND the writer.
//!
//! Backwards compatibility: every helper here is a no-op when the
//! `audio-in-rust` cargo feature is OFF — see the `cfg` gates below.
//! Default builds neither pull cpal nor touch stdin.

#[cfg(feature = "audio-in-rust")]
use crate::audio::{default_silero_loader, spawn_bridge, BridgeError, BridgeHandle};
use crate::runtime::{audio_pipeline_available, audio_pipeline_requested};
#[cfg(feature = "audio-in-rust")]
use std::sync::mpsc::Receiver;

/// Env var the Python worker uses to select a specific microphone. We
/// honour the same name on the Rust side so the user's saved choice
/// applies to BOTH backends — see `vp_capture._audio_device_setting`.
pub const AUDIO_DEVICE_ENV: &str = "VOICEPI_AUDIO_DEVICE";

/// Effective gate: ARE we using the Rust audio backend for this run?
///
/// Returns `true` only when BOTH the cargo feature is compiled in AND
/// the user opted in via `VOICEPI_AUDIO_BACKEND=rust`. When the env var
/// is set but the feature is off, the supervisor logs a warning and
/// falls back to the Python sounddevice path — the user is never
/// silently surprised. The warning string is the one returned by
/// [`requested_but_unavailable_warning`] so call sites stay in sync.
pub fn should_use_rust_audio_backend() -> bool {
    audio_pipeline_requested() && audio_pipeline_available()
}

/// One-line warning to print on the supervisor's stderr when the user
/// asked for the Rust backend but the binary was built without
/// `audio-in-rust`. Returned as a `String` (not `&'static str`) so a
/// later iteration can include the requested-vs-available delta without
/// a breaking change. Caller decides the destination (stderr in
/// production; a captured buffer in tests).
pub fn requested_but_unavailable_warning() -> String {
    "VOICEPI_AUDIO_BACKEND=rust was set but this binary was built without the \
     `audio-in-rust` cargo feature; falling back to the Python sounddevice \
     capture path."
        .to_owned()
}

/// Spawn the Rust audio bridge for a freshly-launched worker child.
///
/// Stub when the feature is OFF: every caller goes through this so we
/// can drop the `#[cfg]` gates here and not pollute the supervisor.
/// Returns `Ok(None)` when the feature is OFF or the gate said no, and
/// `Ok(Some(handle))` after a successful bridge spawn.
///
/// The `stdin_writer` is taken by ownership — typically `child.stdin`
/// drained via `Option::take()`. A unit-test harness can pass any
/// `Write + Send + 'static` here (a captured `Vec<u8>` channel etc).
///
/// `device_name` is the microphone identifier resolved by the caller
/// from the effective worker command env (see
/// [`resolve_audio_device_from_env`]). Empty string means "system
/// default" — `audio::capture::start_capture` honours that contract.
#[cfg(feature = "audio-in-rust")]
pub fn spawn_audio_bridge_for_child<W>(
    stdin_writer: W,
    device_name: &str,
) -> Result<(BridgeHandle, Receiver<BridgeError>), anyhow::Error>
where
    W: std::io::Write + Send + 'static,
{
    spawn_bridge(device_name, stdin_writer, default_silero_loader())
}

/// Microphone the Rust pipeline should open, resolved from the same
/// effective worker env we're about to hand to the Python child.
///
/// Iteration-2 review finding #1: previously this read directly from
/// `std::env`, which silently ignored a `VOICEPI_AUDIO_DEVICE` value
/// that the user selected through Settings (the UI persists it to the
/// on-disk config; `config::worker_env_overrides()` materialises it
/// into the `WorkerCommand.env` we then pass to the child). Resolution
/// order:
///
/// 1. `env_overrides` (the `WorkerCommand.env` slice the supervisor
///    will pass to the child — same source of truth the Python worker
///    sees via `_audio_device_setting`).
/// 2. The supervisor's own `std::env` (legacy / shell-exported case).
/// 3. Empty string (= "system default").
///
/// Returning empty for missing/blank preserves the existing contract.
pub fn resolve_audio_device_from_env(env_overrides: &[(String, String)]) -> String {
    for (key, value) in env_overrides {
        if key == AUDIO_DEVICE_ENV {
            return value.clone();
        }
    }
    std::env::var(AUDIO_DEVICE_ENV).unwrap_or_default()
}

/// Back-compat wrapper that consults only the process env. Retained
/// for tests and any external caller that doesn't yet have a
/// `WorkerCommand` in hand; production code should prefer
/// [`resolve_audio_device_from_env`] so the saved device choice
/// applies on Windows installs where the env var is set in config,
/// not in the parent shell.
pub fn resolved_audio_device() -> String {
    resolve_audio_device_from_env(&[])
}

// Tests for this module live in `audio_spawn_tests.rs` so they share a
// file naming convention with the rest of the runtime tests (the test
// runner gathers them via `#[cfg(test)] mod audio_spawn_tests` in
// `runtime.rs`).
