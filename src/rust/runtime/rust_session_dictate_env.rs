//! Env-var gate helpers for the Rust-dictation supervisor path.
//!
//! Split out of [`super::rust_session_sink`] in PR #441 review round 2
//! so that file stays under the AGENTS.md ~500-LOC-per-file modularity
//! guideline. Owns two tiny knobs:
//!
//! * [`dictate_backend_rust_session_requested`] -- did the user opt IN
//!   to the historical `VOICEPI_DICTATE_BACKEND=rust-session` value?
//!   Still honoured post-PR 7 so users with the setting exported in
//!   their profile continue to hit the session-sink hotkey routing
//!   on the Python-fallback path.
//! * [`dictate_backend_python_legacy_requested`] -- did the user opt
//!   OUT of the PR 7 default (Rust worker) via
//!   `VOICEPI_DICTATE_BACKEND=python-legacy`? This is the emergency
//!   rollback consulted from
//!   [`super::worker_rust::should_delegate_to_worker_rust`].
//!
//! Both helpers accept any casing / surrounding whitespace so a
//! shell-set value from a crash-cart edit still works. See
//! `super::worker_rust` for the composed delegation gate and
//! `super::rust_session_sink` for the sink builder itself.

/// Env-var name. Matches the existing `VOICEPI_DICTATE_BACKEND` env var
/// the Python wrapper reads. Post PR 7 this env var doubles as the
/// emergency-rollback knob when set to [`DICTATE_BACKEND_PYTHON_LEGACY`].
pub(crate) const DICTATE_BACKEND_ENV: &str = "VOICEPI_DICTATE_BACKEND";

/// Historical opt-in value for the in-process Rust session sink wiring.
///
/// After PR 7 this value is redundant on a full-feature build (the Rust
/// worker is the default) but stays recognised so the supervisor's
/// session-sink hotkey routing on the Python fallback path -- and users
/// who explicitly export this in their config -- keep working unchanged.
pub(crate) const DICTATE_BACKEND_RUST_SESSION: &str = "rust-session";

/// PR 7 escape hatch value: setting the backend env var to this string
/// opts OUT of the new default (Rust worker) and forces the supervisor
/// back onto the pre-PR-7 Python `runtime.py` orchestrator. Wave 8
/// removes the Python bundle and this constant together (issue #348).
pub(crate) const DICTATE_BACKEND_PYTHON_LEGACY: &str = "python-legacy";

/// True when the user opted in to the Rust-session sink wiring via env
/// var. Pure helper (no side effects) so the gate is unit-testable
/// without spawning a coordinator. Returns false for unset / empty /
/// any non-`rust-session` value.
pub(crate) fn dictate_backend_rust_session_requested() -> bool {
    std::env::var(DICTATE_BACKEND_ENV)
        .map(|v| v.trim().eq_ignore_ascii_case(DICTATE_BACKEND_RUST_SESSION))
        .unwrap_or(false)
}

/// True when the user opted OUT of the new Rust worker default via
/// `VOICEPI_DICTATE_BACKEND=python-legacy`. The supervisor consults
/// this (via [`super::worker_rust::should_delegate_to_worker_rust`])
/// to decide whether to swap the spawned command for `worker-rust` or
/// keep the pre-PR-7 Python orchestrator command as-is.
///
/// Pure helper -- accepts any casing / surrounding whitespace so a
/// shell-set escape-hatch value ("Python-Legacy", " python-legacy ")
/// still opts out. Returns false for unset / empty / any other value.
pub(crate) fn dictate_backend_python_legacy_requested() -> bool {
    std::env::var(DICTATE_BACKEND_ENV)
        .map(|v| v.trim().eq_ignore_ascii_case(DICTATE_BACKEND_PYTHON_LEGACY))
        .unwrap_or(false)
}
