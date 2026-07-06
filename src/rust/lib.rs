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
// Pure scoring / reporting port of `vp_benchmark` + `vp_benchmark_report`
// (Wave 6 of #348). The full benchmark orchestrator stays in Python because it
// drives the heavyweight STT backends; this module owns the WER/CER/term
// matching + summary line shaping that's worth cross-checking in Rust, plus
// the thin `bench` CLI handler that shells out to the existing worker
// command.
pub mod benchmark;
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
// Golden-corpus loader + manifest path resolution (Rust port of
// `vp_benchmark.load_corpus` + `vp_benchmark_paths.resolve_corpus_manifest`,
// Wave 6 follow-up to the dictionary-training CLI port). Used by the
// `dictionary build-from-corpus` subcommand.
pub mod corpus;
// Pure-logic port of the `--record-corpus-item` user tool (Wave 6 of #348).
// Audio capture stays in Python (reuses the negotiated `vp_capture` path);
// this module owns the corpus-id safety guard + the duration heuristic +
// the thin `corpus-record` CLI handler that shells out to the existing worker
// command.
pub mod corpus_record;
// Pure corpus filter-profiles: select a subset of corpus items by
// language/category (Rust port of `vp_corpus_profile.py`). Used by the
// `dictionary build-from-corpus` subcommand to mirror the Python flags.
pub mod corpus_profile;
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
// Local SQLite-backed transcription history (issue #324). Owns the
// schema, insert path, and search API for the per-user history.sqlite3
// store. Gated behind the `history-sqlite` cargo feature (default-on)
// so a `--no-default-features` build doesn't pull rusqlite + the
// bundled SQLite C compile. The supervisor's utterance-event hook
// (`crate::runtime::stream_lines`) calls into this module on each
// `event="utterance"` line; failures are logged and swallowed so a DB
// hiccup never breaks dictation. The `History` CLI subcommand
// (`history list / last / search`) also routes through here when the
// feature is on.
#[cfg(feature = "history-sqlite")]
pub mod history;
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
// Auto-mute the system audio output while recording (issue #322). Pure
// state machine + tiny per-OS subprocess/COM shims; no cpal / ONNX
// deps, so it compiles into every build regardless of `audio-in-rust`.
// Feature is behind the AppSettings.mute_output_while_recording toggle
// (default OFF); `runtime::stream_lines` fans worker-state events into
// `output_mute::session::observe_worker_state`, which is a cheap no-op
// when the toggle is off.
pub mod output_mute;
// Rust port of `vp_postprocess.py` (Wave 4-B of #348). Owns the full
// post-STT formatting / LLM cleanup pipeline: settings validation,
// cloud-safe redaction, prompt construction, provider call (local
// Ollama via /api/generate or OpenAI-compatible /chat/completions),
// extract-final-text and the redaction restore. Python shells out via
// the `postprocess` subcommand when VOICEPI_POSTPROCESS_BACKEND=rust.
pub mod postprocess;
// Second-hotkey LLM post-processing (issue #319). Layered on top of the
// Wave 4-B `postprocess` module: adds a profile registry the user can
// cycle through with a second hotkey and a dispatcher that runs the
// active profile against the last dictated utterance (SQLite history or
// caller-supplied clipboard fallback). Exposes a hidden
// `postprocess-hotkey` subcommand mirroring the `postprocess` envelope
// shape so the Python worker can shell out to the same pipeline.
pub mod postprocess_hotkey;
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
// Local Whisper integration. The catalog / download / cache machinery under
// `whisper::model_manager` is always compiled in (lightweight: ureq + sha2,
// already in the dep graph), so the `models` CLI subcommand and the Settings
// tab download UI work on every binary. The whisper.cpp inference + the
// `transcribe-wav` dispatcher still sit behind the `whisper-rs-local`
// feature (which pulls whisper.cpp / CMake).
pub mod whisper;
