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

    /// Zero the inner Silero VAD's recurrent state (`h`/`c` LSTM
    /// tensors). Called from [`SmoothedVad::reset`] so a mid-utterance
    /// cancel doesn't bleed LSTM context from the discarded audio into
    /// the next recording's first VAD decisions (PR #335 iteration-2
    /// review finding #3). The smoothing-only wrapper fields are
    /// reset alongside this in `SmoothedVad::reset`.
    ///
    /// No-op for the RMS and AlwaysError test backends: they are
    /// stateless across calls.
    pub fn reset(&mut self) {
        match &mut self.backend {
            Backend::Silero(inner) => inner.reset(),
            Backend::Rms => {}
            #[cfg(test)]
            Backend::AlwaysError => {}
            #[cfg(test)]
            Backend::ErrorAfter(_) => {}
        }
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
        // Iteration-2 review finding #3: also zero the inner Silero
        // VAD's LSTM recurrent state (`h`/`c` tensors). Without this,
        // a cancel mid-utterance leaves the next recording's first
        // VAD decisions biased by audio the user explicitly threw
        // away — typically a clipped onset or a spurious early
        // SpeechStart on the residual phoneme context.
        self.inner.reset();
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

// Smoothing-logic unit tests live in a sibling file to keep this
// module under the 500-LOC cap. See `vad_tests.rs`.
#[cfg(test)]
#[path = "vad_tests.rs"]
mod tests;
