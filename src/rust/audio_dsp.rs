//! Pure noise-floor / SNR / gain / silence-trim DSP.
//!
//! Rust port of `src/python/whisper_dictate/vp_audio.py` (Wave 4-C of the
//! Python-removal roadmap, issue #348). All functions here are pure — no
//! audio device, no I/O, no env reads — and operate on borrowed `f32`
//! sample slices. They mirror the numpy implementation in `vp_audio.py`
//! byte-for-byte at the algorithmic level so the Python caller-facing
//! API can be cut over to this module in a follow-up without changing
//! observable behaviour (the Python `AudioDspTests` characterisation
//! suite continues to pin Python behaviour; see [`tests`] below for the
//! mirrored Rust assertions).
//!
//! Lives at the crate root rather than under `src/rust/audio/` because
//! that tree is gated behind the `audio-in-rust` cargo feature (it pulls
//! cpal + the Silero ONNX runtime). This module is pure stdlib + `f32`
//! arithmetic, so unconditional compilation keeps it reachable for tests
//! and any future caller regardless of the audio-capture feature gate.
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

// Python `float(x) or 1e-9` — replace exactly-zero (incl. -0.0) with
// a tiny epsilon so the subsequent `log10` doesn't blow up. NaN is
// truthy in Python's `or`, so it passes through unchanged.
fn nonzero_or_eps(value: f64) -> f64 {
    if value == 0.0 {
        1e-9
    } else {
        value
    }
}

/// numpy.percentile(values, p) with the default (`"linear"`) interpolation
/// method. The percentile-noise-floor math relies on the exact interp
/// behaviour (10th + 90th of small frame counts land between bins), so
/// re-implementing it cheaply here avoids a numpy dep just to match it.
///
/// `values` is mutated in-place (sorted ascending) — callers that need to
/// preserve order should clone first. `percentile` is in 0..=100 and is
/// clamped to that range; an empty slice returns `0.0` to mirror numpy's
/// "all-nan" handling on an empty input (the caller's downstream `or 1e-9`
/// guard then takes over).
fn percentile(values: &mut [f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    let p = percentile.clamp(0.0, 100.0);
    let pos = p / 100.0 * (n as f64 - 1.0);
    let low = pos.floor() as usize;
    let high = pos.ceil() as usize;
    if low == high {
        return values[low];
    }
    let frac = pos - low as f64;
    values[low] + frac * (values[high] - values[low])
}

/// Per-frame RMS values for the 30 ms framing the noise-floor + trim
/// math share. Returns one entry per full frame; callers that need to
/// score the trailing partial-frame remainder push it themselves.
fn frame_rms(samples: &[f32]) -> Vec<f64> {
    let n = samples.len() / FRAME_SAMPLES;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let frame = &samples[i * FRAME_SAMPLES..(i + 1) * FRAME_SAMPLES];
        out.push(rms_f64(frame));
    }
    out
}

fn rms_f64(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sum_sq = 0.0_f64;
    for &s in samples {
        let v = s as f64;
        sum_sq += v * v;
    }
    (sum_sq / samples.len() as f64).sqrt()
}

fn peak_abs_f64(samples: &[f32]) -> f64 {
    let mut peak = 0.0_f64;
    for &s in samples {
        let abs = (s as f64).abs();
        if abs > peak {
            peak = abs;
        }
    }
    peak
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

/// Cut a sustained run of trailing dead air (at the noise floor) off
/// `samples`. Returns `(trimmed_slice, trimmed_ms)`. Leaves the clip
/// untouched (`trimmed_ms == 0.0`) when there is no clear silence
/// floor, no trailing silence, or the cut would remove less than ~150
/// ms. A frame counts as silence only when within
/// [`TRIM_NOISE_MARGIN_DB`] of the 10th-percentile noise floor, so any
/// voiced energy — even a very soft trailing word — sits above it and
/// is preserved.
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
    //! Mirrors `src/python/tests/test_audio.py::AudioDspTests` one-to-one.
    //! Same asserts, same fixtures — the Python characterisation suite
    //! pins behaviour and these confirm the Rust port matches.

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

    // --- noise_snr ---

    #[test]
    fn noise_snr_too_few_frames() {
        let a = vec![0.0_f32; 1000];
        assert_eq!(noise_snr(&a), (-90.0, 0.0));
    }

    #[test]
    fn noise_snr_constant_signal() {
        let a = make(0.5, 8);
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

    // --- trim_trailing_silence ---

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

    // --- percentile helper (unit) ---

    #[test]
    fn percentile_matches_numpy_linear_interpolation() {
        // numpy.percentile([1,2,3,4,5,6,7,8,9,10], 10) == 1.9
        let mut v: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        assert!((percentile(&mut v, 10.0) - 1.9).abs() < 1e-12);
        // numpy.percentile([1..=10], 90) == 9.1
        let mut v: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        assert!((percentile(&mut v, 90.0) - 9.1).abs() < 1e-12);
    }

    #[test]
    fn nonzero_or_eps_replaces_zero() {
        assert_eq!(nonzero_or_eps(0.0), 1e-9);
        assert_eq!(nonzero_or_eps(-0.0), 1e-9);
        assert_eq!(nonzero_or_eps(0.5), 0.5);
        assert_eq!(nonzero_or_eps(-0.5), -0.5);
    }
}
