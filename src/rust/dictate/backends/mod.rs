//! Production `TranscribeBackend` / `InjectBackend` trait impls that wrap
//! the existing inference + injection code.
//!
//! Wave 5 PR 5-prep of issue #348. The trait boundaries themselves were
//! introduced in PR 2 (#413) inside [`crate::dictate::session::types`];
//! this module supplies the **real** implementations that PR 5 will swap
//! in place of the stub backends today's coordinator-sink wiring (PR 4)
//! installs.
//!
//! # No production caller in this PR
//!
//! This PR adds the trait impls only. The sink wiring in
//! `crate::runtime` continues to use the PR 4 stubs. The Wave 5 PR 5
//! follow-up will rename `StubTranscribe::new()` → call sites to
//! [`WhisperLocalTranscribeBackend::new`] and the equivalent for
//! injection — a trivial change once both this PR and PR 4 land.
//!
//! # Feature gating
//!
//! Each backend is gated on the cargo feature that already controls the
//! underlying dependency, so default builds compile zero new code from
//! this PR:
//!
//! - [`whisper_local`] — gated on `whisper-rs-local` (whisper.cpp).
//! - [`inject`] — gated on `rust-injection` (enigo).
//! - [`cloud`] — no feature gate. `cloud_api::cloud_transcribe` is
//!   always compiled in, so every build can talk to an OpenAI-
//!   compatible transcription endpoint. Added in Wave 5.5 gap #1 of
//!   #348 so a rust-session install with `stt_backend = "openai"`
//!   stops falling back to Python for cloud STT.
//!
//! Tests for each backend live in a sibling `*_tests.rs` file, also
//! gated on the same feature so they only run when the underlying
//! dependency is available.

pub mod cloud;
#[cfg(feature = "rust-injection")]
pub mod inject;
#[cfg(feature = "whisper-rs-local")]
pub mod whisper_local;

pub use cloud::{CloudBackendConfig, CloudTranscribeBackend};
#[cfg(feature = "rust-injection")]
pub use inject::EnigoInjectBackend;
#[cfg(feature = "whisper-rs-local")]
pub use whisper_local::{is_hallucination, WhisperLocalTranscribeBackend};
