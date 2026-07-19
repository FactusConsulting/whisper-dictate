//! Runner for `self-test audio-capture`.
//!
//! Opens the cpal capture path via [`crate::audio::capture::start_capture`],
//! consumes chunks on a background thread for the requested duration, and
//! rolls up an [`AudioCaptureReport`]. Applies the v1.20.6 PipeWire
//! quantum lesson via [`crate::audio::pipewire::configure_pipewire_env`]
//! BEFORE opening the stream so the fix is exercised on every run.
//!
//! The [`Accumulator`] is deliberately a pure struct so the streaming
//! behaviour (arbitrary partition of the sample stream) is expressible
//! as a property test — cpal is free to hand us callbacks of any size,
//! and the RMS + peak MUST NOT depend on the callback boundary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::capture::{start_capture, AudioChunk};
use crate::audio::pipewire::configure_pipewire_env;

use super::report::AudioCaptureReport;

/// Options for [`run_audio_capture_test`]. Extracted so the CLI dispatch
/// (which reads clap-parsed values) and the future in-process
/// integration test have the same call shape.
#[derive(Debug, Clone)]
pub struct AudioCaptureOptions {
    /// How long to capture. Sub-100 ms values are legal but risk missing
    /// even the first cpal callback on high-latency devices.
    pub duration: Duration,
    /// Device selector — empty string picks the system default input.
    /// Same lookup precedence as `crate::audio::capture::pick_device`
    /// (exact → substring → numeric).
    pub device: String,
    /// When true, treat "captured only silence" (RMS < 1e-6) as a hard
    /// failure. Off by default because most CI containers have no audio
    /// device wired to the ALSA loopback; leave it off unless the caller
    /// KNOWS the box has real audio.
    pub fail_on_silence: bool,
}

impl Default for AudioCaptureOptions {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(2),
            device: String::new(),
            fail_on_silence: false,
        }
    }
}

/// Approximate silence threshold — an RMS below this value on a device
/// that IS active is almost certainly a permission bug or a muted mic,
/// not real quiet audio. 1e-6 is well below the noise floor of any
/// consumer mic; a bit-perfect all-zero signal will trip it.
pub const SILENCE_RMS_THRESHOLD: f32 = 1e-6;

