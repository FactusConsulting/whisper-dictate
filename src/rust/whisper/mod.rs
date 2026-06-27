//! Local Whisper integration: model catalog / download / inference.
//!
//! Module layout:
//! - [`model_manager`] — curated catalog of GGML models, download with
//!   SHA-256 integrity check, OS user-cache placement. Compiled
//!   **unconditionally** so the `models` CLI subcommand and the Settings tab
//!   download UI work on every binary, including stock builds that do not
//!   include the whisper.cpp inference path.
//! - [`dispatch`] — JSON-over-stdio dispatcher for the hidden
//!   `transcribe-wav` sub-command (Python ↔ Rust shell-out). Pulls in
//!   whisper.cpp, so it is gated behind `whisper-rs-local`.
//! - [`local`] — the [`LocalWhisper`] type wrapping `whisper-rs`. Also
//!   feature-gated since it links whisper.cpp.
//! - [`idle`] — `IdleUnloadingModel` library primitive (#325, Wave 7-A).
//!   Wraps a loaded model behind a configurable idle timer + background
//!   watcher. Compiled unconditionally so the lifecycle state machine is
//!   unit-tested on every CI run against a fake model. Awaits in-process
//!   runtime wiring (wave 8) — has no runtime effect under today's
//!   subprocess-per-utterance dispatcher.
//! - [`gpu`] — `GpuPolicy` env-var parsing for the Vulkan / future
//!   DirectML / Metal backends (#348 Wave 7-C). Compiled unconditionally
//!   so the env-var schema is the same on every build; `should_use_gpu`
//!   uses `cfg!(feature = ...)` to gate the actual GPU codepath on the
//!   compiled-in backend.
//!
//! The split keeps the cache/download machinery independent of the heavy
//! whisper.cpp dep, so a stock `cargo build` still ships the UI and CLI
//! affordances even without CMake / a C++ toolchain on the build host.

pub mod gpu;
pub mod idle;
pub mod model_manager;
pub mod models_cli;
/// WAV decode helpers (16 kHz mono). Compiled unconditionally so the pure
/// WAV logic is unit-tested without the `whisper-rs-local` / CMake build.
pub mod wav;

#[cfg(feature = "whisper-rs-local")]
pub mod dispatch;
#[cfg(feature = "whisper-rs-local")]
mod local;

pub use gpu::{parse_gpu_policy_from_env, should_use_gpu, GpuPolicy, GPU_ENV};
pub use idle::{parse_idle_timeout_from_env, IdleUnloadingModel, IDLE_UNLOAD_ENV};
pub use wav::{decode_wav_16k_mono, WHISPER_SAMPLE_RATE_HZ};

#[cfg(feature = "whisper-rs-local")]
pub use dispatch::{handle_transcribe_wav, MODEL_PATH_ENV};
#[cfg(feature = "whisper-rs-local")]
pub use local::LocalWhisper;
