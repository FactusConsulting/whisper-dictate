//! Save/restore state machine for the auto-mute feature.
//!
//! The controller owns exactly enough state to answer:
//!
//! * "did *we* mute the output for the current recording?" — so a stop
//!   after a no-op start (backend error, already muted) does not
//!   accidentally unmute something the user wanted muted;
//! * "what was the user's mute state before we touched it?" — so we
//!   restore that precise value on stop, whether or not the user had it
//!   already muted.
//!
//! Two nested transitions (recording → recording, or stop → stop) are
//! idempotent so a duplicate worker event never desynchronises the
//! saved state. The controller also emits a [`Drop`] restore so a
//! panic between start and stop still returns the output to the user's
//! prior state.

use std::fmt;
use std::sync::Arc;

use crate::output_mute::OutputMuteBackend;

/// Non-fatal errors from the OS boundary.
///
/// Kept as a flat `String`-payload enum so backends can attach a
/// human-readable cause without leaking OS-specific types across the
/// module boundary. The controller downgrades any error to a logged
/// warning — a failed mute must never break dictation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuteError {
    /// The required OS tool / API was not available at all
    /// (e.g. `pactl` not on `PATH`, PowerShell exited with an error,
    /// COM initialization failed). Payload is a short human tag.
    Unavailable(String),
    /// The tool ran but returned an unexpected result we could not
    /// parse (e.g. `pactl get-sink-mute` printed something we did not
    /// recognise). Payload is the raw output for the log line.
    UnexpectedOutput(String),
    /// The tool failed with a non-zero exit / HRESULT. Payload is
    /// the diagnostic (stderr / HRESULT hex).
    OsFailure(String),
}

impl fmt::Display for MuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MuteError::Unavailable(cause) => {
                write!(f, "output-mute backend unavailable: {cause}")
            }
            MuteError::UnexpectedOutput(cause) => {
                write!(f, "output-mute backend gave unexpected output: {cause}")
            }
            MuteError::OsFailure(cause) => write!(f, "output-mute OS call failed: {cause}"),
        }
    }
}

impl std::error::Error for MuteError {}

/// What we remembered about the user's mute state at recording start.
///
/// The two-variant shape (rather than a raw `Option<bool>`) keeps the
/// intent explicit at the call sites in the state machine and its tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorMuteState {
    /// The output was already muted before we started — nothing for
    /// us to change, and nothing for us to restore.
    AlreadyMuted,
    /// The output was unmuted; we muted it and owe the user a restore.
    Unmuted,
}

/// Live state of the controller across recording start/stop pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControllerPhase {
    /// Idle. `on_recording_stop` is a no-op in this phase.
    Idle,
    /// Recording active. Payload is what we saved at start so we know
    /// how to restore on stop / drop.
    Recording(PriorMuteState),
}

/// Save/restore state machine tied to a single backend.
///
/// Public methods are intentionally infallible (return `()`) because
/// the controller is called from the audio hot path: an unavailable
/// backend must degrade to a warning, not propagate. Errors are
/// captured on [`MuteController::last_error`] so tests can assert on
/// them without the caller needing a `Result` at every event.
pub struct MuteController {
    backend: Arc<dyn OutputMuteBackend>,
    phase: ControllerPhase,
    last_error: Option<MuteError>,
}

impl MuteController {
    /// Build a controller around a backend. The controller starts in
    /// the idle phase — call [`Self::on_recording_start`] to arm it.
    pub fn new(backend: Arc<dyn OutputMuteBackend>) -> Self {
        Self {
            backend,
            phase: ControllerPhase::Idle,
            last_error: None,
        }
    }

    /// Whether we currently believe the output is muted *by us*.
    pub fn is_muting(&self) -> bool {
        matches!(
            self.phase,
            ControllerPhase::Recording(PriorMuteState::Unmuted)
        )
    }

    /// The last error observed on any OS call, cleared when the next
    /// call succeeds. Exposed for logging + tests; production callers
    /// should not treat it as a fatal signal.
    pub fn last_error(&self) -> Option<&MuteError> {
        self.last_error.as_ref()
    }

