//! Pure noise-floor / SNR / gain / silence-trim DSP.
//!
//! All functions here are pure — no audio device, no I/O, no env reads
//! — and operate on borrowed `f32` sample slices. The `cfg(test)`
//! modules alongside each submodule pin the algorithmic behaviour.
//!
//! Lives at the crate root rather than under `src/rust/audio/` because
//! that tree is gated behind the `audio-capture` cargo feature. This module is
//! pure stdlib + `f32`
//! arithmetic, so unconditional compilation keeps it reachable for tests
//! and any future caller regardless of the audio-capture feature gate.
//!
//! # Module layout
//!
//! The implementation is split into focused submodules and re-exported
//! here so existing callers keep using `crate::audio_dsp::{...}`
//! unchanged:
//!
//! - [`metrics`] — per-buffer RMS / peak / SNR snapshot, gain boost,
//!   coarse status verdict, and the looks-like-speech gate.
//! - [`silence`] — trailing dead-air trimmer.
//! - `helpers` (private) — numpy-percentile + RMS/peak primitives the
//!   other submodules share.
//!

mod helpers;
pub mod metrics;
pub mod silence;

pub use metrics::{
    boost_quiet, capture_metrics, input_level_status, looks_like_speech, noise_snr,
    AudioCaptureMetrics,
};
pub use silence::trim_trailing_silence;

/// 30 ms @ 16 kHz — the framing the percentile-based noise floor runs on.
/// Pinned by the characterisation tests.
pub const FRAME_SAMPLES: usize = 480;

/// Default target loudness the gain stage normalises quiet input toward.
/// Mirrors `VOICEPI_TARGET_DBFS` (default: -20.0 dBFS).
pub const DEFAULT_TARGET_DBFS: f64 = -20.0;

/// Default raw-input gate below which the gain stage refuses to boost
/// (otherwise near-silence gets amplified into Whisper's comfort range
/// and decodes as a plausible short phrase). Mirrors
/// `VOICEPI_MIN_INPUT_DBFS` (default: -55.0 dBFS).
pub const DEFAULT_MIN_INPUT_DBFS: f64 = -55.0;

/// Default speech-vs-noise contrast required by the looks-like-speech
/// gate. Below this the buffer is rejected as "no speech contrast".
/// Mirrors `VOICEPI_MIN_SNR_DB` (default: 6.0 dB).
pub const DEFAULT_MIN_INPUT_SNR_DB: f64 = 6.0;

/// Knobs that vary at runtime (read from env + the live
/// profile-tunable dict). Defaults match the compile-time constants so
/// callers that don't care about overrides get identical output.
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

/// Read [`StatusThresholds`] from the process env: `VOICEPI_TARGET_DBFS`
/// / `VOICEPI_MIN_INPUT_DBFS` / `VOICEPI_MIN_SNR_DB`, each falling back to the
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
/// be transcribed, in the pre-model order (trim -> gate, before the
/// model).
pub fn speech_gate_reason(pcm: &[f32], thresholds: &StatusThresholds) -> Option<String> {
    gate_and_trim(pcm, thresholds).reject
}

/// The trailing-silence-trimmed audio a backend should actually decode +
/// time, paired with the speech-gate decision.
///
/// [`GatedAudio::trimmed`] is a sub-slice of the input with its sustained
/// dead-air tail removed, and [`GatedAudio::reject`] is `Some(reason)` when
/// the (trimmed) buffer is too quiet / too flat to be worth decoding, else
/// `None`.
#[derive(Debug)]
pub struct GatedAudio<'a> {
    /// The buffer to decode. Callers MUST feed this (not the original PCM)
    /// to the model AND derive `duration_s` from its length, so a long dead
    /// tail neither gives Whisper empty audio to hallucinate a caption over
    /// nor inflates the chars-per-second denominator of the speech-rate
    /// guard: the trim runs FIRST, then the gate, decode, and duration
    /// math all see the same trimmed buffer.
    pub trimmed: &'a [f32],
    /// `Some(reason)` to reject before decoding (the reason string is what
    /// `crate::dictate::session::normalize_gate_reason` maps to
    /// `too_quiet`/`no_speech`), or `None` to proceed with transcription.
    pub reject: Option<String>,
}

/// Trim the trailing dead-air tail ONCE, then run the [`looks_like_speech`]
/// gate on the trimmed buffer, returning both so the caller can decode + time
/// the same trimmed slice: one trimmed buffer feeds gate, decode, and
/// duration. Prefer this over
/// [`speech_gate_reason`] in a transcribe backend: the latter discards the
/// trimmed slice, which would leave the untrimmed tail feeding the model and
/// stretching the speech-rate denominator.
pub fn gate_and_trim<'a>(pcm: &'a [f32], thresholds: &StatusThresholds) -> GatedAudio<'a> {
    let (trimmed, _trimmed_ms) = trim_trailing_silence(pcm);
    let reject = match looks_like_speech(trimmed, thresholds) {
        (true, _) => None,
        (false, reason) => Some(reason),
    };
    GatedAudio { trimmed, reject }
}

/// The decode-ready audio a transcribe backend hands to the model, or the
/// gate rejection -- the full pre-model pipeline.
#[derive(Debug)]
pub enum PreparedAudio {
    /// The trimmed + boosted audio to decode/encode, plus its clip duration.
    Decode { audio: Vec<f32>, duration_s: f64 },
    /// The gate rejected the (trimmed) audio: the free-form reason (mapped to
    /// `too_quiet`/`no_speech` by `crate::dictate::session::normalize_gate_reason`)
    /// plus the duration to report on the resulting no-text event.
    Reject { reason: String, duration_s: f64 },
}

/// Prepare `pcm` for transcription, in order:
///
/// 1. [`trim_trailing_silence`] the dead-air tail ONCE,
/// 2. gate the trimmed buffer with [`looks_like_speech`] (reject too-quiet
///    no-contrast audio before any model work), and
/// 3. on a pass, [`boost_quiet`] the trimmed audio toward the target level.
///
/// `duration_s` is measured from the TRIMMED length -- the boost is gain-only
/// and does not change it -- so a long dead tail neither feeds the model empty
/// audio to hallucinate a caption over nor inflates the chars-per-second
/// denominator of the speech-rate guard. Shared by BOTH the local and cloud
/// STT backends so their pre-model processing is identical and unit-tested
/// once here.
pub fn prepare_for_transcription(
    pcm: &[f32],
    sample_rate: u32,
    thresholds: &StatusThresholds,
) -> PreparedAudio {
    let gated = gate_and_trim(pcm, thresholds);
    // Guard `sample_rate == 0` (a direct backend caller could pass it) so the
    // f64 division never reports the sample count as seconds -- an inflated
    // duration would slip an over-rate transcript past the speech-rate guard.
    // Mirrors the local backend's original `if sample_rate == 0 { 0.0 }`.
    let duration_s = if sample_rate == 0 {
        0.0
    } else {
        gated.trimmed.len() as f64 / f64::from(sample_rate)
    };
    match gated.reject {
        Some(reason) => PreparedAudio::Reject { reason, duration_s },
        None => {
            let (audio, _metrics) = boost_quiet(gated.trimmed, thresholds);
            PreparedAudio::Decode { audio, duration_s }
        }
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;
