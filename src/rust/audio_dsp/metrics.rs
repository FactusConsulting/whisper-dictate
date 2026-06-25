//! Per-buffer mic-input metrics: RMS / peak / noise-floor / SNR
//! snapshot, gain-boost stage, coarse health verdict, and the
//! `looks_like_speech` gate. Mirrors `vp_audio.py`.

use super::helpers::{frame_rms, nonzero_or_eps, peak_abs_f64, percentile, rms_f64};
use super::StatusThresholds;

/// Per-buffer mic-input snapshot — the same five values
/// `AudioCaptureMetrics` carries in Python.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCaptureMetrics {
    /// RMS loudness of the raw (pre-boost) buffer, in dBFS.
    pub raw_dbfs: f64,
    /// Peak absolute sample value in the raw buffer (0.0..=1.0).
    pub peak: f64,
    /// Gain multiplier the boost stage would apply. Always <= 0.99/peak
    /// so the boosted output never clips.
    pub gain: f64,
    /// Estimated room-tone noise floor in dBFS (10th-percentile frame).
    pub noise_dbfs: f64,
    /// Speech-vs-noise contrast in dB (90th vs 10th percentile RMS).
    pub snr_db: f64,
    /// Coarse health verdict for the user-facing meter. One of the
    /// stable tokens emitted by [`input_level_status`].
    pub input_status: &'static str,
}

/// Percentile-based noise floor + SNR estimate. Frame into 30 ms
/// windows; the quiet frames between/around words ARE the noise. Noise
/// floor = 10th pct of per-frame RMS (in dBFS); SNR = how far speech
/// (90th pct) sits above it. SNR is gain-invariant so a uniform boost
/// can't flatter it. Returns `(-90.0, 0.0)` on buffers with fewer than
/// 4 full frames (matching the Python guard against `log10(0)`).
pub fn noise_snr(samples: &[f32]) -> (f64, f64) {
    let mut rms = frame_rms(samples);
    if rms.len() < 4 {
        return (-90.0, 0.0);
    }
    let lo = nonzero_or_eps(percentile(&mut rms.clone(), 10.0));
    let hi = nonzero_or_eps(percentile(&mut rms, 90.0));
    let noise_dbfs = 20.0 * lo.log10();
    let snr_db = 20.0 * (hi / lo).log10();
    (noise_dbfs, snr_db)
}

/// Coarse health verdict the UI uses to colour the mic meter. Order
/// matters — too-quiet beats low-snr beats clip-risk beats hot/quiet.
/// Returned tokens are stable (the Rust UI maps them to colour/icon).
pub fn input_level_status(
    raw_dbfs: f64,
    peak: f64,
    snr_db: f64,
    thresholds: &StatusThresholds,
) -> &'static str {
    if raw_dbfs < thresholds.min_input_dbfs {
        return "too_quiet";
    }
    if snr_db < thresholds.min_input_snr_db {
        return "low_snr";
    }
    if peak >= 0.98 {
        return "clip_risk";
    }
    if peak >= 0.75 || raw_dbfs > -18.0 {
        return "hot";
    }
    if raw_dbfs < -42.0 {
        return "quiet";
    }
    "good"
}

/// Full per-buffer capture snapshot — gain that the boost stage would
/// apply, raw loudness, peak, noise floor, SNR, and the coarse status
/// token. Mirrors `vp_audio._capture_metrics`.
pub fn capture_metrics(samples: &[f32], thresholds: &StatusThresholds) -> AudioCaptureMetrics {
    let rms = nonzero_or_eps(rms_f64(samples));
    let cur_dbfs = 20.0 * rms.log10();
    let mut gain = 10.0_f64.powf((thresholds.target_dbfs - cur_dbfs) / 20.0);
    let peak = nonzero_or_eps(peak_abs_f64(samples));
    // Never clip: the gain*peak must stay under 0.99.
    let clip_cap = 0.99 / peak;
    if clip_cap < gain {
        gain = clip_cap;
    }
    let (noise_dbfs, snr_db) = noise_snr(samples);
    let input_status = input_level_status(cur_dbfs, peak, snr_db, thresholds);
    AudioCaptureMetrics {
        raw_dbfs: cur_dbfs,
        peak,
        gain,
        noise_dbfs,
        snr_db,
        input_status,
    }
}

