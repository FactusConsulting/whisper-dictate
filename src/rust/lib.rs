// The cpal capture + rubato resample pipeline. Compiled only with the
// `audio-capture` feature; the default build does not touch native audio.
#[cfg(feature = "audio-capture")]
pub mod audio;
// Pure noise-floor / SNR / gain / silence-trim DSP. Lives at the
// crate root rather than under `audio/` because it has no cpal deps
// and must compile in stock builds for tests + future callers.
pub(crate) mod atomic_file;
pub mod audio_dsp;
// Benchmark scoring / reporting: WER/CER/term matching + summary line
// shaping, plus the thin `bench` CLI handler.
pub mod benchmark;
pub(crate) mod bounded_process;
pub mod calibration;
pub mod cli;
pub mod cloud_api;
pub mod command_hook;
pub mod config;
#[cfg(test)]
mod test_http_stream;
// Pure-logic helpers for the live PTT dictation loop: skip-gate /
// restart-required diff / backend-label / env-flag decisions. Exposes
// a hidden `dictate-ops` JSON-RPC subcommand for scripted control.
pub mod dictate;
// Golden-corpus loader + manifest path resolution. Used by the
// `dictionary build-from-corpus` subcommand.
pub mod corpus;
// Pure-logic helpers for the `corpus-record` user tool. Owns the
// corpus-id safety guard + the duration heuristic + the
// `corpus-record` CLI handler. On `audio-capture` builds the handler
// dispatches to the native cpal recorder in `corpus_record_native`; on
// stock dev builds (no `audio-capture`) it returns a "rebuild with
// --features audio-capture" error.
pub mod corpus_record;
// Native cpal recorder for `corpus-record <id>` — the sole surface for
// corpus recording. Emits the SAME JSON envelope the UI parser expects and writes 16 kHz mono
// int16 WAVs to `<appdata>/benchmark/audio/<id>.wav` so existing recordings
// remain interchangeable. Gated on `audio-capture` because it needs the
// `crate::audio` capture stack.
#[cfg(feature = "audio-capture")]
pub mod corpus_record_native;
// Pure corpus filter-profiles: select a subset of corpus items by
// language/category. Used by the `dictionary build-from-corpus`
// subcommand.
pub mod corpus_profile;
// Diagnostic file sink for the Windows GUI binary. Solves the
// "stderr is silent" symptom of Windows PTT bug reports
// `whisper-dictate-gui.exe` is `windows_subsystem = "windows"` so it
// has no console, and every `eprintln!` from the rdev listener,
// supervisor Phase-B branches, and hotkey install path goes to a
// discarded stderr. This module lets the GUI open a tee log at
// `%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log` so future
// Windows PTT wedges are inspectable after the fact without a rebuild.
pub mod diag;
// Drop accounting for the off-callback diagnostic queue. Split out of
// `diag.rs` (which is the sink, not the ledger) so the lock-free
// two-counter logic is readable and unit-testable on its own; re-exported
// as `crate::diag::DropLedger`.
pub(crate) mod diag_drop_ledger;
// Admission gate for the off-callback diagnostic queue: teardown closes
// it before polling for shutdown-sentinel space, so a producer that keeps
// firing through exit cannot take back every slot the writer frees and
// starve the sentinel for the whole deadline (comment
// 3669689764).
pub(crate) mod diag_shutdown_gate;
#[cfg(test)]
mod diag_shutdown_gate_tests;
#[cfg(test)]
mod diag_tests;
// Input-device enumeration. Gated behind `audio-capture` so the
// default build does not pull cpal. See `src/rust/devices.rs` for the API + JSON
// envelope.
#[cfg(feature = "audio-capture")]
pub mod devices;
pub mod dictionary;
// Platform-readiness diagnostic CLI (`wd doctor`). Runs a
// battery of read-only checks (OS, session, models cache, injection
// backend availability, audio input, config validity, configured model)
// and reports a pass/fail matrix in plain text or JSON. Designed to
// help users troubleshoot when the native runtime is unavailable. See `docs/ARCHITECTURE.md`.
pub mod doctor;
// Shared binary-entrypoint shell: both shipping binaries
// (`whisper-dictate.exe`, `whisper-dictate-gui.exe`) delegate their exit-code
// + stderr-on-error handling into `error_exit_shell` so the pattern is
// unit-tested once here instead of duplicated (and untested) in each `main`.
pub mod entrypoint;
#[cfg(test)]
mod entrypoint_tests;
pub mod formatting;
pub mod health;
// Public `history` CLI verbs (list, last, copy-last, reinject-last,
// search). The append + preview helpers stay in `telemetry`; this
// module wraps the READER side. See `history.rs` for the dispatch +
// clipboard subprocess helper.
pub mod history;
pub(crate) mod history_retention;
mod jsonl;
pub(crate) mod jsonl_file;
pub(crate) mod log_rotation;
// Rust-side PTT hotkey coordinator (issue #318). The side-aware modifier
// matcher and the stage state machine compile unconditionally so their unit
// tests run on every CI job; the OS listener layer is gated behind the
// `rust-hotkeys` cargo feature. See src/rust/hotkey/mod.rs for the rollout.
pub mod hotkey;
pub mod injection;
pub mod model_capacity;
// Shared OS-cache helpers (`replace_atomic`, `user_cache_dir`) used by both
// `audio::model_cache` (feature-gated) and `whisper::model_manager`
// (unconditional). Extracted here to avoid a cross-module dependency that
// crosses a feature boundary.
pub(crate) mod os_cache;
// Cross-platform host integration seams the dictate engine needs but that
// don't cleanly belong to any feature-gated module. Today: the foreground-
// window probe (title + process) that drives per-utterance target-profile
// matching). Kept at the crate root so the probe compiles in every build
// config (no cargo features required).
pub mod platform;
// Owns the full post-STT formatting / LLM cleanup pipeline: settings
// validation, cloud-safe redaction, prompt construction, provider call
// (local Ollama via /api/generate or OpenAI-compatible
// /chat/completions), extract-final-text and the redaction restore.
pub mod postprocess;
pub mod privacy;
pub mod profiles;
pub mod redaction;
pub mod runtime;
pub mod setup;
pub mod telemetry;
pub mod transcribe_file;
#[cfg(test)]
mod transcribe_file_tests;
// Shared crate-wide lock for tests that mutate process env vars. Lives at the
// crate root so every module's `test_support` can re-export the same lock
// see the module's docs for why a single lock is the only sound design.
// Console-output ASCII guard. Tokenizes every source file with the real Rust
// lexer -- see the module docs for why this is not a script.
#[cfg(test)]
#[path = "console_ascii_tests.rs"]
mod console_ascii_tests;
pub mod credentials;
#[cfg(test)]
mod credentials_tests;
#[cfg(test)]
pub(crate) mod diag_test_lock;
#[cfg(test)]
pub(crate) mod test_env_lock;
#[cfg(all(test, target_os = "linux"))]
pub(crate) mod test_xdotool;
pub mod ui;
// Local Whisper integration. The catalog / download / cache machinery under
// `whisper::model_manager` is always compiled in (lightweight: ureq + sha2,
// already in the dep graph), so the `models` CLI subcommand and the Settings
// tab download UI work on every binary. The whisper.cpp inference + the
// `transcribe-wav` dispatcher still sit behind the `whisper-rs-local`
// feature (which pulls whisper.cpp / CMake).
pub mod whisper;
