// The cpal capture + rubato resample + Silero VAD pipeline. Compiled in only
// when the `audio-in-rust` feature is on, so the default build still has
// nothing to do with the ONNX runtime / cpal's native backends. See
// src/rust/audio/mod.rs for the wiring + the PR description for the rollout.
#[cfg(feature = "audio-in-rust")]
pub mod audio;
pub mod cli;
pub mod cloud_api;
pub mod command_hook;
pub mod config;
pub mod dictionary;
pub mod formatting;
pub mod health;
// Rust-side PTT hotkey coordinator (issue #318). The side-aware modifier
// matcher and the stage state machine compile unconditionally so their unit
// tests run on every CI job; the OS listener layer is gated behind the
// `rust-hotkeys` cargo feature. See src/rust/hotkey/mod.rs for the rollout.
pub mod hotkey;
pub mod injection;
pub mod model_capacity;
pub mod privacy;
pub mod profiles;
pub mod redaction;
pub mod runtime;
pub mod telemetry;
// Shared crate-wide lock for tests that mutate process env vars. Lives at the
// crate root so every module's `test_support` can re-export the same lock —
// see the module's docs for why a single lock is the only sound design.
#[cfg(test)]
pub(crate) mod test_env_lock;
pub mod ui;
// Local Whisper inference (CPU-only spike, roadmap issue #317 sub-task 1).
// Gated behind the `whisper-rs-local` cargo feature so the default build
// never pulls in whisper.cpp / CMake.
#[cfg(feature = "whisper-rs-local")]
pub mod whisper;
