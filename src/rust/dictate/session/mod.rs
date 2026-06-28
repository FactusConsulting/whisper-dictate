//! Pure-logic per-utterance state machine for the live PTT dictation
//! loop. Mirrors `src/python/whisper_dictate/vp_dictate.py::Dictate`'s
//! per-utterance lifecycle (start → push frames → stop/transcribe →
//! inject, with cancel) but with NO audio capture, NO model loading and
//! NO real injection: every side-effecting boundary goes through a
//! trait-bound mock so unit tests run without cpal / whisper / enigo.
//!
//! Wave 5 PR 2 of issue #348. The audio-route (PR 3), hotkey wiring
//! (PR 4) and full Rust supervisor (PR 5+) are what consume this — there
//! is no production caller in this PR yet. Adding the state machine in
//! isolation lets the per-utterance transition logic be unit-tested
//! end-to-end (the six characterisation tests ported from
//! `src/python/tests/test_dictate_loop.py`) before the I/O layer lands.
//!
//! # Why a trait-bound design
//!
//! `vp_dictate.py` weaves capture / transcribe / inject side-effects
//! into the same per-utterance method that owns the state machine, which
//! is why `test_dictate_loop.py` has to build a `Dictate` via
//! `object.__new__` and monkey-patch six boundary functions to test the
//! orchestration. Splitting capture / transcribe / inject out as traits
//! up-front gives us the same testability without the monkey-patching
//! gymnastics, and is the shape PR 3/4 already need anyway because cpal
//! lives in a `cfg(feature = "audio-in-rust")` module.
//!
//! # Module layout
//!
//! - [`types`] — public trait boundaries + result / state / error / config
//!   types. Re-exported through this module.
//! - [`wire`] — the narrow `[worker-event] {…}\n` line emitter the
//!   session uses for status / utterance events. Will be swapped for
//!   the richer `crate::dictate::events` emitter from PR 1 (#412) by
//!   PR 3, once both PRs are in `main`.
//! - [`tests_support`] — `cfg(test)` test backends + helpers shared
//!   across the test files.
//! - [`tests_ported`] — the six characterisation tests ported from
//!   `src/python/tests/test_dictate_loop.py`.
//! - [`tests_transitions`] — supplementary state-transition invariants.

use std::io::Write;

use serde_json::{json, Value};

pub mod types;
mod wire;

#[cfg(test)]
mod tests_ported;
#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_transitions;

pub use types::{
    InjectBackend, InjectError, SessionConfig, SessionError, SessionState, TranscribeBackend,
    TranscribeError, TranscribeResult, UtteranceOutcome, SR,
};

