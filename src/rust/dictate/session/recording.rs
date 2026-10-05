//! recording for the session-owned utterance lifecycle.

use super::*;

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Open a fresh utterance.
    ///
    /// Mirrors the canonical `_start` ordering:
    /// 1. early-return if a recording is already in flight (no events,
    ///    no state change);
    /// 2. clear the frame buffer;
    /// 3. bump the recording epoch (the chord-race generation counter);
    /// 4. emit `status=opening`;
    /// 5. transition to [`SessionState::Recording`] and emit
    ///    `status=recording` with capture backend / device / channels.
    ///
    /// Returns the new epoch so the caller (e.g. a chord-cancel
    /// dispatcher) can stamp the value and pass it back to
    /// [`Self::cancel`].
    pub fn start<W: Write>(&mut self, writer: &mut W) -> Result<u64, SessionError> {
        let id = self.begin_recording(writer)?;
        self.announce_recording(writer)?;
        Ok(id)
    }

    /// First half of [`Self::start`]: reset the buffer, resolve the target
    /// profile, emit `status=opening` and enter [`SessionState::Recording`]
    /// so frames are buffered -- without telling the user to speak yet. The
    /// native runtime opens the microphone between this and
    /// [`Self::announce_recording`] (#323).
    pub(crate) fn begin_recording<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<u64, SessionError> {
        if !matches!(self.state, SessionState::Idle) {
            return Err(SessionError::AlreadyActive { state: self.state });
        }
        self.frame_buf.clear();
        self.max_record_cap_logged = false;
        self.recording_audio_loss = None;
        self.epoch = self.epoch.wrapping_add(1);
        let id = self.epoch;
        // Per-utterance target-profile resolution, called BEFORE the
        // "opening" event fires. Kept
        // ahead of the state flip so the emitted `[worker-event]
        // event=profile` line lands adjacent to the utterance it
        // applies to, and so the `apply_active_profile` reset happens
        // before `stop_and_transcribe` reads `self.config`. A None
        // matcher is a zero-cost no-op (see `resolve_profile`).
        let (window, applied) = self.resolve_profile();
        self.active_profile = applied;
        // Stash the window snapshot so the utterance event carries the
        // target the user was focused on at PTT-press (not at
        // inject-time). metrics-schema follow-up.
        self.active_window = if window.is_empty() {
            None
        } else {
            Some(window.clone())
        };
        self.apply_active_profile();
        self.max_record_samples = max_record_samples(self.config.max_record_seconds);
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] recording buffer reset max_record_samples={:?}",
                self.max_record_samples
            );
        }
        if self.profile_matcher.is_some() {
            emit_profile_status(
                writer,
                &window,
                self.active_profile.as_ref(),
                self.worker_event_output,
            )?;
        }
        self.state = SessionState::Opening { id };
        // Restore Idle if status output fails; callers otherwise cannot start
        // a new recording. The epoch remains monotonic, so gaps are harmless.
        if let Err(e) =
            wire::emit_status_with_output(writer, "opening", &[], self.worker_event_output)
        {
            self.state = SessionState::Idle;
            return Err(e);
        }
        self.state = SessionState::Recording { id };
        Ok(id)
    }

    /// Second half of [`Self::start`]: emit `status=recording`, play the
    /// start cue, arm the preview and enter audio ducking. The native runtime
    /// calls this only once the microphone is open, so the user is never told
    /// to speak before capture exists (#323). A no-op outside a recording.
    pub(crate) fn announce_recording<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Recording { .. }) {
            return Ok(());
        }
        if let Err(e) = wire::emit_status_with_output(
            writer,
            "recording",
            &self.capture_extras(),
            self.worker_event_output,
        ) {
            self.state = SessionState::Idle;
            return Err(e);
        }
        // Audible press cue -- `play_cue("start")` fires AFTER the
        // "listening..." status flip. `NoOpCueSink` (the default) makes this
        // a no-op; production wires `SystemCueSink`, which itself
        // gates on `VOICEPI_FEEDBACK_SOUNDS`. Non-blocking + never
        // fails, so no error path is threaded here.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Start);
        // Arm the live-preview worker for this recording (parity: `_start_preview`).
        if let Some(engine) = self.preview.as_ref() {
            engine.notify_start();
        }
        // Audio ducking -- `audio_ducker.enter()` right before the
        // capture handshake.
        // Infallible by trait contract; failures swallowed into a one-shot warning.
        self.audio_ducker.enter();
        Ok(())
    }

    /// Undo [`Self::begin_recording`] when no microphone could be opened:
    /// drop the (empty) buffer, return to Idle and emit `status=ready`. No
    /// stop cue is played because no start cue was. A no-op outside a
    /// recording.
    pub(crate) fn abandon_recording<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Recording { .. }) {
            return Ok(());
        }
        self.frame_buf.clear();
        self.state = SessionState::Idle;
        self.recording_audio_loss = None;
        wire::emit_status_with_output(
            writer,
            "ready",
            &self.capture_extras(),
            self.worker_event_output,
        )
    }

    /// True while recording once the buffer reached `max_record_s`; the
    /// native capture lifecycle then closes the microphone (#323).
    #[cfg(feature = "audio-capture")]
    #[cfg_attr(
        not(all(feature = "whisper-rs-local", feature = "rust-injection")),
        allow(dead_code)
    )]
    pub(crate) fn recording_buffer_full(&self) -> bool {
        matches!(self.state, SessionState::Recording { .. })
            && self
                .max_record_samples
                .is_some_and(|cap| self.frame_buf.len() >= cap)
    }

    /// Append a chunk of post-resample, post-channel-select PCM to the
    /// capture buffer.
    ///
    /// Frames pushed while the session is not in [`SessionState::Recording`]
    /// are silently dropped. This makes the
    /// session safe to drive from a long-lived audio reader thread that
    /// outlives any single utterance.
    pub fn push_frame(&mut self, frame: &[f32]) {
        if matches!(self.state, SessionState::Recording { .. }) {
            let accepted_len = self
                .max_record_samples
                .map(|cap| cap.saturating_sub(self.frame_buf.len()).min(frame.len()))
                .unwrap_or(frame.len());
            let accepted = &frame[..accepted_len];
            self.frame_buf.extend_from_slice(accepted);
            // Forward a copy to the preview worker so it can accumulate its
            // own sliding-window buffer without locking on the session's
            // hot-path Vec. `push_frame` on the engine is a channel send;
            // if the receiver is missing (shouldn't happen while the engine
            // is alive) the message is silently dropped.
            if let Some(engine) = self.preview.as_ref() {
                engine.push_frame(accepted);
            }
            if accepted_len < frame.len() && !self.max_record_cap_logged {
                self.max_record_cap_logged = true;
                crate::diag::log!(
                    "[runtime] recording reached max_record_s; discarding additional audio cap_samples={} dropped_samples={}",
                    self.max_record_samples.unwrap_or(0),
                    frame.len() - accepted_len
                );
                if crate::diag::trace_enabled() {
                    crate::diag::log!(
                        "[runtime/trace] recording cap state buffered_samples={} incoming_samples={} accepted_samples={accepted_len}",
                        self.frame_buf.len(),
                        frame.len()
                    );
                }
            }
        }
    }

    /// Close the recording, decide skip / hallucination / inject, and
    /// emit the matching status + utterance events.
    ///
    /// Stop-and-transcribe ordering:
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
    /// on error (the settling `status=ready` always fires).
    pub fn stop_and_transcribe<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, SessionError> {
        if !self.begin_transcription() {
            return Ok(UtteranceOutcome::NotRecording);
        }
        self.finish_transcription(writer)
    }

    /// Only the state transition: safe under the capture teardown guard.
    /// Do not invoke cues, preview workers, backends, or sinks in this phase.
    pub(crate) fn begin_transcription(&mut self) -> bool {
        if !matches!(self.state, SessionState::Recording { .. }) {
            // Not recording: no events, no state change.
            return false;
        }
        let id = match self.state {
            SessionState::Recording { id } => id,
            // Unreachable thanks to the matches! above, but pattern-
            // matching keeps the compiler honest if SessionState gains
            // a variant later.
            _ => unreachable!("guarded by matches! above"),
        };
        self.state = SessionState::Transcribing { id };
        true
    }

    /// Attach the joined capture's diagnostics to this accepted utterance.
    pub(crate) fn set_recording_audio_loss(
        &mut self,
        loss: Option<audio_loss::RecordingAudioLoss>,
    ) {
        self.recording_audio_loss = loss;
    }

    /// Run an already-accepted utterance without holding the capture guard.
    pub(crate) fn finish_transcription<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, SessionError> {
        debug_assert!(matches!(self.state, SessionState::Transcribing { .. }));

        // Audible release cue -- `play_cue("stop")` fires after capture
        // is stopped and BEFORE the transcribe pass runs. Fires exactly
        // once per utterance (guarded by the Recording -> Transcribing
        // transition above); a `NotRecording` early-return never reaches
        // this line.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Stop);

        // Signal the live-preview worker to stop BEFORE the final pass runs
        // so no stale `state="preview"` events land on the wire while the
        // authoritative transcribe result is being computed, just before
        // the final transcribe pass.
        if let Some(engine) = self.preview.as_ref() {
            engine.notify_stop();
        }

        // Drain the buffer up-front so any early-return path leaves the
        // session ready for the next press.
        let buf = std::mem::take(&mut self.frame_buf);

        // Restore audio-ducking BEFORE running transcription: doing it
        // here (not after transcription) means background media returns
        // to its normal level the moment the user releases PTT —
        // transcription can take seconds and we don't want to keep other
        // apps dampened that whole time. `exit()` is infallible by trait
        // contract.
        self.audio_ducker.exit();
        let outcome = self.run_transcription(writer, &buf);
        // Includes skipped/empty/error paths, before a ready-event write can
        // fail. A recording without an utterance must never flag the next one.
        self.recording_audio_loss = None;
        // This is an accepted recording attempt, unlike startup and cancel.
        // It has finished even when transcription or injection reported an
        // error; the cue signals completion, not success.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Done);
        // Always settle back to Idle + emit `status=ready`.
        self.state = SessionState::Idle;
        wire::emit_status_with_output(
            writer,
            "ready",
            &self.capture_extras(),
            self.worker_event_output,
        )?;
        outcome
    }
}
