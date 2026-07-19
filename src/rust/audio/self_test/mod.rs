//! `whisper-dictate self-test audio-capture` — headless regression test
//! for the cpal capture path (item 5 prereq 4 — foundation for real Rust
//! dictation).
//!
//! ## Bug class this catches
//!
//! Two independent failure modes the shipping capture path must not
//! silently regress:
//!
//! 1. **Stream never starts.** cpal returns Ok from `build_input_stream`
//!    but the callback never fires (v1.20.6 DMIC-on-PipeWire class — see
//!    [`super::pipewire`] for the quantum fix). The report's
//!    `frames_captured` is zero and the exit code is non-zero, so CI trips.
//! 2. **Stream starts but delivers silence.** Backend picked, callbacks
//!    firing, but every sample is 0.0 — usually a permission problem or a
//!    muted device. The report includes RMS + peak; a `--fail-on-silence`
//!    flag turns this into a hard fail for CI legs where an audio device
//!    is guaranteed present.
//!
//! ## Layout
//!
//! Split across two siblings so no single file passes AGENTS.md's
//! ~500-line "new file" ceiling — same pattern the injection self-test
//! (`injection::self_test`) uses:
//!
//! * [`report`] — [`AudioCaptureReport`] plus the JSON / plain
//!   rendering + report-shape unit tests. Owns nothing that touches
//!   cpal, so a future stock-build stub could compile just this half if
//!   needed.
//! * [`runner`] — [`AudioCaptureOptions`], [`run_audio_capture_test`],
//!   and the pure [`runner::Accumulator`] used to tally RMS + peak.
//!
//! ## Feature gating
//!
//! This entire module lives inside `audio/`, which is gated on the
//! `audio-in-rust` cargo feature. Stock builds surface the CLI verb but
//! error out with a rebuild hint at dispatch time (see the two-branch
//! `handle_audio_capture_self_test` in `main.rs`).
//!
//! ## Scope
//!
//! Deliberately does NOT wire into the VAD / resampler / stdin bridge —
//! this verb is a pure "does cpal give me samples?" check. The end-to-end
//! pipeline is exercised by the Phase C step 2 integration tests once
//! DictateSession has a real audio source; landing that with a broken
//! cpal open would waste a debug cycle, so this verb catches it first.

pub mod report;
pub mod runner;

pub use report::AudioCaptureReport;
pub use runner::{run_audio_capture_test, AudioCaptureOptions, SILENCE_RMS_THRESHOLD};
