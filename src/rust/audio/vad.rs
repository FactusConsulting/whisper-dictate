//! Silero VAD wrapper with smoothing/pre-roll on top of the raw probability.
//!
//! The pipeline produces 480-sample / 30 ms frames at 16 kHz (see
//! [`super::resampler`]). Each frame is passed to [`SmoothedVad::feed`],
//! which delegates the raw decision to [`SileroVad`] and adds three things
//! the raw model doesn't give us:
//!
//! 1. **Prefill (pre-roll buffer)** — we hold the last `PREFILL_FRAMES`
//!    frames in a ring. When we cross from silence into speech, those
//!    cached frames are flushed BEFORE the triggering frame so the consumer
//!    receives the ~450 ms of audio immediately preceding the onset.
//!    Without this, the user's first syllable is clipped.
//! 2. **Onset debounce** — Silero occasionally flips to "voice" on a single
//!    breath or a click; requiring `ONSET_FRAMES` consecutive voice frames
//!    keeps spurious starts out of the consumer's pipeline.
//! 3. **Hangover** — speech naturally has short pauses; accepting up to
//!    `HANGOVER_FRAMES` of silence between voice frames stops us from
//!    chopping the utterance in the middle of a word.
//!
//! The state machine is intentionally four explicit cases over
//! `(in_speech, is_voice)` so each transition is auditable.

use std::collections::VecDeque;

use vad_rs::Vad as InnerVad;

/// Silero probability threshold above which a frame is "voice". Below
/// the threshold a frame is "silence". The Silero v4 README recommends
/// 0.3 as the speech/silence cutoff for the bundled model.
pub const VOICE_THRESHOLD: f32 = 0.3;
/// How many 30 ms frames of audio we cache to flush as pre-roll on speech
/// onset. 15 frames × 30 ms = 450 ms.
pub const PREFILL_FRAMES: usize = 15;
/// Consecutive voice frames required before we commit to "speech started".
pub const ONSET_FRAMES: usize = 2;
/// Silence frames tolerated inside speech before we commit to "speech ended".
/// 15 × 30 ms = 450 ms.
pub const HANGOVER_FRAMES: usize = 15;
/// Frame size matching [`super::resampler::FRAME_SIZE`].
pub const FRAME_SAMPLES: usize = 480;

/// Whether the consumer should treat a frame as speech, silence, or part of
/// the pre-roll burst at speech onset.
//
// No `Eq`: `Vec<f32>` doesn't implement `Eq` because `f32` has NaN.
// `PartialEq` is enough for the unit-test assertions in this module.
#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    /// Speech onset: the consumer should treat every frame in the burst as
    /// part of one utterance. The frames are returned in chronological order
    /// (oldest pre-roll first, triggering frame last).
    SpeechStart(Vec<Vec<f32>>),
    /// Speech continues — pass this single frame through.
    SpeechFrame(Vec<f32>),
    /// Speech ended after the hangover expired.
    SpeechEnd,
    /// Silence frame that is NOT part of the pre-roll or speech. The
    /// consumer normally drops these. Returned (rather than `None`) so the
    /// state machine remains a total function of `(in_speech, is_voice)`.
    Silence,
}

/// Decision source for [`SileroVad`]: either the real Silero v4 ONNX
/// model (via `vad-rs`/`ort`) or a deterministic RMS-based stub used in
/// unit tests that don't want to load a multi-MB ONNX model into every
/// test process. The pipeline always uses the ONNX backend at runtime;
/// the stub is opt-in and exists solely so the `SmoothedVad` smoothing
/// logic can be exercised without the ORT dependency on every assert.
enum Backend {
    /// Real Silero VAD. Boxed because `vad_rs::Vad` is ~264 bytes
    /// (ONNX session handle + LSTM state tensors); putting it on the
    /// heap keeps `Backend::Rms` from carrying a giant unused payload
    /// per enum value and satisfies `clippy::large_enum_variant`.
    Silero(Box<InnerVad>),
    /// Deterministic RMS → probability mapping used by tests.
    Rms,
    /// Always returns an error on `probability()`. Used by the pump
    /// regression test for the "DeviceError is terminal" wire contract.
    #[cfg(test)]
    AlwaysError,
    /// Returns the same fixed voice probability (above-threshold) for
    /// the first `n` calls, then errors on every subsequent call. Used
    /// by the iteration-2 regression test that drives the pump into
    /// in-speech BEFORE the VAD starts erroring, so the EOS-flush
    /// branch with `in_speech == true` is exercised.
    #[cfg(test)]
    ErrorAfter(std::cell::Cell<usize>),
}

