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
//!
//! Tests for each backend live in a sibling `*_tests.rs` file, also
//! gated on the same feature so they only run when the underlying
//! dependency is available.

#[cfg(feature = "rust-injection")]
pub mod inject;
#[cfg(feature = "whisper-rs-local")]
pub mod whisper_local;

#[cfg(feature = "rust-injection")]
pub use inject::EnigoInjectBackend;
#[cfg(feature = "whisper-rs-local")]
pub use whisper_local::{is_hallucination, WhisperLocalTranscribeBackend};
