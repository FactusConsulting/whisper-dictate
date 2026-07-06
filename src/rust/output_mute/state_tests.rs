//! Save/restore state-machine unit tests for [`super::MuteController`].
//!
//! Codex P2 (state.rs:1, PR #440) — pulled out of `state.rs` (which was
//! 611 lines with tests inline) into a sibling file so the impl file
//! stays under AGENTS.md's ~500-LOC modularity cap. Wired in via
//! `#[path = "state_tests.rs"] mod tests;` from `state.rs`.

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
