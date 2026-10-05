//! Pure-logic helpers for the live push-to-talk dictation loop.
//!
//! This module owns the live PTT event loop's logic and native orchestration:
//! skip-gating, restart-required diffs, backend/model resolution, env parsing,
//! capture, transcription, and injection.
//!
//! # The in-process engine
//!
//! The dictation loop is the per-utterance hot path; a subprocess shim
//! would add tens of milliseconds of JSON encode/decode latency to
//! every recording. The small pure helpers live in Rust, unit-tested,
//! and are exposed through a hidden `dictate-ops` JSON-RPC subcommand
//! for one-shot startup-time queries.
//!
//! # Module layout
//!
//! - [`skip`] — capture-clip skip decision (`min_record_seconds` floor).
//! - [`restart`] — restart-required diff against the key set.
//! - [`backend`] — backend label + validation.
//! - [`env_gates`] — env-flag parsing (`config_dump`, `trace`).
//! - [`ops`] — JSON envelope dispatcher wired into the hidden
//!   `dictate-ops` CLI subcommand.
//! - [`events`] — worker-event emitter; the wire format is locked by
//!   the tests in `events_tests.rs`.
// Windows audio ducking -- lowers other apps' volume while dictating
// and restores on release. Always compiled; the WASAPI backend is
// runtime-gated inside the module (Windows + `audio-capture` feature),
// non-Windows / non-capture builds fall through to a warn-once no-op.
pub mod audio_ducking;
pub mod backend;
// Production `TranscribeBackend` / `InjectBackend` trait impls
// (`WhisperLocalTranscribeBackend`, `EnigoInjectBackend`). Each
// submodule is feature-gated on the cargo feature that already controls
// its underlying dependency (`whisper-rs-local`, `rust-injection`), so
// default builds compile zero new code here.
pub mod backends;
pub mod env_gates;
pub mod events;
// Audible PTT press/release cues: default builds get audible
// confirmation when `VOICEPI_FEEDBACK_SOUNDS=1`.
pub mod feedback;
pub mod ops;
// Per-utterance target-profile resolver: consulted by DictateSession at
// each PTT press to swap settings for the currently-focused window
// `src/rust/platform/foreground_window.rs` for the title/process probe
// this feeds and `src/rust/dictate/session/mod.rs::with_profile_matcher`
// for the wire-up.
pub mod profile;
// Engine / STT-implementation provenance vocabulary (`engine`,
// `stt_impl`, `stt_accel`) shared by the utterance emitter, the sinks,
// and the startup diagnostic line. Always compiled: the labels are a
// wire contract, so they must be unit-tested on every build regardless
// of which backends are linked in.
pub mod provenance;
pub mod restart;
// Pure-logic per-utterance state machine used by the native runtime.
pub mod session;
// Offline WAV-driven drive of a `DictateSession` for CLI integration
// testing of the Rust engine (the `simulate-session` verb). Stock: cloud
// backend + preview inject, no feature gate.
pub mod simulate;
// Live-microphone drive of a `DictateSession` (the `dictate-mic` verb).
// Gated on `audio-capture` because it opens the cpal capture pipeline;
// the stock-build stub lives in
// `main.rs`.
#[cfg(feature = "audio-capture")]
pub mod mic;
// Backend self-tests: one CLI verb per backend
// (feedback cues, WASAPI audio ducking, profile matching, history +
// metrics JSONL sinks, live preview). Each verb is a pure, headless
// exercise of the backend so it can be run on any dev box without
// cloud STT keys, audio hardware, or a display. See
// `src/rust/dictate/self_test/mod.rs` for the module map.
pub mod self_test;
pub mod skip;

pub use backend::{backend_label, validate_backend, BackendKind, BackendLabelError};
// Re-exports: surface the real backends through the `crate::dictate`
// namespace so callers don't have to reach into the `backends`
// submodule. Each re-export is gated on the same cargo feature as the
// source module.
// Cloud STT backend is stock (no cargo-feature gate).
pub use audio_ducking::{AudioDucker, NoOpAudioDucker, SystemAudioDucker};
#[cfg(feature = "rust-injection")]
pub use backends::EnigoInjectBackend;
pub use backends::{CloudTranscribeBackend, CloudTranscribeConfig, ProductionTranscribeBackend};
#[cfg(feature = "nemotron-local")]
pub use backends::{NemotronLocalBackendConfig, NemotronLocalTranscribeBackend};
#[cfg(feature = "whisper-rs-local")]
pub use backends::{WhisperLocalPreviewBackend, WhisperLocalTranscribeBackend};
pub use env_gates::{config_dump_enabled, is_truthy, trace_enabled};
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) use feedback::SessionCueSink;
pub use feedback::{play_cue, CueKind, CueSink, NoOpCueSink, SystemCueSink};
pub use profile::{AppliedProfile, ProfileMatcher, ReloadingProfileMatcher, StaticProfileMatcher};
pub use provenance::{
    cloud_stt_impl_for_base_url, ENGINE_RUST_IN_PROCESS, STT_IMPL_CLOUD_CUSTOM,
    STT_IMPL_CLOUD_GROQ, STT_IMPL_CLOUD_NEMOTRON, STT_IMPL_CLOUD_OPENAI, STT_IMPL_NEMOTRON_LOCAL,
    STT_IMPL_WHISPER_CPP,
};
pub use restart::{changed_restart_keys, RESTART_REQUIRED_KEYS};
pub(crate) use session::path_util::expand_user;
pub use session::{
    build_preview_status, history_sink_from_settings, metrics_sink_from_settings,
    stderr_preview_sink, DictateSession, HistorySink, InjectBackend, InjectError, JsonlHistorySink,
    JsonlMetricsSink, MetricsSink, NoopHistorySink, NoopMetricsSink, PostProcessBackend,
    PostProcessOutcome, PostRedaction, PreviewBackend, PreviewEmission, PreviewEngine,
    PreviewEngineConfig, PreviewError, PreviewSink, SessionConfig, SessionError, SessionState,
    TranscribeBackend, TranscribeError, TranscribeResult, UtteranceOutcome, SR,
};
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) use session::{history_sink_from_app_settings, metrics_sink_from_app_settings};
pub use skip::{should_skip, SkipDecision, MIN_RECORD_FLOOR_S};

#[cfg(test)]
mod events_tests;