/// Thin wrapper that returns a single voice/silence decision for one
/// 480-sample frame at 16 kHz.
pub struct SileroVad {
    threshold: f32,
    backend: Backend,
}

impl SileroVad {
    /// Build a Silero VAD from a path to a `silero_vad.onnx` model file.
    pub fn from_path<P: AsRef<std::path::Path>>(model_path: P) -> Result<Self, anyhow::Error> {
        let inner = InnerVad::new(model_path, 16_000)
            .map_err(|err| anyhow::anyhow!("init Silero VAD: {err}"))?;
        Ok(Self {
            threshold: VOICE_THRESHOLD,
            backend: Backend::Silero(Box::new(inner)),
        })
    }

    /// Build a Silero VAD from the bundled ONNX bytes.
    ///
    /// Lifecycle: we extract the model bytes to a stable cache location
    /// (`$LOCALAPPDATA/whisper-dictate/silero_vad.onnx` on Windows;
    /// `$XDG_CACHE_HOME/whisper-dictate/silero_vad.onnx` or
    /// `~/.cache/whisper-dictate/...` on Linux/macOS) and re-use the
    /// same file across runs. We do this instead of [`tempfile`] because:
    ///
    /// 1. `vad-rs::Vad::new` only accepts a path, so the bytes have to
    ///    live on disk somewhere.
    /// 2. On Windows, anti-virus and temp-dir cleanup occasionally race
    ///    session init if the model lives under `%TEMP%` — a stable
    ///    cache path sidesteps both.
    /// 3. Re-using the file avoids a multi-MB write on every launch.
    ///
    /// If the cache dir is unavailable (locked-down sandbox, etc.) we
    /// fall back to a [`tempfile::NamedTempFile`] that is `keep()`-ed so
    /// the file outlives the `NamedTempFile` handle (vad-rs may hold it
    /// open via mmap inside its ORT session). The handle isn't tracked
    /// across runs; the OS cleans the temp dir eventually.
    pub fn from_embedded_bytes(model_bytes: &[u8]) -> Result<Self, anyhow::Error> {
        let path = super::model_cache::cache_or_temp_model_path(model_bytes)?;
        Self::from_path(path)
    }

    /// Deterministic RMS-based stub used by `SmoothedVad` unit tests so
    /// they don't need to load the ONNX model on every assert. The
    /// production pipeline never uses this.
    pub fn rms_stub_for_tests() -> Self {
        Self {
            threshold: VOICE_THRESHOLD,
            backend: Backend::Rms,
        }
    }

    /// Always-erroring stub: every `probability()` / `is_voice()` call
    /// returns `Err`. Used by the pump regression test to verify that a
    /// VAD error produces a single `DeviceError` event and then stops
    /// the pump (the documented terminal-event wire contract).
    #[cfg(test)]
    pub(crate) fn always_error_for_tests() -> Self {
        Self {
            threshold: VOICE_THRESHOLD,
            backend: Backend::AlwaysError,
        }
    }

    /// Test stub: report above-threshold "voice" for the first `n`
    /// frames, then error on every frame after that. Lets the pump
    /// regression test enter `in_speech == true` and THEN trip a VAD
    /// error inside the EOS-flush branch (iteration-2 finding #1).
    #[cfg(test)]
    pub(crate) fn error_after_for_tests(n: usize) -> Self {
        Self {
            threshold: VOICE_THRESHOLD,
            backend: Backend::ErrorAfter(std::cell::Cell::new(n)),
        }
    }

