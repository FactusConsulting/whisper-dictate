//! Env-var gate helpers for the Rust-dictation supervisor path.
//!
//! Split out of [`super::rust_session_sink`] in PR #441 review round 2
//! so that file stays under the AGENTS.md ~500-LOC-per-file modularity
//! guideline. Owns one tiny knob (Wave 8 Part 2 collapsed the second):
//!
//! * [`dictate_backend_python_legacy_requested`] -- did the user opt
//!   OUT of the PR 7 default (Rust worker) via
//!   `VOICEPI_DICTATE_BACKEND=python-legacy`? Still consulted from
//!   [`super::worker_rust::should_delegate_to_worker_rust`] so a stale
//!   `python-legacy` value in a user's profile surfaces the delegate-
//!   gate reason cleanly instead of silently falling through to a
//!   dead Python invocation.
//!
//! Accepts any casing / surrounding whitespace so a shell-set value
//! from a crash-cart edit still works. See `super::worker_rust` for the
//! composed delegation gate and `super::rust_session_sink` for the sink
//! builder itself.
//!
//! **Wave 8 Part 2** deleted `DICTATE_BACKEND_RUST_SESSION` and
//! `dictate_backend_rust_session_requested`: post-PR-7 the supervisor
//! ALWAYS delegates to the in-process Rust worker on a full-feature
//! build, so the historical `VOICEPI_DICTATE_BACKEND=rust-session`
//! opt-in has no consumer -- the "logger sink vs session sink" branch
//! that used to consult it lived in the deleted
//! `install_rust_hotkey_from_command` helper.

/// Env-var name. Matches the existing `VOICEPI_DICTATE_BACKEND` env var
/// the Python wrapper reads. Post PR 7 this env var doubles as the
/// emergency-rollback knob when set to [`DICTATE_BACKEND_PYTHON_LEGACY`].
pub(crate) const DICTATE_BACKEND_ENV: &str = "VOICEPI_DICTATE_BACKEND";

/// PR 7 escape hatch value: setting the backend env var to this string
/// opts OUT of the new default (Rust worker) and forces the supervisor
/// back onto the pre-PR-7 Python `runtime.py` orchestrator. Wave 8
/// removed the Python bundle; the constant stays wired to the delegate
/// gate so a stale `python-legacy` value in a user's profile surfaces
/// the "Python worker was removed in v1.20" error cleanly.
pub(crate) const DICTATE_BACKEND_PYTHON_LEGACY: &str = "python-legacy";

/// Vec-based variant of [`dictate_backend_python_legacy_requested`]
/// for callers holding an effective `WorkerCommand::env`. Same
/// case-insensitivity + whitespace-trim semantics; fallback path
/// consults `std::env` when the key is absent from the vec so a
/// process-env-only escape hatch still opts out. Wave 8 Part 2
/// (Codex #453 P2 stale-env warning).
pub(crate) fn dictate_backend_python_legacy_requested_from(env: &[(String, String)]) -> bool {
    let raw = env
        .iter()
        .find(|(k, _)| k == DICTATE_BACKEND_ENV)
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var(DICTATE_BACKEND_ENV).ok());
    match raw {
        Some(v) => v.trim().eq_ignore_ascii_case(DICTATE_BACKEND_PYTHON_LEGACY),
        None => false,
    }
}

/// Test-only convenience wrapper over
/// [`dictate_backend_python_legacy_requested_from`] with an empty env
/// vec so the sibling tests (`rust_session_sink_tests.rs`) can pin the
/// case-insensitivity + whitespace-trim contract against `std::env`
/// directly. Production code paths always have an
/// `effective_command.env` on hand and call the `_from` variant.
#[cfg(test)]
pub(crate) fn dictate_backend_python_legacy_requested() -> bool {
    dictate_backend_python_legacy_requested_from(&[])
}
