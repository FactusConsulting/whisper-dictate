//! WAV decoding helpers for the local Whisper inference path.
//!
//! Extracted from `local.rs` to keep module sizes under the 500-LOC cap.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Sample rate Whisper expects on its input PCM buffer (16 kHz mono).
pub const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

/// Decode a WAV file into f32 mono samples, enforcing 16 kHz / 1 channel.
///
/// The WAV must be exactly 16 kHz, single-channel, integer or float PCM
/// (we convert to `f32` in [-1.0, 1.0]). Any other shape is rejected
/// with a descriptive error rather than being silently resampled —
/// resampling is a runtime-wiring concern and out of scope for the
/// library-level spike.
pub fn decode_wav_16k_mono(wav_path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(wav_path)
        .with_context(|| format!("failed to open WAV file {}", wav_path.display()))?;
    let spec = reader.spec();

    if spec.channels != 1 {
        return Err(anyhow!(
            "WAV must be mono (1 channel); {} has {} channels",
            wav_path.display(),
            spec.channels
        ));
    }
    if spec.sample_rate != WHISPER_SAMPLE_RATE_HZ {
        return Err(anyhow!(
            "WAV must be {} Hz; {} is {} Hz",
            WHISPER_SAMPLE_RATE_HZ,
            wav_path.display(),
            spec.sample_rate
        ));
    }

    // Normalize whatever the file stores into f32 in [-1.0, 1.0]. hound
    // exposes integer PCM as i32 (sign-extended from the actual bit depth)
    // and float PCM as f32; we cover the two we are likely to encounter
    // from any standard recorder.
    let mut samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read float samples from {}", wav_path.display()))?,
        hound::SampleFormat::Int => {
            // Guard against malformed/exotic WAVs claiming bit depths that
            // would either overflow our shift (`1i64 << 63` panics in debug)
            // or that hound can't decode into i32 anyway. WAV PCM tops out
            // at 32-bit integer in practice; reject anything wider with a
            // clean error instead of crashing.
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(anyhow!(
                    "integer WAV bit depth {} is not supported (must be 1..=32); {}",
                    spec.bits_per_sample,
                    wav_path.display()
                ));
            }
            // Compute the full-scale magnitude in i64 to avoid overflow when
            // bits_per_sample == 32: `i32::pow(2, 31)` panics in debug and
            // wraps to i32::MIN in release, which would silently invert every
            // sample. i64 has plenty of headroom for any hound-supported depth.
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| {
                    format!("failed to read int samples from {}", wav_path.display())
                })?
        }
    };

    // Float WAVs are *spec'd* to range in [-1.0, 1.0] but real-world files
    // (loud masters, 0-dBFS exports, mixdowns that preserve headroom) often
    // ship outside that range. Whisper expects normalised audio; out-of-range
    // peaks produce silently wrong transcriptions on otherwise valid input.
    //
    // Reject NaN/Inf up front (we can't meaningfully scale them), then
    // divide by max-abs if any sample exceeds 1.0. We don't *amplify* quiet
    // files (max < 1.0) — they may be intentionally low and whisper handles
    // silence-padded windows fine. Integer paths already produce values in
    // [-1, 1] by construction, but the normalisation pass is cheap and
    // protects against a future int decoder change too.
    if matches!(spec.sample_format, hound::SampleFormat::Float) {
        let mut max_abs: f32 = 0.0;
        for &s in &samples {
            if !s.is_finite() {
                return Err(anyhow!(
                    "float WAV {} contains a non-finite sample (NaN/Inf)",
                    wav_path.display()
                ));
            }
            let a = s.abs();
            if a > max_abs {
                max_abs = a;
            }
        }
        if max_abs > 1.0 {
            for s in &mut samples {
                *s /= max_abs;
            }
        }
    }

    if samples.is_empty() {
        return Err(anyhow!(
            "WAV file {} contains no samples",
            wav_path.display()
        ));
    }
    Ok(samples)
}
