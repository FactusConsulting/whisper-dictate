//! Wave 5 PR 5 of #348 — construct the REAL
//! [`crate::dictate::backends::WhisperLocalTranscribeBackend`] +
//! [`crate::dictate::backends::EnigoInjectBackend`] session that the
//! coordinator-sink wiring drives when both the `whisper-rs-local` and
//! `rust-injection` features are compiled in.
//!
//! PR 4 (#416) installed the wiring with two stub backends so the
//! coordinator → session → worker-event loop was observable end-to-end
//! without pulling whisper.cpp or enigo into the dep graph. PR 5-prep
//! (#417) added the real trait impls (model loader, idle-unloader,
//! enigo dispatcher) but kept the production sink on the stubs. This
//! module is the small swap-in step: when the binary is compiled with
//! both features AND a model resolves successfully via
//! [`crate::whisper::resolve_model_path_from_env`], the supervisor's
//! [`super::rust_session_sink::build_production_sink`] returns a sink
//! backed by the real backends.
//!
//! # Gating
//!
//! The whole module is `#[cfg(all(feature = "whisper-rs-local", feature
//! = "rust-injection"))]` — default builds compile zero new code from
//! this PR, and a build with only one feature still falls through to
//! the PR 4 stub path. End-user impact is therefore opt-in twice:
//!
//! 1. Pass `--features whisper-rs-local,rust-injection,rust-hotkeys`
//!    at build time.
//! 2. Set `VOICEPI_DICTATE_BACKEND=rust-session` at run time.
//!
//! Without (1) the call to [`make_real_session`] does not exist;
//! without (2) `dictate_backend_rust_session_requested()` returns false
//! and the supervisor installs the historical logger sink instead.
//!
//! # Why this lives in its own module
//!
//! `rust_session_sink.rs` is already in the ~400-LOC range; adding the
//! real-backend constructor inline would push it past the 500-LOC
//! modularity guideline (AGENTS.md "Modularity"). Splitting also
//! isolates the heavy whisper.cpp / enigo deps behind a single cfg gate
//! so a default build does not even parse the real backend types.
//!
//! # Deferred to follow-up PRs
//!
//! The PR 5-prep backends today wire `transcribe → inject` directly.
//! The full Python flow from `vp_dictate.py:431-491` also runs
//! `postprocess::run::run()` → `formatting::apply_format_commands` →
//! per-utterance health-line bookkeeping between transcription and
//! injection. That chaining is out of scope for THIS PR (per the
//! Wave 5 slicing plan) — see issue follow-up `wave5-pr5-postprocess`
//! (filed by this PR).

use std::sync::{Arc, Mutex};

use crate::dictate::backends::whisper_local::WhisperBackendConfig;
use crate::dictate::backends::{EnigoInjectBackend, WhisperLocalTranscribeBackend};
use crate::dictate::{DictateSession, SessionConfig};
use crate::injection::{InjectMethod, Injector};
use crate::whisper::{
    parse_idle_timeout_from_env, resolve_model_path_from_env, IdleUnloadingModel,
};

/// The real production session type that PR 5 wires behind
/// `VOICEPI_DICTATE_BACKEND=rust-session` when both features are on.
pub(crate) type RealSession = DictateSession<WhisperLocalTranscribeBackend, EnigoInjectBackend>;

/// Build the real-backend session, wrapped in `Arc<Mutex<…>>` so the
/// coordinator-sink closure can hold a clone while exposing a separate
/// clone for tests / supervisor introspection.
///
/// Resolution rules:
///
/// - Model path: [`resolve_model_path_from_env`] — same env-var /
///   user-cache lookup the dispatcher and long-running server use, so
///   the contract is identical whether the user is on the
///   subprocess-per-utterance path, the long-running line server, or
///   the in-process Rust session.
/// - Idle timeout: [`parse_idle_timeout_from_env`] — same
///   `VOICEPI_WHISPER_IDLE_UNLOAD_S` knob.
/// - Inject method: [`InjectMethod::Typing`] by default. Paste mode
///   needs a Rust `Clipboard` impl to populate the OS clipboard before
///   the Ctrl+V chord fires (see `dictate/backends/inject.rs` module
///   docs, "Caller-owned pre-conditions"); until that lands the typing
///   path is the production-safe choice because the dispatcher types
///   the literal text through enigo / the helper chain regardless of
///   clipboard state.
///
/// Returns `Err(String)` (rather than a typed error) so the caller can
/// log the message and fall back to the stub session without having to
/// learn the union of underlying error types from
/// `resolve_model_path_from_env`, `parse_idle_timeout_from_env`, and
/// the injector construction. The caller treats the string as
/// human-readable and surfaces it on the runtime event channel.
pub(crate) fn make_real_session() -> Result<Arc<Mutex<RealSession>>, String> {
    let model_path = resolve_model_path_from_env().map_err(|e| format!("model path: {e:#}"))?;
    let idle = parse_idle_timeout_from_env().map_err(|e| format!("idle timeout: {e:#}"))?;
    let model = IdleUnloadingModel::for_local_whisper(model_path, idle);
    let transcribe = WhisperLocalTranscribeBackend::new(model, WhisperBackendConfig::default());

    // `Injector::new()` does no system calls — the underlying enigo
    // backend (on Windows / macOS) is constructed lazily inside
    // `Injector::inject_text` on first use. Constructing the wrapper
    // here is cheap and infallible.
    let inject = EnigoInjectBackend::new(Injector::new(), InjectMethod::Typing);

    Ok(Arc::new(Mutex::new(DictateSession::new(
        transcribe,
        inject,
        SessionConfig::default(),
    ))))
}

#[cfg(test)]
#[path = "rust_session_real_backends_tests.rs"]
mod tests;
