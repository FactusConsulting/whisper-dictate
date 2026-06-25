//! Trailing dead-air trimmer. Anti-hallucination defence: Whisper
//! confidently invents phrases for sustained near-silence tails, so we
//! cut them before decode. Mirrors `vp_audio._trim_trailing_silence`.

use super::helpers::{frame_rms, nonzero_or_eps, percentile, rms_f64};
use super::FRAME_SAMPLES;

// Trim parameters — verbatim from vp_audio.py. See that module's
// inline comments for the rationale (anti-hallucination defence on
// the trailing dead-air tail).
const TRIM_NOISE_MARGIN_DB: f64 = 12.0;
const TRIM_MIN_GAP_DB: f64 = 30.0;
const TRIM_PAD_FRAMES: usize = 4;
const TRIM_MIN_FRAMES: usize = 5;

// Sample rate baked into the framing (16 kHz). Kept as f64 so the
// ms-trimmed conversion `(samples_removed) / 16.0` stays exact.
const SAMPLES_PER_MS: f64 = 16.0;

/// Cut a sustained run of trailing dead air (at the noise floor) off
/// `samples`. Returns `(trimmed_slice, trimmed_ms)`. Leaves the clip
/// untouched (`trimmed_ms == 0.0`) when there is no clear silence
/// floor, no trailing silence, or the cut would remove less than ~150
/// ms. A frame counts as silence only when within ~12 dB of the
/// 10th-percentile noise floor (the `TRIM_NOISE_MARGIN_DB` constant),
/// so any voiced energy — even a very soft trailing word — sits above
/// it and is preserved.
///
/// Mirrors `vp_audio._trim_trailing_silence` verbatim: same framing,
/// same percentile-based noise floor, same `TRIM_MIN_GAP_DB` gate, same
/// remainder-scoring for the trailing partial frame, same ms math.
pub fn trim_trailing_silence(samples: &[f32]) -> (&[f32], f64) {
    let n = samples.len() / FRAME_SAMPLES;
    if n < 4 {
        return (samples, 0.0);
    }
    let mut rms = frame_rms(samples);
    let remainder = &samples[n * FRAME_SAMPLES..];
    if !remainder.is_empty() {
        rms.push(rms_f64(remainder));
    }
    let n_frames = rms.len();
    let noise = nonzero_or_eps(percentile(&mut rms.clone(), 10.0));
    let body = nonzero_or_eps(rms.iter().copied().fold(0.0_f64, f64::max));
    if body <= noise || 20.0 * (body / noise).log10() < TRIM_MIN_GAP_DB {
        return (samples, 0.0);
    }
    let threshold = noise * 10.0_f64.powf(TRIM_NOISE_MARGIN_DB / 20.0);
    let last_speech =
        rms.iter()
            .enumerate()
            .rev()
            .find_map(|(i, v)| if *v > threshold { Some(i) } else { None });
    let Some(last) = last_speech else {
        return (samples, 0.0);
    };
    let keep_frames = (last + 1 + TRIM_PAD_FRAMES).min(n_frames);
    let removed_frames = n_frames - keep_frames;
    if removed_frames < TRIM_MIN_FRAMES {
        return (samples, 0.0);
    }
    let keep = (keep_frames * FRAME_SAMPLES).min(samples.len());
    let trimmed_ms = (samples.len() - keep) as f64 / SAMPLES_PER_MS;
    (&samples[..keep], trimmed_ms)
}

#[cfg(test)]
mod tests {
    //! Mirrors the trim-silence slice of
    //! `src/python/tests/test_audio.py::AudioDspTests`.

    use super::*;

    fn make(value: f32, frames: usize) -> Vec<f32> {
        vec![value; FRAME_SAMPLES * frames]
    }

    fn concat(parts: &[(f32, usize)]) -> Vec<f32> {
        let mut out = Vec::new();
        for (v, frames) in parts {
            out.extend(std::iter::repeat_n(*v, FRAME_SAMPLES * *frames));
        }
        out
    }

    #[test]
    fn trim_cuts_trailing_noise_floor_keeping_speech_plus_pad() {
        let a = concat(&[(0.2, 20), (0.0005, 30)]);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert_eq!(trimmed.len(), FRAME_SAMPLES * 24);
        assert!((ms - 26.0 * 30.0).abs() < 1e-3, "ms={ms}");
    }

    #[test]
    fn trim_keeps_tight_clip_unchanged() {
        let a = concat(&[(0.2, 20), (0.0005, 3)]);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert_eq!(ms, 0.0);
        assert_eq!(trimmed.len(), a.len());
    }

    #[test]
    fn trim_leaves_all_silence_untouched() {
        let a = make(0.0005, 10);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert_eq!(ms, 0.0);
        assert_eq!(trimmed.len(), a.len());
    }

    #[test]
    fn trim_keeps_quietly_trailing_word() {
        let a = concat(&[(0.2, 10), (0.008, 10), (0.0005, 20)]);
        let (trimmed, _) = trim_trailing_silence(&a);
        assert_eq!(trimmed.len(), FRAME_SAMPLES * 24);
    }

    #[test]
    fn trim_too_short_buffer_unchanged() {
        let a = make(0.2, 2);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert_eq!(ms, 0.0);
        assert_eq!(trimmed.len(), a.len());
    }

    #[test]
    fn trim_preserves_speech_in_final_partial_frame() {
        let mut a = Vec::new();
        a.extend(vec![0.2_f32; FRAME_SAMPLES * 20]);
        a.extend(vec![0.0005_f32; FRAME_SAMPLES * 30]);
        a.extend(vec![0.2_f32; 240]); // blip in the partial frame
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert_eq!(ms, 0.0);
        assert_eq!(trimmed.len(), a.len());
    }

    #[test]
    fn trim_keeps_soft_trailing_speech_without_silence() {
        let a = concat(&[(0.2, 20), (0.02, 20)]);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert_eq!(ms, 0.0);
        assert_eq!(trimmed.len(), a.len());
    }

    #[test]
    fn trim_cuts_long_tail_after_short_speech() {
        let a = concat(&[(0.2, 3), (0.0005, 47)]);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert!(ms > 0.0, "ms={ms}");
        assert_eq!(trimmed.len(), FRAME_SAMPLES * 7);
    }

    #[test]
    fn trim_keeps_very_soft_trailing_word_above_noise_floor() {
        let a = concat(&[(0.2, 15), (0.005, 5), (0.0003, 20)]);
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert!(ms > 0.0, "ms={ms}");
        assert_eq!(trimmed.len(), FRAME_SAMPLES * 24);
    }

    #[test]
    fn trim_robust_to_stray_silent_frame() {
        let mut a = Vec::new();
        a.extend(vec![0.0_f32; FRAME_SAMPLES]); // 1 dropout
        a.extend(vec![0.2_f32; FRAME_SAMPLES * 19]); // speech
        a.extend(vec![0.0005_f32; FRAME_SAMPLES * 30]); // dead tail
        let (trimmed, ms) = trim_trailing_silence(&a);
        assert!(ms > 0.0, "ms={ms}");
        assert!(trimmed.len() < a.len());
    }
}