    /// Voice probability for one 480-sample / 30 ms frame at 16 kHz.
    pub fn probability(&mut self, frame: &[f32]) -> Result<f32, anyhow::Error> {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        match &mut self.backend {
            Backend::Silero(inner) => {
                let result = inner
                    .compute(frame)
                    .map_err(|err| anyhow::anyhow!("Silero VAD compute: {err}"))?;
                Ok(result.prob)
            }
            Backend::Rms => {
                // RMS → synthetic probability. Tuned so a 1 kHz sine of
                // amplitude 0.5 (rms ≈ 0.35) maps comfortably above the
                // 0.3 threshold and pure silence stays at exactly 0.
                let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
                let rms = (sum_sq / frame.len() as f32).sqrt();
                Ok((rms * 2.0).min(1.0))
            }
            #[cfg(test)]
            Backend::AlwaysError => Err(anyhow::anyhow!("synthetic vad failure for tests")),
            #[cfg(test)]
            Backend::ErrorAfter(remaining) => {
                let n = remaining.get();
                if n == 0 {
                    Err(anyhow::anyhow!("synthetic vad failure after-N for tests"))
                } else {
                    remaining.set(n - 1);
                    // Above-threshold so the smoothed wrapper enters speech.
                    Ok(0.95)
                }
            }
        }
    }

    /// True iff the frame's probability is at/above the threshold.
    pub fn is_voice(&mut self, frame: &[f32]) -> Result<bool, anyhow::Error> {
        Ok(self.probability(frame)? >= self.threshold)
    }
}

/// State machine that wraps [`SileroVad`] with prefill, onset debounce and
/// hangover.
pub struct SmoothedVad {
    inner: SileroVad,
    /// Ring of the most recent frames, capped at [`PREFILL_FRAMES`].
    prefill: VecDeque<Vec<f32>>,
    /// True iff we are currently inside an utterance.
    in_speech: bool,
    /// Consecutive voice frames seen while NOT in speech — for onset debounce.
    voice_run: usize,
    /// Consecutive silence frames seen while IN speech — for hangover.
    silence_run: usize,
}

impl SmoothedVad {
    pub fn new(inner: SileroVad) -> Self {
        Self {
            inner,
            prefill: VecDeque::with_capacity(PREFILL_FRAMES),
            in_speech: false,
            voice_run: 0,
            silence_run: 0,
        }
    }

    /// Reset to "not in speech" without losing the VAD's loaded weights.
    /// Use when the consumer cancels a recording mid-utterance so we don't
    /// emit a spurious `SpeechEnd` later.
    ///
    /// Returns `true` iff we were in the middle of an utterance when
    /// reset was called. The caller should translate a `true` return into
    /// a `PipelineEvent::Cancelled` BEFORE delivering any subsequent
    /// events — otherwise the Python consumer would hold a growing
    /// utterance buffer indefinitely (it saw `SpeechStart` and will never
    /// see the matching `SpeechEnd`).
    #[must_use]
    pub fn reset(&mut self) -> bool {
        let was_in_speech = self.in_speech;
        self.prefill.clear();
        self.in_speech = false;
        self.voice_run = 0;
        self.silence_run = 0;
        was_in_speech
    }

    /// Whether we're currently inside an utterance. Exposed for the runtime
    /// integration test + UI status updates.
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Feed one 480-sample / 30 ms frame at 16 kHz and get the resulting
    /// [`VadEvent`]. The frame is cloned into the pre-roll buffer
    /// regardless of outcome, so the caller can hand a borrowed slice in.
    pub fn feed(&mut self, frame: &[f32]) -> Result<VadEvent, anyhow::Error> {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        let is_voice = self.inner.is_voice(frame)?;

        let event = match (self.in_speech, is_voice) {
            // Case 1: not in speech, frame is silence — count nothing, keep
            //         caching the frame in the pre-roll ring.
            (false, false) => {
                self.voice_run = 0;
                VadEvent::Silence
            }
            // Case 2: not in speech, frame is voice — count toward onset.
            //         If we've crossed the onset threshold, flush prefill +
            //         the current frame as one burst.
            (false, true) => {
                self.voice_run = self.voice_run.saturating_add(1);
                if self.voice_run >= ONSET_FRAMES {
                    self.in_speech = true;
                    self.silence_run = 0;
                    self.voice_run = 0;
                    let mut burst: Vec<Vec<f32>> = self.prefill.drain(..).collect();
                    burst.push(frame.to_vec());
                    // Don't also push this frame back into the prefill ring;
                    // it's already part of the burst. Subsequent silence
                    // will be handled by the (true, false) arm.
                    return Ok(VadEvent::SpeechStart(burst));
                }
                VadEvent::Silence
            }
            // Case 3: in speech, frame is voice — pass it through, reset
            //         hangover counter.
            (true, true) => {
                self.silence_run = 0;
                VadEvent::SpeechFrame(frame.to_vec())
            }
            // Case 4: in speech, frame is silence — pass it through (still
            //         part of the utterance under hangover) until the
            //         hangover budget runs out, then SpeechEnd.
            (true, false) => {
                self.silence_run = self.silence_run.saturating_add(1);
                if self.silence_run > HANGOVER_FRAMES {
                    self.in_speech = false;
                    self.silence_run = 0;
                    self.voice_run = 0;
                    self.prefill.clear();
                    return Ok(VadEvent::SpeechEnd);
                }
                VadEvent::SpeechFrame(frame.to_vec())
            }
        };

        // Maintain the prefill ring AFTER deciding on the event so the
        // burst flushed in Case 2 doesn't already include the triggering
        // frame's predecessors twice.
        self.push_prefill(frame);
        Ok(event)
    }

