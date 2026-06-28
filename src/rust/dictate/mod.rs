//! Pure-logic helpers for the live push-to-talk dictation loop.
//!
//! Wave 5 of the Python-removal roadmap (issue #348). The Python orchestrator
//! `src/python/whisper_dictate/vp_dictate.py` (the live PTT event loop) plus
//! `src/python/whisper_dictate/runtime.py` (the worker entry point) drive a
//! mixture of OS/IPC orchestration (subprocess spawning, file I/O, signal
//! handling, pynput callbacks) and small pure-logic decisions (skip-gating,
//! restart-required diff, backend/model label resolution, env-flag parsing).
//!
//! This module ports the **pure-logic** half to Rust so the canonical
//! implementation is reachable from the Rust supervisor that takes over the
//! full event loop in Wave 8. The orchestration half stays Python until
//! then — `vp_dictate.py` and `runtime.py` continue to be the caller-facing
//! product surface, exactly as Wave 4-A/B/C left them.
//!
//! # Wave 5 choice: Option B (Python wrapper stays caller-facing)
//!
//! The dictation loop is the per-utterance hot path; a subprocess shim per
//! `Dictate._should_skip_pcm` call would add tens of milliseconds of JSON
//! encode/decode latency to every recording. So this PR ports the small
//! pure helpers to Rust + unit-tests them (positioning Wave 8 to drop
//! Python entirely), exposes them through a hidden `dictate-ops` JSON-RPC
//! subcommand for one-shot startup-time queries, but leaves the Python
//! `Dictate` class as the in-process implementation for the hot path.
//! `vp_dictate_rust.py` opt-in via `VOICEPI_DICTATE_BACKEND=rust` shells
//! out for the startup-time queries; the default install keeps Python.
//!
//! # Module layout
//!
//! - [`skip`] — `Dictate._should_skip_pcm` decision (`min_record_seconds`
//!   floor; the legacy Parakeet-minimum branch was dropped together with
//!   the backend in Wave 8 of #348).
//! - [`restart`] — `Dictate._report_restart_required` diff against the
//!   restart-required key set.
//! - [`backend`] — `runtime._resolve_backend_and_device` /
//!   `runtime._resolve_model_name` label + validation.
//! - [`env_gates`] — `runtime._truthy`, `_config_dump_enabled`,
//!   `_trace_enabled` env-flag parsing.
//! - [`ops`] — JSON envelope dispatcher wired into the hidden
//!   `dictate-ops` CLI subcommand.
//! - [`events`] — Worker-event emitter that mirrors
//!   `vp_events.py::_emit_worker_event` byte-for-byte. Added in Wave 5
//!   PR 1 of #348 and intentionally NOT wired into any production
//!   caller yet — PR 2 routes the supervisor through it once the wire
//!   format is locked by the tests in `events_tests.rs`.

// Wave 5 PR 3 (#348): the bridge that pumps `AudioPipeline` events
// into a `DictateSession`. Gated on the `audio-in-rust` feature
// because the `audio` module — and therefore `PipelineEvent` /
// `AudioPipeline` — only compiles with that feature on. No production
// caller in this PR; PR 4 wires it from the supervisor. See module
// docs for the four behaviour gates it mirrors from
// `vp_capture_rust_stdin.py`.
#[cfg(feature = "audio-in-rust")]
pub mod audio_route;
pub mod backend;
pub mod env_gates;
pub mod events;
pub mod ops;
pub mod restart;
// Wave 5 PR 2 (#348): pure-logic per-utterance state machine that
// mirrors `vp_dictate.py::Dictate`'s lifecycle. No production caller in
// this PR — PR 3 (audio_route) and PR 4 (hotkey wiring) consume it.
// See module docs for the design rationale.
pub mod session;
pub mod skip;

pub use backend::{backend_label, validate_backend, BackendKind, BackendLabelError};
pub use env_gates::{config_dump_enabled, is_truthy, trace_enabled};
pub use restart::{changed_restart_keys, RESTART_REQUIRED_KEYS};
pub use session::{
    DictateSession, InjectBackend, InjectError, SessionConfig, SessionError, SessionState,
    TranscribeBackend, TranscribeError, TranscribeResult, UtteranceOutcome,
};
pub use skip::{should_skip, SkipDecision, MIN_RECORD_FLOOR_S};

#[cfg(test)]
mod events_tests;

// Wave 5 PR 3 (#348): unit tests for the audio_route bridge. Gated on
// `audio-in-rust` because the tests construct `PipelineEvent`s from
// the audio module (no cpal usage — see audio_route_tests.rs).
#[cfg(all(test, feature = "audio-in-rust"))]
mod audio_route_tests;
