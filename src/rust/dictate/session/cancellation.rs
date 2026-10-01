//! cancellation for the session-owned utterance lifecycle.

use super::*;

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
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
        self.recording_audio_loss = None;
        // Chord-cancel parity with `vp_dictate.py::_cancel_and_discard`
        // (lines 662-681): Python routes the cancel THROUGH
        // `_stop_and_transcribe`, which fires `play_cue("stop")` before
        // the discard branch runs. The Rust `cancel()` shortcuts around
        // `stop_and_transcribe`, so we play the cue explicitly here to
        // keep the audible "recording ended" signal even when the
        // clip is dropped.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Stop);
        // Preview worker must stop (parity: _cancel_and_discard -> _stop_and_transcribe).
        if let Some(engine) = self.preview.as_ref() {
            engine.notify_stop();
        }
        // Audio ducking must exit explicitly here — Rust cancel() shortcuts
        // around stop_and_transcribe, so restore background volume manually.
        self.audio_ducker.exit();
        wire::emit_status_with_output(
            writer,
            "cancelled",
            &[("reason", Value::from("chord"))],
            self.worker_event_output,
        )?;
        wire::emit_status_with_output(
            writer,
            "ready",
            &self.capture_extras(),
            self.worker_event_output,
        )?;
        Ok(())
    }
}