    fn push_prefill(&mut self, frame: &[f32]) {
        if self.prefill.len() >= PREFILL_FRAMES {
            self.prefill.pop_front();
        }
        self.prefill.push_back(frame.to_vec());
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn silence_frame() -> Vec<f32> {
        vec![0.0; FRAME_SAMPLES]
    }

    fn voice_frame(amplitude: f32) -> Vec<f32> {
        // 1 kHz sine at 16 kHz = 16 samples per period.
        (0..FRAME_SAMPLES)
            .map(|i| amplitude * (2.0 * PI * (i as f32) / 16.0).sin())
            .collect()
    }

    fn make_vad() -> SmoothedVad {
        // Use the RMS stub so the smoothing-logic unit tests run quickly
        // and deterministically without loading the multi-MB ONNX model.
        // End-to-end coverage of the real Silero backend lives in the
        // pipeline integration test in src/rust/tests/.
        SmoothedVad::new(SileroVad::rms_stub_for_tests())
    }

    #[test]
    fn pure_silence_never_triggers_speech_start() {
        let mut vad = make_vad();
        let mut starts = 0;
        // ~1 second of silence = 33 frames of 30 ms.
        for _ in 0..33 {
            let event = vad.feed(&silence_frame()).expect("vad feed");
            if matches!(event, VadEvent::SpeechStart(_)) {
                starts += 1;
            }
        }
        assert_eq!(starts, 0, "silence must never produce SpeechStart");
        assert!(!vad.in_speech(), "VAD must remain in non-speech state");
    }

    #[test]
    fn loud_sine_triggers_speech_start_after_onset_debounce() {
        let mut vad = make_vad();
        let mut events = Vec::new();
        // Feed enough silence to fill the prefill ring, then voice frames.
        for _ in 0..PREFILL_FRAMES {
            events.push(vad.feed(&silence_frame()).expect("vad feed"));
        }
        // 1 s of voice → ~33 frames. SpeechStart should fire at frame
        // index ONSET_FRAMES - 1 of the voice run (i.e. the 2nd voice frame).
        for _ in 0..33 {
            events.push(vad.feed(&voice_frame(0.5)).expect("vad feed"));
        }
        let first_start = events
            .iter()
            .position(|e| matches!(e, VadEvent::SpeechStart(_)))
            .expect("SpeechStart fires somewhere in the loud-sine run");
        // Must land within the voice region (after the silence prefill).
        assert!(
            first_start >= PREFILL_FRAMES,
            "SpeechStart fired at {first_start}, before voice began",
        );
        // And it must NOT be the very first voice frame: the onset
        // debounce requires ONSET_FRAMES consecutive voice frames.
        assert!(
            first_start >= PREFILL_FRAMES + ONSET_FRAMES - 1,
            "onset debounce violated: SpeechStart at {first_start}",
        );
    }

    #[test]
    fn speech_start_flushes_prefill_in_chronological_order() {
        let mut vad = make_vad();
        // Push twice as many tagged silence frames as the prefill ring
        // size, so we can verify that the ring (a) keeps the most recent
        // PREFILL_FRAMES and (b) returns them in chronological order on
        // SpeechStart. Each tag is small enough that the RMS stub still
        // treats the frame as silence — the per-frame contribution is on
        // the order of 1e-9.
        let total_silence = PREFILL_FRAMES * 2;
        for tag in 0..total_silence {
            let mut f = silence_frame();
            f[1] = (tag as f32) * 1e-4 + 1e-6;
            let _ = vad.feed(&f).expect("vad feed");
        }
        // Now feed voice frames until SpeechStart fires.
        let mut burst: Option<Vec<Vec<f32>>> = None;
        for _ in 0..ONSET_FRAMES + 2 {
            if let VadEvent::SpeechStart(b) = vad.feed(&voice_frame(0.5)).expect("vad feed") {
                burst = Some(b);
                break;
            }
        }
        let burst = burst.expect("SpeechStart fires within the voice run");
        // The prefill ring is "the last PREFILL_FRAMES frames seen". The
        // burst is therefore: the last (PREFILL_FRAMES - (ONSET_FRAMES-1))
        // silence frames + the (ONSET_FRAMES - 1) voice frames captured
        // during the onset debounce + the SpeechStart-triggering voice
        // frame. Total = PREFILL_FRAMES + 1.
        assert_eq!(
            burst.len(),
            PREFILL_FRAMES + 1,
            "burst should be the prefill ring plus the triggering frame",
        );
        // The leading silence portion of the burst must be in chronological
        // order — tag i+1 follows tag i for the silence prefix. The first
        // silence frame in the burst is tag `total_silence - (PREFILL_FRAMES
        // - (ONSET_FRAMES - 1))` because the onset-debounce voice frames
        // evicted that many of the oldest silence tags.
        let silence_prefix_len = PREFILL_FRAMES - (ONSET_FRAMES - 1);
        let first_silence_tag = total_silence - silence_prefix_len;
        for i in 0..silence_prefix_len {
            let expected = ((first_silence_tag + i) as f32) * 1e-4 + 1e-6;
            assert!(
                (burst[i][1] - expected).abs() < 1e-9,
                "prefill frame {i} out of order: got tag {} want {expected}",
                burst[i][1],
            );
        }
    }

    #[test]
    fn hangover_keeps_short_pauses_inside_speech() {
        let mut vad = make_vad();
        // Drive into speech.
        for _ in 0..PREFILL_FRAMES {
            let _ = vad.feed(&silence_frame()).expect("vad feed");
        }
        for _ in 0..ONSET_FRAMES + 1 {
            let _ = vad.feed(&voice_frame(0.5)).expect("vad feed");
        }
        assert!(vad.in_speech());
        // Feed a short silence pause (less than HANGOVER_FRAMES) — must
        // NOT end speech.
        for _ in 0..(HANGOVER_FRAMES / 2) {
            let event = vad.feed(&silence_frame()).expect("vad feed");
            assert!(
                !matches!(event, VadEvent::SpeechEnd),
                "short pause must not end speech"
            );
        }
        assert!(vad.in_speech());
        // Now feed beyond the hangover budget — SpeechEnd must fire.
        let mut ended = false;
        for _ in 0..HANGOVER_FRAMES + 2 {
            if matches!(
                vad.feed(&silence_frame()).expect("vad feed"),
                VadEvent::SpeechEnd
            ) {
                ended = true;
                break;
            }
        }
        assert!(ended, "long pause must produce SpeechEnd");
        assert!(!vad.in_speech());
    }

    #[test]
    fn reset_during_silence_returns_false() {
        let mut vad = make_vad();
        // Pure silence — never in speech.
        for _ in 0..10 {
            let _ = vad.feed(&silence_frame()).expect("vad feed");
        }
        assert!(!vad.in_speech());
        let was_in_speech = vad.reset();
        assert!(
            !was_in_speech,
            "reset() during silence must return false (no Cancelled to emit)"
        );
    }

    #[test]
    fn reset_during_speech_returns_true() {
        let mut vad = make_vad();
        // Drive into speech.
        for _ in 0..PREFILL_FRAMES {
            let _ = vad.feed(&silence_frame()).expect("vad feed");
        }
        for _ in 0..ONSET_FRAMES + 1 {
            let _ = vad.feed(&voice_frame(0.5)).expect("vad feed");
        }
        assert!(vad.in_speech(), "should be in speech now");
        let was_in_speech = vad.reset();
        assert!(
            was_in_speech,
            "reset() during speech must return true so caller emits Cancelled",
        );
        assert!(!vad.in_speech(), "reset clears in_speech");
    }
}