/// Run the capture path for `opts.duration`, tally the samples, and
/// return an [`AudioCaptureReport`]. Never panics on device / permission
/// failure — every recoverable error is captured in `report.error`.
///
/// Invariants asserted by construction:
///   * `pipewire::configure_pipewire_env` fires BEFORE any cpal call so
///     the v1.20.6 quantum lesson is applied on Linux even for the
///     first invocation.
///   * The consumer thread drains chunks on a background thread so the
///     cpal callback stays minimal (a slow RMS computation on the main
///     thread would drop chunks on Windows/WASAPI).
pub fn run_audio_capture_test(opts: AudioCaptureOptions) -> AudioCaptureReport {
    // Fire the PipeWire quantum decision first — this is the concrete
    // v1.20.6 mitigation. On non-Linux it's a no-op; on Linux we set
    // `PIPEWIRE_QUANTUM=2048` iff the operator hasn't set it themselves.
    let quantum_decision = configure_pipewire_env();

    let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();

    // Open the stream. The capture worker owns the cpal Stream; the
    // returned handle carries the negotiated sample rate we surface
    // in the report.
    let capture = match start_capture(&opts.device, chunk_tx) {
        Ok(c) => c,
        Err(err) => {
            return AudioCaptureReport {
                requested_duration: opts.duration,
                device: opts.device,
                quantum_decision,
                frames_captured: 0,
                samples_captured: 0,
                rms: 0.0,
                peak: 0.0,
                sample_rate: 0,
                error: format!("open capture: {err}"),
                succeeded: false,
            };
        }
    };
    let sample_rate = capture.sample_rate();

    // Collect on the consumer thread. Everything shared with the main
    // thread is behind a mutex OR an atomic — the counts are atomic so a
    // hung capture worker still leaves us a live snapshot to report.
    let frames = Arc::new(AtomicUsize::new(0));
    let samples_total = Arc::new(AtomicUsize::new(0));
    let acc: Arc<Mutex<Accumulator>> = Arc::new(Mutex::new(Accumulator::new()));

    let frames_c = frames.clone();
    let samples_c = samples_total.clone();
    let acc_c = acc.clone();
    let deadline = Instant::now() + opts.duration;
    let consumer = std::thread::spawn(move || loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline - now;
        match chunk_rx.recv_timeout(remaining) {
            Ok(AudioChunk::Samples(samples)) => {
                frames_c.fetch_add(1, Ordering::Relaxed);
                samples_c.fetch_add(samples.len(), Ordering::Relaxed);
                if let Ok(mut a) = acc_c.lock() {
                    a.absorb(&samples);
                }
            }
            Ok(AudioChunk::EndOfStream) => return,
            Ok(AudioChunk::Error(_)) => return,
            Err(RecvTimeoutError::Timeout) => return,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    });

    // Wait out the requested duration on the main thread, then stop the
    // capture worker. Dropping the CaptureHandle would also stop it, but
    // the explicit stop() lets us join the pump BEFORE we read the
    // final counts so no chunk can arrive between our snapshot and the
    // return.
    std::thread::sleep(opts.duration);
    let mut capture = capture;
    capture.stop();
    // The consumer thread also exits when the deadline hits OR when the
    // chunk_tx is dropped by the capture worker's stop(). Either way,
    // joining is bounded — no additional deadline needed.
    let _ = consumer.join();

    let frames_captured = frames.load(Ordering::Relaxed);
    let samples_captured = samples_total.load(Ordering::Relaxed);
    let (rms, peak) = acc.lock().map(|a| a.finalize()).unwrap_or((0.0, 0.0));

    if frames_captured == 0 {
        return AudioCaptureReport {
            requested_duration: opts.duration,
            device: opts.device,
            quantum_decision,
            frames_captured: 0,
            samples_captured: 0,
            rms: 0.0,
            peak: 0.0,
            sample_rate,
            error: format!(
                "no audio device delivered samples within {} ms — check permissions, mic mute state, or PIPEWIRE_QUANTUM",
                opts.duration.as_millis(),
            ),
            succeeded: false,
        };
    }

    if opts.fail_on_silence && rms < SILENCE_RMS_THRESHOLD {
        return AudioCaptureReport {
            requested_duration: opts.duration,
            device: opts.device,
            quantum_decision,
            frames_captured,
            samples_captured,
            rms,
            peak,
            sample_rate,
            error: format!(
                "captured {frames_captured} chunk(s) but the signal was silence (rms {rms:.6} < {SILENCE_RMS_THRESHOLD:.6})",
            ),
            succeeded: false,
        };
    }

    AudioCaptureReport {
        requested_duration: opts.duration,
        device: opts.device,
        quantum_decision,
        frames_captured,
        samples_captured,
        rms,
        peak,
        sample_rate,
        error: String::new(),
        succeeded: true,
    }
}

/// Accumulator for the RMS + peak stats. Kept out of the hot-path
/// consumer loop so it can be tested in isolation — the accumulator is
/// pure and the streaming behaviour (arbitrary partition of the sample
/// stream) is expressible as a property test.
#[derive(Debug, Default)]
pub(super) struct Accumulator {
    /// Sum of squared samples. `f64` because 2 s at 48 kHz = 96 000
    /// samples, each up to 1.0 squared, easily fits in f32 — but a
    /// higher-rate device (192 kHz) over a longer run could accumulate
    /// enough to lose precision. f64 is free at this scale.
    sum_sq: f64,
    n: usize,
    peak: f32,
}