/// Apply the boost-quiet gain stage. Returns `(boosted_samples,
/// metrics)`. The samples are scaled by `metrics.gain` and cast back to
/// `f32` — never clipping (the gain is capped against the peak).
///
/// Unlike the Python `_boost_quiet_detail`, this function does NOT
/// print a `[cap]` log line. Logging is the caller's choice; the Rust
/// pipeline emits structured events instead of stdout ASCII, and the
/// Python wrapper still owns the user-facing log line until it is cut
/// over to this module.
pub fn boost_quiet(
    samples: &[f32],
    thresholds: &StatusThresholds,
) -> (Vec<f32>, AudioCaptureMetrics) {
    let metrics = capture_metrics(samples, thresholds);
    let gain = metrics.gain as f32;
    let boosted: Vec<f32> = samples.iter().map(|s| s * gain).collect();
    (boosted, metrics)
}

/// "Does this buffer plausibly contain speech?" — the gate
/// `vp_transcribe._looks_like_speech` runs before sending audio to
/// Whisper. Returns `(ok, message)` where `ok=false` means the buffer
/// is too quiet or too flat to be worth decoding; `message` is the
/// human-readable reason (stable tokens for the test harness).
pub fn looks_like_speech(samples: &[f32], thresholds: &StatusThresholds) -> (bool, String) {
    let rms = nonzero_or_eps(rms_f64(samples));
    let raw_dbfs = 20.0 * rms.log10();
    let peak = nonzero_or_eps(peak_abs_f64(samples));
    let (noise_dbfs, snr_db) = noise_snr(samples);
    let input_status = input_level_status(raw_dbfs, peak, snr_db, thresholds);
    if raw_dbfs < thresholds.min_input_dbfs {
        let msg = format!(
            "input too quiet: raw={:.0}dBFS < {:.0}dBFS input={}",
            raw_dbfs, thresholds.min_input_dbfs, input_status
        );
        return (false, msg);
    }
    if snr_db < thresholds.min_input_snr_db {
        let msg = format!(
            "no speech contrast: snr={:.0}dB < {:.0}dB input={}",
            snr_db, thresholds.min_input_snr_db, input_status
        );
        return (false, msg);
    }
    let msg = format!(
        "raw={:.0}dBFS noise={:.0}dBFS snr={:.0}dB input={}",
        raw_dbfs, noise_dbfs, snr_db, input_status
    );
    (true, msg)
}

#[cfg(test)]
mod tests {
    //! Mirrors the metrics-related slice of
    //! `src/python/tests/test_audio.py::AudioDspTests` one-to-one.

    use super::super::FRAME_SAMPLES;
    use super::*;

    // --- noise_snr ---

    #[test]
    fn noise_snr_too_few_frames() {
        let a = vec![0.0_f32; 1000];
        assert_eq!(noise_snr(&a), (-90.0, 0.0));
    }

    #[test]
    fn noise_snr_constant_signal() {
        let a = vec![0.5_f32; FRAME_SAMPLES * 8];
        let (noise, snr) = noise_snr(&a);
        // RMS of constant 0.5 = 0.5 -> 20*log10(0.5) ≈ -6.0206
        assert!((noise - (-6.0206)).abs() < 0.01, "noise={noise}");
        assert!(snr.abs() < 1e-6, "snr={snr}");
    }

    #[test]
    fn noise_snr_contrast_has_high_snr() {
        let mut a = Vec::new();
        for i in 0..10 {
            let v = if i % 2 == 0 { 1.0 } else { 0.001 };
            a.extend(vec![v; FRAME_SAMPLES]);
        }
        let (noise, snr) = noise_snr(&a);
        assert!(snr > 40.0, "snr={snr}");
        assert!(noise < -40.0, "noise={noise}");
    }

