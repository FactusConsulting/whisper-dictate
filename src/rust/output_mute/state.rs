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

// Codex P2 (state.rs:1, PR #440) — tests live in a sibling file
// (`state_tests.rs`) so this module stays under AGENTS.md's ~500-LOC
// modularity cap. Impl + tests inline previously weighed 611 lines.
#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