    /// Called when the worker transitions into the `recording` state.
    ///
    /// Idempotent: a duplicate start (e.g. two `state="recording"`
    /// events for one session) does not re-save state. If the backend
    /// reports the output is already muted we remember that so the
    /// stop is a no-op.
    pub fn on_recording_start(&mut self) {
        if !matches!(self.phase, ControllerPhase::Idle) {
            return;
        }
        // Codex P2 (state.rs:175, PR #440) — pin the current default
        // endpoint so a mid-recording device switch does not leave the
        // original speakers muted / silently unmute a newly-selected
        // device. Backends that don't implement pinning fall back to
        // the no-op default. Failure to pin is non-fatal: we log it
        // and continue with the previous "always-default" behaviour so
        // a transient `pactl get-default-sink` hiccup does not disable
        // the whole feature.
        if let Err(err) = self.backend.pin_current_endpoint() {
            self.last_error = Some(err);
        }
        match self.backend.get_mute() {
            Ok(true) => {
                self.phase = ControllerPhase::Recording(PriorMuteState::AlreadyMuted);
                self.last_error = None;
            }
            Ok(false) => match self.backend.set_mute(true) {
                Ok(()) => {
                    self.phase = ControllerPhase::Recording(PriorMuteState::Unmuted);
                    self.last_error = None;
                }
                Err(err) => {
                    // Failed to mute — treat as "not our mute" so stop
                    // does nothing. Recording continues; only auto-mute
                    // is skipped.
                    self.last_error = Some(err);
                    self.phase = ControllerPhase::Idle;
                    self.backend.clear_endpoint_pin();
                }
            },
            Err(err) => {
                // Cannot even read the state — bail without muting.
                self.last_error = Some(err);
                self.phase = ControllerPhase::Idle;
                self.backend.clear_endpoint_pin();
            }
        }
    }

    /// Called when the worker leaves the `recording` state (any
    /// terminal state: transcribing, ready, no_text, cancelled, error,
    /// capture_lost).
    ///
    /// Idempotent: a stop with no matching start is a no-op. Restores
    /// exactly the mute state that was in effect before we changed it.
    ///
    /// Codex P2 (state.rs:168, PR #440) — on a `set_mute(false)`
    /// failure we KEEP the phase in `Recording(Unmuted)` so:
    ///
    /// 1. [`Drop`] still retries the restore (previous behaviour
    ///    silently dropped the pending restore because we had already
    ///    transitioned to Idle).
    /// 2. A duplicate stop event (e.g. the user releases PTT again)
    ///    retries the restore rather than becoming a no-op.
    /// 3. A follow-up `on_recording_start` observes the still-muted
    ///    output but does not re-save the state (idempotent guard),
    ///    so once the transient backend/endpoint hiccup clears the
    ///    matching stop unmutes correctly.
    ///
    /// The previous behaviour transitioned to Idle regardless, which
    /// meant the next start observed `Ok(true)` from the backend, saved
    /// it as `AlreadyMuted`, and the following stop skipped the unmute
    /// entirely — leaving the user's speakers muted indefinitely until
    /// manual intervention.
    pub fn on_recording_stop(&mut self) {
        let prior = match self.phase {
            ControllerPhase::Idle => return,
            ControllerPhase::Recording(prior) => prior,
        };
        if matches!(prior, PriorMuteState::AlreadyMuted) {
            // Nothing to restore; the output was already muted when we
            // started and we did not change it.
            self.phase = ControllerPhase::Idle;
            self.last_error = None;
            // Codex P2 (state.rs:175, PR #440) — release the endpoint
            // pin so the next start re-resolves the (possibly changed)
            // default.
            self.backend.clear_endpoint_pin();
            return;
        }
        match self.backend.set_mute(false) {
            Ok(()) => {
                self.phase = ControllerPhase::Idle;
                self.last_error = None;
                self.backend.clear_endpoint_pin();
            }
            Err(err) => {
                // Keep the phase in Recording(Unmuted) so Drop / a
                // follow-up stop retries. Codex P2 state.rs:168, PR #440.
                // NOTE: the endpoint pin is DELIBERATELY not cleared
                // here — a duplicate stop retry (or the Drop restore)
                // must target the ORIGINAL endpoint, not whatever the
                // default is now.
                self.last_error = Some(err);
            }
        }
    }
}