    // --- boost_quiet ---

    #[test]
    fn boost_quiet_normalises_toward_target() {
        let thresholds = StatusThresholds::default();
        let a = vec![0.01_f32; 1920];
        let (out, _) = boost_quiet(&a, &thresholds);
        let r = rms_f64(&out);
        let dbfs = 20.0 * r.log10();
        assert!(
            (dbfs - thresholds.target_dbfs).abs() < 0.15,
            "dbfs={dbfs} target={}",
            thresholds.target_dbfs
        );
    }

    #[test]
    fn boost_quiet_never_clips() {
        let mut a = vec![0.0_f32; 1920];
        for v in a.iter_mut().take(10) {
            *v = 0.9;
        }
        let (out, _) = boost_quiet(&a, &StatusThresholds::default());
        let peak = peak_abs_f64(&out);
        assert!(peak <= 0.99 + 1e-6, "peak={peak}");
    }

    #[test]
    fn boost_quiet_detail_returns_structured_capture_metrics() {
        let mut a = Vec::new();
        for i in 0..10 {
            let v = if i % 2 == 0 { 0.1 } else { 0.002 };
            a.extend(vec![v; FRAME_SAMPLES]);
        }
        let (_, metrics) = boost_quiet(&a, &StatusThresholds::default());
        assert!(
            (metrics.raw_dbfs - (-23.0)).abs() < 0.15,
            "raw_dbfs={}",
            metrics.raw_dbfs
        );
        assert!((metrics.peak - 0.1).abs() < 0.02, "peak={}", metrics.peak);
        assert!(metrics.gain > 1.0, "gain={}", metrics.gain);
        assert!(metrics.noise_dbfs < -50.0, "noise={}", metrics.noise_dbfs);
        assert!(metrics.snr_db > 20.0, "snr={}", metrics.snr_db);
        assert_eq!(metrics.input_status, "good");
    }

    // --- input_level_status ---

    #[test]
    fn input_level_status_labels_actionable_gain_ranges() {
        let t = StatusThresholds::default();
        assert_eq!(input_level_status(-60.0, 0.01, 40.0, &t), "too_quiet");
        assert_eq!(input_level_status(-35.0, 0.20, 40.0, &t), "good");
        assert_eq!(input_level_status(-47.0, 0.07, 35.0, &t), "quiet");
        assert_eq!(input_level_status(-20.0, 0.10, 2.0, &t), "low_snr");
        assert_eq!(input_level_status(-16.0, 0.30, 35.0, &t), "hot");
        assert_eq!(input_level_status(-24.0, 0.99, 35.0, &t), "clip_risk");
    }

    // --- looks_like_speech ---

    #[test]
    fn looks_like_speech_rejects_too_quiet() {
        let a = vec![1e-4_f32; 1920];
        let (ok, msg) = looks_like_speech(&a, &StatusThresholds::default());
        assert!(!ok);
        assert!(msg.contains("too quiet"), "{msg}");
        assert!(msg.contains("input=too_quiet"), "{msg}");
    }

    #[test]
    fn looks_like_speech_rejects_flat_signal() {
        let a = vec![0.1_f32; 1920];
        let (ok, msg) = looks_like_speech(&a, &StatusThresholds::default());
        assert!(!ok);
        assert!(msg.contains("no speech contrast"), "{msg}");
        assert!(msg.contains("input=low_snr"), "{msg}");
    }

    #[test]
    fn looks_like_speech_accepts_contrasted_speech() {
        let mut a = Vec::new();
        for i in 0..10 {
            let v = if i % 2 == 0 { 0.8 } else { 0.05 };
            a.extend(vec![v; FRAME_SAMPLES]);
        }
        let (ok, _) = looks_like_speech(&a, &StatusThresholds::default());
        assert!(ok);
    }
}
