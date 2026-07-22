//! Pure noise-floor / SNR / gain / silence-trim DSP.
//!
//! Rust port of `src/python/whisper_dictate/vp_audio.py` (Wave 4-C of the
//! Python-removal roadmap, issue #348). All functions here are pure — no
//! audio device, no I/O, no env reads — and operate on borrowed `f32`
//! sample slices. They mirror the numpy implementation in `vp_audio.py`
//! byte-for-byte at the algorithmic level so the Python caller-facing
//! API can be cut over to this module in a follow-up without changing
//! observable behaviour (the Python `AudioDspTests` characterisation
//! suite continues to pin Python behaviour; the `cfg(test)` modules
//! alongside each submodule here mirror those assertions one-to-one).
//!
//! Lives at the crate root rather than under `src/rust/audio/` because
//! that tree is gated behind the `audio-in-rust` cargo feature (it pulls
//! cpal + the Silero ONNX runtime). This module is pure stdlib + `f32`
//! arithmetic, so unconditional compilation keeps it reachable for tests
//! and any future caller regardless of the audio-capture feature gate.
//!
//! # Module layout
//!
//! The Codex review on PR #354 flagged the original single-file port
//! (~590 LOC) as crossing the repo's modularity gate, so the
//! implementation is split into focused submodules and re-exported here
//! so existing callers keep using `crate::audio_dsp::{...}` unchanged:
//!
//! - [`metrics`] — per-buffer RMS / peak / SNR snapshot, gain boost,
//!   coarse status verdict, and the looks-like-speech gate.
//! - [`silence`] — trailing dead-air trimmer.
//! - `helpers` (private) — numpy-percentile + RMS/peak primitives the
//!   other submodules share.
//!
//! # Wave 4-C choice: Option B (Python wrapper stays caller-facing)
//!
//! `vp_audio.py` is hit on every utterance (gain boost, trim, capture
//! metrics, looks-like-speech gate). A subprocess shim per utterance
//! would add tens of milliseconds of JSON-encode/decode latency to the
//! transcription hot path, which is unacceptable. So this PR ports the
//! logic to Rust + unit-tests it (so a future pure-Rust audio pipeline
//! can drop the Python entirely), but leaves `vp_audio.py` in place as
//! the caller-facing Python API. Same pattern #342 used for vp_health.

mod helpers;
pub mod metrics;
pub mod silence;

pub use metrics::{
    boost_quiet, capture_metrics, input_level_status, looks_like_speech, noise_snr,
    AudioCaptureMetrics,
};
pub use silence::trim_trailing_silence;

/// 30 ms @ 16 kHz — the framing the percentile-based noise floor runs on.
/// Pinned by the Python `AudioDspTests` characterisation suite.
pub const FRAME_SAMPLES: usize = 480;

/// Default target loudness the gain stage normalises quiet input toward.
/// Mirrors `VOICEPI_TARGET_DBFS` (Python default: -20.0 dBFS).
pub const DEFAULT_TARGET_DBFS: f64 = -20.0;

/// Default raw-input gate below which the gain stage refuses to boost
/// (otherwise near-silence gets amplified into Whisper's comfort range
/// and decodes as a plausible short phrase). Mirrors
/// `VOICEPI_MIN_INPUT_DBFS` (Python default: -55.0 dBFS).
pub const DEFAULT_MIN_INPUT_DBFS: f64 = -55.0;

/// Default speech-vs-noise contrast required by the looks-like-speech
/// gate. Below this the buffer is rejected as "no speech contrast".
/// Mirrors `VOICEPI_MIN_SNR_DB` (Python default: 6.0 dB).
pub const DEFAULT_MIN_INPUT_SNR_DB: f64 = 6.0;

/// Knobs that vary at runtime (Python reads them from
/// `apply_config_to_environ()` + the live profile-tunable dict). The
/// defaults match the Python module's compile-time constants so callers
/// that don't care about overrides get behaviour-identical output.
#[derive(Debug, Clone, Copy)]
pub struct StatusThresholds {
    pub target_dbfs: f64,
    pub min_input_dbfs: f64,
    pub min_input_snr_db: f64,
}

impl Default for StatusThresholds {
    fn default() -> Self {
        Self {
            target_dbfs: DEFAULT_TARGET_DBFS,
            min_input_dbfs: DEFAULT_MIN_INPUT_DBFS,
            min_input_snr_db: DEFAULT_MIN_INPUT_SNR_DB,
        }
    }
}

/// `VOICEPI_TARGET_DBFS` env key (boost target).
pub const TARGET_DBFS_ENV: &str = "VOICEPI_TARGET_DBFS";
/// `VOICEPI_MIN_INPUT_DBFS` env key (too-quiet floor).
pub const MIN_INPUT_DBFS_ENV: &str = "VOICEPI_MIN_INPUT_DBFS";
/// `VOICEPI_MIN_SNR_DB` env key (no-contrast floor).
pub const MIN_SNR_DB_ENV: &str = "VOICEPI_MIN_SNR_DB";

/// Read [`StatusThresholds`] from the process env, mirroring the Python
/// module constants (`vp_audio.py`): `VOICEPI_TARGET_DBFS` /
/// `VOICEPI_MIN_INPUT_DBFS` / `VOICEPI_MIN_SNR_DB`, each falling back to the
/// same default when unset, blank, or unparseable.
pub fn thresholds_from_env() -> StatusThresholds {
    thresholds_from_env_with(|name| std::env::var(name).ok())
}

/// Testable core of [`thresholds_from_env`]: resolves each field through
/// `lookup` so the parse is unit-tested without touching process env.
pub fn thresholds_from_env_with(lookup: impl Fn(&str) -> Option<String>) -> StatusThresholds {
    let get = |name: &str, default: f64| {
        lookup(name)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(default)
    };
    StatusThresholds {
        target_dbfs: get(TARGET_DBFS_ENV, DEFAULT_TARGET_DBFS),
        min_input_dbfs: get(MIN_INPUT_DBFS_ENV, DEFAULT_MIN_INPUT_DBFS),
        min_input_snr_db: get(MIN_SNR_DB_ENV, DEFAULT_MIN_INPUT_SNR_DB),
    }
}

/// The pre-transcription speech gate. Trims trailing silence (so a long dead
/// tail doesn't drag the mean level below the too-quiet floor), then runs
/// [`looks_like_speech`]. Returns `Some(reason)` when the buffer is too
/// quiet or too flat to be worth decoding -- the reason string is what
/// `crate::dictate::session::normalize_gate_reason` maps to
/// `too_quiet`/`no_speech` -- or `None` when it looks like speech and should
/// be transcribed. Mirrors the pre-check order in
/// `vp_transcribe._transcribe_detail` (trim -> gate, before the model).
pub fn speech_gate_reason(pcm: &[f32], thresholds: &StatusThresholds) -> Option<String> {
    let (trimmed, _trimmed_ms) = trim_trailing_silence(pcm);
    match looks_like_speech(trimmed, thresholds) {
        (true, _) => None,
        (false, reason) => Some(reason),
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;