impl Drop for MuteController {
    fn drop(&mut self) {
        // Panic safety: if we muted and never got a matching stop
        // (thread panic, abrupt shutdown), still put the user's audio
        // back the way we found it. Errors here are silent — we're
        // in a drop.
        if self.is_muting() {
            let _ = self.backend.set_mute(false);
        }
        // Codex P2 (state.rs:175, PR #440) — release the endpoint pin
        // regardless of phase so backends that hold onto endpoint
        // handles (e.g. a sink name captured at start) don't leak state
        // across controller lifetimes.
        self.backend.clear_endpoint_pin();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test double that records every `set_mute` call and lets a test
    /// script the current mute state + inject errors.
    #[derive(Default)]
    struct RecordingBackend {
        state: Mutex<BackendState>,
    }

    #[derive(Default)]
    struct BackendState {
        muted: bool,
        get_err: Option<MuteError>,
        set_err: Option<MuteError>,
        set_calls: Vec<bool>,
        /// Codex P2 (state.rs:175, PR #440) — count pin/clear calls so
        /// endpoint-lifecycle assertions work at the state.rs layer
        /// without needing a real backend.
        pin_calls: usize,
        clear_calls: usize,
    }

    impl RecordingBackend {
        fn set_initial_muted(&self, muted: bool) {
            self.state.lock().unwrap().muted = muted;
        }
        fn set_get_err(&self, err: MuteError) {
            self.state.lock().unwrap().get_err = Some(err);
        }
        fn set_set_err(&self, err: MuteError) {
            self.state.lock().unwrap().set_err = Some(err);
        }
        fn set_calls(&self) -> Vec<bool> {
            self.state.lock().unwrap().set_calls.clone()
        }
        fn muted(&self) -> bool {
            self.state.lock().unwrap().muted
        }
        fn pin_calls(&self) -> usize {
            self.state.lock().unwrap().pin_calls
        }
        fn clear_calls(&self) -> usize {
            self.state.lock().unwrap().clear_calls
        }
    }

    impl OutputMuteBackend for RecordingBackend {
        fn get_mute(&self) -> Result<bool, MuteError> {
            let mut state = self.state.lock().unwrap();
            if let Some(err) = state.get_err.take() {
                return Err(err);
            }
            Ok(state.muted)
        }

        fn set_mute(&self, muted: bool) -> Result<(), MuteError> {
            let mut state = self.state.lock().unwrap();
            if let Some(err) = state.set_err.take() {
                return Err(err);
            }
            state.muted = muted;
            state.set_calls.push(muted);
            Ok(())
        }

        fn pin_current_endpoint(&self) -> Result<(), MuteError> {
            self.state.lock().unwrap().pin_calls += 1;
            Ok(())
        }

        fn clear_endpoint_pin(&self) {
            self.state.lock().unwrap().clear_calls += 1;
        }
    }

    fn controller(backend: Arc<RecordingBackend>) -> MuteController {
        MuteController::new(backend as Arc<dyn OutputMuteBackend>)
    }

    #[test]
    fn start_mutes_and_stop_restores_when_output_was_unmuted() {
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        assert!(controller.is_muting());
        assert!(backend.muted());

        controller.on_recording_stop();
        assert!(!controller.is_muting());
        assert!(!backend.muted());
        assert_eq!(backend.set_calls(), vec![true, false]);
    }

    #[test]
    fn start_is_noop_when_output_was_already_muted() {
        let backend = Arc::new(RecordingBackend::default());
        backend.set_initial_muted(true);
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        assert!(!controller.is_muting()); // we did NOT mute; user did.
        controller.on_recording_stop();
        // The state must remain muted — we never owned that mute.
        assert!(backend.muted());
        // No set_mute calls at all.
        assert!(backend.set_calls().is_empty());
    }

    #[test]
    fn duplicate_start_events_are_idempotent() {
        // Guard against duplicate `state="recording"` worker events.
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        // A second start must not touch the backend — otherwise we
        // could overwrite our saved prior state with the state we
        // ourselves just installed.
        controller.on_recording_start();

        assert_eq!(backend.set_calls(), vec![true]);
        controller.on_recording_stop();
        assert_eq!(backend.set_calls(), vec![true, false]);
    }

    #[test]
    fn stop_without_matching_start_is_noop() {
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_stop();
        controller.on_recording_stop();

        assert!(backend.set_calls().is_empty());
        assert!(!backend.muted());
    }

    #[test]
    fn duplicate_stop_events_do_not_double_restore() {
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        controller.on_recording_stop();
        controller.on_recording_stop();

        assert_eq!(backend.set_calls(), vec![true, false]);
    }

    #[test]
    fn get_mute_error_records_and_skips_muting() {
        let backend = Arc::new(RecordingBackend::default());
        backend.set_get_err(MuteError::Unavailable("no pactl".to_owned()));
        let mut controller = controller(backend.clone());

        controller.on_recording_start();

        assert!(!controller.is_muting());
        assert!(backend.set_calls().is_empty());
        assert!(matches!(
            controller.last_error(),
            Some(MuteError::Unavailable(_))
        ));

        // A subsequent stop is a no-op and does not overwrite the
        // stored diagnostic.
        controller.on_recording_stop();
        assert!(backend.set_calls().is_empty());
    }

    #[test]
    fn set_mute_error_at_start_records_and_stays_idle() {
        let backend = Arc::new(RecordingBackend::default());
        backend.set_set_err(MuteError::OsFailure("HRESULT 0x88890001".to_owned()));
        let mut controller = controller(backend.clone());

        controller.on_recording_start();

        assert!(!controller.is_muting());
        assert_eq!(backend.set_calls(), Vec::<bool>::new());
        assert!(matches!(
            controller.last_error(),
            Some(MuteError::OsFailure(_))
        ));

        // Stop must not attempt to restore anything.
        controller.on_recording_stop();
        assert!(backend.set_calls().is_empty());
    }

    #[test]
    fn set_mute_error_at_stop_keeps_restore_pending_for_retry() {
        // Codex P2 (state.rs:168, PR #440): a transient set_mute(false)
        // failure MUST NOT drop the pending restore. Previous behaviour
        // transitioned to Idle regardless, then the next start saw
        // muted=true, saved it as AlreadyMuted, and the following stop
        // skipped the unmute — leaving the user's speakers muted
        // indefinitely.
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        assert!(controller.is_muting());

        // Inject an error on the restore call. Because the mock returns
        // the error before mutating state, the backend still reports
        // muted=true.
        backend.set_set_err(MuteError::OsFailure("HRESULT 0x88890001".to_owned()));
        controller.on_recording_stop();

        // The controller keeps the restore pending so a follow-up stop
        // (duplicate event / user retry) OR Drop retries the unmute.
        assert!(
            controller.is_muting(),
            "pending restore must survive a transient failure"
        );
        assert!(matches!(
            controller.last_error(),
            Some(MuteError::OsFailure(_))
        ));

        // A duplicate stop retries the restore. This second attempt
        // succeeds (no injected error) and both flips the backend and
        // transitions to Idle.
        controller.on_recording_stop();
        assert!(!controller.is_muting(), "successful retry clears Recording");
        assert!(!backend.muted(), "duplicate stop retries the restore");
        assert_eq!(backend.set_calls(), vec![true, false]);
        assert!(controller.last_error().is_none());
    }

    #[test]
    fn drop_retries_restore_when_stop_failed() {
        // Codex P2 (state.rs:168, PR #440): the drop-time restore is
        // the safety net that recovers from a transient stop failure.
        // Preserving the phase in Recording(Unmuted) makes is_muting()
        // stay true so the Drop impl fires its final restore attempt.
        let backend = Arc::new(RecordingBackend::default());
        {
            let mut controller = controller(backend.clone());
            controller.on_recording_start();
            backend.set_set_err(MuteError::OsFailure("HRESULT 0x88890001".to_owned()));
            controller.on_recording_stop();
            assert!(controller.is_muting());
            // Drop fires here on scope exit.
        }
        // The drop restore ran (no injected error left) and unmuted
        // the backend — no leftover mute.
        assert!(!backend.muted(), "Drop must retry the pending restore");
        assert_eq!(backend.set_calls(), vec![true, false]);
    }

    #[test]
    fn start_pins_endpoint_and_stop_clears_it() {
        // Codex P2 (state.rs:175, PR #440): every recording start must
        // pin the endpoint before reading state, and every successful
        // stop must clear the pin so the next start re-resolves the
        // (possibly changed) default.
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        assert_eq!(backend.pin_calls(), 1, "start must pin the endpoint");
        assert_eq!(backend.clear_calls(), 0);

        controller.on_recording_stop();
        assert_eq!(
            backend.clear_calls(),
            1,
            "successful stop must clear the pin"
        );
        assert_eq!(backend.pin_calls(), 1);
    }

    #[test]
    fn stop_does_not_clear_pin_when_restore_failed() {
        // Codex P2 (state.rs:175, PR #440): if the unmute fails the
        // pin MUST survive so Drop / a retry restore targets the
        // originally-muted endpoint, not whatever the default is now.
        let backend = Arc::new(RecordingBackend::default());
        let mut controller = controller(backend.clone());

        controller.on_recording_start();
        backend.set_set_err(MuteError::OsFailure("boom".to_owned()));
        controller.on_recording_stop();

        assert_eq!(
            backend.clear_calls(),
            0,
            "failed restore MUST NOT clear the endpoint pin"
        );
    }

    #[test]
    fn drop_clears_pin_regardless_of_phase() {
        // Codex P2 (state.rs:175, PR #440): Drop always clears the
        // pin so a shared backend does not carry state across
        // controller lifetimes.
        let backend = Arc::new(RecordingBackend::default());
        {
            let mut controller = controller(backend.clone());
            controller.on_recording_start();
            // Drop with a live recording — Drop restores AND clears.
        }
        assert!(
            backend.clear_calls() >= 1,
            "Drop must clear the endpoint pin"
        );
    }

    #[test]
    fn drop_restores_mute_when_recording_was_active() {
        let backend = Arc::new(RecordingBackend::default());
        {
            let mut controller = controller(backend.clone());
            controller.on_recording_start();
            assert!(backend.muted());
            // Drop with no matching stop — simulates panic.
        }
        assert!(!backend.muted());
    }

    #[test]
    fn drop_does_not_restore_when_output_was_already_muted() {
        let backend = Arc::new(RecordingBackend::default());
        backend.set_initial_muted(true);
        {
            let mut controller = controller(backend.clone());
            controller.on_recording_start();
        }
        // We never touched the mute — it must remain muted after drop.
        assert!(backend.muted());
        assert!(backend.set_calls().is_empty());
    }

    #[test]
    fn drop_is_noop_when_idle() {
        let backend = Arc::new(RecordingBackend::default());
        {
            let controller = controller(backend.clone());
            drop(controller);
        }
        assert!(backend.set_calls().is_empty());
    }

    #[test]
    fn mute_error_display_covers_every_variant() {
        // Smoke-test the Display impl so any future variant addition
        // has to touch the test too. The exact wording is not
        // stability-guaranteed — only that it mentions the payload.
        for err in [
            MuteError::Unavailable("cause-a".to_owned()),
            MuteError::UnexpectedOutput("cause-b".to_owned()),
            MuteError::OsFailure("cause-c".to_owned()),
        ] {
            let text = err.to_string();
            assert!(text.contains("cause-"));
        }
    }
}
