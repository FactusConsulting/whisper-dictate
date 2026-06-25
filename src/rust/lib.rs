// The cpal capture + rubato resample + Silero VAD pipeline. Compiled in only
// when the `audio-in-rust` feature is on, so the default build still has
// nothing to do with the ONNX runtime / cpal's native backends. See
// src/rust/audio/mod.rs for the wiring + the PR description for the rollout.
#[cfg(feature = "audio-in-rust")]
pub mod audio;
// Pure noise-floor / SNR / gain / silence-trim DSP — Wave 4-C port of
// `src/python/whisper_dictate/vp_audio.py` (#348). Lives at the crate
// root rather than under `audio/` because it has no cpal/ONNX deps and
// must compile in stock builds for tests + future callers.
pub mod audio_dsp;
pub mod cli;
pub mod cloud_api;
pub mod command_hook;
pub mod config;
// Pure-logic helpers for the live PTT dictation loop — Wave 5 port of
// `src/python/whisper_dictate/vp_dictate.py` + `runtime.py` (#348). The
// orchestration layer stays Python; the skip-gate / restart-required
// diff / backend-label / env-flag decisions are mirrored here so the
// Wave 8 Rust supervisor can drop the Python helper. Exposes a hidden
// `dictate-ops` JSON-RPC subcommand the Python caller shells out to
// when `VOICEPI_DICTATE_BACKEND=rust` (default keeps Python).
pub mod dictate;
// Input-device enumeration (Rust port of vp_devices.py, Phase 2.2.z of the
// Python-removal roadmap #348). Gated behind `audio-in-rust` so the default
// build does not pull cpal — the audio capture feature already requires the
// same native deps (libasound on Linux), so sharing the gate keeps the dep
// graph clean. See `src/rust/devices.rs` for the API + JSON envelope.
#[cfg(feature = "audio-in-rust")]
pub mod devices;
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
// Rust port of `vp_postprocess.py` (Wave 4-B of #348). Owns the full
// post-STT formatting / LLM cleanup pipeline: settings validation,
// cloud-safe redaction, prompt construction, provider call (local
// Ollama via /api/generate or OpenAI-compatible /chat/completions),
// extract-final-text and the redaction restore. Python shells out via
// the `postprocess` subcommand when VOICEPI_POSTPROCESS_BACKEND=rust.
pub mod postprocess;
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
