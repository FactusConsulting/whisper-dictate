//! Production `TranscribeBackend` / `InjectBackend` trait impls that wrap
//! the existing inference + injection code.
//!
//! The trait boundaries live in [`crate::dictate::session::types`]; this
//! module supplies the real implementations the coordinator-sink wiring
//! in `crate::runtime` installs.
//!
//! # Feature gating
//!
//! Each backend is gated on the cargo feature that already controls the
//! underlying dependency, so default builds compile zero new code:
//!
//! - [`whisper_local`] — gated on `whisper-rs-local` (whisper.cpp).
//! - [`inject`] — gated on `rust-injection` (enigo).
//!
//! Tests for each backend live in a sibling `*_tests.rs` file, also
//! gated on the same feature so they only run when the underlying
//! dependency is available.

// Cloud STT is stock (cloud_api + hound are unconditional deps), so unlike
// the local-whisper / enigo backends it carries no cargo-feature gate.
pub mod cloud_transcribe;
// Stock hallucination blacklist filter shared by the local + cloud
// backends. No feature gate so
// the cloud path can filter too and the filter is tested on every build.
pub mod hallucination;
#[cfg(feature = "rust-injection")]
pub mod inject;
#[cfg(feature = "nemotron-local")]
mod nemotron_assets;
#[cfg(feature = "nemotron-local")]
mod nemotron_ffi;
#[cfg(feature = "nemotron-local")]
pub mod nemotron_local;
// Runtime local-vs-cloud transcribe selector. Stock (generic over the
// local backend `L`) so it compiles + unit-tests on every build; the
// feature-gated `make_real_session` binds `L = WhisperLocalTranscribeBackend`.
pub mod production_transcribe;
#[cfg(feature = "whisper-rs-local")]
pub mod whisper_local;

pub use cloud_transcribe::{CloudTranscribeBackend, CloudTranscribeConfig};
pub use hallucination::is_hallucination;
#[cfg(feature = "rust-injection")]
pub(crate) use inject::lock_pipeline;
#[cfg(feature = "rust-injection")]
pub use inject::EnigoInjectBackend;
#[cfg(all(feature = "rust-injection", any(target_os = "windows", test)))]
pub(crate) use inject::RestoreState;
#[cfg(feature = "nemotron-local")]
pub use nemotron_local::{NemotronLocalBackendConfig, NemotronLocalTranscribeBackend};
pub use production_transcribe::ProductionTranscribeBackend;
#[cfg(feature = "whisper-rs-local")]
pub use whisper_local::{WhisperLocalPreviewBackend, WhisperLocalTranscribeBackend};