impl Accumulator {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn absorb(&mut self, samples: &[f32]) {
        for &s in samples {
            let abs = s.abs();
            if abs > self.peak {
                self.peak = abs;
            }
            self.sum_sq += (s as f64) * (s as f64);
        }
        self.n += samples.len();
    }

    /// Finalise into `(rms, peak)`. Zero samples → `(0.0, 0.0)` so
    /// callers can render the report without special-casing.
    pub(super) fn finalize(&self) -> (f32, f32) {
        if self.n == 0 {
            return (0.0, 0.0);
        }
        let mean_sq = self.sum_sq / self.n as f64;
        (mean_sq.sqrt() as f32, self.peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_zero_samples_finalises_to_zeros() {
        let acc = Accumulator::new();
        assert_eq!(acc.finalize(), (0.0, 0.0));
    }

    #[test]
    fn accumulator_matches_direct_rms_and_peak_on_single_burst() {
        let mut acc = Accumulator::new();
        let samples: Vec<f32> = vec![0.1, -0.2, 0.3, -0.4, 0.5];
        acc.absorb(&samples);
        let (rms, peak) = acc.finalize();
        // Reference computation from scratch — RMS = sqrt(mean(x^2))
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        let expected_rms = (sum_sq / samples.len() as f32).sqrt();
        let expected_peak = 0.5;
        assert!(
            (rms - expected_rms).abs() < 1e-5,
            "rms {rms} vs {expected_rms}"
        );
        assert!(
            (peak - expected_peak).abs() < 1e-6,
            "peak {peak} vs {expected_peak}",
        );
    }

    #[test]
    fn accumulator_is_order_invariant_across_absorb_partitions() {
        // Property the run_audio_capture_test path relies on: the RMS +
        // peak must not depend on how cpal chose to chop the sample
        // stream into callbacks. Verify by absorbing the same samples in
        // two different partitions and comparing.
        let all: Vec<f32> = (0..1024).map(|i| (i as f32).sin() * 0.5).collect();

        let mut a = Accumulator::new();
        a.absorb(&all);
        let (rms_a, peak_a) = a.finalize();

        let mut b = Accumulator::new();
        // Arbitrary partition: 3 chunks of unequal sizes.
        b.absorb(&all[..100]);
        b.absorb(&all[100..777]);
        b.absorb(&all[777..]);
        let (rms_b, peak_b) = b.finalize();

        assert!((rms_a - rms_b).abs() < 1e-6, "rms {rms_a} vs {rms_b}");
        assert!((peak_a - peak_b).abs() < 1e-6, "peak {peak_a} vs {peak_b}");
    }

    #[test]
    fn options_default_is_two_seconds_default_device_no_silence_gate() {
        // The defaults are consumed by the CLI dispatcher when no flag
        // is passed — pin them here so a future field rename can't
        // silently change the shipping behaviour.
        let opts = AudioCaptureOptions::default();
        assert_eq!(opts.duration, Duration::from_secs(2));
        assert!(opts.device.is_empty());
        assert!(!opts.fail_on_silence);
    }

    #[test]
    fn silence_threshold_is_below_consumer_mic_noise_floor() {
        // Guardrail: SILENCE_RMS_THRESHOLD must stay well below what a
        // real mic would produce even in a quiet room (typical noise
        // floor rms ~ 1e-4 for a consumer USB mic). If someone bumps
        // this up to something like 1e-3 the `--fail-on-silence` check
        // would start flagging legitimate audio as "silence". Bound as
        // `const` so clippy's `assertions_on_constants` lint is happy —
        // the check remains a compile-time guard.
        const _: () = assert!(
            SILENCE_RMS_THRESHOLD < 1e-4,
            "SILENCE_RMS_THRESHOLD would flag real quiet audio as silence",
        );
        const _: () = assert!(
            SILENCE_RMS_THRESHOLD > 0.0,
            "SILENCE_RMS_THRESHOLD must be positive (== 0 would never trip)",
        );
    }
}