/// Per-utterance state machine. Owns the capture buffer and the
/// transcribe / inject backends; emits status events through a
/// caller-supplied writer.
///
/// See the module docs for the design rationale. See `tests_ported.rs`
/// for the six characterisation tests ported from
/// `src/python/tests/test_dictate_loop.py` and `tests_transitions.rs`
/// for the supplementary state-transition invariants.
pub struct DictateSession<T: TranscribeBackend, I: InjectBackend> {
    state: SessionState,
    /// Captured PCM at the model's sample rate (16 kHz mono). In this
    /// PR `push_frame` already-resampled samples; PR 3 owns the
    /// channel-select + resample at consumption.
    frame_buf: Vec<f32>,
    /// Monotonic recording generation. Bumped on every `start()` so
    /// the chord-race guard in `cancel()` can detect a stale request.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    epoch: u64,
    config: SessionConfig,
    transcribe: T,
    inject: I,
}

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Build a fresh session. The session starts in
    /// [`SessionState::Idle`] with an empty buffer and `epoch == 0`.
    pub fn new(transcribe: T, inject: I, config: SessionConfig) -> Self {
        Self {
            state: SessionState::Idle,
            frame_buf: Vec::new(),
            epoch: 0,
            config,
            transcribe,
            inject,
        }
    }

    /// Current state-machine phase. Exposed for tests and the
    /// supervisor's UI; the session itself is the source of truth.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Current recording epoch. Returned by [`Self::start`] and read by
    /// [`Self::cancel`] for the chord-race guard.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Read-only access to the transcribe backend. Tests use this to
    /// inspect what the session passed to the mock; production callers
    /// will rarely need it.
    pub fn transcribe_backend(&self) -> &T {
        &self.transcribe
    }

    /// Read-only access to the inject backend. Tests use this to assert
    /// what the session injected.
    pub fn inject_backend(&self) -> &I {
        &self.inject
    }

    /// Open a fresh utterance.
    ///
    /// Mirrors `vp_dictate.py::_start`:
    /// 1. early-return if a recording is already in flight (no events,
    ///    no state change — same guard as Python's `if self.recording`);
    /// 2. clear the frame buffer;
    /// 3. bump the recording epoch (the chord-race generation counter
    ///    — see `vp_dictate.py:140-147`);
    /// 4. emit `status=opening`;
    /// 5. transition to [`SessionState::Recording`] and emit
    ///    `status=recording` with capture backend / device / channels.
    ///
    /// Returns the new epoch so the caller (e.g. a chord-cancel
    /// dispatcher) can stamp the value and pass it back to
    /// [`Self::cancel`].
    pub fn start<W: Write>(&mut self, writer: &mut W) -> Result<u64, SessionError> {
        if !matches!(self.state, SessionState::Idle) {
            return Err(SessionError::AlreadyActive { state: self.state });
        }
        self.frame_buf.clear();
        self.epoch = self.epoch.wrapping_add(1);
        let id = self.epoch;
        self.state = SessionState::Opening { id };
        wire::emit_status(writer, "opening", &[])?;
        self.state = SessionState::Recording { id };
        wire::emit_status(writer, "recording", &self.capture_extras())?;
        Ok(id)
    }

    /// Append a chunk of post-resample, post-channel-select PCM to the
    /// capture buffer.
    ///
    /// Frames pushed while the session is not in [`SessionState::Recording`]
    /// are silently dropped — matching the Python capture mixin, which
    /// gates frame ingestion on `self.recording == True`. This makes the
    /// session safe to drive from a long-lived audio reader thread that
    /// outlives any single utterance.
    pub fn push_frame(&mut self, frame: &[f32]) {
        if matches!(self.state, SessionState::Recording { .. }) {
            self.frame_buf.extend_from_slice(frame);
        }
    }

    /// Close the recording, decide skip / hallucination / inject, and
    /// emit the matching status + utterance events.
    ///
    /// Mirrors `vp_dictate.py::_stop_and_transcribe`:
    /// * empty buffer → `status=no_text reason=no_audio`,
    ///   returns [`UtteranceOutcome::NoAudio`].
    /// * buffer below the min-duration floor →
    ///   `status=no_text reason=too_short`, returns
    ///   [`UtteranceOutcome::Skipped`].
    /// * backend error or empty / hallucinated text →
    ///   `status=no_text reason=…`, returns [`UtteranceOutcome::NoText`].
    /// * success → inject, emit `event=utterance`, return
    ///   [`UtteranceOutcome::Injected`].
    ///
    /// Always returns to [`SessionState::Idle`] before returning, even
    /// on error (matching Python's `finally:` that emits `status=ready`).
    pub fn stop_and_transcribe<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, SessionError> {
        if !matches!(self.state, SessionState::Recording { .. }) {
            // Mirrors `if not self.recording: return` in Python. No
            // events, no state change.
            return Ok(UtteranceOutcome::NotRecording);
        }
        let id = match self.state {
            SessionState::Recording { id } => id,
            // Unreachable thanks to the matches! above, but pattern-
            // matching keeps the compiler honest if SessionState gains
            // a variant later.
            _ => unreachable!("guarded by matches! above"),
        };
        self.state = SessionState::Transcribing { id };

        // Drain the buffer up-front so any early-return path leaves the
        // session ready for the next press.
        let buf = std::mem::take(&mut self.frame_buf);

        let outcome = self.run_transcription(writer, &buf);
        // Always settle back to Idle + emit `status=ready`, matching
        // Python's `finally: _emit_worker_event(..., state="ready")`.
        self.state = SessionState::Idle;
        wire::emit_status(writer, "ready", &self.capture_extras())?;
        outcome
    }

    /// The post-`Transcribing` branch from `_stop_and_transcribe`.
    /// Split out so the `finally`-equivalent reset + `status=ready` at
    /// the bottom of `stop_and_transcribe` cannot drift out of sync
    /// with the early-return paths.
    fn run_transcription<W: Write>(
        &mut self,
        writer: &mut W,
        buf: &[f32],
    ) -> Result<UtteranceOutcome, SessionError> {
        // No frames ever pushed — Python's `if not self.frames:` branch.
        if buf.is_empty() {
            wire::emit_status(writer, "no_text", &[("reason", Value::from("no_audio"))])?;
            return Ok(UtteranceOutcome::NoAudio);
        }

        // Status flip to `transcribing` mirrors the Python emit that
        // immediately precedes the skip-gate / transcribe call.
        wire::emit_status(writer, "transcribing", &self.capture_extras())?;

        // Min-duration gate. Delegates to the existing skip helper so
        // the threshold semantics (0.3 s floor, fractional comparison)
        // stay in lock-step with `Dictate._should_skip_pcm`.
        let skip = crate::dictate::skip::should_skip(buf.len(), self.config.min_record_seconds);
        if let Some(reason) = skip.reason() {
            let recording_s = buf.len() as f64 / SR as f64;
            wire::emit_status(
                writer,
                "no_text",
                &[
                    ("reason", Value::from(reason)),
                    ("recording_s", json!(wire::round2(recording_s))),
                ],
            )?;
            return Ok(UtteranceOutcome::Skipped { reason });
        }

        match self.transcribe.transcribe(buf, SR) {
            Err(err) => {
                // Python wraps the error and treats it as no_speech.
                wire::emit_status(
                    writer,
                    "no_text",
                    &[
                        ("reason", Value::from("no_speech")),
                        ("error", Value::from(err.to_string())),
                    ],
                )?;
                Ok(UtteranceOutcome::NoText {
                    reason: "no_speech",
                })
            }
            Ok(result) if result.text.is_empty() => {
                wire::emit_status(writer, "no_text", &[("reason", Value::from("empty"))])?;
                Ok(UtteranceOutcome::NoText { reason: "empty" })
            }
            Ok(result) if result.is_hallucination => {
                wire::emit_status(writer, "no_text", &[("reason", Value::from("no_speech"))])?;
                Ok(UtteranceOutcome::NoText {
                    reason: "no_speech",
                })
            }
            Ok(result) => {
                // PR 5 will run post-processing + format commands here;
                // for now the backend's text is what goes to the
                // injector. The trait split keeps this PR pure-logic.
                let text = result.text.clone();
                if let Err(err) = self.inject.inject(&text) {
                    // Python logs and continues — the utterance event
                    // still fires with the text we attempted to inject.
                    // Surface the failure on the utterance event so the
                    // supervisor can decide whether to retry / show UI.
                    wire::emit_utterance(writer, &text, &result, Some(err.to_string()))?;
                    return Ok(UtteranceOutcome::Injected { text, result });
                }
                wire::emit_utterance(writer, &text, &result, None)?;
                Ok(UtteranceOutcome::Injected { text, result })
            }
        }
    }

    /// Discard the in-flight recording if `requested_epoch` matches the
    /// current recording generation.
    ///
    /// This is the chord-cancel race guard. The chord-cancel callback in
    /// `vp_keys` runs on a daemon thread that may be delayed past a
    /// release + re-press; it captures the recording generation at
    /// chord-detection time and passes it back here. Without the epoch
    /// guard a stale cancel would silently discard the NEW recording.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    ///
    /// On a matching epoch:
    /// * drops the buffered frames,
    /// * settles back to [`SessionState::Idle`],
    /// * emits `status=cancelled reason=chord` then `status=ready`,
    ///   matching Python.
    ///
    /// On a stale epoch (or while idle): no-op, no events, no state
    /// change.
    pub fn cancel<W: Write>(
        &mut self,
        requested_epoch: u64,
        writer: &mut W,
    ) -> Result<(), SessionError> {
        let active_id = match self.state {
            SessionState::Recording { id } | SessionState::Opening { id } => id,
            // Idle / Transcribing: nothing to cancel. Transcribing is
            // racy in Python too — the cancel arrives after capture has
            // already stopped, so the audio is already on its way to
            // the model; matching Python we no-op.
            _ => return Ok(()),
        };
        if requested_epoch != active_id {
            // Stale cancel — the NEW recording's epoch is `active_id`,
            // not `requested_epoch`. Must NOT discard. This is the
            // load-bearing race-correctness check.
            return Ok(());
        }
        self.frame_buf.clear();
        self.state = SessionState::Idle;
        wire::emit_status(writer, "cancelled", &[("reason", Value::from("chord"))])?;
        wire::emit_status(writer, "ready", &self.capture_extras())?;
        Ok(())
    }

    /// The capture-backend / audio-device / capture-channels extras
    /// every status event carries. Empty strings and zero values are
    /// dropped by [`wire::emit_status`], so an unconfigured session
    /// emits a clean minimal event.
    fn capture_extras(&self) -> [(&'static str, Value); 3] {
        [
            (
                "capture_backend",
                Value::from(self.config.capture_backend.clone()),
            ),
            (
                "audio_device",
                Value::from(self.config.audio_device.clone()),
            ),
            (
                "capture_channels",
                Value::from(self.config.capture_channels),
            ),
        ]
    }
}
